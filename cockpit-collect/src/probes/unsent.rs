//! `unsent_keys` — the slow-tier git-graph block: the unsent-branch backlog across every
//! registered repository, the landing gate's yield, suite-time totals, the 24h landing
//! funnel, ACCEPTANCE and the GATE/landing.progress passthrough. Ported from
//! `spira/cockpit.sh` (DESIGN.md "Design"); this is the single most `git`-call-heavy probe,
//! so every call here mirrors the bash's own repo/ref resolution through the `lib.sh` bridge
//! rather than re-deriving it.

use super::{push, Cfg, Kv};
use crate::io;
use crate::quoting::{parse_iso8601, rel_age, sanitize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn unsent_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    branch_backlog_section(&mut out, cfg);
    yield_section(&mut out);
    suite_times_section(&mut out);
    landing_funnel_section(&mut out, &io::run_dir(), cfg);
    acceptance_section(&mut out, cfg);
    gate_section(&mut out, &io::run_dir());
    out
}

// ---------------------------------------------------------------------------------------
// The unsent backlog: how many `spira/*` branches exist right now, across every repository,
// classified into done/unadopted/orphan-work/round-branches/protected/batched-stranded/
// batched-too-long/closed-stranded — and the oldest unsent branch's age.
// ---------------------------------------------------------------------------------------

fn branch_backlog_section(out: &mut Kv, cfg: &Cfg) {
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
    let mut awaiting_round = 0usize;

    let reg = io::repo_registry();
    let qdir = std::path::PathBuf::from(&cfg.queue_dir);
    let in_delivery: std::collections::HashMap<String, super::lc::LcRow> =
        super::lc::list(Some("IN_DELIVERY")).unwrap_or_default().into_iter().map(|r| (r.id.clone(), r)).collect();
    // A branch's bead state is its lifecycle row's (design §3.4, sp-mve9i): one read for
    // every branch instead of a bd show per branch.
    let lc_index = super::lc::state_index();
    let batch_wait: i64 = cfg.queue_batch_wait;

    for rname in reg.all() {
        let Some(rp) = reg.root(&rname) else { continue };
        let rp_path = Path::new(&rp);
        if !rp_path.join(".git").exists() {
            continue;
        }
        // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
        // spira_landref/spira_landrefs/ref_remote lib.sh seam.
        let base = spira_config::repos::landref(&reg, &rname);
        let land = reg.land(&rname);

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

                    let status = bead_status(id, lc_index.as_ref());
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
                        BeadStatus::Closed { submitted } => {
                            done += 1;
                            match closed_branch_class(&land, submitted, b.starts_with("spira/queue/"), in_delivery.contains_key(id)) {
                                ClosedClass::AwaitingRound => awaiting_round += 1,
                                ClosedClass::Delivering | ClosedClass::BatchBranch => {}
                                ClosedClass::Stranded => {
                                    closed_stranded += 1;
                                    if let Some(ts) = ts {
                                        if closed_stranded_oldest.map(|o| ts < o).unwrap_or(true) {
                                            closed_stranded_oldest = Some(ts);
                                        }
                                    }
                                }
                            }
                        }
                        BeadStatus::Open => {}
                    }
                    n += 1;
                    if !b.starts_with("spira/queue/") && !matches!(status, BeadStatus::Closed { .. }) {
                        if let Some(ts) = ts {
                            if oldest.map(|o| ts < o).unwrap_or(true) {
                                oldest = Some(ts);
                            }
                        }
                    }
                    if batched_check_applies(&status, in_delivery.contains_key(id)) {
                        if let Some(row) = in_delivery.get(id) {
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
                            if let Some(ep) = row.updated_at {
                                if ep > 0 && (now - ep) > batch_wait {
                                    batched_too_long += 1;
                                    batched_too_long_names.push(id.to_string());
                                }
                            }
                        }
                    }
                }
            }
            None => fail = true,
        }

        // The batch PR branches only: a suite-state edit is a bead on spira/<id> like any
        // other change since sp-lck63, so no `spira-suite-state/*` namespace is protected.
        for ns in ["refs/heads/spira/queue/"] {
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
    push(out, "SP_AWAITING_ROUND", awaiting_round.to_string());
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

/// A submitted bead's branch under queue.local waits for a round by design; only elsewhere
/// is a closed bead's surviving branch a stranding.
fn awaits_round(land: &str, submitted: bool) -> bool {
    submitted && land == "queue.local"
}

/// How a done branch (its bead past the builder) is counted.
#[derive(Debug, PartialEq, Eq)]
enum ClosedClass {
    /// Submitted under queue.local: waits for a round by design.
    AwaitingRound,
    /// The machine has it IN_DELIVERY: the batch check judges it.
    Delivering,
    /// A batch PR branch: never a stranding.
    BatchBranch,
    /// Done and nobody is moving it.
    Stranded,
}

fn closed_branch_class(land: &str, submitted: bool, batch_branch: bool, delivering: bool) -> ClosedClass {
    if delivering {
        ClosedClass::Delivering
    } else if awaits_round(land, submitted) {
        ClosedClass::AwaitingRound
    } else if batch_branch {
        ClosedClass::BatchBranch
    } else {
        ClosedClass::Stranded
    }
}

/// The BATCHED checks (in an open batch? delivering too long?) apply to every branch whose
/// bead the machine has IN_DELIVERY, whatever else it reads as.
fn batched_check_applies(_status: &BeadStatus, delivering: bool) -> bool {
    delivering
}

#[derive(Debug, PartialEq, Eq)]
enum BeadStatus {
    ProbeFailed,
    NoBead,
    Closed { submitted: bool },
    Open,
}

/// A branch's bead, by its lifecycle row: past the builder is "done" (submitted while the
/// machine still delivers it, over once terminal), anything earlier is open. No machine
/// answer is a probe fault. A bead with no lifecycle row falls back to bd for existence
/// only (content, not state): present in bd it counts as open — its state cannot be told,
/// so it is never read as done — and absent it is no bead.
fn bead_status(id: &str, lc: Option<&HashMap<String, super::lc::Row>>) -> BeadStatus {
    let Some(lc) = lc else { return BeadStatus::ProbeFailed };
    if let Some(s) = status_of_row(lc.get(id)) {
        return s;
    }
    match bead_exists(id) {
        BeadStatus::Closed { .. } => BeadStatus::Open,
        other => other,
    }
}

/// The lifecycle half of [`bead_status`]; `None` when the machine has no row.
fn status_of_row(row: Option<&super::lc::Row>) -> Option<BeadStatus> {
    let row = row?;
    Some(if row.past_builder() { BeadStatus::Closed { submitted: !row.terminal() } } else { BeadStatus::Open })
}

/// `BD_TIMEOUT=2 bdjson show <id>` — deliberately short: this runs once per rowless branch
/// across every repository on the 600s tier, and a hung `bd` must not stall the whole probe.
fn bead_exists(id: &str) -> BeadStatus {
    match bead_exists_at(id, "2") {
        // An empty/error-shaped answer under the short timeout is ambiguous (sp-cyc1t: an extant
        // closed bead read as NoBead). Re-confirm once with a longer timeout; only a second
        // not-found counts as NoBead, anything else is a probe fault.
        BeadStatus::NoBead => bead_exists_at(id, "10"),
        other => other,
    }
}

/// `Closed` here only means "bd has it" — [`bead_status`] reads no state from bd.
fn bead_exists_at(id: &str, timeout: &str) -> BeadStatus {
    let prev = std::env::var("BD_TIMEOUT").ok();
    std::env::set_var("BD_TIMEOUT", timeout);
    let raw = io::bdjson(&["show", id]);
    match prev {
        Some(p) => std::env::set_var("BD_TIMEOUT", p),
        None => std::env::remove_var("BD_TIMEOUT"),
    }
    match io::bd_rows(raw) {
        None => BeadStatus::ProbeFailed,
        Some(rows) => match rows.first().and_then(|r| r.get("id")).and_then(Value::as_str) {
            Some(_) => BeadStatus::Closed { submitted: false },
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

fn landing_funnel_section(out: &mut Kv, run: &Path, cfg: &Cfg) {
    let scope = &cfg.scope_label;
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let home_repo = io::repo_registry().home_repo().to_string();
    // Every plan bead's content; which of them the builder finished is the lifecycle row's
    // to say (design §3.4, sp-mve9i), not bd's `closed`.
    let raw = io::bdq(&["list", "--all", "--limit", "0", "--label", &label, "--json"]);
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
    let Some(lc_index) = super::lc::state_index() else {
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
        let Some(ts) = finished_at(i, lc_index.get(id)) else { continue };
        let closed_at = i.get("closed_at").or_else(|| i.get("updated_at")).and_then(Value::as_str).unwrap_or("");
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
    let reg = io::repo_registry();
    for r in repos {
        let Some(rp) = reg.root(r) else { continue };
        let rp_path = Path::new(&rp);
        if !rp_path.join(".git").exists() {
            continue;
        }
        // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
        // spira_landrefs/ref_remote lib.sh seam.
        let Some((base, local)) = spira_config::repos::landrefs(&reg, &rp) else { continue };
        let refs = match local {
            Some(l) => format!("{base} {l}"),
            None => base.clone(),
        };
        if let Some(remote) = spira_config::repos::ref_remote(&base, Some(&rp)) {
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

    let cert_win: i64 = cfg.cert_window_mins;

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
        if !awaits_certification(lc_index.get(r.id.as_str())) {
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

fn acceptance_section(out: &mut Kv, cfg: &Cfg) {
    let mut verdict = "?".to_string();
    let mut tag = "-".to_string();
    let mut at = "?".to_string();
    let mut since = "?".to_string();

    let prod = &cfg.prod;
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

/// An unlanded, branch-carrying finished bead the funnel counts (split by age into stranded
/// and awaiting): one the machine holds handed on and not yet certified. Before sp-mve9i the
/// test was "no lifecycle row at all" — a bd-closed bead the machine never took — which can
/// no longer happen once "finished" is the row's own answer, so it left the counters at zero.
fn awaits_certification(row: Option<&super::lc::Row>) -> bool {
    row.is_some_and(|r| r.state == "SUBMITTED")
}

/// When a plan bead's builder finished it, for the 24h funnel: only a bead the machine has
/// past the builder counts; the time is bd's `closed_at`/`updated_at` (content), else the
/// lifecycle row's lease/state time is unknown and the bead is skipped.
fn finished_at(bead: &Value, row: Option<&super::lc::Row>) -> Option<i64> {
    if !row.is_some_and(|r| r.past_builder()) {
        return None;
    }
    let at = bead.get("closed_at").or_else(|| bead.get("updated_at")).and_then(Value::as_str).unwrap_or("");
    parse_iso8601(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> super::super::lc::Row {
        super::super::lc::Row { bead_id: "sp-a".into(), state: state.into(), ..Default::default() }
    }

    /// sp-mve9i: a branch's bead is done by its lifecycle row, never bd's status.
    #[test]
    fn a_branch_bead_is_done_by_its_lifecycle_row() {
        assert_eq!(status_of_row(Some(&row("SUBMITTED"))), Some(BeadStatus::Closed { submitted: true }));
        assert_eq!(status_of_row(Some(&row("IN_DELIVERY"))), Some(BeadStatus::Closed { submitted: true }));
        assert_eq!(status_of_row(Some(&row("LANDED"))), Some(BeadStatus::Closed { submitted: false }));
        assert_eq!(status_of_row(Some(&row("REWORK"))), Some(BeadStatus::Open));
        assert_eq!(status_of_row(None), None);
        assert_eq!(bead_status("sp-a", None), BeadStatus::ProbeFailed);
    }

    /// sp-mve9i: the funnel counts what the machine has past the builder, whatever bd says.
    #[test]
    fn the_funnel_counts_lifecycle_finished_beads() {
        let b: Value = serde_json::json!({"id": "sp-a", "status": "open", "closed_at": "2026-10-01T00:00:00Z"});
        assert!(finished_at(&b, Some(&row("CERTIFIED"))).is_some());
        let closed: Value = serde_json::json!({"id": "sp-a", "status": "closed", "closed_at": "2026-10-01T00:00:00Z"});
        assert!(finished_at(&closed, Some(&row("REWORK"))).is_none());
        assert!(finished_at(&closed, None).is_none());
    }

    /// sp-mve9i: the unlanded/stranded funnel counts finished beads still waiting on their
    /// certification — a SUBMITTED row — never "no row", which a finished bead cannot have.
    #[test]
    fn the_unlanded_funnel_counts_beads_awaiting_certification() {
        assert!(awaits_certification(Some(&row("SUBMITTED"))));
        assert!(!awaits_certification(Some(&row("CERTIFIED"))), "certified: the round's to deliver");
        assert!(!awaits_certification(Some(&row("IN_DELIVERY"))), "judged by its batch");
        assert!(!awaits_certification(Some(&row("LANDED"))));
        assert!(!awaits_certification(None), "not finished");
    }

    /// sp-mve9i: a branch whose bead the machine has IN_DELIVERY is past the builder, so it
    /// reads "done" — but the delivery pipeline holds it: its stranding is the BATCHED check
    /// (no open batch names it), never the closed-bead stranding. With bd status it was never
    /// "closed" here, so both checks ran as they should; with the lifecycle row they must be
    /// told apart explicitly.
    #[test]
    fn a_delivering_branch_is_judged_by_its_batch_not_as_a_stranded_closed_bead() {
        assert_eq!(closed_branch_class("push", true, false, true), ClosedClass::Delivering);
        assert_eq!(closed_branch_class("push", true, false, false), ClosedClass::Stranded);
        assert_eq!(closed_branch_class("queue.local", true, false, false), ClosedClass::AwaitingRound);
        assert_eq!(closed_branch_class("push", false, true, false), ClosedClass::BatchBranch);
        assert!(batched_check_applies(&BeadStatus::Closed { submitted: true }, true));
        assert!(batched_check_applies(&BeadStatus::Open, true));
        assert!(!batched_check_applies(&BeadStatus::Open, false));
    }

    #[test]
    fn a_submitted_bead_awaits_a_round_only_under_queue_local() {
        assert!(awaits_round("queue.local", true));
        assert!(!awaits_round("queue", true));
        assert!(!awaits_round("push", true));
        assert!(!awaits_round("queue.local", false));
    }

    #[test]
    fn missing_run_dir_renders_unsent_zero_not_refusal() {
        // With no repos configured, spira_repos resolves to nothing and the branch scan
        // loop never runs; SP_UNSENT must still read 0, not ?, matching "an empty readable
        // store yields a numeric 0" (UC-cockpit-observability-08).
        let run = testkit::TempDir::new("cc-unsent-empty");
        let no_map = run.path().join("no-map");
        let _env = crate::test_support::set_run_with(run.path(), &[("SPIRA_REPO_MAP", no_map.to_str())]);
        let mut out = Kv::new();
        branch_backlog_section(&mut out, &Cfg::default());
        let get = |k: &str| out.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_UNSENT"), Some("0".to_string()));
    }
}
