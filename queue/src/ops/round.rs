//! `queue round`: the round lifecycle the batcher and the Concierge share, under queue.local.
//! A round is one record (`<queue_dir>/<repo>/round`), one worktree and one spira-lc batch;
//! the verbs differ from a hand's scratch scripts in that spira-lc, loom and round-duty can
//! all see the round (DESIGN.md §2.2).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::batch::lc_return;
use super::land::land_local_with;
use super::{actor, czar_ok, idents, landing_log, lc_cas, lock_held_by_caller, read_text, repo_path, require_lc, resolve, take_lock, Ctx, World, FAIL, OK, USAGE};
use crate::cli::{Round, Text};
use landing_pass::gateq::GateQueue;
use landing_pass::model::GateOutcome;
use crate::ident::bounded_text;
use crate::lock::Guard;
use crate::model::{parse_members, render_members, EjectCause, LandMode, Member};
use crate::records::{self, one_line, write_atomic, Kv};

/// certify's exit when the corpus could not be judged (the VM, the wall, the install), as
/// opposed to FAIL, which is a red round.
pub const FAULT: i32 = 4;

const RECORD: &str = "round";
const BUILD_RED: &str = "workspace-build";
const LC_ACTOR: &str = "queue.sh";
const PASSING: [&str; 2] = ["ok", "skip"];
pub const BLOCKING: [&str; 5] = ["red", "timeout", "unreached", "deferred", "fault"];

pub fn run(w: &World, r: &Round) -> i32 {
    match r {
        Round::Open { repo, members, name, worktree } => open(w, repo.as_deref(), members, name.as_deref(), worktree.as_deref()),
        Round::Certify { batch, repo, attest } => certify(w, batch, repo.as_deref(), attest.as_deref()),
        Round::Eject { batch, id, repo, reason, suites, red, harness_fault, rebuild } => eject(w, batch, id, repo.as_deref(), reason, suites, *red, *harness_fault, *rebuild),
        Round::Land { batch, repo } => land(w, batch, repo.as_deref()),
        Round::Abandon { batch, repo, reason } => abandon(w, batch, repo.as_deref(), reason),
        Round::Status { repo } => status(w, repo.as_deref()),
        Round::PassStart { batch, repo } => pass_start(w, batch, repo.as_deref()),
        Round::SuitesStarted { batch, repo } => suites_started_verb(w, batch, repo.as_deref()),
        Round::PassVerdict { batch, repo, verdict, red_suites, suites_s, build_s, reason } => {
            pass_verdict(w, batch, repo.as_deref(), verdict, red_suites, Timings { suites_s: *suites_s, build_s: *build_s }, reason)
        }
    }
}

#[derive(Clone, Copy)]
struct Timings {
    suites_s: u64,
    build_s: u64,
}

enum Verdict<'a> {
    Green(Timings),
    Red(&'a [String], Timings),
    Incomplete(&'a str),
}

/// One batch event chosen from the round's current (state, pass, phase); `None` sends nothing.
fn pass_event<F>(w: &World, batch: &str, kind: F) -> Result<(), (i32, String)>
where
    F: FnOnce(&str, u32, &str) -> Option<String>,
{
    lc_cas(w, batch, |s, v| {
        let (n, phase) = w.lc.batch_pass(batch).ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        match kind(s, n, &phase) {
            Some(k) => w.lc.batch_event(batch, s, v, LC_ACTOR, &k),
            None => Ok(()),
        }
    })
}

/// `strict` is a hand verb: it always sends the event, so the machine refuses a wrong-phase call
/// and names the phase. certify is not strict: it resumes a pass already recorded.
fn start_pass(w: &World, batch: &str, head: &str, strict: bool) -> Result<(), (i32, String)> {
    let rebuilt = serde_json::json!({"PassRebuilt": {"head": head}}).to_string();
    pass_event(w, batch, |s, _, _| (s == "ATTRIBUTING").then_some(rebuilt))?;
    pass_event(w, batch, |s, n, _| {
        (strict || matches!(s, "OPEN" | "REBUILDING")).then(|| serde_json::json!({"PassStarted": {"n": n + 1, "head": head}}).to_string())
    })
}

fn suites_started(w: &World, batch: &str, strict: bool) -> Result<(), (i32, String)> {
    pass_event(w, batch, |_, n, phase| (strict || phase == "build").then(|| serde_json::json!({"SuitesStarted": {"n": n}}).to_string()))
}

fn pass_verdict_event(w: &World, batch: &str, verdict: &Verdict, strict: bool) -> Result<(), (i32, String)> {
    lc_cas(w, batch, |s, v| {
        if !strict && s != "CI_RUNNING" {
            return match (verdict, s) {
                (Verdict::Green(_), "GREEN") | (Verdict::Red(..) | Verdict::Incomplete(_), _) => Ok(()),
                (Verdict::Green(_), other) => Err((1, format!("the batch is {other}, not CI_RUNNING"))),
            };
        }
        let (n, _) = w.lc.batch_pass(batch).ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        let kind = match verdict {
            Verdict::Green(t) => serde_json::json!({"PassGreen": {"n": n, "suites_s": t.suites_s, "build_s": t.build_s}}),
            Verdict::Red(red, t) => serde_json::json!({"PassRed": {"n": n, "red_suites": red, "suites_s": t.suites_s, "build_s": t.build_s}}),
            Verdict::Incomplete(why) => serde_json::json!({"PassIncomplete": {"n": n, "reason": why}}),
        };
        w.lc.batch_event(batch, s, v, LC_ACTOR, &kind.to_string())
    })
}

fn pass_verb<F>(w: &World, label: &str, batch: &str, repo: Option<&str>, f: F) -> i32
where
    F: FnOnce(&Ctx, &Kv) -> Result<(), (i32, String)>,
{
    let Ok((c, _)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(kv) = load(w, label, &c, batch) else { return FAIL };
    match f(&c, &kv) {
        Ok(()) => {
            w.out(format!("queue.sh {label}: recorded for {batch}"));
            OK
        }
        Err((rc, out)) => {
            w.err(format!("queue.sh {label}: spira-lc refused for {batch} (rc={rc}): {out}"));
            FAIL
        }
    }
}

fn pass_start(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    pass_verb(w, "round pass-start", batch, repo, |_, kv| start_pass(w, batch, kv.get("head").unwrap_or(""), true))
}

fn suites_started_verb(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    pass_verb(w, "round suites-started", batch, repo, |_, _| suites_started(w, batch, true))
}

fn pass_verdict(w: &World, batch: &str, repo: Option<&str>, verdict: &str, red_suites: &str, t: Timings, reason: &Text) -> i32 {
    let why = read_text(w, reason).unwrap_or_default();
    let red: Vec<String> = red_suites.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    if verdict == "incomplete" && why.trim().is_empty() {
        w.err("queue.sh round pass-verdict: --reason is required for an incomplete pass".to_string());
        return USAGE;
    }
    if verdict == "red" && red.is_empty() {
        w.err("queue.sh round pass-verdict: --red-suites is required for a red pass".to_string());
        return USAGE;
    }
    if red.iter().try_for_each(|s| idents(w, "round pass-verdict", &[("suite", s)])).is_err() {
        return FAIL;
    }
    let bounded = bounded_text(&why);
    let v = match verdict {
        "green" => Verdict::Green(t),
        "red" => Verdict::Red(&red, t),
        _ => Verdict::Incomplete(&bounded),
    };
    pass_verb(w, "round pass-verdict", batch, repo, |_, _| pass_verdict_event(w, batch, &v, true))
}

fn warn_unrecorded(w: &World, label: &str, batch: &str, what: &str, r: Result<(), (i32, String)>) {
    if let Err((rc, out)) = r {
        w.err(format!("queue.sh {label}: WARNING — {what} of round {batch} was not recorded on spira-lc (rc={rc}): {out}"));
    }
}

/// The build's seconds as the VM's runner reported them; the rest of the wall is the suites.
fn pass_timings(results: &Path, wall_s: u64) -> Timings {
    let build_s = fs::read_to_string(results.join("runner.meta"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("build_wall_s=").and_then(|v| v.trim().parse::<u64>().ok())))
        .unwrap_or(0);
    Timings { build_s, suites_s: wall_s.saturating_sub(build_s) }
}

fn local_ctx(w: &World, label: &str, repo: Option<&str>) -> Result<(Ctx, PathBuf), i32> {
    if !czar_ok(w) {
        return Err(FAIL);
    }
    let c = resolve(w, label, repo)?;
    let path = repo_path(w, label, &c)?;
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!(
            "queue.sh {label}: repo is not in queue.local mode (mode={}) — a queue.forge round is a batch PR (open-batch, verdict)",
            c.r.mode.as_str()
        ));
        return Err(FAIL);
    }
    Ok((c, path))
}

fn lock(w: &World, label: &str, c: &Ctx) -> Result<Option<Guard>, i32> {
    if lock_held_by_caller(w) {
        Ok(None)
    } else {
        take_lock(w, label, c, "")
    }
}

fn load(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    match records::read_kv(&c.queue_file(RECORD)) {
        Ok(Some(kv)) if kv.get("batch_id") == Some(batch) => Ok(kv),
        Ok(Some(kv)) => {
            w.err(format!("queue.sh {label}: the open round for {} is {}, not {batch}", c.r.name, kv.get("batch_id").unwrap_or("?")));
            Err(FAIL)
        }
        _ => {
            w.err(format!("queue.sh {label}: no open round for {}", c.r.name));
            Err(FAIL)
        }
    }
}

fn save(w: &World, label: &str, c: &Ctx, kv: &Kv) -> Result<(), i32> {
    write_atomic(&c.queue_file(RECORD), &kv.render()).map_err(|e| {
        w.err(format!("queue.sh {label}: cannot write the round record: {e}"));
        FAIL
    })
}

fn set(kv: &mut Kv, k: &str, v: &str) {
    kv.remove(k);
    kv.push(k, v);
}

fn set_phase(w: &World, kv: &mut Kv, phase: &str) {
    set(kv, "phase", phase);
    set(kv, "phase_at", &w.clock.now().to_string());
}

fn rounds_dir(c: &Ctx) -> PathBuf {
    c.s.run.join("rounds")
}

fn mark_running(c: &Ctx, batch: &str) {
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(format!("{batch}.running")), "");
}

fn finish(c: &Ctx, batch: &str, line: &str) {
    use std::io::Write;
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{batch}.result"))) {
        let _ = writeln!(f, "{line}");
    }
    let _ = fs::remove_file(dir.join(format!("{batch}.running")));
}

fn csv(ms: &[Member]) -> String {
    ms.iter().map(Member::render).collect::<Vec<_>>().join(",")
}

fn worktree_of(kv: &Kv) -> PathBuf {
    PathBuf::from(kv.get("worktree").unwrap_or(""))
}

/// The round's tree rebuilt from `base_sha` with `members` merged in order, in `wt`. A
/// member that no longer merges leaves `wt` as it was (at `fallback`) and is the Err.
fn assemble(w: &World, c: &Ctx, path: &Path, wt: &Path, base_sha: &str, members: &[Member], fallback: &str) -> Result<String, String> {
    w.git.worktree_prune(path);
    w.git.worktree_remove(path, wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(path, wt, base_sha) {
        return Err("cannot create the round worktree".into());
    }
    for m in members {
        if !w.git.merge_no_ff(wt, &w.lib.land_subject(&m.id), &m.tip, &c.s.git_name, &c.s.git_email) {
            w.git.merge_abort(wt);
            w.git.worktree_remove(path, wt);
            let _ = w.git.worktree_add_detached(path, wt, fallback);
            return Err(format!("{} no longer merges onto the round", m.id));
        }
    }
    w.git.rev_parse(wt, "HEAD").ok_or_else(|| "the round worktree has no HEAD".to_string())
}

/// A member that no longer merges onto the base goes back to REWORK on its own tip, so a
/// builder claims it to rebase; left as it was, every later round would skip it again. The
/// claim is made only after reading the row back in REWORK.
fn return_to_rework(w: &World, m: &Member) -> Result<(), String> {
    let Some((state, version)) = w.lc.bead_state(&m.id) else { return Err("no lifecycle row — not returned to rework".into()) };
    let kind = format!("{{\"GateRed\":{{\"tip\":\"{}\",\"reason\":\"no-rebase\"}}}}", m.tip);
    if let Err((rc, e)) = w.lc.bead_event(&m.id, &state, version.trim(), LC_ACTOR, &kind) {
        return Err(format!("return to rework refused rc={rc}: {}", one_line(&e)));
    }
    match w.lc.bead_state(&m.id) {
        Some((now, _)) if now == "REWORK" => Ok(()),
        Some((now, _)) => Err(format!("return to rework accepted but the bead is {now}, not REWORK")),
        None => Err("return to rework accepted but the bead's row cannot be read back".into()),
    }
}

/// The gate-worker's recorded FAIL for `spira/<id>` at exactly `tip`, as the gate's own reason.
/// BASE_FAIL and NO_VERDICT are not the branch's fault and a missing verdict says nothing.
fn gate_red_at(c: &Ctx, id: &str, tip: &str) -> Option<String> {
    let done = GateQueue::new(&c.s.run).peek_done(&c.r.name, &format!("spira/{id}"), tip)?;
    (done.run.outcome == GateOutcome::Fail).then(|| done.run.reason_or("unspecified").to_string())
}

fn open(w: &World, repo: Option<&str>, members_arg: &Text, name: Option<&str>, worktree: Option<&Path>) -> i32 {
    let label = "round open";
    let text = match read_text(w, members_arg) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let wanted = parse_members(&text);
    if wanted.is_empty() {
        w.err(format!("queue.sh {label}: --members names no bead"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    let repo_name = c.r.name.clone();
    if let Some(n) = name {
        if idents(w, label, &[("name", n)]).is_err() {
            return FAIL;
        }
    }
    if let Some(wt) = worktree {
        if let Err(e) = crate::ident::check_path("worktree", &wt.display().to_string()) {
            w.err(format!("queue.sh {label}: {e} — refused"));
            return FAIL;
        }
    }
    for m in &wanted {
        if idents(w, label, &[("member id", &m.id)]).is_err() || (!m.tip.is_empty() && idents(w, label, &[("member tip", &m.tip)]).is_err()) {
            return FAIL;
        }
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let record = c.queue_file(RECORD);
    if let Ok(Some(kv)) = records::read_kv(&record) {
        w.err(format!(
            "queue.sh {label}: a round is already open for {repo_name} ({}, phase {}) — land or abandon it first",
            kv.get("batch_id").unwrap_or("?"),
            kv.get("phase").unwrap_or("?")
        ));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh {label}: cannot resolve landing ref for {repo_name}"));
        return FAIL;
    };
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh {label}: cannot resolve {base}"));
        return FAIL;
    };

    let mut skips: Vec<String> = Vec::new();
    let mut admitted: Vec<Member> = Vec::new();
    let mut blocked_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in wanted {
        match w.lc.bead_row(&m.id) {
            None => skips.push(format!("{}: no lifecycle row (spira-lc could not say) — not admitted", m.id)),
            // SUBMITTED or CERTIFIED (law-a-round-is-feature-first-then-catch-all): the round's own
            // full suite is the certification, so a submitted tip need not pass a per-bead gate first.
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => {
                skips.push(format!("{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted", m.id, r.state))
            }
            Some(r) => match r.tip.filter(|t| !t.is_empty()) {
                Some(tip) if tip.starts_with(&m.tip) => match gate_red_at(&c, &m.id, &tip) {
                    Some(why) => skips.push(format!("{}: its gate is FAIL at {tip} ({why}) — not admitted", m.id)),
                    None => {
                        blocked_by.insert(m.id.clone(), r.blocked_by);
                        admitted.push(Member { id: m.id, tip })
                    }
                },
                Some(tip) => skips.push(format!("{}: {} is not its row's tip {tip} — not admitted", m.id, m.tip)),
                None => skips.push(format!("{}: no submitted tip — not admitted", m.id)),
            },
        }
    }

    let wt = worktree.map(Path::to_path_buf).unwrap_or_else(|| c.s.run.join("worktree").join(format!(".round-{repo_name}")));
    w.git.worktree_prune(&path);
    w.git.worktree_remove(&path, &wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &base_sha) {
        w.err(format!("queue.sh {label}: cannot create the round worktree {}", wt.display()));
        return FAIL;
    }
    let mut merged: Vec<Member> = Vec::new();
    let mut unreturned = false;
    for m in admitted {
        if let Some(b) = blocked_by.get(&m.id).and_then(|bs| bs.iter().find(|b| !merged.iter().any(|x| &x.id == *b))) {
            skips.push(format!("{}: blocked by {b}, which has not landed and is not merged ahead of it in this round — not admitted", m.id));
            continue;
        }
        if w.git.merge_no_ff(&wt, &w.lib.land_subject(&m.id), &m.tip, &c.s.git_name, &c.s.git_email) {
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            if w.lib.base_conflict(&path, &base_sha, &m.tip) {
                skips.push(match return_to_rework(w, &m) {
                    Ok(()) => format!("{}: conflicts with base — returned to rework", m.id),
                    Err(e) => {
                        unreturned = true;
                        format!("{}: conflicts with base ({e})", m.id)
                    }
                });
            } else {
                skips.push(format!("{}: conflicts with the round", m.id));
            }
        }
    }
    for s in &skips {
        w.out(format!("queue.sh {label}: skip — {s}"));
    }
    if unreturned {
        w.err(format!("queue.sh {label}: a member that conflicts with base could not be returned to rework — no round opened for {repo_name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    if merged.is_empty() {
        w.err(format!("queue.sh {label}: no round — nothing admissible for {repo_name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let batch = name.map(str::to_string).unwrap_or_else(|| format!("{repo_name}-{}", w.clock.stamp()));
    if let Err((rc, out)) = w.lc.cut(&batch, &repo_name, &head, &base_sha, &csv(&merged), LC_ACTOR) {
        w.err(format!("queue.sh {label}: spira-lc cut refused for {batch} (rc={rc}): {out}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }

    let now = w.clock.now().to_string();
    let mut kv = Kv::default();
    kv.push("batch_id", &batch);
    kv.push("repo", &repo_name);
    kv.push("head", &head);
    kv.push("base", &base_sha);
    kv.push("members", &render_members(&merged));
    kv.push("worktree", &wt.display().to_string());
    kv.push("actor", &actor(w));
    kv.push("opened", &now);
    set_phase(w, &mut kv, "opened");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, &batch);
    landing_log(&c.s.run, &format!("QUEUE ROUND-OPEN {now} repo={repo_name} batch={batch} head={head} members={}", csv(&merged)));
    w.out(format!("batch={batch}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    OK
}

fn status(w: &World, repo: Option<&str>) -> i32 {
    let label = "round status";
    let Ok(c) = resolve(w, label, repo) else { return FAIL };
    let kv = match records::read_kv(&c.queue_file(RECORD)) {
        Ok(Some(kv)) => kv,
        _ => {
            w.out(format!("round=none\nrepo={}", c.r.name));
            return OK;
        }
    };
    let since = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok()).map_or(0, |t| w.clock.now().saturating_sub(t));
    w.out("round=open");
    for k in ["batch_id", "repo", "phase", "head", "base", "members"] {
        w.out(format!("{k}={}", kv.get(k).unwrap_or("")));
    }
    if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
        w.out(format!("red={red}"));
    }
    w.out(format!("wall_secs={}", since("opened")));
    w.out(format!("phase_secs={}", since("phase_at")));
    OK
}

/// Each `<suite>.result` under `dir` (two levels deep) with its first word, the status.
/// Public so the sim's stub round-vm is tested against this exact reader.
pub fn suite_statuses(dir: &Path, found: &mut Vec<(String, String)>, depth: u32) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 2 {
            suite_statuses(&p, found, depth + 1);
        } else if let Some(suite) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".result")) {
            let status = fs::read_to_string(&p).ok().and_then(|t| t.split_whitespace().next().map(String::from)).unwrap_or_default();
            found.push((suite.to_string(), status));
        }
    }
}

/// Names the suites whose verdict cannot be read: a word that is neither passing nor blocking
/// (an empty file included), or a corpus suite with no result at all.
pub(crate) fn unjudgeable(wt: &Path, found: &[(String, String)]) -> Option<String> {
    let mut bad: Vec<String> = found
        .iter()
        .filter(|(_, s)| !PASSING.contains(&s.as_str()) && !BLOCKING.contains(&s.as_str()))
        .map(|(n, s)| format!("{n} (verdict {s:?})"))
        .collect();
    if let Ok(rd) = fs::read_dir(wt.join("spira")) {
        for name in rd.flatten().filter_map(|e| e.file_name().into_string().ok()) {
            if name.starts_with("test-") && name.ends_with(".sh") && !found.iter().any(|(n, _)| *n == name) {
                bad.push(format!("{name} (no result)"));
            }
        }
    }
    bad.sort();
    if bad.is_empty() {
        None
    } else {
        Some(format!("round-vm left no readable verdict for: {}", bad.join(", ")))
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn certify(w: &World, batch: &str, repo: Option<&str>, attest: Option<&str>) -> i32 {
    let label = "round certify";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let who = actor(w);

    let (head, wt, run_id) = {
        let Ok(_g) = lock(w, label, &c) else { return FAIL };
        let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
        let wt = worktree_of(&kv);
        let mut head = kv.get("head").unwrap_or("").to_string();
        if let Some(a) = attest {
            let at = w.git.rev_parse(&path, a);
            let at_wt = w.git.rev_parse(&wt, "HEAD");
            if at.is_none() || at != at_wt {
                w.err(format!("queue.sh {label}: --attest {a} is not the round worktree's head ({}) — refused", at_wt.unwrap_or_else(|| "<none>".into())));
                return FAIL;
            }
            head = at.unwrap_or_default();
            set(&mut kv, "head", &head);
        } else if w.git.rev_parse(&wt, "HEAD").as_deref() != Some(head.as_str()) {
            w.err(format!("queue.sh {label}: the round worktree {} is not at the round's head {head} — refused", wt.display()));
            return FAIL;
        }
        let run_id = format!("{batch}-{}", w.clock.now());
        let started = start_pass(w, batch, &head, false);
        if let Err((rc, out)) = started {
            w.err(format!("queue.sh {label}: spira-lc refused to start the round {batch} (rc={rc}): {out}"));
            return FAIL;
        }
        set_phase(w, &mut kv, "certifying");
        kv.remove("red");
        if save(w, label, &c, &kv).is_err() {
            return FAIL;
        }
        mark_running(&c, batch);
        (head, wt, run_id)
    };

    let mut reds: Vec<String> = Vec::new();
    let mut timings = Timings { suites_s: 0, build_s: 0 };
    if attest.is_none() {
        let results = rounds_dir(&c).join(format!("{batch}.results"));
        let _ = fs::remove_dir_all(&results);
        let Some(base) = c.r.landref.clone() else {
            w.err(format!("queue.sh {label}: cannot resolve the landing ref of {} — the round's lint has nothing to judge against", c.r.name));
            return FAIL;
        };
        let began = w.clock.now();
        let out = w.scripts.round_vm(&wt, &results, &base, c.s.round_wall_secs);
        timings = pass_timings(&results, w.clock.now().saturating_sub(began));
        let mut found = Vec::new();
        suite_statuses(&results, &mut found, 0);
        let fault = match out.rc {
            0 | 1 if found.is_empty() => Some("round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string()),
            0 | 1 => unjudgeable(&wt, &found),
            4 => {
                reds.push(BUILD_RED.into());
                None
            }
            124 | 137 => Some(format!("round-vm exceeded the {}s wall", c.s.round_wall_secs)),
            rc => Some(format!("round-vm exited {rc}")),
        };
        if let Some(why) = fault {
            warn_unrecorded(w, label, batch, "the incomplete pass", pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false));
            let _g = lock(w, label, &c);
            if let Ok(mut kv) = load(w, label, &c, batch) {
                set_phase(w, &mut kv, "fault");
                let _ = save(w, label, &c, &kv);
            }
            w.err(format!("queue.sh {label}: {why}; the round is not judged\n{}", tail(&out.err, 20)));
            return FAULT;
        }
        reds.extend(found.iter().filter(|(_, s)| BLOCKING.contains(&s.as_str())).map(|(n, _)| n.clone()));
        reds.sort();
        reds.dedup();
    }

    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
    if kv.get("phase") != Some("certifying") || kv.get("head") != Some(head.as_str()) {
        w.err(format!("queue.sh {label}: the round {batch} changed while it was being certified — the result is discarded; certify again"));
        return FAIL;
    }
    if !reds.is_empty() {
        if reds != [BUILD_RED] {
            warn_unrecorded(w, label, batch, "the suites phase", suites_started(w, batch, false));
        }
        warn_unrecorded(w, label, batch, "the red pass", pass_verdict_event(w, batch, &Verdict::Red(&reds, timings), false));
        set(&mut kv, "red", &reds.join(","));
        set_phase(w, &mut kv, "red");
        if save(w, label, &c, &kv).is_err() {
            return FAIL;
        }
        mark_running(&c, batch);
        w.out(format!("queue.sh {label}: round {batch} is RED at {head}: {}", reds.join(",")));
        w.out(format!("red={}", reds.join(",")));
        return FAIL;
    }

    let greened = suites_started(w, batch, false).and_then(|()| pass_verdict_event(w, batch, &Verdict::Green(timings), false));
    if let Err((rc, out)) = greened {
        w.err(format!("queue.sh {label}: spira-lc refused the GREEN for {batch} (rc={rc}): {out}"));
        return FAIL;
    }
    let Some(tree) = w.git.rev_parse(&path, &format!("{head}^{{tree}}")) else {
        w.err(format!("queue.sh {label}: cannot resolve {head}^{{tree}} — nothing can say what was judged"));
        return FAIL;
    };
    let cert = gate::cert::Cert {
        source: gate::cert::Source::Round,
        tree: tree.clone(),
        repo: c.r.name.clone(),
        rev: head.clone(),
        branch: format!("round/{batch}"),
        by: who,
        when: w.clock.stamp(),
        at: w.clock.now(),
        harness: "-".into(),
        suites: "full-corpus".into(),
    };
    let verdicts = gate::cert::verdicts_dir(w.var("SPIRA_VERDICTS").as_deref(), &c.s.run);
    if let Err(e) = gate::cert::write(&verdicts, &cert) {
        w.err(format!("queue.sh {label}: cannot record the round certificate for {head}: {e}"));
        return FAIL;
    }
    // A green round IS the full-suite pass on its head (sp-x334k): publish refuses a head with no
    // full-suite local-pass record, and the round's full corpus is the run that earns it.
    if let Err(e) = spira_config::local_pass::record(&c.s.run, spira_config::local_pass::Kind::FullSuite, &head, &format!("round {batch}"), &w.clock.now().to_string()) {
        // Not a reason to discard a green round: publish fails closed on the missing record and
        // names it, so the gap is loud at the one place it matters.
        w.err(format!("queue.sh {label}: WARNING — round {batch} is green but its full-suite local pass for {head} was not recorded ({e}); publish will refuse this head until it is"));
    }
    set_phase(w, &mut kv, "green");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, batch);
    landing_log(&c.s.run, &format!("QUEUE ROUND-GREEN {} repo={} batch={batch} head={head} tree={tree} run={run_id}", w.clock.now(), c.r.name));
    w.out(format!("queue.sh {label}: round {batch} is GREEN at {head} (tree {tree})"));
    OK
}

#[allow(clippy::too_many_arguments)]
fn eject(w: &World, batch: &str, id: &str, repo: Option<&str>, reason: &Text, suites: &str, red: bool, harness_fault: bool, rebuild: bool) -> i32 {
    let label = "round eject";
    let reason = match read_text(w, reason) {
        Ok(r) if !r.trim().is_empty() => r,
        Ok(_) => {
            w.err(format!("queue.sh {label}: --reason is required — the bead carries it back to its builder"));
            return USAGE;
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch), ("bead id", id)]).is_err() {
        return FAIL;
    }
    if !suites.is_empty() && suites.split(',').try_for_each(|s| idents(w, label, &[("suite", s)])).is_err() {
        return FAIL;
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
    let members = kv.members();
    if !members.iter().any(|m| m.id == id) {
        let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
        w.err(format!("queue.sh {label}: {id} is not a member of round {batch} (members: {})", ids.join(" ")));
        return FAIL;
    }
    let wt = worktree_of(&kv);
    let old_head = kv.get("head").unwrap_or("").to_string();
    let base_sha = kv.get("base").unwrap_or("").to_string();
    let ejected_tip = members.iter().find(|m| m.id == id).map(|m| m.tip.clone()).unwrap_or_default();
    let stacked: Vec<Member> = members
        .iter()
        .filter(|m| m.id != id && w.git.merge_base(&path, &ejected_tip, &m.tip).is_some_and(|mb| !w.git.is_ancestor(&path, &mb, &base_sha)))
        .cloned()
        .collect();
    let survivors: Vec<Member> = members.iter().filter(|m| m.id != id && !stacked.iter().any(|s| s.id == m.id)).cloned().collect();

    let mut new_head = old_head.clone();
    if rebuild && !survivors.is_empty() {
        match assemble(w, &c, &path, &wt, &base_sha, &survivors, &old_head) {
            Ok(h) => new_head = h,
            Err(why) => {
                w.err(format!("queue.sh {label}: cannot rebuild the round without {id}: {why} — nothing changed"));
                return FAIL;
            }
        }
    }

    let why = bounded_text(&reason);
    if let Err((rc, out)) = lc_cas(w, batch, |s, v| w.lc.eject_member(batch, id, s, v, LC_ACTOR, &why)) {
        if new_head != old_head {
            w.git.worktree_remove(&path, &wt);
            let _ = w.git.worktree_add_detached(&path, &wt, &old_head);
        }
        w.err(format!("queue.sh {label}: spira-lc eject-member refused for {id} (rc={rc}): {out} — nothing changed"));
        return FAIL;
    }
    let cause = EjectCause::decide(red, harness_fault, suites);
    let mut unreturned: Vec<&str> = Vec::new();
    for (bead, why_text, own_cause) in std::iter::once((id, reason.clone(), cause)).chain(stacked.iter().map(|m| (m.id.as_str(), format!("stacked on {id}"), cause))) {
        if !w.lib.bead_reopen(bead, own_cause.as_str(), suites) {
            w.err(format!("queue.sh {label}: cannot reopen {bead} (cause {})", own_cause.as_str()));
            unreturned.push(bead);
        }
        if lc_return(w, bead).is_err() && !unreturned.contains(&bead) {
            unreturned.push(bead);
        }
        w.lib.release_claim(bead);
        if bead != id {
            let why = bounded_text(&why_text);
            if let Err((rc, out)) = lc_cas(w, batch, |s, v| w.lc.eject_member(batch, bead, s, v, LC_ACTOR, &why)) {
                w.err(format!("queue.sh {label}: spira-lc eject-member refused for {bead} (rc={rc}): {out}"));
            }
        }
        let mut comment = format!("Ejected from round {batch} in {}.\n\n{why_text}", c.r.name);
        comment.push_str("\n\nFix the failing issue and re-certify before rejoining the queue.");
        if !suites.is_empty() {
            comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
        }
        w.lib.comment(bead, &comment);
        landing_log(&c.s.run, &format!("QUEUE ROUND-EJECT {} repo={} batch={batch} id={bead} cause={} reason={}", w.clock.now(), c.r.name, own_cause.as_str(), one_line(&why_text)));
        if bead != id {
            w.out(format!("queue.sh {label}: ejected {bead} from round {batch} (stacked on {id})"));
        }
    }

    if survivors.is_empty() {
        let emptied = bounded_text(&format!("round emptied: {id} ejected — {reason}"));
        if let Err((rc, out)) = lc_cas(w, batch, |s, v| w.lc.abandon_batch(batch, s, v, LC_ACTOR, &emptied)) {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for the emptied round {batch} (rc={rc}): {out}"));
        }
        let _ = fs::remove_file(c.queue_file(RECORD));
        w.git.worktree_remove(&path, &wt);
        finish(&c, batch, &format!("emptied: {id} ejected, no member remains"));
        w.out(format!("queue.sh {label}: ejected {id}; the round {batch} has no member left and is closed"));
        if !unreturned.is_empty() {
            w.err(format!("queue.sh {label}: {} left round {batch} but did not return to REWORK: {} — see above", id, unreturned.join(" ")));
            return FAIL;
        }
        return OK;
    }

    let rebuilt = serde_json::json!({"PassRebuilt": {"head": new_head}}).to_string();
    warn_unrecorded(w, label, batch, "the rebuilt head", pass_event(w, batch, |s, _, _| (s == "ATTRIBUTING").then_some(rebuilt)));
    set(&mut kv, "members", &render_members(&survivors));
    set(&mut kv, "head", &new_head);
    kv.remove("red");
    let ejected = stacked.iter().fold(format!("{}{id} ", kv.get("ejected").unwrap_or("")), |acc, m| format!("{acc}{} ", m.id));
    set(&mut kv, "ejected", &ejected);
    set_phase(w, &mut kv, "opened");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, batch);
    w.out(format!("queue.sh {label}: ejected {id} from round {batch}; {} member(s) remain", survivors.len()));
    w.out(format!("head={new_head}"));
    w.out(format!("members={}", render_members(&survivors)));
    if !unreturned.is_empty() {
        w.err(format!("queue.sh {label}: {} left round {batch} but did not return to REWORK: {} — see above", id, unreturned.join(" ")));
        return FAIL;
    }
    OK
}

fn land(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    let label = "round land";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(kv) = load(w, label, &c, batch) else { return FAIL };
    let phase = kv.get("phase").unwrap_or("");
    if phase != "green" {
        w.err(format!("queue.sh {label}: round {batch} is {phase}, not green — certify it first"));
        return FAIL;
    }
    let head = kv.get("head").unwrap_or("").to_string();
    let members = kv.members();
    let wt = worktree_of(&kv);
    let bins = wt.join("target").join("release").is_dir().then_some(wt.as_path());
    let rc = land_local_with(w, Some(&c.r.name), &head, &Text::Arg(csv(&members)), bins, true);
    let base = c.r.landref.clone().unwrap_or_default();
    if rc != OK && !w.git.is_ancestor(&path, &head, &base) {
        return rc;
    }

    let recorded = match lc_cas(w, batch, |_, v| w.lc.land_batch(batch, v, LC_ACTOR, &head)) {
        Ok(()) => true,
        Err((rc, out)) => {
            let why = if out.trim().is_empty() { "spira-lc gave no reason on stderr or stdout" } else { out.trim() };
            w.err(format!("queue.sh {label}: ALARM: {head} is landed and live but spira-lc refused the batch landing record for {batch} (rc={rc}): {why}"));
            false
        }
    };
    let _ = fs::remove_file(c.queue_file(RECORD));
    w.git.worktree_remove(&path, &wt);
    finish(&c, batch, &format!("landed {head} members={}", csv(&members)));
    landing_log(&c.s.run, &format!("QUEUE ROUND-LAND {} repo={} batch={batch} head={head} members={}", w.clock.now(), c.r.name, csv(&members)));
    w.out(format!("queue.sh {label}: round {batch} landed at {head} — {} member(s)", members.len()));
    if !recorded {
        w.err(format!("queue.sh {label}: round {batch} has NO batch landing record; the release is live — exiting non-zero"));
        return if rc == OK { FAIL } else { rc };
    }
    rc
}

fn requeue_stranded(w: &World, label: &str, id: &str, tip: &str) -> Option<String> {
    let (state, version) = w.lc.bead_state(id)?;
    if state != "IN_DELIVERY" {
        return Some(state);
    }
    let kind = serde_json::json!({"Requeued": {"tip": tip}}).to_string();
    if let Err((rc, e)) = w.lc.bead_event(id, &state, &version, LC_ACTOR, &kind) {
        w.err(format!("queue.sh {label}: {id} is still IN_DELIVERY after the abandon and the requeue was refused (rc={rc}): {e}"));
        return None;
    }
    w.lc.bead_state(id).map(|(s, _)| s)
}

fn abandon(w: &World, batch: &str, repo: Option<&str>, reason: &Text) -> i32 {
    let label = "round abandon";
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.trim().is_empty() {
        w.err(format!("queue.sh {label}: --reason is required — pass --reason \"<why>\" so the abandon is auditable"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(kv) = load(w, label, &c, batch) else { return FAIL };
    let members = kv.members();
    let why = bounded_text(&reason);
    match lc_cas(w, batch, |s, v| w.lc.abandon_batch(batch, s, v, LC_ACTOR, &why)) {
        Ok(()) => w.out(format!("queue.sh {label}: {batch} abandoned on spira-lc")),
        Err((1, out)) if out.is_empty() => w.err(format!("queue.sh {label}: spira-lc has no batch row for {batch}")),
        Err((rc, out)) => {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for {batch} (rc={rc}): {out} — the round stays open"));
            return FAIL;
        }
    }
    for m in &members {
        let mut state = w.lc.bead_row(&m.id).map(|r| r.state).unwrap_or_else(|| "unknown".into());
        if state == "IN_DELIVERY" {
            state = requeue_stranded(w, label, &m.id, &m.tip).unwrap_or(state);
        }
        w.out(format!("queue.sh {label}: {}: {state}", m.id));
    }
    let _ = fs::remove_file(c.queue_file(RECORD));
    w.git.worktree_remove(&path, &worktree_of(&kv));
    finish(&c, batch, &format!("abandoned: {}", one_line(&reason)));
    landing_log(&c.s.run, &format!("QUEUE ROUND-ABANDON {} repo={} batch={batch} reason={}", w.clock.now(), c.r.name, one_line(&reason)));
    w.out(format!("queue.sh {label}: round {batch} abandoned for {}", c.r.name));
    OK
}
