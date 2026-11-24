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
    let mut t = Tally::default();
    let mut protected = 0usize;
    let mut protected_names: Vec<String> = Vec::new();

    let reg = io::repo_registry();
    let forge_repos: Vec<String> = reg.all().into_iter().filter(|r| reg.land(r) == "queue").collect();
    let forge = forge_batches(&forge_repos, &super::round::batch_show);
    let in_delivery: std::collections::HashMap<String, super::lc::LcRow> =
        super::lc::list(Some("IN_DELIVERY")).unwrap_or_default().into_iter().map(|r| (r.id.clone(), r)).collect();
    // A branch's bead state is its lifecycle row's (design §3.4, sp-mve9i): one read for
    // every branch instead of a bd show per branch.
    let lc_index = super::lc::state_index_of(&branch_ids(&reg));
    let batch_wait: i64 = cfg.queue_batch_wait;
    let grace: i64 = cfg.batched_stranded_grace;

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
        let round_members = if in_delivery.is_empty() { Some(HashSet::new()) } else { round_status_members(&rname) };
        let forge_batch = forge.get(&rname);

        match io::git_held(rp_path, &["for-each-ref", "--format=%(refname:short) %(committerdate:unix)", "refs/heads/spira/*"]) {
            Some(brs) => {
                let on_base = |b: &str| base.as_deref().is_some_and(|base| io::git_held(rp_path, &["merge-base", "--is-ancestor", b, base]).is_some());
                let ctx = Ctx { now, batch_wait, grace, land: &land, in_delivery: &in_delivery, round_members: &round_members, forge_batch };
                t.tally(&brs, &ctx, &|id| bead_status(id, lc_index.as_ref()), &on_base);
            }
            None => fail = true,
        }

        // The batch PR branches only: every other change is a bead on spira/<id>.
        for ns in ["refs/heads/spira/queue/"] {
            if let Some(refs) = io::git_held(rp_path, &["for-each-ref", "--format=%(refname:short)", ns]) {
                for pb in refs.lines() {
                    if pb.is_empty() {
                        continue;
                    }
                    protected += 1;
                    protected_names.push(pb.trim_start_matches("spira/").to_string());
                }
            }
        }
        if let Some(ob) = forge_batch.and_then(|b| b.as_ref()).and_then(|b| b.branch.as_deref()) {
            let ob_id = ob.trim_start_matches("spira/");
            if !protected_names.iter().any(|p| p == ob_id) {
                protected += 1;
                protected_names.push(ob_id.to_string());
            }
        }
    }

    push(out, "SP_BRANCH_DONE", t.done.to_string());
    push(out, "SP_UNADOPTED", t.unadopted.to_string());
    super::push(out, "SP_UNADOPTED_NAMES", t.unadopted_names.join(" "));
    push(out, "SP_ORPHAN_WORK", t.orphan_work.to_string());
    push(out, "SP_ROUND_BRANCHES", t.round_branches.to_string());
    super::push(out, "SP_ROUND_BRANCHES_NAMES", t.round_branches_names.join(" "));
    push(out, "SP_PROBE_FAIL", t.probe_fail.to_string());
    super::push(out, "SP_PROBE_FAIL_NAMES", t.probe_fail_names.join(" "));
    push(out, "SP_PROTECTED", protected.to_string());
    super::push(out, "SP_PROTECTED_NAMES", protected_names.join(" "));
    push(out, "SP_BATCHED_STRANDED", t.batched_stranded.to_string());
    super::push(out, "SP_BATCHED_STRANDED_NAMES", t.batched_stranded_names.join(" "));
    super::push(out, "SP_BATCHED_STRANDED_AGES", t.batched_stranded_ages.join(" "));
    push(out, "SP_BATCHED_TOO_LONG", t.batched_too_long.to_string());
    super::push(out, "SP_BATCHED_TOO_LONG_NAMES", t.batched_too_long_names.join(" "));
    push(out, "SP_AWAITING_ROUND", t.awaiting_round.to_string());
    push(out, "SP_CLOSED_STRANDED", t.closed_stranded.to_string());
    if t.closed_stranded > 0 {
        if let Some(o) = t.closed_stranded_oldest {
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
        push(out, "SP_UNSENT", t.n.to_string());
        if t.n > 0 {
            if let Some(o) = t.oldest {
                push(out, "SP_UNSENT_OLDEST_H", ((now - o) / 3600).to_string());
                return;
            }
        }
        push(out, "SP_UNSENT_OLDEST_H", "0");
    }
}


struct Ctx<'a> {
    now: i64,
    batch_wait: i64,
    grace: i64,
    land: &'a str,
    in_delivery: &'a HashMap<String, super::lc::LcRow>,
    round_members: &'a Option<HashSet<String>>,
    forge_batch: Option<&'a Option<ForgeBatch>>,
}

/// The branch backlog's counters across every repository.
#[derive(Default)]
struct Tally {
    n: usize,
    oldest: Option<i64>,
    done: usize,
    unadopted: usize,
    unadopted_names: Vec<String>,
    orphan_work: usize,
    round_branches: usize,
    round_branches_names: Vec<String>,
    probe_fail: usize,
    probe_fail_names: Vec<String>,
    batched_stranded: usize,
    batched_stranded_names: Vec<String>,
    batched_stranded_ages: Vec<String>,
    batched_too_long: usize,
    batched_too_long_names: Vec<String>,
    closed_stranded: usize,
    closed_stranded_oldest: Option<i64>,
    awaiting_round: usize,
}

impl Tally {
    /// Classifies one repo's `<branch> <committer-epoch>` lines. `status_of` is the bead's
    /// state by id; `on_base` is whether a branch's tip is already on the land ref.
    fn tally(&mut self, brs: &str, ctx: &Ctx, status_of: &dyn Fn(&str) -> BeadStatus, on_base: &dyn Fn(&str) -> bool) {
        let Ctx { now, batch_wait, grace, land, in_delivery, round_members, forge_batch } = ctx;
        let (now, batch_wait, grace) = (*now, *batch_wait, *grace);
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

            let status = status_of(id);
            match status {
                BeadStatus::ProbeFailed => {
                    self.probe_fail += 1;
                    self.probe_fail_names.push(id.to_string());
                    continue;
                }
                BeadStatus::NoBead => {
                    if b.starts_with("spira/round-") {
                        self.round_branches += 1;
                        self.round_branches_names.push(id.to_string());
                        continue;
                    }
                    let is_ancestor = on_base(b);
                    if is_ancestor {
                        self.unadopted += 1;
                        self.unadopted_names.push(id.to_string());
                    } else {
                        self.orphan_work += 1;
                    }
                    continue;
                }
                BeadStatus::Closed { submitted } => {
                    self.done += 1;
                    match closed_branch_class(land, submitted, b.starts_with("spira/queue/"), in_delivery.contains_key(id)) {
                        ClosedClass::AwaitingRound => self.awaiting_round += 1,
                        ClosedClass::Delivering | ClosedClass::BatchBranch => {}
                        ClosedClass::Stranded => {
                            self.closed_stranded += 1;
                            if let Some(ts) = ts {
                                if self.closed_stranded_oldest.map(|o| ts < o).unwrap_or(true) {
                                    self.closed_stranded_oldest = Some(ts);
                                }
                            }
                        }
                    }
                }
                BeadStatus::Open => {}
            }
            self.n += 1;
            if !b.starts_with("spira/queue/") && !matches!(status, BeadStatus::Closed { .. }) {
                if let Some(ts) = ts {
                    if self.oldest.map(|o| ts < o).unwrap_or(true) {
                        self.oldest = Some(ts);
                    }
                }
            }
            if batched_check_applies(&status, in_delivery.contains_key(id)) {
                if let Some(row) = in_delivery.get(id) {
                    let stranded_for = stranded_for(row.updated_at, now, grace);
                    match (forge_batch, round_members) {
                        (Some(None), _) | (_, None) => {
                            self.probe_fail += 1;
                            self.probe_fail_names.push(id.to_string());
                        }
                        (told, Some(round_members)) => {
                            let in_batch = told.is_some_and(|b| b.as_ref().is_some_and(|b| b.members.contains(id))) || round_members.contains(id);
                            if let (false, Some(secs)) = (in_batch, stranded_for) {
                                self.batched_stranded += 1;
                                self.batched_stranded_names.push(id.to_string());
                                self.batched_stranded_ages.push(format!("{id}={secs}"));
                            }
                        }
                    }
                    if let Some(ep) = row.updated_at {
                        if ep > 0 && (now - ep) > batch_wait {
                            self.batched_too_long += 1;
                            self.batched_too_long_names.push(id.to_string());
                        }
                    }
                }
            }
        }
    }
}

/// Seconds an IN_DELIVERY bead outside every open batch has been so, once past `grace`; a
/// bead with no state time cannot be told stranded.
fn stranded_for(state_at: Option<i64>, now: i64, grace: i64) -> Option<i64> {
    state_at.map(|ep| now - ep).filter(|age| *age >= grace)
}

/// A forge repo's open batch, as `queue batch show` gives it: its members and its branch.
#[derive(Debug, Default, PartialEq, Eq)]
struct ForgeBatch {
    members: std::collections::BTreeSet<String>,
    branch: Option<String>,
}

/// Each forge repo's open batch (`show`); `None` for a repo the queue could not answer for,
/// whose beads' batch membership cannot be told.
fn forge_batches(repos: &[String], show: &dyn Fn(&str) -> Option<String>) -> std::collections::BTreeMap<String, Option<ForgeBatch>> {
    repos
        .iter()
        .map(|repo| {
            let batch = show(repo).map(|text| {
                let mut b = ForgeBatch::default();
                for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
                    match k {
                        "members" => b.members.extend(v.split_whitespace().map(|m| m.split(':').next().unwrap_or(m).to_string())),
                        "branch" => b.branch = Some(v.to_string()),
                        _ => {}
                    }
                }
                b
            });
            (repo.clone(), batch)
        })
        .collect()
}

/// Every member in flight for a repo: its open rounds and the staged one, from the store's
/// round view. A store that cannot answer is `None`: a delivering bead is then a probe fault, never stranded.
fn round_status_members(repo: &str) -> Option<HashSet<String>> {
    crate::store::round_members(repo)
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
fn branch_ids(reg: &spira_config::repos::Registry) -> Vec<String> {
    let mut ids = Vec::new();
    for rname in reg.all() {
        let Some(rp) = reg.root(&rname) else { continue };
        let Some(brs) = io::git_held(Path::new(&rp), &["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/*"]) else { continue };
        ids.extend(brs.lines().filter(|b| !b.starts_with("spira/queue/")).map(|b| b.trim_start_matches("spira/").to_string()));
    }
    ids
}

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

/// `BD_TIMEOUT=2 contentjson show <id>` — deliberately short: this runs once per rowless branch
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
    let raw = io::contentjson(&["show", id]);
    match prev {
        Some(p) => std::env::set_var("BD_TIMEOUT", p),
        None => std::env::remove_var("BD_TIMEOUT"),
    }
    match io::json_rows(raw) {
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
// it (a body mention does not count) — `law-dwarf-commits-name-their-bead`.
// ---------------------------------------------------------------------------------------

fn landing_funnel_section(out: &mut Kv, run: &Path, cfg: &Cfg) {
    let scope = &cfg.scope_label;
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let home_repo = io::repo_registry().home_repo().to_string();
    // Every plan bead's content; which of them the builder finished is the lifecycle row's
    // to say (design §3.4, sp-mve9i), not bd's `closed`.
    let raw = io::history_beads(&label, io::DAY_WINDOW_SECS);
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        for k in ["SP_CLOSED", "SP_LANDED", "SP_UNLANDED_N", "SP_STRANDED_N", "SP_CERT_N", "SP_FUNNEL_DONE_AGE"] {
            push(out, k, "?");
        }
        return;
    };
    let Some(rows) = io::json_rows(Some(raw)) else {
        for k in ["SP_CLOSED", "SP_LANDED", "SP_UNLANDED_N", "SP_STRANDED_N", "SP_CERT_N", "SP_FUNNEL_DONE_AGE"] {
            push(out, k, "?");
        }
        return;
    };
    let logged: Vec<String> = rows
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .filter(|id| run.join(format!("{id}.log")).exists())
        .map(str::to_string)
        .collect();
    let Some(lc_index) = super::lc::state_index_of(&logged) else {
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
    let mut cherry_refs: std::collections::HashMap<String, (std::path::PathBuf, String)> = std::collections::HashMap::new();
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
        cherry_refs.insert(r.to_string(), (rp_path.to_path_buf(), local.clone().unwrap_or_else(|| base.clone())));
        let refs = match local {
            Some(l) => format!("{base} {l}"),
            None => base.clone(),
        };
        if let Some(remote) = spira_config::repos::ref_remote(&base, Some(&rp)).filter(|_| has_spira_branch(rp_path)) {
            let _ = io::git(rp_path, &["fetch", "-q", &remote]);
        }
        let mut log_args = vec!["log", "--format=%s", "-n", "2000"];
        let ref_list: Vec<&str> = refs.split_whitespace().collect();
        log_args.extend(ref_list.iter().copied());
        if let Some(log) = io::git_held(rp_path, &log_args) {
            subjects.push(log);
        }
        if let Some(brs) = io::git_held(rp_path, &["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/", "refs/remotes/*/spira/"]) {
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

    let ids: Vec<(&str, &str, &str)> = closed_pairs.iter().map(|r| (r.id.as_str(), r.repo.as_str(), r.closed_at.as_str())).collect();
    let f = classify_landing(
        &ids,
        &subject_lines,
        &branch_lines,
        &|id| lc_index.get(id),
        now,
        cfg.cert_window_mins,
        &|repo, br| cherry_refs.get(repo).is_some_and(|(rp, land_ref)| branch_has_no_own_commit(rp, land_ref, br)),
        &|id| gate_queued(run, id),
    );
    let (landed_set, anomaly, stranded, awaiting, done_oldest) = (f.landed, f.anomaly, f.stranded, f.awaiting, f.done_oldest);
    push(out, "SP_CLOSED", closed_pairs.len().to_string());
    push(out, "SP_LANDED", landed_set.to_string());
    push(out, "SP_UNLANDED_N", anomaly.to_string());
    push(out, "SP_STRANDED_N", stranded.to_string());
    push(out, "SP_CERT_N", awaiting.to_string());
    push(out, "SP_FUNNEL_DONE_AGE", done_oldest.map(|d| rel_age(now - d)).unwrap_or_default());
}

/// The funnel's counts over the closed beads `(id, repo, closed_at)` of the last 24 h.
#[derive(Debug, Default, PartialEq, Eq)]
struct Landing {
    landed: usize,
    anomaly: usize,
    stranded: usize,
    awaiting: usize,
    done_oldest: Option<i64>,
}

/// A commit subject names a bead only in the forms the landing pass writes; a body or a
/// longer id that merely starts with it does not.
fn subject_names(subject: &str, id: &str) -> bool {
    let s = subject.trim();
    s == format!("spira: land {id}") || s.starts_with(&format!("spira: land {id} ")) || s.starts_with(&format!("{id}: "))
}

/// LANDED: a subject on the base names the bead. Unlanded: not landed, a `spira/` branch
/// for it exists, and the machine holds it SUBMITTED with a commit of its own; stranded when
/// no gate job is queued for it and it is older than the certification window.
#[allow(clippy::too_many_arguments)]
fn classify_landing<'a>(
    closed: &[(&str, &str, &str)],
    subjects: &[&str],
    branches: &[&str],
    row_of: &dyn Fn(&str) -> Option<&'a super::lc::Row>,
    now: i64,
    cert_win_mins: i64,
    no_own_commit: &dyn Fn(&str, &str) -> bool,
    queued: &dyn Fn(&str) -> bool,
) -> Landing {
    let mut f = Landing::default();
    for (id, repo, closed_at) in closed {
        if subjects.iter().any(|s| subject_names(s, id)) {
            f.landed += 1;
            continue;
        }
        let Some(br) = branches.iter().find(|b| b.trim_end().ends_with(&format!("/{id}"))) else { continue };
        if !awaits_certification(row_of(id)) || no_own_commit(repo, br.trim()) {
            continue;
        }
        f.anomaly += 1;
        let ts = parse_iso8601(closed_at);
        if let Some(ts) = ts {
            if f.done_oldest.map(|d| ts < d).unwrap_or(true) {
                f.done_oldest = Some(ts);
            }
        }
        let age_mins = ts.map(|t| (now - t) / 60).unwrap_or(9_999_999);
        if !queued(id) && age_mins > cert_win_mins {
            f.stranded += 1;
        } else {
            f.awaiting += 1;
        }
    }
    f
}

/// `git cherry` prints `+` for each commit not on the land ref; none means the branch never
/// made a commit of its own. A failed cherry is not evidence of anything.
fn branch_has_no_own_commit(repo: &Path, land_ref: &str, branch: &str) -> bool {
    io::git_held(repo, &["cherry", land_ref, branch]).map(|o| !o.lines().any(|l| l.starts_with('+'))).unwrap_or(false)
}

/// Whether the gate-worker holds a job for this bead's branch, in the inbox or claimed by any slot.
fn gate_queued(run: &Path, id: &str) -> bool {
    let root = run.join("gate-worker");
    let want = format!("spira/{id}");
    let mut dirs = vec![root.join("inbox"), root.join("claimed")];
    if let Ok(rd) = std::fs::read_dir(root.join("claimed")) {
        dirs.extend(rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()));
    }
    dirs.iter().any(|d| {
        std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| {
            std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .map(|j| j.get("branch").and_then(Value::as_str) == Some(want.as_str()) || j.get("bead").and_then(Value::as_str) == Some(id))
                .unwrap_or(false)
        })
    })
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
        let acc_repo = prod.trim_end_matches("/spira").to_string(); // not a path: suffix of a repo path
        let acc_repo_path = Path::new(&acc_repo);
        if acc_repo_path.join(".git").exists() {
            let noted = io::git_held(acc_repo_path, &["notes", "--ref=acceptance", "list"])
                .map(|s| s.lines().filter_map(|l| l.split_whitespace().nth(1).map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut acc_obj: Option<String> = None;
            if !noted.is_empty() {
                let mut tags = release_tags(acc_repo_path);
                tags.reverse();
                'outer: for t in &tags {
                    for rev in [t.clone(), format!("{t}^{{commit}}")] {
                        if let Some(c) = io::git_held(acc_repo_path, &["rev-parse", "-q", "--verify", &rev]) {
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
                tag = io::git_held(acc_repo_path, &["tag", "--points-at", &obj])
                    .and_then(|s| s.lines().find(|l| spira_config::release_tag::is_release_tag(l)).map(String::from))
                    .unwrap_or_else(|| "-".to_string());
                let note = io::git_held(acc_repo_path, &["notes", "--ref=acceptance", "show", &obj]).unwrap_or_default();
                verdict = if note.starts_with("PASS") {
                    "PASS".to_string()
                } else if note.starts_with("FAIL") {
                    "FAIL".to_string()
                } else if note.is_empty() {
                    "?".to_string()
                } else {
                    note.lines().next().unwrap_or("").chars().take(8).collect()
                };
                at = io::git_held(acc_repo_path, &["log", "-1", "--format=%ct", &obj]).map(|s| s.trim().to_string()).unwrap_or_else(|| "?".to_string());
                if tag != "-" {
                    let tags2 = release_tags(acc_repo_path);
                    let idx = tags2.iter().position(|t| t == &tag);
                    since = idx.map(|i| (tags2.len() - i - 1).to_string()).unwrap_or_else(|| "?".to_string());
                }
            } else if io::git_held(acc_repo_path, &["rev-parse", "--git-dir"]).is_some() {
                verdict = "NEVER".to_string();
                since = release_tags(acc_repo_path).len().to_string();
            }
        }
    }
    push(out, "SP_ACCEPT_VERDICT", verdict);
    push(out, "SP_ACCEPT_TAG", tag);
    push(out, "SP_ACCEPT_AT", at);
    push(out, "SP_ACCEPT_SINCE", since);
}

/// Whether the checkout has a `spira/*` branch, local or remote-tracking; unknown reads as yes.
fn has_spira_branch(repo: &Path) -> bool {
    use spira_config::git_refs::{has_branch_under, remotes};
    if has_branch_under(repo, "refs/heads/", "spira/") != Some(false) {
        return true;
    }
    remotes(repo).is_none_or(|names| names.iter().any(|r| has_branch_under(repo, &format!("refs/remotes/{r}/"), "spira/") != Some(false)))
}

fn release_tags(repo: &Path) -> Vec<String> {
    let listed = io::git_held(repo, &["tag", "-l", "spira-*"]).unwrap_or_default();
    spira_config::release_tag::sort_release_tags(listed.lines().map(String::from))
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
    #[test_id(tk03ad1)]
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
    #[test_id(tuyuv35)]
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
    #[test_id(tjq72mf)]
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
    #[test_id(t4wm4py)]
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

    #[test_id(ttud413)]
    #[test]
    fn a_bead_between_two_rounds_is_not_stranded_until_the_grace_has_passed() {
        let now = 1_000_000;
        assert_eq!(stranded_for(Some(now - 40), now, 300), None, "left a batch 40s ago");
        assert_eq!(stranded_for(Some(now - 360), now, 300), Some(360), "six minutes outside every batch");
        assert_eq!(stranded_for(None, now, 300), None, "no state time: cannot tell");
    }

    #[test_id(t68n838)]
    #[test]
    fn a_stranded_bead_is_reported_with_its_age() {
        let no_batch: Option<ForgeBatch> = Some(ForgeBatch::default());
        let t = tally_of("spira/sp-bat 90000\n", &[("sp-bat", 100_000 - 360)], Some(&no_batch), 1800);
        assert_eq!(t.batched_stranded_ages, ["sp-bat=360"]);
        let t = tally_of("spira/sp-bat 90000\n", &[("sp-bat", 100_000 - 40)], Some(&no_batch), 1800);
        assert_eq!(t.batched_stranded, 0, "between two rounds for 40s");
    }

    #[test_id(ti5c4v7)]
    #[test]
    fn a_forge_batch_on_the_store_names_its_members_and_branch() {
        let show = |repo: &str| match repo {
            "r" => Some("batch_id=r-1\nforge.phase=armed\npr=1\nbranch=spira/queue/r-1\nmembers=sp-in:t\n".to_string()),
            "s" => Some(String::new()),
            _ => None,
        };
        let f = forge_batches(&["r".into(), "s".into(), "lost".into()], &show);
        let r = f["r"].as_ref().expect("r answered");
        assert!(r.members.contains("sp-in") && !r.members.contains("sp-out"));
        assert_eq!(r.branch.as_deref(), Some("spira/queue/r-1"));
        assert_eq!(f["s"], Some(ForgeBatch::default()), "no record: no batch");
        assert_eq!(f["lost"], None, "cannot tell is not no batch");
    }

    #[test_id(t6yari0)]
    #[test]
    fn a_submitted_bead_awaits_a_round_only_under_queue_local() {
        assert!(awaits_round("queue.local", true));
        assert!(!awaits_round("queue", true));
        assert!(!awaits_round("push", true));
        assert!(!awaits_round("queue.local", false));
    }

    const TEST_GRACE: i64 = 137;

    fn tally_of(lines: &str, delivering: &[(&str, i64)], batch: Option<&Option<ForgeBatch>>, wait: i64) -> Tally {
        tally_with_rounds(lines, delivering, batch, wait, &Some(HashSet::new()))
    }

    fn tally_with_rounds(lines: &str, delivering: &[(&str, i64)], batch: Option<&Option<ForgeBatch>>, wait: i64, rounds: &Option<HashSet<String>>) -> Tally {
        let in_delivery: HashMap<String, super::super::lc::LcRow> = delivering
            .iter()
            .map(|(id, at)| (id.to_string(), super::super::lc::LcRow { id: id.to_string(), state: "IN_DELIVERY".into(), reason: String::new(), updated_at: Some(*at) }))
            .collect();
        let ctx = Ctx { now: 100_000, batch_wait: wait, grace: TEST_GRACE, land: "push", in_delivery: &in_delivery, round_members: rounds, forge_batch: batch };
        let status = |id: &str| match id {
            "sp-open" | "sp-bat" => BeadStatus::Open,
            "sp-done" => BeadStatus::Closed { submitted: false },
            "sp-lookup" => BeadStatus::ProbeFailed,
            _ => BeadStatus::NoBead,
        };
        let mut t = Tally::default();
        t.tally(lines, &ctx, &status, &|b| b.ends_with("true-stray"));
        t
    }

    // covers: UC-cockpit-observability-12
    #[test_id(tdgh5as)]
    #[test]
    fn only_bead_backed_branches_count_and_a_no_bead_branch_is_orphan_work_or_unadopted() {
        let t = tally_of(
            "spira/sp-open 90000\nspira/sp-done 90000\nspira/tmp-stray 90000\nspira/sp-true-stray 90000\nspira/round-81 90000\nspira/queue/20260101 90000\nspira/sp-lookup 90000\n",
            &[],
            None,
            1,
        );
        assert_eq!((t.n, t.done), (2, 1), "bead-backed branches only; no-bead, round, queue and failed lookups are not unsent");
        assert_eq!((t.orphan_work, t.unadopted), (1, 1));
        assert_eq!(t.unadopted_names, ["sp-true-stray"]);
        assert_eq!(t.round_branches_names, ["round-81"]);
        assert_eq!((t.probe_fail, t.probe_fail_names.as_slice()), (1, ["sp-lookup".to_string()].as_slice()), "a failed lookup is a probe fault, never unadopted");
    }

    // covers: UC-cockpit-observability-12
    #[test_id(tp9hjy4)]
    #[test]
    fn a_batched_bead_outside_every_batch_is_stranded_and_one_waiting_past_the_wait_is_too_long() {
        let line = "spira/sp-bat 90000\n";
        let no_batch: Option<ForgeBatch> = Some(ForgeBatch::default());
        let stale = 100_000 - TEST_GRACE - 1;
        let t = tally_of(line, &[("sp-bat", stale)], Some(&no_batch), 60);
        assert_eq!((t.batched_stranded, t.batched_stranded_names.as_slice()), (1, ["sp-bat".to_string()].as_slice()));
        assert_eq!(t.batched_too_long, 1);
        let t = tally_of(line, &[("sp-bat", 100_000 - 5)], Some(&no_batch), 60);
        assert_eq!((t.batched_stranded, t.batched_too_long), (0, 0), "inside the grace and the wait");
        let member = Some(ForgeBatch { members: ["sp-bat".to_string()].into(), ..ForgeBatch::default() });
        assert_eq!(tally_of(line, &[("sp-bat", stale)], Some(&member), 60).batched_stranded, 0, "a batch member is not stranded");
        assert_eq!(tally_of(line, &[("sp-bat", stale)], Some(&None), 60).probe_fail, 1, "an unreadable batch is a fault, not a stranding");
    }

    // covers: UC-cockpit-observability-12
    #[test_id(tpjvc39)]
    #[test]
    fn a_delivering_bead_whose_round_view_is_unreadable_is_a_probe_fault_not_stranded() {
        let line = "spira/sp-bat 90000\n";
        let stale = 100_000 - TEST_GRACE - 1;
        let t = tally_with_rounds(line, &[("sp-bat", stale)], None, 60, &None);
        assert_eq!((t.batched_stranded, t.probe_fail, t.probe_fail_names.as_slice()), (0, 1, ["sp-bat".to_string()].as_slice()));
        let named = Some(HashSet::from(["sp-bat".to_string()]));
        let t = tally_with_rounds(line, &[("sp-bat", stale)], None, 60, &named);
        assert_eq!((t.batched_stranded, t.probe_fail), (0, 0), "a round naming the bead keeps it from stranding");
        let t = tally_with_rounds(line, &[("sp-bat", stale)], None, 60, &Some(HashSet::new()));
        assert_eq!(t.batched_stranded, 1, "a readable view that does not name it is a stranding");
    }

    // covers: UC-cockpit-observability-10
    #[test_id(t5oyoqd)]
    #[test]
    fn a_closed_bead_is_landed_only_when_a_commit_subject_names_it() {
        let submitted = row("SUBMITTED");
        let now = 100_000;
        let recent = "1970-01-02T03:00:00Z"; // inside the certification window of `now`
        let closed = [("sp-a", "r", recent), ("sp-b", "r", recent), ("sp-c", "r", recent), ("sp-d", "r", recent)];
        let subjects = ["spira: land sp-a", "tidy up, see also sp-b and sp-c", "spira: land sp-dd"];
        let branches = ["origin/spira/sp-b", "origin/spira/sp-d"];
        let f = classify_landing(&closed, &subjects, &branches, &|_| Some(&submitted), now, 90, &|_, _| false, &|_| false);
        assert_eq!(f.landed, 1, "sp-a only: a body mention and a longer id are not landings");
        assert_eq!(f.anomaly, 2, "sp-b and sp-d: closed, branch, no landing; sp-c has neither and is not an anomaly");
    }

    // covers: UC-cockpit-observability-10
    #[test_id(tvvp9ca)]
    #[test]
    fn an_unlanded_branch_is_stranded_only_past_the_certification_window_and_with_no_gate_job() {
        let submitted = row("SUBMITTED");
        let now = parse_iso8601("2026-01-01T12:00:00Z").unwrap();
        let closed = [("sp-old", "r", "2026-01-01T08:00:00Z"), ("sp-queued", "r", "2026-01-01T08:00:00Z"), ("sp-new", "r", "2026-01-01T11:50:00Z"), ("sp-empty", "r", "2026-01-01T08:00:00Z")];
        let branches = ["origin/spira/sp-old", "origin/spira/sp-queued", "origin/spira/sp-new", "origin/spira/sp-empty"];
        let f = classify_landing(&closed, &[], &branches, &|_| Some(&submitted), now, 90, &|_, br| br.ends_with("sp-empty"), &|id| id == "sp-queued");
        assert_eq!((f.anomaly, f.stranded, f.awaiting), (3, 1, 2), "a branch with no commit of its own is no anomaly");
        assert_eq!(f.done_oldest, parse_iso8601("2026-01-01T08:00:00Z"));
        let certified = row("CERTIFIED");
        let f = classify_landing(&closed, &[], &branches, &|_| Some(&certified), now, 90, &|_, _| false, &|_| false);
        assert_eq!(f.anomaly, 0, "only a SUBMITTED bead awaits certification");
    }

    // covers: UC-cockpit-observability-15
    #[test_id(tjyltb1)]
    #[test]
    fn landing_status_and_progress_are_copied_verbatim_and_a_missing_status_is_unknown() {
        let d = testkit::TempDir::new("cc-landing");
        let get = |kv: &Kv, k: &str| kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        std::fs::write(d.path().join("landing.status"), "SP_LAND_AT=1788845658\nSP_LAND_RC=3\nSP_LAND_BRANCHES=4\nSP_LAND_MOVED=1\n").unwrap();
        std::fs::write(d.path().join("landing.progress"), "landed spira/sp-35pl\nreopened sp-dvlq -- does not rebase\n").unwrap();
        let mut kv = Kv::new();
        gate_section(&mut kv, d.path());
        assert_eq!((get(&kv, "SP_LAND_AT").as_deref(), get(&kv, "SP_LAND_RC").as_deref(), get(&kv, "SP_LAND_BRANCHES").as_deref()), (Some("1788845658"), Some("3"), Some("4")));
        assert_eq!((get(&kv, "SP_LANDPROG_N").as_deref(), get(&kv, "SP_LANDPROG0").as_deref()), (Some("2"), Some("landed spira/sp-35pl")));
        std::fs::remove_file(d.path().join("landing.status")).unwrap();
        let mut kv = Kv::new();
        gate_section(&mut kv, d.path());
        for k in ["SP_LAND_AT", "SP_LAND_RC", "SP_LAND_BRANCHES"] {
            assert_eq!(get(&kv, k).as_deref(), Some("?"), "{k}");
        }
    }

    #[test_id(t53solv)]
    #[test]
    fn gate_queue_matches_inbox_and_claimed_slots_by_branch() {
        let d = testkit::TempDir::new("cc-unsent-queue");
        let gw = d.path().join("gate-worker");
        std::fs::create_dir_all(gw.join("inbox")).unwrap();
        std::fs::create_dir_all(gw.join("claimed/2")).unwrap();
        std::fs::write(gw.join("inbox/a.json"), r#"{"branch":"spira/sp-in","bead":"sp-in"}"#).unwrap();
        std::fs::write(gw.join("claimed/2/b.json"), r#"{"branch":"spira/sp-cl","bead":"sp-cl"}"#).unwrap();
        assert!(gate_queued(d.path(), "sp-in"));
        assert!(gate_queued(d.path(), "sp-cl"));
        assert!(!gate_queued(d.path(), "sp-old"));
    }

    fn yield_keys(stub: &str) -> Kv {
        let dir = testkit::TempDir::new("yield-section");
        testkit::write_exe(dir.path().join("yield.sh"), stub);
        let path = format!("{}:{}", dir.path().display(), std::env::var("PATH").unwrap_or_default());
        let _env = testkit::env(&[("PATH", Some(&path))]);
        let mut out = Kv::new();
        yield_section(&mut out);
        out
    }

    // covers: UC-gate-verdict-24
    #[test_id(tlqhevu)]
    #[test]
    fn every_yield_key_is_written_from_the_report_and_a_report_that_cannot_be_read_is_a_question_mark() {
        let keys = ["SP_YIELD_REDS", "SP_YIELD_DEFECT", "SP_YIELD_FAULT", "SP_YIELD_UNKNOWN", "SP_YIELD_SOLO_MED", "SP_YIELD_CONC_MED", "SP_YIELD_TOP_FAULT"];
        let read = yield_keys("#!/bin/sh\nprintf 'YIELD_REDS=4\\nYIELD_DEFECT=1\\nYIELD_FAULT=2\\nYIELD_UNKNOWN=1\\nYIELD_SOLO_MED=60\\nYIELD_CONC_MED=95\\nYIELD_TOP_FAULT=test-b.sh x2\\n'\n");
        let got: Vec<(&str, &str)> = read.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(got, keys.iter().copied().zip(["4", "1", "2", "1", "60", "95", "test-b.sh x2"]).collect::<Vec<_>>());
        for broken in ["#!/bin/sh\nexit 3\n", "#!/bin/sh\nprintf 'YIELD_REDS=7\\n'\nexit 3\n"] {
            let out = yield_keys(broken);
            assert_eq!(out.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), keys, "every key is still written");
            assert!(out.iter().all(|(_, v)| v == "?"), "a failed report is ? everywhere, never 0: {out:?}");
        }
    }
}
