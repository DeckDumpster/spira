//! The stranded-work detectors (wave4-decomposition.md family T, row 29, sp-8ofmt):
//! `detect_livelocked`, `detect_incident_needs_builder`, `detect_invalid_closed` and
//! `all_partition_members` (`detect_landed_but_open`, `detect_closed_unlanded_states` and
//! `detect_false_blockers` were deleted in sp-mve9i — see their section below).
//!
//! OUTPUT IS BYTE-COMPATIBLE with the lib.sh functions this replaces: `groomer`,
//! `mimir-trigger` and `cockpit-collect` parse `LIVELOCK …`/`STATE …`/`INVALID-CLOSED …`/
//! `UNFILED-FOLLOW …` lines, and lib.sh's own functions become one-line shims onto this
//! crate's binary (`strand detect-livelocked`, …) so a caller that still sources lib.sh
//! directly (a test suite, `attempts.sh`) keeps working unchanged.
//!
//! GIT AND `spira-lc` ARE REACHED AS SUBPROCESSES, BY NAME ON PATH, never re-derived:
//! "landed" is the lifecycle record's LANDED state (`spira-lc state <id>`). Content-on-base is
//! the Hel's own proof (`hel::git::Git::content_on_base`, the same answer as
//! `spira-lc content-landed`), called in-process; the plain merge-tree clean check runs `git`.
//! `detect_invalid_closed`'s statute-phrase predicates live in [`crate::closereason`], shared
//! with dwarf's close-time fence (`strand close-reason`) so the two cannot disagree.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

use spira_config::repos::Registry;
use spira_store::lc_call::LcClient;

use crate::config::Config;
use crate::model::{self, Bead};
use crate::probe;

/// `re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", s)[:max]` — every lib.sh title-sanitizer in this
/// family uses this exact character class and cutoff.
fn sanitize_title(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == ' ' || c.is_ascii_alphanumeric() || "._/:,()#+-".contains(c) { c } else { ' ' })
        .collect();
    cleaned.chars().take(max).collect()
}

/// The first label value carrying `prefix` (`"repo:"`, `"branch:"`), or `None`.
fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

fn registry(cfg: &Config) -> Registry {
    let home = cfg.home.clone().unwrap_or_default();
    Registry::from_env(std::env::vars().collect(), &home)
}

/// Best-effort `content list`: a transient failure reads exactly as "no rows" — this family
/// never turns a flaky query into a propagated error.
fn list_beads(lc: &LcClient, args: &[&str]) -> Vec<Bead> {
    let mut full = vec!["content", "list"];
    full.extend_from_slice(args);
    full.extend(["--limit", "0", "--brief", "--json"]);
    let raw = lc.answer(&full).unwrap_or_default();
    if raw.trim().is_empty() {
        return Vec::new();
    }
    model::parse_beads(&raw).unwrap_or_default()
}

// ──────────────────────────────────────────────────────────────────────────────
// git plumbing the detectors need. The content-on-base proof itself is the Hel's own
// (`hel::git::Git::content_on_base`), not a copy here.
// ──────────────────────────────────────────────────────────────────────────────

fn git_branch_exists(repo: &Path, branch: &str) -> bool {
    spira_config::bounded::bounded("git")
        .current_dir(repo)
        .args(["show-ref", "--verify", "-q", &format!("refs/heads/{branch}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_rev_list_count(repo: &Path, range: &str) -> Option<u64> {
    let o = spira_config::bounded::bounded("git").current_dir(repo).args(["rev-list", "--count", range]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

// ──────────────────────────────────────────────────────────────────────────────
// all_partition_members
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `all_partition_members`: every unfinished bead a partition's own labels match,
/// EVERY EXCLUSION DROPPED — the claimable-set predicate answers "can this be claimed right
/// now"; this answers "whose is it", which a poisoned or asked-about bead still is. One id
/// per line, first-seen order, deduplicated. "Unfinished" is the lifecycle row's answer
/// (READY, WORKING or REWORK — what bd's `open,in_progress` meant), never bd's status
/// (sp-mve9i); with the machine unreadable there is no answer, so no members.
pub fn all_partition_members(cfg: &Config, lc_door: &LcClient) -> String {
    let specs = probe::roster(cfg, None).unwrap_or_default();
    let Ok(lc) = spira_store::lc_state::list_live().map(spira_store::lc_state::index) else {
        return String::new();
    };
    let per_spec = specs
        .iter()
        .filter(|spec| !spec.labels.is_empty())
        .map(|spec| list_beads(lc_door, &["--all", "--label", spec.labels.as_str()]).into_iter().map(|b| b.id).collect())
        .collect();
    unfinished_members(per_spec, &lc).join("\n")
}

/// The ids, first-seen and deduplicated across `per_spec`, whose lifecycle row is still
/// before its builder's hand-off. A bead with no row can never be worked: not a member.
pub fn unfinished_members(per_spec: Vec<Vec<String>>, lc: &std::collections::HashMap<String, spira_store::lc_state::Row>) -> Vec<String> {
    let mut seen = HashSet::new();
    per_spec
        .into_iter()
        .flatten()
        .filter(|id| lc.get(id).is_some_and(|r| !r.past_builder()))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────

// detect_landed_but_open, detect_closed_unlanded_states and detect_false_blockers are
// deleted (sp-mve9i): each was a disagreement between bd's `status` and the lifecycle
// record (open-but-LANDED, closed-but-not-LANDED, and the dependents of the latter). bd's
// status is inert for work beads (design §3.4), so the disagreement is no longer a state a
// bead can be in; CHECK5-LC (sentinel/src/lifecycle.rs) reads the drifts that remain from the
// spira-lc rows alone. The groomer stopped calling them in sp-jnwbn.

// ──────────────────────────────────────────────────────────────────────────────
// detect_incident_needs_builder
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_incident_needs_builder`: one `STATE <id> incident-is-code — …` line for
/// every unfinished bead carrying the incident label whose recorded `branch:` already has a
/// commit ahead of the repo's base. An incident bead is a work bead (sp-jgjvh): bd supplies
/// the labelled beads' content, and "unfinished" (what bd's open/in_progress meant) is the
/// lifecycle row's READY/WORKING/REWORK — [`unfinished_incidents`]. A machine that cannot
/// answer is an Err, never "no incident".
pub fn detect_incident_needs_builder(cfg: &Config, door: &LcClient) -> Result<String, String> {
    if cfg.incident_label.is_empty() {
        return Err("SPIRA_INCIDENT_LABEL is unset — source conf.sh".into());
    }
    let lc = spira_store::lc_state::list_live().map(spira_store::lc_state::index).map_err(|e| format!("cannot tell: {e}"))?;
    let reg = registry(cfg);
    let home = reg.home_repo().to_string();
    let mut out = Vec::new();
    let labelled = list_beads(door, &["--all", "--label", cfg.incident_label.as_str()]);
    for b in unfinished_incidents(labelled, &lc) {
        let Some(br) = label_value(&b.labels, "branch:") else { continue };
        let br = br.to_string();
        let repo = label_value(&b.labels, "repo:").unwrap_or("").to_string();
        let repo_disp = if repo.is_empty() { home.clone() } else { repo };
        let Some(root) = reg.root(&repo_disp) else { continue };
        let repo_path = Path::new(&root);
        if !git_branch_exists(repo_path, &br) {
            continue;
        }
        let Some((base, _local)) = spira_config::repos::landrefs(&reg, &root) else { continue };
        if base.is_empty() {
            continue;
        }
        let Some(ahead) = git_rev_list_count(repo_path, &format!("{base}..{br}")) else { continue };
        if ahead == 0 {
            continue;
        }
        out.push(format!("STATE {} incident-is-code — {} commit(s) already on {} ahead of {}; remaining work is a code change, not operational", b.id, ahead, br, base));
    }
    Ok(out.join("\n"))
}

/// The incident beads whose lifecycle row still owes builder work; a bead with no row is not
/// live work (it can never be claimed; CHECK-ROWLESS reports it).
pub fn unfinished_incidents(beads: Vec<Bead>, lc: &std::collections::HashMap<String, spira_store::lc_state::Row>) -> Vec<Bead> {
    beads.into_iter().filter(|b| lc.get(&b.id).is_some_and(|r| !r.past_builder())).collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_livelocked
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_livelocked`: one `LIVELOCK <id> <category> — <reason>` line per row, three
/// categories — `ask-no-overseer`, `ci-stuck`, `unmapped-repo`. Never fails outright: a sub-query that cannot run reads as
/// "nothing in that category", matching every bash `2>/dev/null` guard in the original.
pub fn detect_livelocked(cfg: &Config, door: &LcClient) -> String {
    let reg = registry(cfg);
    let held: HashSet<String> = spira_store::lc_state::list_live()
        .map(|rows| rows.into_iter().filter(|r| r.held("ask") || r.held("poison")).map(|r| r.bead_id).collect())
        .unwrap_or_default();
    livelock_rows(
        cfg,
        &reg,
        &held,
        &list_beads(door, &["--label", cfg.vocab.ask.as_str()]),
        &list_beads(door, &["--label", cfg.ci_label.as_str()]),
        &list_beads(door, &[]),
    )
}

fn livelock_rows(cfg: &Config, reg: &Registry, held: &HashSet<String>, asks: &[Bead], ci_labelled: &[Bead], all: &[Bead]) -> String {
    let mut out = Vec::new();

    for b in asks {
        if !b.has(&cfg.vocab.ask) || b.has("overseer") {
            continue;
        }
        let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
        out.push(format!(
            "LIVELOCK {} ask-no-overseer — missing overseer label; the decisions pane cannot see this bead and no dwarf can claim it; add overseer label. title: {}",
            b.id, title
        ));
    }

    let home = reg.home_repo().to_string();
    for b in ci_labelled {
        if b.is_closed() {
            continue;
        }
        let repo = label_value(&b.labels, "repo:").unwrap_or(home.as_str()).to_string();
        let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
        if reg.land(&repo) != "pr" {
            out.push(format!(
                "LIVELOCK {} ci-stuck — repo {} land mode is not pr; {} will never clear; strip the label or change the repo land mode. title: {}",
                b.id, repo, cfg.ci_label, title
            ));
        }
    }

    if reg.map_present() {
        let valid: HashSet<String> = reg.names().into_iter().collect();
        for b in all {
            if held.contains(&b.id) || b.has(&cfg.groom_ask_label) {
                continue;
            }
            let repo_labels: Vec<&str> = b.labels.iter().filter_map(|l| l.strip_prefix("repo:")).collect();
            if repo_labels.is_empty() {
                continue;
            }
            let bad: Vec<&str> = repo_labels.into_iter().filter(|r| !valid.contains(*r)).collect();
            if bad.is_empty() {
                continue;
            }
            let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
            out.push(format!(
                "LIVELOCK {} unmapped-repo — repo:{} not in the repo map; dwarf.sh refuses to claim it; fix the label or add the repo to the repo map. title: {}",
                b.id,
                bad.join(", "),
                title
            ));
        }
    }

    out.join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_invalid_closed
// ──────────────────────────────────────────────────────────────────────────────

/// The rows of a `bd list --json` array that carry a non-empty `close_reason`, re-serialised
/// for the embedded script; the input unchanged when it is not an array.
fn with_close_reason(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(serde_json::Value::Array(a)) => serde_json::Value::Array(
            a.into_iter()
                .filter(|r| r.get("close_reason").and_then(|v| v.as_str()).is_some_and(|v| !v.trim().is_empty()))
                .collect(),
        )
        .to_string(),
        _ => raw.to_string(),
    }
}

fn watermark_path(cfg: &Config) -> Option<std::path::PathBuf> {
    cfg.run.as_ref().filter(|r| !r.as_os_str().is_empty()).map(|r| r.join("strand").join("invalid-closed.watermark"))
}

/// The newest `closed_at` among `rows`, as the store prints it.
fn newest_close(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    v.as_array()?.iter().filter_map(|r| r.get("closed_at")?.as_str()).max().map(str::to_string)
}

/// lib.sh `detect_invalid_closed`: `INVALID-CLOSED`/`UNFILED-FOLLOW`/`ALLOWED-IC` lines for
/// closed beads whose close reasons admit unfinished work or imply untracked follow-on work.
/// Reads only closes after the watermark file's instant, so each close is classified once; an
/// absent watermark reads the whole history. The watermark moves only after a successful read.
pub fn detect_invalid_closed(cfg: &Config, door: &LcClient) -> String {
    let mark_file = watermark_path(cfg);
    let mark = mark_file.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let Some(raw) = read_closed(cfg, door, mark.as_deref()) else { return String::new() };
    if let (Some(p), Some(newest)) = (&mark_file, newest_close(&raw)) {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(p));
        let _ = std::fs::write(p, format!("{newest}\n"));
    }
    classify_closed(&raw, cfg)
}

/// [`detect_invalid_closed`] over the closes after `after`, with no watermark read or moved:
/// a display that must not take rows from the pass that acts on them.
pub fn detect_invalid_closed_since(cfg: &Config, door: &LcClient, after: &str) -> String {
    invalid_closed_since(cfg, door, after).unwrap_or_default()
}

/// [`detect_invalid_closed_since`] that keeps a refused read apart from an answered empty one:
/// `None` is the store not answering, `Some("")` is it answering with no invalid close.
pub fn invalid_closed_since(cfg: &Config, door: &LcClient, after: &str) -> Option<String> {
    read_closed(cfg, door, Some(after)).map(|raw| classify_closed(&raw, cfg))
}

fn read_closed(cfg: &Config, door: &LcClient, after: Option<&str>) -> Option<String> {
    let mut args: Vec<&str> = vec!["content", "list", "--all"];
    if !cfg.scope_label.is_empty() {
        args.extend(["--label", &cfg.scope_label]);
    }
    if let Some(t) = after {
        args.extend(["--closed-after", t]);
    }
    args.extend(["--limit", "0", "--brief", "--json"]);
    door.answer(&args).ok().filter(|raw| !raw.trim().is_empty())
}

fn classify_closed(raw: &str, cfg: &Config) -> String {
    // The close records are the subject: a bead that carries a close reason, whatever bd's
    // status says (sp-mve9i: bd status is not read for a work bead; the reason is content).
    classify_invalid_closed(&with_close_reason(raw), cfg)
}

fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn read_allowlist(cfg: &Config) -> std::collections::HashMap<String, String> {
    let mut allow = std::collections::HashMap::new();
    let Some(run) = cfg.run.as_ref().filter(|r| !r.as_os_str().is_empty()) else { return allow };
    let Ok(text) = std::fs::read_to_string(run.join("invalid-closed.allow")) else { return allow };
    let id_re = regex::Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)+$").unwrap();
    for line in text.lines() {
        let line = line.trim();
        let (id, reason) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if id_re.is_match(id) {
            allow.insert(id.to_string(), reason.trim().to_string());
        }
    }
    allow
}

/// `INVALID-CLOSED` / `UNFILED-FOLLOW` / `ALLOWED-IC` lines over a `bd list --json` payload.
/// The detector scans the raw reason (`detect_close_reason`) so every occurrence reaches
/// Mimir; the quote-masked fence belongs to dwarf.
fn classify_invalid_closed(raw: &str, cfg: &Config) -> String {
    use crate::closereason::{check_unfiled_follow, detect_close_reason};
    let Ok(d) = serde_json::from_str::<serde_json::Value>(raw) else { return String::new() };
    let rows: Vec<&serde_json::Value> = match &d {
        serde_json::Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let allow = read_allowlist(cfg);
    let mut out: Vec<String> = Vec::new();
    for i in rows {
        let Some(bid) = i.get("id").and_then(|v| v.as_str()) else { return String::new() };
        let reason = i.get("close_reason").and_then(|v| v.as_str()).unwrap_or("");
        let title = sanitize_title(i.get("title").and_then(|v| v.as_str()).unwrap_or(""), 60);
        let reason_short: String = reason.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
        if let Some(why) = allow.get(bid) {
            out.push(format!("ALLOWED-IC {bid} — {why}"));
        } else if let Some(hit) = detect_close_reason(reason) {
            out.push(format!("INVALID-CLOSED {bid} — close reason contains {}: {reason_short}. title: {title}", py_repr(&hit)));
        } else if let Some(hit) = check_unfiled_follow(reason, &cfg.id_prefix) {
            out.push(format!("UNFILED-FOLLOW {bid} — follow-on phrase {} without a tracking reference: {reason_short}. title: {title}", py_repr(&hit)));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(teqxerl)]
    #[test]
    fn sanitize_title_replaces_disallowed_characters_and_cuts_at_max() {
        assert_eq!(sanitize_title("hi \"there\" <x>", 20), "hi  there   x ");
        assert_eq!(sanitize_title(&"x".repeat(100), 5), "xxxxx");
    }

    #[test_id(tfwb95u)]
    #[test]
    fn label_value_finds_the_first_match() {
        let labels = vec!["repo:spira".to_string(), "branch:sp-1".to_string()];
        assert_eq!(label_value(&labels, "repo:"), Some("spira"));
        assert_eq!(label_value(&labels, "team:"), None);
    }


    /// sp-mve9i: a partition's members are the beads whose lifecycle row is unfinished —
    /// READY, WORKING, REWORK — deduplicated in first-seen order; no row, no member.
    #[test_id(t3b7snc)]
    #[test]
    fn partition_members_are_the_unfinished_lifecycle_rows() {
        use spira_store::lc_state::Row;
        let lc = [("a", "READY"), ("b", "WORKING"), ("c", "SUBMITTED"), ("d", "REWORK"), ("e", "LANDED")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let per = vec![vec!["b".into(), "c".into(), "a".into()], vec!["a".into(), "d".into(), "e".into(), "z".into()]];
        assert_eq!(unfinished_members(per, &lc), vec!["b", "a", "d"]);
    }

    /// sp-jgjvh: an incident is unfinished by its lifecycle row, whatever bd's status says;
    /// a rowless bead is not live work.
    #[test_id(term2ps)]
    #[test]
    fn incidents_needing_a_builder_are_the_unfinished_lifecycle_rows() {
        use spira_store::lc_state::Row;
        let beads = model::parse_beads(
            r#"[{"id":"a","status":"closed"},{"id":"b","status":"open"},{"id":"c","status":"in_progress"},{"id":"d","status":"open"}]"#,
        )
        .unwrap();
        let lc = [("a", "WORKING"), ("b", "SUBMITTED"), ("c", "REWORK")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let ids: Vec<String> = unfinished_incidents(beads, &lc).into_iter().map(|b| b.id).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    struct Closes(std::sync::Mutex<Vec<Vec<String>>>, &'static str, i32);

    impl spira_store::lc_call::Transport for Closes {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(spira_store::lc_call::Reply { code: self.2, out: self.1.into(), err: String::new(), read_ended: false })
        }
    }

    #[test_id(t6ppvvt)]
    #[test]
    fn detect_invalid_closed_reads_only_closes_after_its_watermark() {
        let run = testkit::TempDir::new("strand-ic-watermark");
        let cfg = Config { run: Some(run.to_path_buf()), ..Config::test_fixture() };
        let rows = r#"[{"id":"sp-a","closed_at":"2026-11-01T00:00:00Z","close_reason":"done"},{"id":"sp-b","closed_at":"2026-11-03T00:00:00Z","close_reason":"done"}]"#;
        let first = std::sync::Arc::new(Closes(Default::default(), rows, 0));
        detect_invalid_closed(&cfg, &LcClient::over(first.clone(), true));
        let seen = first.0.lock().unwrap().clone();
        assert!(!seen[0].iter().any(|a| a == "--closed-after"), "an absent watermark reads from the start: {seen:?}");
        let file = run.join("strand").join("invalid-closed.watermark");
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "2026-11-03T00:00:00Z");

        let second = std::sync::Arc::new(Closes(Default::default(), "[]", 0));
        detect_invalid_closed(&cfg, &LcClient::over(second.clone(), true));
        let seen = second.0.lock().unwrap().clone();
        assert_eq!(seen[0], ["content", "list", "--all", "--closed-after", "2026-11-03T00:00:00Z", "--limit", "0", "--brief", "--json"]);
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "2026-11-03T00:00:00Z", "an empty read leaves the watermark");
    }

    #[test_id(ttbw9m6)]
    #[test]
    fn the_invalid_closed_read_asks_for_no_free_text_and_classifies_as_before() {
        let rows = r#"[{"id":"sp-a","title":"A","status":"closed","closed_at":"2026-11-03T00:00:00Z","close_reason":"done"},{"id":"sp-b","title":"B","status":"closed","closed_at":"2026-11-03T00:00:00Z","close_reason":"TODO later"},{"id":"sp-c","title":"C","status":"closed","closed_at":"2026-11-03T00:00:00Z","close_reason":"the real fix is pending"}]"#;
        let door = std::sync::Arc::new(Closes(Default::default(), rows, 0));
        let out = detect_invalid_closed_since(&Config::test_fixture(), &LcClient::over(door.clone(), true), "2026-11-01T00:00:00Z");
        assert!(door.0.lock().unwrap()[0].iter().any(|a| a == "--brief"));
        assert_eq!(
            out,
            "INVALID-CLOSED sp-b — close reason contains 'TODO': TODO later. title: B\nUNFILED-FOLLOW sp-c — follow-on phrase 'the real fix' without a tracking reference: the real fix is pending. title: C"
        );
    }

    #[test_id(twph9vg)]
    #[test]
    fn list_beads_asks_for_no_free_text_and_parses_the_rows_as_before() {
        let door = std::sync::Arc::new(Closes(Default::default(), r#"[{"id":"sp-a","status":"open"}]"#, 0));
        let beads = list_beads(&LcClient::over(door.clone(), true), &[]);
        assert_eq!(door.0.lock().unwrap()[0], ["content", "list", "--limit", "0", "--brief", "--json"]);
        assert_eq!((beads.len(), beads[0].id.as_str(), beads[0].status.as_str()), (1, "sp-a", "open"));
    }

    #[test_id(tju6olc)]
    #[test]
    fn a_refused_read_leaves_the_watermark_where_it_was() {
        let run = testkit::TempDir::new("strand-ic-refused");
        let cfg = Config { run: Some(run.to_path_buf()), ..Config::test_fixture() };
        let file = run.join("strand").join("invalid-closed.watermark");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "2026-11-03T00:00:00Z\n").unwrap();
        let refused = std::sync::Arc::new(Closes(Default::default(), "", 3));
        assert_eq!(detect_invalid_closed(&cfg, &LcClient::over(refused, true)), "");
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "2026-11-03T00:00:00Z");
    }

    #[test_id(t47n5tg)]
    #[test]
    fn an_answered_empty_closed_read_is_some_and_a_refused_one_is_none() {
        let cfg = Config::test_fixture();
        let after = "2026-11-01T00:00:00Z";
        let empty = std::sync::Arc::new(Closes(Default::default(), "[]", 0));
        assert_eq!(invalid_closed_since(&cfg, &LcClient::over(empty, true), after), Some(String::new()));
        let refused = std::sync::Arc::new(Closes(Default::default(), "", 3));
        assert_eq!(invalid_closed_since(&cfg, &LcClient::over(refused, true), after), None);
        let blank = std::sync::Arc::new(Closes(Default::default(), "  \n", 0));
        assert_eq!(invalid_closed_since(&cfg, &LcClient::over(blank, true), after), None, "a blank answer is no answer");
    }

    #[test_id(t1ap5ot)]
    #[test]
    fn the_listings_go_to_the_content_door_with_no_bound_but_their_label() {
        let door = std::sync::Arc::new(Closes(Default::default(), "[]", 0));
        list_beads(&LcClient::over(door.clone(), true), &["--all", "--label", "incident"]);
        assert_eq!(door.0.lock().unwrap()[0], ["content", "list", "--all", "--label", "incident", "--limit", "0", "--brief", "--json"]);
    }

    #[test_id(twhf0l9)]
    #[test]
    fn invalid_closed_reads_the_rows_that_carry_a_close_reason() {
        let out = with_close_reason(r#"[{"id":"a","status":"open","close_reason":"done"},{"id":"b","status":"closed","close_reason":""},{"id":"c","status":"closed"}]"#);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["id"], "a");
        assert_eq!(with_close_reason("not json"), "not json");
    }

    fn invalid_closed_rows(reason: &str, allow: Option<&str>) -> String {
        let dir = testkit::TempDir::new("strand-invalid-closed");
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        if let Some(a) = allow {
            std::fs::write(run.join("invalid-closed.allow"), a).unwrap();
        }
        let cfg = Config {
            home: Some(spira_config::resolve::locate_home_for_process().unwrap()),
            run: Some(run),
            id_prefix: "zz".into(),
            ..Config::test_fixture()
        };
        let raw = serde_json::json!([{"id": "zz-cr-1", "title": "t", "status": "closed", "close_reason": reason}]).to_string();
        classify_invalid_closed(&raw, &cfg)
    }

    #[test_id(tdl5j5d)]
    #[test]
    fn an_allowlisted_unfiled_follow_id_is_reported_allowed_not_unfiled() {
        let reason = "The real fix is a proper guard. Deployed the stopgap.";
        let plain = invalid_closed_rows(reason, None);
        assert!(plain.starts_with("UNFILED-FOLLOW zz-cr-1"), "{plain}");
        let allowed = invalid_closed_rows(reason, Some("zz-cr-1 follow-up filed\n"));
        assert_eq!(allowed, "ALLOWED-IC zz-cr-1 — follow-up filed");
    }

    fn livelock_report(map: &str, held: &[&str], asks: &str, ci: &str, all: &str) -> String {
        let cfg = Config { ci_label: "ci-wait".into(), groom_ask_label: "groomed-ask".into(), ..Config::test_fixture() };
        let reg = Registry::new(Some(map), &Default::default(), std::path::Path::new("nonexistent"));
        let held = held.iter().map(|h| h.to_string()).collect();
        let beads = |j: &str| model::parse_beads(j).unwrap();
        livelock_rows(&cfg, &reg, &held, &beads(asks), &beads(ci), &beads(all))
    }

    const MAP: &str = "pushrepo | /opt/pushrepo | push | origin/main | | \nprerepo | /opt/prerepo | pr | origin/main | | \n";

    // covers: UC-ops-detection-remediation-29
    #[test_id(tko4dyt)]
    #[test]
    fn an_empty_store_reports_no_livelock() {
        assert_eq!(livelock_report(MAP, &[], "[]", "[]", "[]"), "");
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(t1gz7qz)]
    #[test]
    fn an_ask_without_overseer_is_reported_and_one_with_it_is_not() {
        // literal-ok: fixture bead labelled with the fixture config's own ask label
        let asks = r#"[{"id":"sp-a1","title":"q","labels":["needs-operator"]},{"id":"sp-a2","title":"q","labels":["needs-operator","overseer"]}]"#;
        let out = livelock_report(MAP, &[], asks, "[]", "[]");
        assert!(out.contains("LIVELOCK sp-a1 ask-no-overseer"), "{out}");
        assert!(!out.contains("sp-a2"), "{out}");
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(tiju6fy)]
    #[test]
    fn a_ci_wait_is_stuck_on_a_push_repo_and_waiting_on_a_pr_repo() {
        let ci = r#"[{"id":"sp-c1","status":"open","labels":["ci-wait","repo:pushrepo"]},{"id":"sp-c2","status":"open","labels":["ci-wait","repo:prerepo"]},{"id":"sp-c3","status":"closed","labels":["ci-wait","repo:pushrepo"]}]"#;
        let out = livelock_report(MAP, &[], "[]", ci, "[]");
        assert!(out.contains("LIVELOCK sp-c1 ci-stuck"), "{out}");
        assert!(!out.contains("sp-c2") && !out.contains("sp-c3"), "{out}");
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(tu91alm)]
    #[test]
    fn a_repo_label_outside_the_map_is_unmapped_unless_the_bead_is_held_or_groom_asked() {
        let all = r#"[
            {"id":"sp-m1","labels":["repo:pushrepo"]},
            {"id":"sp-m2","labels":["repo:bogus"]},
            {"id":"sp-m3","labels":["repo:bogus"]},
            {"id":"sp-m4","labels":["repo:bogus","groomed-ask"]},
            {"id":"sp-m5","labels":["plan"]}]"#;
        let out = livelock_report(MAP, &["sp-m3"], "[]", "[]", all);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("LIVELOCK sp-m2 unmapped-repo — repo:bogus"), "{out}");
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(twx1x0z)]
    #[test]
    fn with_no_repo_map_no_bead_is_unmapped() {
        let cfg = Config::test_fixture();
        let reg = Registry::new(None, &Default::default(), std::path::Path::new("nonexistent"));
        let all = model::parse_beads(r#"[{"id":"sp-n1","labels":["repo:bogus"]}]"#).unwrap();
        assert_eq!(livelock_rows(&cfg, &reg, &HashSet::new(), &[], &[], &all), "");
    }

    fn classify(reason: &str) -> String {
        let raw = serde_json::json!([{"id": "zz-c", "title": "t", "close_reason": reason}]).to_string();
        let cfg = Config { id_prefix: "zz".into(), ..Config::test_fixture() };
        classify_invalid_closed(&raw, &cfg)
    }

    // covers: UC-ops-detection-remediation-30
    #[test_id(tzepolc)]
    #[test]
    fn a_permanent_fix_needed_or_temporary_workaround_close_is_invalid_closed() {
        for r in ["PERMANENT FIX NEEDED: add real detection", "Applied a temporary workaround, real fix still needed.", "mitigated-only", "TODO wire it"] {
            assert!(classify(r).starts_with("INVALID-CLOSED zz-c"), "{r}: {}", classify(r));
        }
    }

    // covers: UC-ops-detection-remediation-30
    #[test_id(t0k9993)]
    #[test]
    fn implied_follow_on_work_with_no_tracking_reference_is_unfiled_follow() {
        for r in ["Builders should add external_ref to the list output.", "The real fix requires a migration.", "Needs a follow-up."] {
            assert!(classify(r).starts_with("UNFILED-FOLLOW zz-c"), "{r}: {}", classify(r));
        }
    }

    // covers: UC-ops-detection-remediation-30
    #[test_id(tpxnvqz)]
    #[test]
    fn a_tracking_reference_acquits_a_follow_on_but_only_under_the_configured_id_prefix() {
        assert_eq!(classify("Follow-up filed as zz-abc12."), "");
        assert_eq!(classify("Follow-up tracked in acme/widgets#42."), "");
        assert_eq!(classify("Follow-up at https://example.test/issues/9"), "");
        assert!(classify("Follow-up filed as sp-abc12.").starts_with("UNFILED-FOLLOW"));
    }

    // covers: UC-ops-detection-remediation-30
    #[test_id(tiw3553)]
    #[test]
    fn a_clean_close_and_the_word_temporary_describing_something_else_are_not_flagged() {
        for r in ["fixed: zz-c commit abc123 landed on main", "Removed the temporary directory after the run.", "Pulled the change from upstream.", "temporary", ""] {
            assert_eq!(classify(r), "", "{r}");
        }
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(tubtjky)]
    #[test]
    fn claimable_and_blocked_beads_beside_the_flagged_ones_are_never_reported() {
        let all = r#"[
            {"id":"sp-good","status":"open","labels":["plan","repo:pushrepo"]},
            {"id":"sp-blocked","status":"open","labels":["plan","repo:pushrepo"],"dependency_count":1},
            {"id":"sp-bad","status":"open","labels":["plan","repo:bogus"]}]"#;
        let out = livelock_report(MAP, &[], "[]", "[]", all);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("sp-bad unmapped-repo"), "{out}");
    }
}
