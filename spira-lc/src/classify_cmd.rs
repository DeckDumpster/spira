//! `spira-lc classify` — the one-time migration classifier (design §4). Assigns every
//! existing work bead, and every open batch, a lifecycle state by reading the legacy
//! records (bd, batch membership files, git) in the fixed precedence
//! `lifecycle::classify` implements, then writes one event per decision with the deciding
//! rule as evidence. Idempotent: a bead that already has a row is left untouched and no new
//! event is written for it, which is what makes re-running this command a no-op. `--dry-run`
//! computes and reports the same decisions without writing anything.
//!
//! This binary never simulates a machine transition here — there is no prior row for a
//! freshly-migrated bead to compare-and-swap against — so it writes the classified row
//! directly via [`crate::db::Conn::insert_if_absent_and_log`], never through
//! `lifecycle::bead::apply`.
//!
//! `--every-bead` is install's one-time population (sp-k62xz8): Spira installed on top of an
//! existing beads database gives every bead in it a row, whatever its labels — the roster is
//! the whole database, each bead judged by its own `repo:` label's repository (or by bd and
//! the ledger alone when it has none). `--home` may then be omitted: the repository table is
//! the one `$SPIRA_TOML` names.
//!
//! Every path flag is an override: `--bd-db`, `--bd-bin` and `--queue-dir`
//! default to `spira.db`, `spira.bd` and `spira.queue_dir` from the
//! same document, and `--home` (a directory holding a legacy `spira.conf`) is only needed to
//! classify against one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lifecycle::bead::HoldKind;
use lifecycle::classify::{self, BeadFacts};
use lifecycle::delivery::DeliveryState;
use serde_json::{json, Value};

use crate::db::{Conn, EventRecord};
use crate::{bd_facts, git_evidence, legacy_files, repo_config, rows};

const CANNOT_TELL: i32 = 2;

#[derive(Debug)]
struct Args {
    /// Where the repository configuration lives (spira-lc's repo_config::detect reads it).
    /// `None`: the document `$SPIRA_TOML` names, the process's own config.
    home: Option<PathBuf>,
    bd_bin: String,
    bd_db: String,
    queue_dir: PathBuf,
    repos: Vec<String>,
    base_override: Option<String>,
    dry_run: bool,
    reclassify: bool,
    /// `--every-bead` (install's one-time population, sp-k62xz8): the roster is every bead in
    /// the database, not one repository's `repo:` label. A bead's own `repo:` label picks the
    /// repository whose git evidence it is judged by; a bead with none — or whose label names a
    /// repository this config does not have (a beads DB from somewhere else) — is judged on bd
    /// and the ledger alone, and the latter is listed in the report's `unknown_repo`.
    every_bead: bool,
    /// The configured ask-hold label (`schema.sh name ask`) — read by the caller, which can
    /// reach the accessor, and handed in rather than hardcoded here (law-schema-over-code).
    /// `None` means no hold ever fires, rather than assuming either of schema.sh's two
    /// historical values for it.
    ask_label: Option<String>,
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn flag_all(args: &[String], name: &str) -> Vec<String> {
    args.iter().zip(args.iter().skip(1)).filter(|(a, _)| a.as_str() == name).map(|(_, v)| v.clone()).collect()
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    parse_args_with(args, &spira_config::process::cfg)
}

/// Every input a flag does not give comes from `cfg`, the process's config; all that cannot be had are
/// named together, so one run reports the whole shortfall.
fn parse_args_with(args: &[String], cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Args, String> {
    let every_bead = args.iter().any(|a| a == "--every-bead");
    let repos = flag_all(args, "--repo");
    let mut missing: Vec<String> = Vec::new();
    if every_bead && !repos.is_empty() {
        return Err("classify: --every-bead and --repo are exclusive — every bead is every repository's".into());
    }
    let mut from_cfg = |flag_name: &str, key: &str, derive: &dyn Fn(String) -> String| -> String {
        if let Some(v) = flag(args, flag_name) {
            return v;
        }
        match cfg(key) {
            Ok(v) if !v.trim().is_empty() => derive(v),
            Ok(_) => {
                missing.push(format!("{flag_name} (or {key} in the config, which is empty)"));
                String::new()
            }
            Err(e) => {
                missing.push(format!("{flag_name} (or {key} in the config: {e})"));
                String::new()
            }
        }
    };
    let bd_db = from_cfg("--bd-db", "SPIRA_DB", &|v| v);
    let queue_dir = from_cfg("--queue-dir", "SPIRA_QUEUE_DIR", &|v| v);
    if !missing.is_empty() {
        return Err(format!("classify: missing {}", missing.join("; ")));
    }
    let bd_bin = flag(args, "--bd-bin").or_else(|| cfg("SPIRA_BD").ok().filter(|b| !b.trim().is_empty())).unwrap_or_else(|| "bd".to_string());
    Ok(Args {
        home: flag(args, "--home").map(PathBuf::from),
        bd_bin,
        bd_db,
        queue_dir: PathBuf::from(queue_dir),
        repos,
        base_override: flag(args, "--base"),
        dry_run: args.iter().any(|a| a == "--dry-run"),
        reclassify: args.iter().any(|a| a == "--reclassify"),
        every_bead,
        ask_label: flag(args, "--ask-label"),
    })
}

/// One configured repository's evidence, read once per run: its land mode, path and base,
/// the landing lines on that base, and the members of its open batch (if any).
struct RepoCtx {
    name: String,
    mode: &'static str,
    path: PathBuf,
    base: String,
    landing_lines: std::collections::HashMap<String, String>,
    batch_members: BTreeSet<String>,
}

#[derive(Default)]
struct Tally {
    beads: usize,
    classified: usize,
    skipped: usize,
    errors: Vec<String>,
    contradictions: Vec<Value>,
    batches_written: usize,
    reclassified: std::collections::BTreeMap<String, usize>,
    reclassify_refused: usize,
    /// Beads classified without git evidence because their `repo:` label names a repository
    /// the config does not have (--every-bead only).
    unknown_repo: Vec<String>,
    started: Option<std::time::Instant>,
}

const PROGRESS_EVERY: usize = 100;

/// The repository a bead's own labels name: `Ok(None)` for a bead with no `repo:` label,
/// `Err` naming the labels when it carries more than one.
fn repo_label(labels: &BTreeSet<String>) -> Result<Option<String>, String> {
    let named: Vec<&str> = labels.iter().filter_map(|l| l.strip_prefix("repo:")).collect();
    match named.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.to_string())),
        many => Err(format!("carries more than one repo: label ({}) — cannot tell whose landing evidence to read", many.join(", "))),
    }
}

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(e) => return (CANNOT_TELL, e),
    };
    let cfg = match &parsed.home {
        Some(home) => repo_config::detect(home),
        None => repo_config::from_process(),
    };
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) => return (CANNOT_TELL, format!("classify: reading repository configuration: {e}")),
    };

    // A store with no configured repository still has beads to populate (--every-bead judges
    // an unlabelled bead on bd and the ledger alone); a per-repository run has nothing to do.
    if cfg.repos.is_empty() && !parsed.every_bead {
        return (
            CANNOT_TELL,
            format!(
                "classify: no repositories resolved from {} under --home {} (repos asked for: {})",
                cfg.source,
                parsed.home.as_deref().map(|h| h.display().to_string()).unwrap_or_default(),
                if parsed.repos.is_empty() { "all".to_string() } else { parsed.repos.join(", ") }
            ),
        );
    }

    let mut t = Tally { started: Some(std::time::Instant::now()), ..Tally::default() };
    let mut ctxs: std::collections::BTreeMap<String, RepoCtx> = std::collections::BTreeMap::new();

    if parsed.every_bead {
        for name in cfg.repos.keys() {
            repo_ctx(conn, &parsed, &cfg, &mut ctxs, &mut t, name);
        }
        match bd_facts::roster_all(&parsed.bd_bin, &parsed.bd_db) {
            Ok(ids) => {
                for id in &ids {
                    classify_one(conn, &parsed, &cfg, &mut ctxs, &mut t, id, None);
                }
            }
            Err(e) => t.errors.push(format!("bd roster: {e}")),
        }
    } else {
        let repo_names: Vec<String> =
            if parsed.repos.is_empty() { cfg.repos.keys().cloned().collect() } else { parsed.repos.clone() };
        for repo_name in &repo_names {
            if !cfg.repos.contains_key(repo_name) {
                t.errors.push(format!("{repo_name}: not present in {}", cfg.source));
                continue;
            }
            repo_ctx(conn, &parsed, &cfg, &mut ctxs, &mut t, repo_name);
            let ids = match bd_facts::roster(&parsed.bd_bin, &parsed.bd_db, repo_name) {
                Ok(ids) => ids,
                Err(e) => {
                    t.errors.push(format!("{repo_name}: bd roster: {e}"));
                    continue;
                }
            };
            for id in &ids {
                classify_one(conn, &parsed, &cfg, &mut ctxs, &mut t, id, Some(repo_name));
            }
        }
    }

    let report = json!({
        "source": cfg.source,
        "dry_run": parsed.dry_run,
        "beads": t.beads,
        "classified": t.classified,
        "skipped_already_classified": t.skipped,
        "batches_written": t.batches_written,
        "reclassified": t.reclassified,
        "reclassify_refused": t.reclassify_refused,
        "unknown_repo": t.unknown_repo,
        "contradictions": t.contradictions,
        "errors": t.errors,
    });
    let code = if t.errors.is_empty() { 0 } else { CANNOT_TELL };
    (code, serde_json::to_string_pretty(&report).unwrap())
}

/// Reads one configured repository's evidence the first time it is needed, writing its open
/// batch (if any) as it does — exactly once per run.
fn repo_ctx(
    conn: &Conn,
    parsed: &Args,
    cfg: &repo_config::RepoConfig,
    ctxs: &mut std::collections::BTreeMap<String, RepoCtx>,
    t: &mut Tally,
    repo_name: &str,
) {
    if ctxs.contains_key(repo_name) {
        return;
    }
    let Some(section) = cfg.repos.get(repo_name) else { return };
    let path = PathBuf::from(&section.path);
    let base = parsed.base_override.clone().or_else(|| section.base.clone()).unwrap_or_else(|| "main".to_string());
    let open_batch = legacy_files::read_open_batch(&parsed.queue_dir, repo_name);
    let batch_members: BTreeSet<String> = open_batch.as_ref().map(|b| b.members.iter().cloned().collect()).unwrap_or_default();
    if let Some(ob) = &open_batch {
        match write_open_batch(conn, repo_name, ob, parsed.dry_run) {
            Ok(true) => t.batches_written += 1,
            Ok(false) => {}
            Err(e) => t.errors.push(format!("{repo_name}: writing open batch: {e}")),
        }
    }
    let landing_lines = git_evidence::landing_lines(&path, &base);
    ctxs.insert(
        repo_name.to_string(),
        RepoCtx { name: repo_name.to_string(), mode: repo_config::mode_str(section.mode), path, base, landing_lines, batch_members },
    );
}

/// One bead: skip it when it already has a row (unless `--reclassify`), else gather its facts,
/// classify, and write. `repo` is the repository whose roster named it; `None` (--every-bead)
/// means the bead's own `repo:` label decides. Every failure is an error naming the bead.
#[allow(clippy::too_many_arguments)]
fn classify_one(
    conn: &Conn,
    parsed: &Args,
    cfg: &repo_config::RepoConfig,
    ctxs: &mut std::collections::BTreeMap<String, RepoCtx>,
    t: &mut Tally,
    id: &str,
    repo: Option<&str>,
) {
    t.beads += 1;
    if t.beads % PROGRESS_EVERY == 0 {
        let secs = t.started.map_or(0.001, |s| s.elapsed().as_secs_f64()).max(0.001);
        eprintln!(
            "classify: {} beads seen, {} classified, {} skipped, {:.1} beads/s",
            t.beads, t.classified, t.skipped, t.beads as f64 / secs
        );
    }
    let existing = match rows::fetch_bead(conn, id) {
        Ok(Some(row)) if parsed.reclassify => Some(row),
        Ok(Some(_)) => {
            t.skipped += 1;
            return;
        }
        Ok(None) => None,
        Err(e) => {
            t.errors.push(format!("{id}: checking for an existing row: {e:?}"));
            return;
        }
    };

    let bd = match bd_facts::fetch(&parsed.bd_bin, &parsed.bd_db, id) {
        Ok(b) => b,
        Err(e) => {
            t.errors.push(format!("{id}: {e}"));
            return;
        }
    };

    let repo_name = match repo {
        Some(r) => Some(r.to_string()),
        None => match repo_label(&bd.labels) {
            // A repository this config does not have: no git evidence to read, so the bead is
            // judged like an unlabelled one, on bd and the ledger alone — and reported.
            Ok(Some(r)) if !cfg.repos.contains_key(&r) => {
                t.unknown_repo.push(id.to_string());
                None
            }
            Ok(r) => r,
            Err(e) => {
                t.errors.push(format!("{id}: {e}"));
                return;
            }
        },
    };
    if let Some(r) = &repo_name {
        repo_ctx(conn, parsed, cfg, ctxs, t, r);
    }
    let ctx = repo_name.as_deref().and_then(|r| ctxs.get(r));

    // No repository, no git evidence: every git-derived fact is false.
    let (content_on_base, branch_ahead, batch_open_member, landing_commit) = match ctx {
        None => (false, false, false, None),
        Some(c) => {
            let branch_ref = format!("spira/{id}");
            let branch_exists = git_output_exists(&c.path, &branch_ref);
            let branch_content_on_base = branch_exists && git_evidence::content_on_base(&c.path, &branch_ref, &c.base);
            (
                branch_content_on_base,
                branch_exists && !branch_content_on_base,
                c.batch_members.contains(id),
                c.landing_lines.get(id).cloned(),
            )
        }
    };

    let facts = BeadFacts {
        bd_status: bd.status,
        holder_alive: false,
        has_ask_hold: parsed.ask_label.as_deref().is_some_and(|l| bd.labels.contains(l)),
        labels: bd.labels.clone(),
        supersedes: bd.supersedes.clone(),
        content_on_base,
        batch_open_member,
        branch_ahead,
        landing_commit,
        is_epic: bd.is_epic,
    };

    let mut outcome = classify::classify(&facts);
    if bd.external && outcome.rule == lifecycle::bead::RESIDUE_RULE {
        outcome.state = lifecycle::bead::BeadState::Ready;
        outcome.rule = EXTERNAL_RESIDUE_RULE;
    }

    if let Some(row) = existing {
        if !correctable(&row, &outcome) {
            t.skipped += 1;
            return;
        }
        let key = format!("{}->{} ({})", row.state.as_str(), outcome.state.as_str(), outcome.rule);
        if !parsed.dry_run {
            let kind = lifecycle::bead::BeadEventKind::Reclassify { state: outcome.state, rule: outcome.rule.to_string() };
            let (code, msg) = crate::apply_bead_event(conn, id, "classifier", kind);
            if code != 0 {
                t.reclassify_refused += 1;
                t.errors.push(format!("{id}: reclassify refused: {msg}"));
                return;
            }
        }
        *t.reclassified.entry(key).or_default() += 1;
        return;
    }

    let repo_shown = ctx.map(|c| c.name.as_str()).unwrap_or("");
    let mode = ctx.map(|c| c.mode).unwrap_or("none");
    if !outcome.contradictions.is_empty() {
        t.contradictions.push(json!({
            "bead_id": id,
            "repo": repo_shown,
            "mode": mode,
            "rule": outcome.rule,
            "state": outcome.state.as_str(),
            "notes": outcome.contradictions,
        }));
    }

    match write_classified_bead(conn, id, &outcome, repo_shown, mode, cfg.source, parsed.dry_run) {
        Ok(()) => t.classified += 1,
        Err(e) => t.errors.push(format!("{id}: writing classification: {e}")),
    }
}

/// A re-run corrects an existing row only by the rules that were missing when it was written,
/// and only where the row is the classifier's own guess: recomputing every other rule would
/// overwrite what the machine has since decided.
/// An external bead closed with no evidence is unresolved work, not a drop (law-a-learned-failure-mode-becomes-a-state).
pub const EXTERNAL_RESIDUE_RULE: &str = "external-closed-no-outcome";

fn correctable(row: &lifecycle::bead::BeadRow, outcome: &classify::Classification) -> bool {
    use lifecycle::bead::BeadState::*;
    if outcome.rule.starts_with("epic-") {
        return !row.state.is_terminal() && row.state != Working && row.state != outcome.state;
    }
    if !matches!(outcome.rule, "terminal-landing-line") {
        return false;
    }
    match row.state {
        Working | InDelivery => false,
        Landed | Superseded | Done => false,
        Dropped => row.reason.as_deref() == Some(lifecycle::bead::RESIDUE_RULE) && row.state != outcome.state,
        Open | Ready | Submitted | Certified | Rework => row.state != outcome.state,
    }
}

fn git_output_exists(repo: &Path, rev: &str) -> bool {
    spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn holds_json(holds: &BTreeSet<HoldKind>) -> String {
    Value::Array(holds.iter().map(|h| Value::String(h.as_str().to_string())).collect()).to_string()
}

#[allow(clippy::too_many_arguments)]
fn write_classified_bead(
    conn: &Conn,
    id: &str,
    outcome: &classify::Classification,
    repo_name: &str,
    mode: &str,
    source: &str,
    dry_run: bool,
) -> Result<(), String> {
    if dry_run {
        return Ok(());
    }
    let at = crate::db::now_epoch();
    let evidence = json!({
        "rule": outcome.rule,
        "contradictions": outcome.contradictions,
        "repo": repo_name,
        "mode": mode,
        "config_source": source,
        "pr_marker": outcome.pr,
    });
    let insert_columns = "bead_id, state, tip, gate_key, holder, lease_until, holds, reason, version, updated_at";
    let insert_values = format!(
        "'{}', '{}', {}, NULL, NULL, NULL, '{}', '{}', 0, {}",
        rows::escape(id),
        outcome.state.as_str(),
        "NULL",
        holds_json(&outcome.holds),
        rows::escape(outcome.rule),
        at,
    );
    let ev = EventRecord::import_absent("bead", id, "Classified", evidence, "classifier", at);
    let bead_inserted = conn
        .insert_if_absent_and_log("bead", insert_columns, &insert_values, &ev, outcome.state.as_str())
        .map_err(|e| format!("{e:?}"))?;

    if bead_inserted {
        if let Some(delivery_state) = outcome.delivery {
            write_classified_delivery(conn, id, delivery_state, repo_name, at)?;
        }
    }
    Ok(())
}

fn write_classified_delivery(conn: &Conn, id: &str, state: DeliveryState, repo_name: &str, at: i64) -> Result<(), String> {
    let mode = match state {
        DeliveryState::Batched | DeliveryState::Queued => "queue",
        DeliveryState::PrOpen => "pr",
        DeliveryState::Pushing => "push",
        DeliveryState::Exited => return Ok(()), // migration never produces an already-exited row
        // migration never produces a row past landed_local — publish facts are only ever
        // reached by a fresh event, not a legacy record this classifier reads.
        DeliveryState::Publishing | DeliveryState::Published | DeliveryState::PublishRed => return Ok(()),
    };
    let batch_id = if state == DeliveryState::Batched { Some(format!("legacy-{repo_name}")) } else { None };
    let insert_columns = "bead_id, mode, state, batch_id, pr, merge_sha, version";
    let insert_values = format!(
        "'{}', '{}', '{}', {}, NULL, NULL, 0",
        rows::escape(id),
        mode,
        state.as_str(),
        opt_sql_str(&batch_id),
    );
    let ev = EventRecord::import_absent("delivery", id, "Classified", json!({"repo": repo_name}), "classifier", at);
    conn.insert_if_absent_and_log("delivery", insert_columns, &insert_values, &ev, state.as_str())
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// Writes a repository's currently open batch as a fresh `OPEN` batch row, plus one
/// `batch_member` row per member. There is no ambiguity to classify here — an `open` file
/// found at migration time IS, by definition, an open batch — so this bypasses
/// `lifecycle::classify` entirely and writes the row directly, the same way a fresh `cut`
/// would. Returns `Ok(true)` if it wrote (skips silently, `Ok(false)`, if the batch already
/// has a row — the same idempotency the bead path gets from `insert_if_absent_and_log`).
fn write_open_batch(conn: &Conn, repo_name: &str, ob: &legacy_files::OpenBatch, dry_run: bool) -> Result<bool, String> {
    let opened_at = ob.opened_at.unwrap_or_else(crate::db::now_epoch);
    let batch_id = format!("legacy-{repo_name}-{opened_at}");
    if rows::fetch_batch(conn, &batch_id).map_err(|e| format!("{e:?}"))?.is_some() {
        return Ok(false);
    }
    if dry_run {
        return Ok(true);
    }
    let at = crate::db::now_epoch();
    let insert_columns = "batch_id, repo, state, parent, head, base, pr, run, version, opened_at";
    let insert_values = format!(
        "'{}', '{}', 'OPEN', NULL, {}, {}, NULL, NULL, 0, {}",
        rows::escape(&batch_id),
        rows::escape(repo_name),
        opt_sql_str(&ob.head),
        opt_sql_str(&ob.base),
        opened_at,
    );
    let ev = EventRecord::import_absent("batch", &batch_id, "Classified", json!({"repo": repo_name, "members": ob.members}), "classifier", at);
    conn.insert_if_absent_and_log("batch", insert_columns, &insert_values, &ev, "OPEN").map_err(|e| format!("{e:?}"))?;

    for member in &ob.members {
        let tip = ob.head.clone().unwrap_or_default();
        let sql = format!(
            "INSERT IGNORE INTO batch_member (batch_id, bead_id, tip, outcome) VALUES ('{}', '{}', '{}', NULL)",
            rows::escape(&batch_id),
            rows::escape(member),
            rows::escape(&tip),
        );
        conn.query(&sql).map_err(|e| format!("{e:?}"))?;
    }
    Ok(true)
}

fn opt_sql_str(v: &Option<String>) -> String {
    match v {
        Some(s) => format!("'{}'", rows::escape(s)),
        None => "NULL".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(ls: &[&str]) -> BTreeSet<String> {
        ls.iter().map(|s| s.to_string()).collect()
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_bead_with_no_repo_label_has_no_repository() {
        assert_eq!(repo_label(&labels(&["spira-submitted", "scope:x"])), Ok(None));
    }

    #[test]
    fn a_bead_with_one_repo_label_names_that_repository() {
        assert_eq!(repo_label(&labels(&["repo:demo", "ask-x"])), Ok(Some("demo".to_string())));
    }

    #[test]
    fn a_bead_with_two_repo_labels_is_an_error_naming_both() {
        let e = repo_label(&labels(&["repo:a", "repo:b"])).unwrap_err();
        assert!(e.contains("repo:") && e.contains('a') && e.contains('b'), "{e}");
    }

    fn cfg_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Result<String, String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()).ok_or_else(|| format!("{k} is not declared"))
    }

    #[test]
    fn no_flags_resolve_every_path_from_the_config() {
        let cfg = cfg_of(&[("SPIRA_DB", "/cfg/db"), ("SPIRA_QUEUE_DIR", "/cfg/q"), ("SPIRA_BD", "/cfg/bd")]);
        let a = parse_args_with(&argv("--repo demo --dry-run"), &cfg).unwrap();
        assert_eq!(a.bd_db, "/cfg/db");
        assert_eq!(a.bd_bin, "/cfg/bd");
        assert_eq!(a.queue_dir, PathBuf::from("/cfg/q"));
        assert!(a.home.is_none());
    }

    #[test]
    fn a_flag_overrides_the_config() {
        let cfg = cfg_of(&[("SPIRA_DB", "/cfg/db"), ("SPIRA_QUEUE_DIR", "/cfg/q")]);
        let a = parse_args_with(&argv("--bd-db /flag/db --queue-dir /flag/q --bd-bin /flag/bd"), &cfg).unwrap();
        assert_eq!((a.bd_db.as_str(), a.bd_bin.as_str()), ("/flag/db", "/flag/bd"));
        assert_eq!(a.queue_dir, PathBuf::from("/flag/q"));
    }

    #[test]
    fn every_missing_input_is_named_in_one_error() {
        let e = parse_args_with(&argv("--repo demo"), &cfg_of(&[])).unwrap_err();
        for want in ["--bd-db", "--queue-dir", "SPIRA_DB", "SPIRA_QUEUE_DIR"] {
            assert!(e.contains(want), "{want} missing from: {e}");
        }
    }

    #[test]
    fn every_bead_needs_no_home() {
        let a = parse_args_with(&argv("--every-bead"), &cfg_of(&[("SPIRA_DB", "/d"), ("SPIRA_QUEUE_DIR", "/q")])).unwrap();
        assert!(a.every_bead && a.home.is_none());
    }

    #[test]
    fn every_bead_refuses_a_repo_filter() {
        let e = parse_args_with(&argv("--every-bead --repo demo --bd-db /db --queue-dir /q"), &cfg_of(&[])).unwrap_err();
        assert!(e.contains("exclusive"), "{e}");
    }
}
