//! `unsent_keys` — the slow-tier git-graph block: the unsent-branch backlog across every
//! registered repository, the landing gate's yield, suite-time totals, the 24h landing
//! funnel, ACCEPTANCE and the GATE/landing.progress passthrough. Ported from
//! `spira/cockpit.sh` (DESIGN.md "Design"); this is the single most `git`-call-heavy probe,
//! so every call here mirrors the bash's own repo/ref resolution through the `lib.sh` bridge
//! rather than re-deriving it.

use super::{push, Kv};
use crate::io;
use crate::quoting::{parse_iso8601, rel_age, sanitize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

pub fn unsent_keys() -> Kv {
    let mut out = Kv::new();
    branch_backlog_section(&mut out);
    yield_section(&mut out);
    suite_times_section(&mut out);
    landing_funnel_section(&mut out, &io::run_dir());
    acceptance_section(&mut out);
    gate_section(&mut out, &io::run_dir());
    out
}

// ---------------------------------------------------------------------------------------
// The unsent backlog: how many `spira/*` branches exist right now, across every repository,
// classified into done/unadopted/orphan-work/round-branches/protected/batched-stranded/
// batched-too-long/closed-stranded — and the oldest unsent branch's age.
// ---------------------------------------------------------------------------------------

fn branch_backlog_section(out: &mut Kv) {
    let home = io::home_dir();
    let run = io::run_dir();
    let now = io::now();

    let mut fail = false;
    let mut n = 0usize;
    let mut oldest: Option<i64> = None;
    let mut done = 0usize;
    let mut unadopted = 0usize;
    let mut unadopted_names: Vec<String> = Vec::new();
    let mut orphan_work = 0usize;
    let mut round_branches = 0usize;
    let mut round_branches_names: Vec<String> = Vec::new();
    let mut probe_fail = 0usize;
    let mut probe_fail_names: Vec<String> = Vec::new();
    let mut protected = 0usize;
    let mut protected_names: Vec<String> = Vec::new();
    let mut batched_stranded = 0usize;
    let mut batched_stranded_names: Vec<String> = Vec::new();
    let mut batched_too_long = 0usize;
    let mut batched_too_long_names: Vec<String> = Vec::new();
    let mut closed_stranded = 0usize;
    let mut closed_stranded_oldest: Option<i64> = None;

    let reg = io::repo_registry();
    let qdir = std::env::var("SPIRA_QUEUE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| run.join("queue"));
    let batch_wait: i64 = std::env::var("SPIRA_QUEUE_BATCH_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(1800);

    for rname in reg.all() {
        let Some(rp) = reg.root(&rname) else { continue };
        let rp_path = Path::new(&rp);
        if !rp_path.join(".git").exists() {
            continue;
        }
        let base = io::lib_call(&home, "spira_landref", &[&rname]);

        match io::git(rp_path, &["for-each-ref", "--format=%(refname:short) %(committerdate:unix)", "refs/heads/spira/*"]) {
            Some(brs) => {
                for line in brs.lines() {
                    let Some((b, ts_s)) = line.rsplit_once(' ') else { continue };
                    if b.is_empty() {
                        continue;
                    }
                    if b.starts_with("spira/queue/") {
                        continue;
                    }
                    let ts: Option<i64> = ts_s.parse().ok();
                    let id = b.trim_start_matches("spira/");

                    let status = bead_status(id);
                    match status {
                        BeadStatus::ProbeFailed => {
                            probe_fail += 1;
                            probe_fail_names.push(id.to_string());
                            continue;
                        }
                        BeadStatus::NoBead => {
                            if b.starts_with("spira/round-") {
                                round_branches += 1;
                                round_branches_names.push(id.to_string());
                                continue;
                            }
                            let is_ancestor = base.as_deref().map(|base| io::git(rp_path, &["merge-base", "--is-ancestor", b, base]).is_some()).unwrap_or(false);
                            if is_ancestor {
                                unadopted += 1;
                                unadopted_names.push(id.to_string());
                            } else {
                                orphan_work += 1;
                            }
                            continue;
                        }
                        BeadStatus::Closed => {
                            done += 1;
                            if !b.starts_with("spira/queue/") {
                                closed_stranded += 1;
                                if let Some(ts) = ts {
                                    if closed_stranded_oldest.map(|o| ts < o).unwrap_or(true) {
                                        closed_stranded_oldest = Some(ts);
                                    }
                                }
                            }
                        }
                        BeadStatus::Open => {}
                    }
                    n += 1;
                    if !b.starts_with("spira/queue/") && !matches!(status, BeadStatus::Closed) {
                        if let Some(ts) = ts {
                            if oldest.map(|o| ts < o).unwrap_or(true) {
                                oldest = Some(ts);
                            }
                        }
                    }
                    if !matches!(status, BeadStatus::Closed) {
                        let ls_file = run.join("landstate").join(id);
                        if let Ok(content) = std::fs::read_to_string(&ls_file) {
                            let mut fields = content.lines().next().unwrap_or("").split_whitespace();
                            let ls_state = fields.next().unwrap_or("");
                            if ls_state == "BATCHED" {
                                let mut in_batch = false;
                                if let Ok(entries) = std::fs::read_dir(&qdir) {
                                    for e in entries.flatten() {
                                        let open_f = e.path().join("open");
                                        let Ok(oc) = std::fs::read_to_string(&open_f) else { continue };
                                        if let Some(mems) = oc.lines().find_map(|l| l.strip_prefix("members=")) {
                                            if mems.split_whitespace().any(|m| m.split(':').next() == Some(id)) {
                                                in_batch = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                                if !in_batch {
                                    batched_stranded += 1;
                                    batched_stranded_names.push(id.to_string());
                                }
                                let ls_epoch: Option<i64> = content.lines().next().unwrap_or("").split_whitespace().nth(2).and_then(|s| s.parse().ok());
                                if let Some(ep) = ls_epoch {
                                    if ep > 0 && (now - ep) > batch_wait {
                                        batched_too_long += 1;
                                        batched_too_long_names.push(id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None => fail = true,
        }

        for ns in ["refs/heads/spira/queue/", "refs/heads/spira-suite-state/"] {
            if let Some(refs) = io::git(rp_path, &["for-each-ref", "--format=%(refname:short)", ns]) {
                for pb in refs.lines() {
                    if pb.is_empty() {
                        continue;
                    }
                    protected += 1;
                    protected_names.push(pb.trim_start_matches("spira/").to_string());
                }
            }
        }
        let open_f = qdir.join(rname).join("open");
        if let Ok(content) = std::fs::read_to_string(&open_f) {
            if let Some(ob) = content.lines().find_map(|l| l.strip_prefix("branch=")) {
                let ob_id = ob.trim_start_matches("spira/");
                if !protected_names.iter().any(|p| p == ob_id) {
                    protected += 1;
                    protected_names.push(ob_id.to_string());
                }
            }
        }
    }

    push(out, "SP_BRANCH_DONE", done.to_string());
    push(out, "SP_UNADOPTED", unadopted.to_string());
    super::push(out, "SP_UNADOPTED_NAMES", unadopted_names.join(" "));
    push(out, "SP_ORPHAN_WORK", orphan_work.to_string());
    push(out, "SP_ROUND_BRANCHES", round_branches.to_string());
    super::push(out, "SP_ROUND_BRANCHES_NAMES", round_branches_names.join(" "));
    push(out, "SP_PROBE_FAIL", probe_fail.to_string());
    super::push(out, "SP_PROBE_FAIL_NAMES", probe_fail_names.join(" "));
    push(out, "SP_PROTECTED", protected.to_string());
    super::push(out, "SP_PROTECTED_NAMES", protected_names.join(" "));
    push(out, "SP_BATCHED_STRANDED", batched_stranded.to_string());
    super::push(out, "SP_BATCHED_STRANDED_NAMES", batched_stranded_names.join(" "));
    push(out, "SP_BATCHED_TOO_LONG", batched_too_long.to_string());
    super::push(out, "SP_BATCHED_TOO_LONG_NAMES", batched_too_long_names.join(" "));
    push(out, "SP_CLOSED_STRANDED", closed_stranded.to_string());
    if closed_stranded > 0 {
        if let Some(o) = closed_stranded_oldest {
            push(out, "SP_CLOSED_STRANDED_OLDEST_H", ((now - o) / 3600).to_string());
        } else {
            push(out, "SP_CLOSED_STRANDED_OLDEST_H", "0");
        }
    } else {
        push(out, "SP_CLOSED_STRANDED_OLDEST_H", "0");
    }
    if fail {
        push(out, "SP_UNSENT", "?");
        push(out, "SP_UNSENT_OLDEST_H", "?");
    } else {
        push(out, "SP_UNSENT", n.to_string());
        if n > 0 {
            if let Some(o) = oldest {
                push(out, "SP_UNSENT_OLDEST_H", ((now - o) / 3600).to_string());
                return;
            }
        }
        push(out, "SP_UNSENT_OLDEST_H", "0");
    }
}

enum BeadStatus {
    ProbeFailed,
    NoBead,
    Closed,
    Open,
}

/// `BD_TIMEOUT=2 bdjson show <id>` — deliberately short: this runs once per branch across
/// every repository on the 600s tier, and a hung `bd` must not stall the whole probe.
fn bead_status(id: &str) -> BeadStatus {
    let prev = std::env::var("BD_TIMEOUT").ok();
    std::env::set_var("BD_TIMEOUT", "2");
    let raw = io::bdjson(&["show", id]);
    match prev {
        Some(p) => std::env::set_var("BD_TIMEOUT", p),
        None => std::env::remove_var("BD_TIMEOUT"),
    }
    match io::bd_rows(raw) {
        None => BeadStatus::ProbeFailed,
        Some(rows) => match rows.first().and_then(|r| r.get("status")).and_then(Value::as_str) {
            Some("closed") => BeadStatus::Closed,
            Some(_) => BeadStatus::Open,
            None => BeadStatus::NoBead,
        },
    }
}

// ---------------------------------------------------------------------------------------
// What the landing gate is worth: yield.sh's own reading, passed through untouched.
// ---------------------------------------------------------------------------------------

fn yield_section(out: &mut Kv) {
    const MAP: &[(&str, &str)] = &[
        ("YIELD_REDS", "SP_YIELD_REDS"),
        ("YIELD_DEFECT", "SP_YIELD_DEFECT"),
        ("YIELD_FAULT", "SP_YIELD_FAULT"),
        ("YIELD_UNKNOWN", "SP_YIELD_UNKNOWN"),
        ("YIELD_SOLO_MED", "SP_YIELD_SOLO_MED"),
        ("YIELD_CONC_MED", "SP_YIELD_CONC_MED"),
        ("YIELD_TOP_FAULT", "SP_YIELD_TOP_FAULT"),
    ];
    let report = io::run_tool("yield.sh", &["report"], None).unwrap_or_default();
    let mut found: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for line in report.lines() {
        if let Some((k, v)) = line.split_once('=') {
            found.insert(k, v.to_string());
        }
    }
    for (src, dst) in MAP {
        push(out, dst, found.get(*src).cloned().unwrap_or_else(|| "?".to_string()));
    }
}

fn suite_times_section(out: &mut Kv) {
    let mut sum = "?".to_string();
    let mut wall = "?".to_string();
    if let Some(json) = io::run_tool("tsd-query.sh", &["last-run"], None) {
        if !json.trim().is_empty() {
            if let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json.trim()) {
                if let Some(row) = rows.first() {
                    sum = row.get("sum_wall").map(value_to_plain).unwrap_or_else(|| "?".to_string());
                    wall = row.get("batch_wall").filter(|v| !v.is_null()).map(value_to_plain).unwrap_or_else(|| "?".to_string());
                }
            }
        }
    }
    push(out, "SP_SUITE_LAST_SUM", sum);
    push(out, "SP_SUITE_LAST_WALL", wall);
}

fn value_to_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "?".to_string(),
    }
}

// ---------------------------------------------------------------------------------------
// The 24h landing funnel: SP_CLOSED/SP_LANDED/SP_UNLANDED_N/SP_STRANDED_N/SP_CERT_N/
// SP_FUNNEL_DONE_AGE. A closed bead is LANDED only when a commit subject on the base names
// it (a body mention does not count) — `law-aeon-commits-name-their-bead`.
// ---------------------------------------------------------------------------------------

fn landing_funnel_section(out: &mut Kv, run: &Path) {
    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let home_repo = io::repo_registry().home_repo().to_string();
    let raw = io::bdq(&["list", "--status", "closed", "--limit", "0", "--label", &label, "--json"]);
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        for k in ["SP_CLOSED", "SP_LANDED", "SP_UNLANDED_N", "SP_STRANDED_N", "SP_CERT_N", "SP_FUNNEL_DONE_AGE"] {
            push(out, k, "?");
        }
        return;
    };
    let Some(rows) = io::bd_rows(Some(raw)) else {
        for k in ["SP_CLOSED", "SP_LANDED", "SP_UNLANDED_N", "SP_STRANDED_N", "SP_CERT_N", "SP_FUNNEL_DONE_AGE"] {
            push(out, k, "?");
        }
        return;
    };
    let now = io::now();
    let cut = now - 24 * 3600;

    struct Row {
        id: String,
        repo: String,
        closed_at: String,
    }
    let mut closed_pairs: Vec<Row> = Vec::new();
    for i in &rows {
        let id = i.get("id").and_then(Value::as_str).unwrap_or("");
        if !run.join(format!("{id}.log")).exists() {
            continue;
        }
        let closed_at = i.get("closed_at").or_else(|| i.get("updated_at")).and_then(Value::as_str).unwrap_or("");
        let Some(ts) = parse_iso8601(closed_at) else { continue };
        if ts < cut {
            continue;
        }
        let repo = i
            .get("labels")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().filter_map(Value::as_str).find_map(|l| l.strip_prefix("repo:")))
            .unwrap_or(&home_repo)
            .to_string();
        closed_pairs.push(Row { id: id.to_string(), repo, closed_at: closed_at.to_string() });
    }
    if closed_pairs.is_empty() {
        push(out, "SP_CLOSED", "0");
        push(out, "SP_LANDED", "0");
        push(out, "SP_UNLANDED_N", "0");
        push(out, "SP_STRANDED_N", "0");
        push(out, "SP_CERT_N", "0");
        push(out, "SP_FUNNEL_DONE_AGE", "");
        return;
    }

    let repos: HashSet<&str> = closed_pairs.iter().map(|r| r.repo.as_str()).collect();
    let mut subjects: Vec<String> = Vec::new();
    let mut branches: Vec<String> = Vec::new();
    let home = io::home_dir();
    let reg = io::repo_registry();
    for r in repos {
        let Some(rp) = reg.root(r) else { continue };
        let rp_path = Path::new(&rp);
        if !rp_path.join(".git").exists() {
            continue;
        }
        let Some(refs) = io::lib_call(&home, "spira_landrefs", &[&rp]) else { continue };
        let first_ref = refs.split_whitespace().next().unwrap_or("");
        if let Some(remote) = io::lib_call(&home, "ref_remote", &[first_ref, &rp]) {
            let _ = io::git(rp_path, &["fetch", "-q", &remote]);
        }
        let mut log_args = vec!["log", "--format=%s", "-n", "2000"];
        let ref_list: Vec<&str> = refs.split_whitespace().collect();
        log_args.extend(ref_list.iter().copied());
        if let Some(log) = io::git(rp_path, &log_args) {
            subjects.push(log);
        }
        if let Some(brs) = io::git(rp_path, &["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/", "refs/remotes/*/spira/"]) {
            branches.push(brs);
        }
    }
    if subjects.is_empty() && branches.is_empty() {
        for k in ["SP_CLOSED", "SP_LANDED", "SP_UNLANDED_N", "SP_STRANDED_N", "SP_CERT_N", "SP_FUNNEL_DONE_AGE"] {
            push(out, k, "?");
        }
        return;
    }
    let subject_lines: Vec<&str> = subjects.iter().flat_map(|s| s.lines()).collect();
    let branch_lines: Vec<&str> = branches.iter().flat_map(|s| s.lines()).collect();

    let cert_win: i64 = std::env::var("SPIRA_CERT_WINDOW_MINS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
    let landstate_dir = run.join("landstate");

    let mut landed_set: HashSet<&str> = HashSet::new();
    for r in &closed_pairs {
        let i = r.id.as_str();
        let landed = subject_lines.iter().any(|s| {
            let s = s.trim();
            s == format!("spira: land {i}") || s.starts_with(&format!("spira: land {i} ")) || s.starts_with(&format!("{i}: "))
        });
        if landed {
            landed_set.insert(i);
        }
    }
    let mut anomaly = 0;
    let mut stranded = 0;
    let mut awaiting = 0;
    let mut done_oldest: Option<i64> = None;
    for r in &closed_pairs {
        if landed_set.contains(r.id.as_str()) {
            continue;
        }
        let has_br = branch_lines.iter().any(|b| b.trim_end().ends_with(&format!("/{}", r.id)));
        if !has_br {
            continue;
        }
        if landstate_dir.join(&r.id).exists() {
            continue;
        }
        anomaly += 1;
        let ts = parse_iso8601(&r.closed_at);
        if let Some(ts) = ts {
            if done_oldest.map(|d| ts < d).unwrap_or(true) {
                done_oldest = Some(ts);
            }
        }
        let age_mins = ts.map(|t| (now - t) / 60).unwrap_or(9_999_999);
        if age_mins > cert_win {
            stranded += 1;
        } else {
            awaiting += 1;
        }
    }
    push(out, "SP_CLOSED", closed_pairs.len().to_string());
    push(out, "SP_LANDED", landed_set.len().to_string());
    push(out, "SP_UNLANDED_N", anomaly.to_string());
    push(out, "SP_STRANDED_N", stranded.to_string());
    push(out, "SP_CERT_N", awaiting.to_string());
    push(out, "SP_FUNNEL_DONE_AGE", done_oldest.map(|d| rel_age(now - d)).unwrap_or_default());
}

// ---------------------------------------------------------------------------------------
// ACCEPTANCE: did the last release actually install on a clean machine, from the
// `refs/notes/acceptance` note on the newest release tag that carries one.
// ---------------------------------------------------------------------------------------

fn acceptance_section(out: &mut Kv) {
    let mut verdict = "?".to_string();
    let mut tag = "-".to_string();
    let mut at = "?".to_string();
    let mut since = "?".to_string();

    let prod = std::env::var("SPIRA_PROD").unwrap_or_default();
    if !prod.is_empty() {
        let acc_repo = prod.trim_end_matches("/spira").to_string();
        let acc_repo_path = Path::new(&acc_repo);
        if acc_repo_path.join(".git").exists() {
            let noted = io::git(acc_repo_path, &["notes", "--ref=acceptance", "list"])
                .map(|s| s.lines().filter_map(|l| l.split_whitespace().nth(1).map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut acc_obj: Option<String> = None;
            if !noted.is_empty() {
                let mut tags: Vec<String> = io::git(acc_repo_path, &["tag", "-l", "spira-release-*"]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
                tags.sort();
                tags.reverse();
                'outer: for t in &tags {
                    for rev in [t.clone(), format!("{t}^{{commit}}")] {
                        if let Some(c) = io::git(acc_repo_path, &["rev-parse", "-q", "--verify", &rev]) {
                            let c = c.trim().to_string();
                            if !c.is_empty() && noted.contains(&c) {
                                acc_obj = Some(c);
                                break 'outer;
                            }
                        }
                    }
                }
                if acc_obj.is_none() {
                    acc_obj = noted.last().cloned();
                }
            }
            if let Some(obj) = acc_obj {
                tag = io::git(acc_repo_path, &["tag", "--points-at", &obj])
                    .and_then(|s| s.lines().find(|l| l.starts_with("spira-release-")).map(String::from))
                    .unwrap_or_else(|| "-".to_string());
                let note = io::git(acc_repo_path, &["notes", "--ref=acceptance", "show", &obj]).unwrap_or_default();
                verdict = if note.starts_with("PASS") {
                    "PASS".to_string()
                } else if note.starts_with("FAIL") {
                    "FAIL".to_string()
                } else if note.is_empty() {
                    "?".to_string()
                } else {
                    note.lines().next().unwrap_or("").chars().take(8).collect()
                };
                at = io::git(acc_repo_path, &["log", "-1", "--format=%ct", &obj]).map(|s| s.trim().to_string()).unwrap_or_else(|| "?".to_string());
                if tag != "-" {
                    let mut tags2: Vec<String> = io::git(acc_repo_path, &["tag", "-l", "spira-release-*"]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
                    tags2.sort();
                    let idx = tags2.iter().position(|t| t == &tag);
                    since = idx.map(|i| (tags2.len() - i - 1).to_string()).unwrap_or_else(|| "?".to_string());
                }
            } else if io::git(acc_repo_path, &["rev-parse", "--git-dir"]).is_some() {
                verdict = "NEVER".to_string();
                since = io::git(acc_repo_path, &["tag", "-l", "spira-release-*"]).map(|s| s.lines().count().to_string()).unwrap_or_else(|| "0".to_string());
            }
        }
    }
    push(out, "SP_ACCEPT_VERDICT", verdict);
    push(out, "SP_ACCEPT_TAG", tag);
    push(out, "SP_ACCEPT_AT", at);
    push(out, "SP_ACCEPT_SINCE", since);
}

// ---------------------------------------------------------------------------------------
// GATE: landing.status passthrough + landing.progress rows.
// ---------------------------------------------------------------------------------------

fn gate_section(out: &mut Kv, run: &Path) {
    let status_path = run.join("landing.status");
    if let Ok(content) = std::fs::read_to_string(&status_path) {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                push(out, k, v.to_string());
            }
        }
    } else {
        for k in ["SP_LAND_AT", "SP_LAND_RC", "SP_LAND_BRANCHES", "SP_LAND_MOVED"] {
            push(out, k, "?");
        }
    }

    let progress_path = run.join("landing.progress");
    let mut n = 0;
    if let Ok(content) = std::fs::read_to_string(&progress_path) {
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let mut s = sanitize(line);
            s.truncate(120);
            push(out, &format!("SP_LANDPROG{n}"), s);
            n += 1;
        }
    }
    push(out, "SP_LANDPROG_N", n.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landed_subject_recognises_land_and_colon_forms() {
        let subjects = ["spira: land sp-abc \u{2014} title here".to_string(), "sp-xyz: some other commit".to_string()];
        let lines: Vec<&str> = subjects.iter().flat_map(|s| s.lines()).collect();
        let landed = |id: &str| {
            lines.iter().any(|s| {
                let s = s.trim();
                s == format!("spira: land {id}") || s.starts_with(&format!("spira: land {id} ")) || s.starts_with(&format!("{id}: "))
            })
        };
        assert!(landed("sp-abc"));
        assert!(landed("sp-xyz"));
        assert!(!landed("sp-none"));
    }

    #[test]
    fn missing_run_dir_renders_unsent_zero_not_refusal() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        // With no repos configured, spira_repos resolves to nothing and the branch scan
        // loop never runs; SP_UNSENT must still read 0, not ?, matching "an empty readable
        // store yields a numeric 0" (UC-cockpit-observability-08).
        let run = testkit::TempDir::new("cc-unsent-empty");
        std::env::set_var("SPIRA_RUN", run.path());
        std::env::set_var("SPIRA_REPO_MAP", run.path().join("no-map"));
        let mut out = Kv::new();
        branch_backlog_section(&mut out);
        let get = |k: &str| out.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_UNSENT"), Some("0".to_string()));
    }
}
