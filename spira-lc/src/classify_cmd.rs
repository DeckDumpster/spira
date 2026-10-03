//! `spira-lc classify` — the one-time migration classifier (design §4). Assigns every
//! existing work bead, and every open batch, a lifecycle state by reading the legacy
//! records (bd, the landstate ledger, batch membership files, git) in the fixed precedence
//! `lifecycle::classify` implements, then writes one event per decision with the deciding
//! rule as evidence. Idempotent: a bead that already has a row is left untouched and no new
//! event is written for it, which is what makes re-running this command a no-op. `--dry-run`
//! computes and reports the same decisions without writing anything.
//!
//! This binary never simulates a machine transition here — there is no prior row for a
//! freshly-migrated bead to compare-and-swap against — so it writes the classified row
//! directly via [`crate::db::Conn::insert_if_absent_and_log`], never through
//! `lifecycle::bead::apply`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lifecycle::bead::HoldKind;
use lifecycle::classify::{self, BeadFacts};
use lifecycle::delivery::DeliveryState;
use serde_json::{json, Value};

use crate::db::{Conn, EventRecord};
use crate::{bd_facts, git_evidence, legacy_files, repo_config, rows};

const CANNOT_TELL: i32 = 2;

struct Args {
    home: PathBuf,
    bd_bin: String,
    bd_db: String,
    landstate_dir: PathBuf,
    queue_dir: PathBuf,
    repos: Vec<String>,
    base_override: Option<String>,
    dry_run: bool,
    reclassify: bool,
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
    let home = flag(args, "--home").ok_or("classify: --home is required")?;
    let bd_bin = flag(args, "--bd-bin").unwrap_or_else(|| "bd".to_string());
    let bd_db = flag(args, "--bd-db").ok_or("classify: --bd-db is required")?;
    let landstate_dir = flag(args, "--landstate-dir").ok_or("classify: --landstate-dir is required")?;
    let queue_dir = flag(args, "--queue-dir").ok_or("classify: --queue-dir is required")?;
    Ok(Args {
        home: PathBuf::from(home),
        bd_bin,
        bd_db,
        landstate_dir: PathBuf::from(landstate_dir),
        queue_dir: PathBuf::from(queue_dir),
        repos: flag_all(args, "--repo"),
        base_override: flag(args, "--base"),
        dry_run: args.iter().any(|a| a == "--dry-run"),
        reclassify: args.iter().any(|a| a == "--reclassify"),
        ask_label: flag(args, "--ask-label"),
    })
}

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(e) => return (CANNOT_TELL, e),
    };
    let cfg = match repo_config::detect(&parsed.home) {
        Ok(c) => c,
        Err(e) => return (CANNOT_TELL, format!("classify: reading repository configuration: {e}")),
    };

    if cfg.repos.is_empty() {
        return (
            CANNOT_TELL,
            format!(
                "classify: no repositories resolved from {} under --home {} (repos asked for: {})",
                cfg.source,
                parsed.home.display(),
                if parsed.repos.is_empty() { "all".to_string() } else { parsed.repos.join(", ") }
            ),
        );
    }

    let repo_names: Vec<String> =
        if parsed.repos.is_empty() { cfg.repos.keys().cloned().collect() } else { parsed.repos.clone() };

    let mut classified = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut contradictions: Vec<Value> = Vec::new();
    let mut batches_written = 0usize;
    let mut reclassified: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut reclassify_refused = 0usize;

    for repo_name in &repo_names {
        let Some(section) = cfg.repos.get(repo_name) else {
            errors.push(format!("{repo_name}: not present in {}", cfg.source));
            continue;
        };
        let mode = repo_config::mode_str(section.mode);
        let repo_path = Path::new(&section.path);
        let base = parsed.base_override.clone().or_else(|| section.base.clone()).unwrap_or_else(|| "main".to_string());

        let open_batch = legacy_files::read_open_batch(&parsed.queue_dir, repo_name);
        let batch_members: BTreeSet<String> = open_batch.as_ref().map(|b| b.members.iter().cloned().collect()).unwrap_or_default();

        if let Some(ob) = &open_batch {
            match write_open_batch(conn, repo_name, ob, parsed.dry_run) {
                Ok(true) => batches_written += 1,
                Ok(false) => {}
                Err(e) => errors.push(format!("{repo_name}: writing open batch: {e}")),
            }
        }

        let landing_lines = git_evidence::landing_lines(repo_path, &base);

        let ids = match bd_facts::roster(&parsed.bd_bin, &parsed.bd_db, repo_name) {
            Ok(ids) => ids,
            Err(e) => {
                errors.push(format!("{repo_name}: bd roster: {e}"));
                continue;
            }
        };

        for id in &ids {
            let existing = match rows::fetch_bead(conn, id) {
                Ok(Some(row)) if parsed.reclassify => Some(row),
                Ok(Some(_)) => {
                    skipped += 1;
                    continue;
                }
                Ok(None) => None,
                Err(e) => {
                    errors.push(format!("{id}: checking for an existing row: {e:?}"));
                    continue;
                }
            };

            let bd = match bd_facts::fetch(&parsed.bd_bin, &parsed.bd_db, id) {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{id}: {e}"));
                    continue;
                }
            };

            let (landstate, landstate_tip) = legacy_files::read_landstate(&parsed.landstate_dir, id)
                .map(|(ls, tip)| (Some(ls), tip))
                .unwrap_or((None, None));

            let tip_ancestor_of_base =
                landstate_tip.as_deref().map(|tip| git_evidence::is_ancestor(repo_path, tip, &base)).unwrap_or(false);

            let branch_ref = format!("spira/{id}");
            let branch_exists = git_output_exists(repo_path, &branch_ref);
            let branch_content_on_base = branch_exists && git_evidence::content_on_base(repo_path, &branch_ref, &base);
            let tip_content_on_base =
                landstate_tip.as_deref().map(|tip| git_evidence::content_on_base(repo_path, tip, &base)).unwrap_or(false);
            let content_on_base = branch_content_on_base || tip_content_on_base;
            let branch_ahead = branch_exists && !branch_content_on_base;

            let facts = BeadFacts {
                bd_status: bd.status,
                holder_alive: false,
                has_ask_hold: parsed.ask_label.as_deref().is_some_and(|l| bd.labels.contains(l)),
                labels: bd.labels.clone(),
                supersedes: bd.supersedes.clone(),
                landstate,
                tip_ancestor_of_base,
                content_on_base,
                batch_open_member: batch_members.contains(id),
                branch_ahead,
                landing_commit: landing_lines.get(id).cloned(),
            };

            let outcome = classify::classify(&facts);

            if let Some(row) = existing {
                if !correctable(&row, &outcome) {
                    skipped += 1;
                    continue;
                }
                let key = format!("{}->{} ({})", row.state.as_str(), outcome.state.as_str(), outcome.rule);
                if !parsed.dry_run {
                    let kind = lifecycle::bead::BeadEventKind::Reclassify { state: outcome.state, rule: outcome.rule.to_string() };
                    let (code, msg) = crate::apply_bead_event(conn, id, "classifier", kind);
                    if code != 0 {
                        reclassify_refused += 1;
                        errors.push(format!("{id}: reclassify refused: {msg}"));
                        continue;
                    }
                }
                *reclassified.entry(key).or_default() += 1;
                continue;
            }

            if !outcome.contradictions.is_empty() {
                contradictions.push(json!({
                    "bead_id": id,
                    "repo": repo_name,
                    "mode": mode,
                    "rule": outcome.rule,
                    "state": outcome.state.as_str(),
                    "notes": outcome.contradictions,
                }));
            }

            match write_classified_bead(conn, id, &landstate_tip, &outcome, repo_name, mode, cfg.source, parsed.dry_run) {
                Ok(()) => classified += 1,
                Err(e) => errors.push(format!("{id}: writing classification: {e}")),
            }
        }
    }

    let report = json!({
        "source": cfg.source,
        "dry_run": parsed.dry_run,
        "classified": classified,
        "skipped_already_classified": skipped,
        "batches_written": batches_written,
        "reclassified": reclassified,
        "reclassify_refused": reclassify_refused,
        "contradictions": contradictions,
        "errors": errors,
    });
    let code = if errors.is_empty() { 0 } else { CANNOT_TELL };
    (code, serde_json::to_string_pretty(&report).unwrap())
}

/// A re-run corrects an existing row only by the rules that were missing when it was written,
/// and only where the row is the classifier's own guess: the ledger files are no longer kept
/// current, so recomputing every other rule would overwrite what the machine has since decided.
fn correctable(row: &lifecycle::bead::BeadRow, outcome: &classify::Classification) -> bool {
    use lifecycle::bead::BeadState::*;
    if !matches!(outcome.rule, "terminal-landing-line" | "closed-branch-never-landed") {
        return false;
    }
    match row.state {
        Working | InDelivery => false,
        Landed | Superseded | Done => false,
        Dropped => row.reason.as_deref() == Some(lifecycle::bead::RESIDUE_RULE) && row.state != outcome.state,
        Ready | Submitted | Certified | Rework => row.state != outcome.state,
    }
}

fn git_output_exists(repo: &Path, rev: &str) -> bool {
    std::process::Command::new("git")
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
    tip: &Option<String>,
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
        opt_sql_str(tip),
        holds_json(&outcome.holds),
        rows::escape(outcome.rule),
        at,
    );
    let ev = EventRecord {
        machine: "bead".into(),
        key: id.into(),
        event: "Classified".into(),
        expect: "NONE".into(),
        from_state: "NONE".into(),
        refusal: None,
        evidence,
        actor: "classifier".into(),
        at,
    };
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
        DeliveryState::Published | DeliveryState::PublishRed => return Ok(()),
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
    let ev = EventRecord {
        machine: "delivery".into(),
        key: id.into(),
        event: "Classified".into(),
        expect: "NONE".into(),
        from_state: "NONE".into(),
        refusal: None,
        evidence: json!({"repo": repo_name}),
        actor: "classifier".into(),
        at,
    };
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
    let ev = EventRecord {
        machine: "batch".into(),
        key: batch_id.clone(),
        event: "Classified".into(),
        expect: "NONE".into(),
        from_state: "NONE".into(),
        refusal: None,
        evidence: json!({"repo": repo_name, "members": ob.members}),
        actor: "classifier".into(),
        at,
    };
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
