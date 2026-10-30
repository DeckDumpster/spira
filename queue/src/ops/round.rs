//! `queue round`: the round lifecycle the batcher and the Concierge share, under queue.local.
//! A round is one record (`<queue_dir>/<repo>/round`), one worktree and one spira-lc batch;
//! the verbs differ from a hand's scratch scripts in that spira-lc, loom and round-duty can
//! all see the round (DESIGN.md §2.2).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::batch::lc_return;
use super::land::land_local_deferring;
use super::stage;
use super::{
    actor, converge_orphans, czar_ok, idents, landing_log, lc_abandon, lc_cas, lc_eject_member,
    lock_held_by_caller, read_text, repo_path, require_lc, resolve, statute_gate, take_lock, Ctx,
    World, FAIL, OK, USAGE,
};
use crate::cli::{Round, Text};
use crate::ident::bounded_text;
use crate::lock::Guard;
use crate::model::{parse_members, render_members, EjectCause, LandMode, Member};
use crate::ports::{ref_branch, AmiBuild, AmiLookup, RunOut};
use crate::records::{self, one_line, write_atomic, Kv};
use landing_pass::gateq::GateQueue;
use landing_pass::model::GateOutcome;

/// certify's exit when the corpus could not be judged (the VM, the wall, the install), as
/// opposed to FAIL, which is a red round.
pub const FAULT: i32 = 4;

/// certify's exit when `round preempt` stopped its pass: the verb, not certify, records that.
pub const PREEMPTED: i32 = 5;

/// round-vm's exit, and certify's, when the round VM died under the pass: no verdict, no member
/// at fault, the pass is run again.
pub const VM_LOST: i32 = 75;

const PREEMPT_POLLS: u32 = 120;

pub(super) const RECORD: &str = "round";
pub(super) const BUILD_RED: &str = "workspace-build";
pub(super) const LINT_RED: &str = "spira-lint";
const FENCE_RED_PREFIX: &str = "fence-";
pub(super) const UNIT_RED: &str = "unit-tests";
/// Steps a round runs over its whole tree that are not testenv suites; only a green round proves them.
pub const ROUND_STEPS: [&str; 3] = [BUILD_RED, LINT_RED, UNIT_RED];
/// The round steps `testenv --suites <step>` cannot run for a tip.
pub const ROUND_ONLY_STEPS: [&str; 2] = [BUILD_RED, LINT_RED];

/// A round step, or a base fence the round runs before its suites (`fence-<name>`).
pub fn is_round_step(name: &str) -> bool {
    ROUND_STEPS.contains(&name) || name.starts_with(FENCE_RED_PREFIX)
}
/// The one red a repository that judges itself reports: its testenv run as a whole.
pub(super) const REPO_TESTENV_RED: &str = "testenv";
pub(super) const LC_ACTOR: &str = "queue.sh";
const PASSING: [&str; 2] = ["ok", "skip"];
pub const BLOCKING: [&str; 5] = ["red", "timeout", "unreached", "deferred", "fault"];

pub fn run(w: &World, r: &Round) -> i32 {
    match r {
        Round::Open {
            repo,
            members,
            name,
            worktree,
        } => open(
            w,
            repo.as_deref(),
            members,
            name.as_deref(),
            worktree.as_deref(),
        ),
        Round::Certify {
            batch,
            repo,
            attest,
            ran,
            rehead,
        } => certify(
            w,
            batch,
            repo.as_deref(),
            attest.as_deref(),
            ran.as_deref().and_then(|r| r.parse().ok()),
            rehead.as_deref(),
        ),
        Round::Eject {
            batch,
            id,
            repo,
            reason,
            suites,
            red,
            harness_fault,
            rebuild,
            with,
        } => eject(
            w,
            batch,
            id,
            repo.as_deref(),
            reason,
            suites,
            *red,
            *harness_fault,
            *rebuild,
            with,
        ),
        Round::Land { batch, repo } => land(w, batch, repo.as_deref()),
        Round::Abandon {
            batch,
            repo,
            reason,
        } => abandon(w, batch, repo.as_deref(), reason),
        Round::Preempt {
            batch,
            repo,
            eject,
            reason,
            suites,
        } => preempt(w, batch, repo.as_deref(), eject, reason, suites),
        Round::Status { repo } => status(w, repo.as_deref()),
        Round::PassStart {
            batch,
            repo,
            resume,
        } => pass_start(w, batch, repo.as_deref(), *resume),
        Round::SuitesStarted { batch, repo } => suites_started_verb(w, batch, repo.as_deref()),
        Round::Failed { batch, repo, suite } => failed_verb(w, batch, repo.as_deref(), suite),
        Round::Mend {
            outcome,
            batch,
            suite,
            repo,
            tips,
            checked,
            ask,
            reason,
        } => super::mend::run(
            w,
            outcome,
            batch,
            suite,
            repo.as_deref(),
            tips,
            checked,
            ask.as_deref(),
            reason,
        ),
        Round::PassVerdict {
            batch,
            repo,
            verdict,
            red_suites,
            suites_s,
            build_s,
            reason,
        } => pass_verdict(
            w,
            batch,
            repo.as_deref(),
            verdict,
            red_suites,
            Timings {
                suites_s: *suites_s,
                build_s: *build_s,
            },
            reason,
        ),
        Round::Stage {
            repo,
            members,
            name,
            worktree,
        } => stage::stage(
            w,
            repo.as_deref(),
            members,
            name.as_deref(),
            worktree.as_deref(),
        ),
        Round::StageTest { repo } => stage::stage_test(w, repo.as_deref()),
        Round::Promote { repo } => stage::promote(w, repo.as_deref()),
        Round::Discard { repo, reason } => stage::discard(w, repo.as_deref(), reason),
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
        let (n, phase) =
            w.lc.batch_pass(batch)
                .ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        match kind(s, n, &phase) {
            Some(k) => w.lc.batch_event(batch, s, v, LC_ACTOR, &k),
            None => Ok(()),
        }
    })
}

/// `strict` is a hand verb: it always sends the event, so the machine refuses a wrong-phase call
/// and names the phase. certify and `pass-start --resume` are not strict: they resume a pass already recorded.
fn start_pass(w: &World, batch: &str, head: &str, strict: bool) -> Result<(), (i32, String)> {
    let rebuilt = serde_json::json!({"PassRebuilt": {"head": head}}).to_string();
    pass_event(w, batch, |s, _, _| (s == "ATTRIBUTING").then_some(rebuilt))?;
    pass_event(w, batch, |s, n, _| {
        (strict || matches!(s, "OPEN" | "REBUILDING"))
            .then(|| serde_json::json!({"PassStarted": {"n": n + 1, "head": head}}).to_string())
    })
}

const STALE_TRIES: u32 = 5;

fn suites_started(w: &World, batch: &str, strict: bool) -> Result<(), (i32, String)> {
    let send = || {
        pass_event(w, batch, |_, n, phase| {
            (phase == "build" || (strict && phase != "suites"))
                .then(|| serde_json::json!({"SuitesStarted": {"n": n}}).to_string())
        })
    };
    let mut sent = send();
    for attempt in 1..STALE_TRIES {
        match &sent {
            Err((_, e)) if e.contains("StaleVersion") => {
                w.clock.sleep(u64::from(attempt));
                sent = send();
            }
            _ => break,
        }
    }
    sent
}

fn pass_verdict_event(
    w: &World,
    batch: &str,
    verdict: &Verdict,
    strict: bool,
) -> Result<(), (i32, String)> {
    lc_cas(w, batch, |s, v| {
        if !strict && s != "CI_RUNNING" {
            return match (verdict, s) {
                (Verdict::Green(_), "GREEN") | (Verdict::Red(..) | Verdict::Incomplete(_), _) => {
                    Ok(())
                }
                (Verdict::Green(_), other) => {
                    Err((1, format!("the batch is {other}, not CI_RUNNING")))
                }
            };
        }
        let (n, _) =
            w.lc.batch_pass(batch)
                .ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        let kind = match verdict {
            Verdict::Green(t) => {
                serde_json::json!({"PassGreen": {"n": n, "suites_s": t.suites_s, "build_s": t.build_s}})
            }
            Verdict::Red(red, t) => {
                serde_json::json!({"PassRed": {"n": n, "red_suites": red, "suites_s": t.suites_s, "build_s": t.build_s}})
            }
            Verdict::Incomplete(why) => {
                serde_json::json!({"PassIncomplete": {"n": n, "reason": why}})
            }
        };
        w.lc.batch_event(batch, s, v, LC_ACTOR, &kind.to_string())
    })
}

fn pass_verb<F>(w: &World, label: &str, batch: &str, repo: Option<&str>, f: F) -> i32
where
    F: FnOnce(&Ctx, &Kv) -> Result<(), (i32, String)>,
{
    let Ok((c, _)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else {
        return FAIL;
    };
    let Ok(kv) = load(w, label, &c, batch) else {
        return FAIL;
    };
    match f(&c, &kv) {
        Ok(()) => {
            w.out(format!("queue.sh {label}: recorded for {batch}"));
            OK
        }
        Err((rc, out)) => {
            w.err(format!(
                "queue.sh {label}: spira-lc refused for {batch} (rc={rc}): {out}"
            ));
            FAIL
        }
    }
}

fn pass_start(w: &World, batch: &str, repo: Option<&str>, resume: bool) -> i32 {
    pass_verb(w, "round pass-start", batch, repo, |_, kv| {
        start_pass(w, batch, kv.get("head").unwrap_or(""), !resume)
    })
}

fn suites_started_verb(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    pass_verb(w, "round suites-started", batch, repo, |_, _| {
        suites_started(w, batch, true)
    })
}

/// A suite went red mid-pass. A result arriving proves the suites phase began, so the phase is
/// advanced first; the event itself is always sent, so a pass already judged is refused by name.
fn failed_verb(w: &World, batch: &str, repo: Option<&str>, suite: &str) -> i32 {
    if idents(w, "round failed", &[("suite", suite)]).is_err() {
        return FAIL;
    }
    pass_verb(w, "round failed", batch, repo, |c, kv| {
        suites_started(w, batch, false)?;
        pass_event(w, batch, |_, n, _| {
            Some(serde_json::json!({"Failed": {"suite": suite, "n": n}}).to_string())
        })?;
        call_mender(w, c, kv, batch, suite, "", None);
        Ok(())
    })
}

/// A forge batch's row stays OPEN while its PR's CI runs, and Failed is legal only from a running
/// pass, so the pass is started from the batch's head first; a row already running is left alone.
fn send_failed(w: &World, batch: &str, head: &str, suite: &str) -> Result<(), (i32, String)> {
    pass_event(w, batch, |s, n, _| {
        (s == "OPEN")
            .then(|| serde_json::json!({"PassStarted": {"n": n + 1, "head": head}}).to_string())
    })?;
    suites_started(w, batch, false)?;
    pass_event(w, batch, |_, n, _| {
        Some(serde_json::json!({"Failed": {"suite": suite, "n": n}}).to_string())
    })
}

/// A forge batch's red step enters the mending machine as a Failed event on the batch's current pass.
pub(super) fn forge_failed(w: &World, batch: &str, head: &str, suite: &str) -> Result<(), ()> {
    send_failed(w, batch, head, suite).map_err(|(rc, e)| w.err(format!("queue.sh verdict: WARNING — no mender for {suite} of {batch}: the Failed event was refused (rc={rc}): {e}")))
}

/// A red pass records its verdict whoever cut the round, so every red suite the on-red hook did not
/// report gets its Failed event and mender here, carrying the path attributor's reading as evidence.
fn raise_unreported_reds(w: &World, c: &Ctx, batch: &str, kv: &Kv, reds: &[String]) {
    let Some((pass, _)) = w.lc.batch_pass(batch) else {
        return;
    };
    let rd = rounds_dir(c);
    let unreported: Vec<&String> = reds
        .iter()
        .filter(|s| *s != BUILD_RED && !mender_marker(&rd, batch, pass, s).exists())
        .collect();
    if unreported.is_empty() {
        return;
    }
    let results = rd.join(format!("{batch}.results"));
    let mut file = String::new();
    for s in reds.iter().filter(|s| *s != BUILD_RED) {
        let out = suite_output(&results, s, 0)
            .map(|o| tail(&o, 80))
            .unwrap_or_default();
        file.push_str(&format!("@@ {s}\n{out}\n"));
    }
    let reds_file = rd.join(format!("{batch}.reds-{pass}"));
    let attribution = match (c.s.batcher_bin.as_ref(), write_atomic(&reds_file, &file)) {
        (Some(bin), Ok(())) => {
            w.scripts
                .path_attribution(bin, &c.r.name, &csv(&kv.members()), &reds_file)
        }
        _ => String::new(),
    };
    let _ = fs::remove_file(&reds_file);
    for suite in unreported {
        let sent = suites_started(w, batch, false).and_then(|()| {
            pass_event(w, batch, |_, n, _| {
                Some(serde_json::json!({"Failed": {"suite": suite, "n": n}}).to_string())
            })
        });
        match sent {
            Ok(()) => call_mender(w, c, kv, batch, suite, &attribution, None),
            Err((rc, e)) => w.err(format!("queue.sh round certify: WARNING — no mender for {suite} of {batch}: the Failed event was refused (rc={rc}): {e}")),
        }
    }
}

/// File the mender-lane bead for a failure whose MENDING row the event just opened. A marker
/// per (batch, pass, suite) keeps a repeated report from filing twice; a filing that fails is
/// said aloud and leaves no marker, so the next report of the same failure files it.
pub(super) fn call_mender(
    w: &World,
    c: &Ctx,
    kv: &Kv,
    batch: &str,
    suite: &str,
    attribution: &str,
    output: Option<&str>,
) {
    let Some((pass, _)) = w.lc.batch_pass(batch) else {
        w.err(format!("queue.sh round failed: WARNING — no mender bead filed for {suite} of {batch}: spira-lc cannot say which pass it is on"));
        return;
    };
    if c.r.name.is_empty() {
        w.err(format!("queue.sh round failed: WARNING — no mender bead filed for {suite} of {batch}: the repo is unnamed, so the bead could not be claimed"));
        return;
    }
    let marker = mender_marker(&rounds_dir(c), batch, pass, suite);
    sweep_menders(w, c);
    if marker.exists() {
        return;
    }
    let results = rounds_dir(c).join(format!("{batch}.results"));
    let output = output
        .map(str::to_string)
        .or_else(|| suite_output(&results, suite, 0).map(|o| failure_excerpt(&o)))
        .unwrap_or_else(|| "(the suite left no output in the round's results)".into());
    if let Some(cause) = repo_config_refusal(&output) {
        w.out(format!("queue.sh round failed: no mender bead for {suite} in pass {pass} of {batch}: {} was refused for a repo-config reason ({cause}) that no mend can fix", c.r.name));
        return;
    }
    let Some(repo_path) = c.r.path.as_ref() else {
        w.err(format!("queue.sh round failed: WARNING — no mender bead filed for {suite} of {batch}: the repo has no path, so the failure spec cannot name it"));
        return;
    };
    let spec_path = spec_file(&rounds_dir(c), batch, pass, suite);
    let spec = serde_json::json!({
        "batch": batch,
        "pass": pass,
        "suite": suite,
        "repo": repo_path.display().to_string(),
        "base": kv.get("base").unwrap_or(""),
        "output": output,
        "tree": kv.get("worktree").filter(|t| !t.is_empty()),
        "members": kv.members().iter().map(|m| serde_json::json!({"bead": m.id, "tip": m.tip})).collect::<Vec<_>>(),
    });
    if let Err(e) = write_atomic(&spec_path, &spec.to_string()) {
        w.err(format!("queue.sh round failed: WARNING — no mender bead filed for {suite} of {batch}: the failure spec could not be written: {e}"));
        return;
    }
    let body = format!(
        "A suite went red in pass {pass} of round {batch} ({repo}): {suite}.\n\nspec: {spec}\nBatch: {batch}\nPass: {pass}\nSuite: {suite}\nDeadline: the machine stamps it when you claim this failure; list it with `spira-lc mending list --batch {batch} --live`. Record your outcome with `queue round mended|return|environment|flaky|unsure {batch} {suite}` before it passes.\n\nSuite output (tail):\n{output}\n",
        repo = c.r.name,
        spec = spec_path.display(),
    );
    let body = if attribution.trim().is_empty() {
        body
    } else {
        format!("{body}\nPath attribution of the pass's reds (batcher path-attribute):\n{attribution}\n")
    };
    let mut labels = String::new();
    if let Some(scope) = w
        .env
        .config("SPIRA_SCOPE_LABEL")
        .ok()
        .filter(|l| !l.is_empty())
    {
        labels.push_str(&scope);
        labels.push(',');
    }
    labels.push_str(&c.s.verdict.mender_label);
    labels.push_str(&format!(",repo:{}", c.r.name));
    let title = format!("mend {suite} red in pass {pass} of round {batch}");
    match w.lib.create_bug(&actor(w), &title, "1", &labels, &body) {
        Some(id) => {
            w.lc.create_bead(&id);
            let _ = write_atomic(&marker, &format!("{id}\n"));
            w.out(format!("queue.sh round failed: mender bead {id} filed for {suite} in pass {pass} of {batch}"));
        }
        None => w.err(format!("queue.sh round failed: WARNING — the mender bead for {suite} of {batch} could not be filed; report the failure again to retry")),
    }
}

const REPO_CONFIG_REFUSALS: [&str; 3] = [
    "declares no ci_entry",
    "no round AMI carries",
    "IMAGE-ABSENT",
];

fn repo_config_refusal(output: &str) -> Option<&'static str> {
    REPO_CONFIG_REFUSALS
        .into_iter()
        .find(|m| output.contains(m))
}

/// Close every mender bead filed for a batch that is closing: the failure it was filed for ends
/// with the batch. Idempotent; a close that fails says why and is retried by the next call, and
/// every call also retires the menders of any other batch the lifecycle already holds terminal.
fn mender_marker(dir: &Path, batch: &str, pass: impl std::fmt::Display, suite: &str) -> PathBuf {
    dir.join(format!(
        "{}.mender-{pass}-{suite}",
        batch.replace('/', "%2F")
    ))
}

fn spec_file(dir: &Path, batch: &str, pass: impl std::fmt::Display, suite: &str) -> PathBuf {
    dir.join(format!(
        "{}.spec-{pass}-{suite}.json",
        batch.replace('/', "%2F")
    ))
}

pub(super) fn retire_menders(w: &World, c: &Ctx, batch: &str, why: &str) {
    retire_marked(w, c, |b| b == batch, why);
    sweep_menders(w, c);
}

pub(super) fn sweep_menders(w: &World, c: &Ctx) {
    if let Err((rc, e)) = w.lc.mending_close_ended("concierge") {
        w.err(format!(
            "queue.sh: WARNING — MENDING rows of closed batches were not ended (rc={rc}): {e}"
        ));
    }
    retire_marked(
        w,
        c,
        |b| matches!(w.lc.batch_state(b), Some((s, _)) if super::reconcile::terminal(&s)),
        "batch already closed",
    );
}

fn retire_marked(w: &World, c: &Ctx, ended: impl Fn(&str) -> bool, why: &str) {
    let Ok(rd) = fs::read_dir(rounds_dir(c)) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((batch, _)) = name.split_once(".mender-") else {
            continue;
        };
        let batch = batch.replace("%2F", "/");
        let batch = batch.as_str();
        let done = e.path().with_file_name(format!("{name}.retired"));
        if name.ends_with(".retired") || done.exists() || !ended(batch) {
            continue;
        }
        let Ok(id) = fs::read_to_string(e.path()) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        if w.lc
            .bead_row(id)
            .is_some_and(|r| r.holds.iter().any(|h| h == "ask"))
        {
            continue;
        }
        match w.lib.bead_retire(id, &format!("Batch {batch} closed ({why}); the failure this mender bead was filed for ended with it.")) {
            Ok(()) => {
                let _ = write_atomic(&done, "");
                w.out(format!("queue.sh: mender bead {id} retired — batch {batch} closed"));
            }
            Err(e) => w.err(format!("queue.sh: WARNING — mender bead {id} of closed batch {batch} could not be closed: {e}")),
        }
    }
}

fn suite_output(dir: &Path, suite: &str, depth: u32) -> Option<String> {
    let want = format!("{suite}.out");
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 2 {
            if let Some(o) = suite_output(&p, suite, depth + 1) {
                return Some(o);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(want.as_str()) {
            return fs::read_to_string(&p).ok();
        }
    }
    None
}

fn pass_verdict(
    w: &World,
    batch: &str,
    repo: Option<&str>,
    verdict: &str,
    red_suites: &str,
    t: Timings,
    reason: &Text,
) -> i32 {
    let why = read_text(w, reason).unwrap_or_default();
    let red: Vec<String> = red_suites
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if verdict == "incomplete" && why.trim().is_empty() {
        w.err(
            "queue.sh round pass-verdict: --reason is required for an incomplete pass",
        );
        return USAGE;
    }
    if verdict == "red" && red.is_empty() {
        w.err("queue.sh round pass-verdict: --red-suites is required for a red pass");
        return USAGE;
    }
    if red
        .iter()
        .try_for_each(|s| idents(w, "round pass-verdict", &[("suite", s)]))
        .is_err()
    {
        return FAIL;
    }
    let bounded = bounded_text(&why);
    let v = match verdict {
        "green" => Verdict::Green(t),
        "red" => Verdict::Red(&red, t),
        _ => Verdict::Incomplete(&bounded),
    };
    pass_verb(w, "round pass-verdict", batch, repo, |_, _| {
        pass_verdict_event(w, batch, &v, true)
    })
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
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("build_wall_s=")
                    .and_then(|v| v.trim().parse::<u64>().ok())
            })
        })
        .unwrap_or(0);
    Timings {
        build_s,
        suites_s: wall_s.saturating_sub(build_s),
    }
}

pub(super) fn local_ctx(w: &World, label: &str, repo: Option<&str>) -> Result<(Ctx, PathBuf), i32> {
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

pub(super) fn lock(w: &World, label: &str, c: &Ctx) -> Result<Option<Guard>, i32> {
    if lock_held_by_caller(w) {
        Ok(None)
    } else {
        take_lock(w, label, c, "")
    }
}

/// The record of the batch a mender reports on: the open round of a queue.local repo, or the open
/// batch PR of a queue.forge one.
pub(super) fn load_failed_batch(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    match records::read_kv(&c.queue_file("open")) {
        Ok(Some(kv)) if c.r.mode != LandMode::QueueLocal && kv.get("batch_id") == Some(batch) => {
            Ok(kv)
        }
        _ => load(w, label, c, batch),
    }
}

/// A queue.forge batch's red suites get the Failed event and mender bead a queue.local round's do.
pub(super) fn raise_forge_reds(w: &World, c: &Ctx, kv: &Kv, suites_csv: &str) {
    let Some(batch) = kv.get("batch_id").filter(|b| !b.is_empty()) else {
        return;
    };
    let members = kv.members();
    let reworking = members
        .iter()
        .any(|m| matches!(w.lc.bead_state(&m.id), Some((s, _)) if s == "REWORK"));
    if members.len() == 1 || reworking {
        return;
    }
    for suite in suites_csv.split(',').filter(|s| !s.is_empty()) {
        if idents(w, "verdict", &[("suite", suite)]).is_err() {
            continue;
        }
        let marker = mender_marker(
            &rounds_dir(c),
            batch,
            w.lc.batch_pass(batch).map_or(0, |p| p.0),
            suite,
        );
        if marker.exists() {
            continue;
        }
        let sent = send_failed(w, batch, kv.get("head").unwrap_or(""), suite);
        match sent {
            Ok(()) => call_mender(w, c, kv, batch, suite, "", None),
            Err((rc, e)) => w.err(format!("verdict {}: WARNING — no mender for {suite} of {batch}: the Failed event was refused (rc={rc}): {e}", c.r.name)),
        }
    }
}

const EXTRA_PREFIX: &str = "round.";

/// Every open round of the repo, oldest first. The oldest sits in `round`, which the tools
/// outside the queue read; any other is `round.<batch>`.
pub(crate) fn open_records(c: &Ctx) -> Vec<(PathBuf, Kv)> {
    let mut extras: Vec<(u64, PathBuf, Kv)> = fs::read_dir(c.queue_file(""))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(EXTRA_PREFIX))
        })
        .filter_map(|e| {
            records::read_kv(&e.path()).ok().flatten().map(|kv| {
                (
                    kv.get("opened").and_then(|v| v.parse().ok()).unwrap_or(0),
                    e.path(),
                    kv,
                )
            })
        })
        .collect();
    extras.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let primary = c.queue_file(RECORD);
    records::read_kv(&primary)
        .ok()
        .flatten()
        .map(|kv| (primary, kv))
        .into_iter()
        .chain(extras.into_iter().map(|(_, p, kv)| (p, kv)))
        .collect()
}

fn lifecycle_dead(w: &World, kv: &Kv) -> bool {
    matches!(w.lc.batch_state(kv.get("batch_id").unwrap_or("")), Some((s, _)) if matches!(s.as_str(), "ABANDONED" | "DISCARDED"))
}

/// The open rounds the lifecycle still holds; status and open both count these, so one never
/// reports no round while the other refuses on it.
fn live_records(w: &World, c: &Ctx) -> Vec<(PathBuf, Kv)> {
    open_records(c)
        .into_iter()
        .filter(|(_, kv)| !lifecycle_dead(w, kv))
        .collect()
}

pub(super) fn record_of(c: &Ctx, batch: &str) -> Option<(PathBuf, Kv)> {
    open_records(c)
        .into_iter()
        .find(|(_, kv)| kv.get("batch_id") == Some(batch))
}

pub(super) fn save_record(c: &Ctx, kv: &Kv) -> Result<(), String> {
    let batch = kv.get("batch_id").unwrap_or("");
    let path = match record_of(c, batch) {
        Some((p, _)) => p,
        None if c.queue_file(RECORD).exists() => c.queue_file(&format!("{EXTRA_PREFIX}{batch}")),
        None => c.queue_file(RECORD),
    };
    write_atomic(&path, &kv.render())
}

/// Removes the round's record; the oldest round left takes over `round`.
pub(super) fn drop_record(c: &Ctx, batch: &str) {
    if let Some((p, _)) = record_of(c, batch) {
        let _ = fs::remove_file(p);
    }
    if !c.queue_file(RECORD).exists() {
        if let Some((p, _)) = open_records(c).into_iter().next() {
            let _ = fs::rename(p, c.queue_file(RECORD));
        }
    }
}

pub(super) fn load(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    if let Some((_, kv)) = record_of(c, batch) {
        return Ok(kv);
    }
    let open: Vec<String> = open_records(c)
        .iter()
        .map(|(_, kv)| kv.get("batch_id").unwrap_or("?").to_string())
        .collect();
    match open.as_slice() {
        [] => w.err(format!("queue.sh {label}: no open round for {}", c.r.name)),
        [one] => w.err(format!(
            "queue.sh {label}: the open round for {} is {one}, not {batch}",
            c.r.name
        )),
        many => w.err(format!(
            "queue.sh {label}: the open rounds for {} are {}, not {batch}",
            c.r.name,
            many.join(" ")
        )),
    }
    Err(FAIL)
}

fn save(w: &World, label: &str, c: &Ctx, kv: &Kv) -> Result<(), i32> {
    save_record(c, kv).map_err(|e| {
        w.err(format!(
            "queue.sh {label}: cannot write the round record: {e}"
        ));
        FAIL
    })
}

pub(super) fn set(kv: &mut Kv, k: &str, v: &str) {
    kv.remove(k);
    kv.push(k, v);
}

pub(super) fn set_phase(w: &World, kv: &mut Kv, phase: &str) {
    set(kv, "phase", phase);
    set(kv, "phase_at", &w.clock.now().to_string());
}

pub(super) fn phase_of(w: &World, batch: &str) -> String {
    match w.lc.batch_state(batch) {
        Some((s, _)) => match s.as_str() {
            "OPEN" => "opened".into(),
            "CI_RUNNING" => "certifying".into(),
            other => other.to_lowercase(),
        },
        None => "?".into(),
    }
}

pub(super) fn rounds_dir(c: &Ctx) -> PathBuf {
    c.s.run.join("rounds")
}

pub(super) fn handle_of(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.pass"))
}

fn preempt_marker(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.preempt"))
}

pub(super) fn mark_running(c: &Ctx, batch: &str) {
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(format!("{batch}.running")), "");
}

pub(super) fn finish(c: &Ctx, batch: &str, line: &str) {
    use std::io::Write;
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{batch}.result")))
    {
        let _ = writeln!(f, "{line}");
    }
    let _ = fs::remove_file(dir.join(format!("{batch}.running")));
}

pub(super) fn csv(ms: &[Member]) -> String {
    ms.iter().map(Member::render).collect::<Vec<_>>().join(",")
}

fn worktree_of(kv: &Kv) -> PathBuf {
    PathBuf::from(kv.get("worktree").unwrap_or(""))
}

/// The round's tree rebuilt from `base_sha` with `members` merged in order, in `wt`. A
/// member that no longer merges leaves `wt` as it was (at `fallback`) and is the Err.
/// Merges a member and renumbers any migration whose number the round already holds; a merge
/// that leaves the numbering broken is undone and reported as not merging.
pub(super) fn merge_member(w: &World, c: &Ctx, wt: &Path, id: &str, tip: &str) -> bool {
    w.git.merge_no_ff(
        wt,
        &w.lib.land_subject(id),
        tip,
        &c.s.git_name,
        &c.s.git_email,
    ) && migration_numbers::settle(wt).is_ok()
}

pub(super) fn assemble(
    w: &World,
    c: &Ctx,
    path: &Path,
    wt: &Path,
    base_sha: &str,
    members: &[Member],
    fallback: &str,
) -> Result<String, String> {
    w.git.worktree_prune(path);
    w.git.worktree_remove(path, wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(path, wt, base_sha) {
        return Err("cannot create the round worktree".into());
    }
    for m in members {
        if !merge_member(w, c, wt, &m.id, &m.tip) {
            w.git.merge_abort(wt);
            w.git.worktree_remove(path, wt);
            let _ = w.git.worktree_add_detached(path, wt, fallback);
            return Err(format!("{} no longer merges onto the round", m.id));
        }
    }
    w.git
        .rev_parse(wt, "HEAD")
        .ok_or_else(|| "the round worktree has no HEAD".to_string())
}

/// A member that no longer merges onto the base goes back to REWORK on its own tip, so a
/// builder claims it to rebase; left as it was, every later round would skip it again. The
/// claim is made only after reading the row back in REWORK.
pub(super) fn return_to_rework(w: &World, m: &Member) -> Result<(), String> {
    let Some((state, version)) = w.lc.bead_state(&m.id) else {
        return Err("no lifecycle row — not returned to rework".into());
    };
    let kind = format!(
        "{{\"GateRed\":{{\"tip\":\"{}\",\"reason\":\"no-rebase\"}}}}",
        m.tip
    );
    if let Err((rc, e)) =
        w.lc.bead_event(&m.id, &state, version.trim(), LC_ACTOR, &kind)
    {
        return Err(format!(
            "return to rework refused rc={rc}: {}",
            one_line(&e)
        ));
    }
    match w.lc.bead_state(&m.id) {
        Some((now, _)) if now == "REWORK" => Ok(()),
        Some((now, _)) => Err(format!(
            "return to rework accepted but the bead is {now}, not REWORK"
        )),
        None => Err("return to rework accepted but the bead's row cannot be read back".into()),
    }
}

/// The gate-worker's recorded FAIL for `spira/<id>` at exactly `tip`, as the gate's own reason.
/// BASE_FAIL and NO_VERDICT are not the branch's fault and a missing verdict says nothing.
pub(super) fn gate_red_at(c: &Ctx, id: &str, tip: &str) -> Option<String> {
    let done = GateQueue::new(&c.s.run).peek_done(&c.r.name, &format!("spira/{id}"), tip)?;
    (done.run.outcome == GateOutcome::Fail).then(|| done.run.reason_or("unspecified").to_string())
}

const RED_TIPS: &str = "red-tips";

/// Whether `id` at exactly `tip` was already ejected red from a batch: only a new tip may rejoin.
pub(super) fn batch_red_at(c: &Ctx, id: &str, tip: &str) -> bool {
    let text = fs::read_to_string(c.queue_file(RED_TIPS)).unwrap_or_default();
    text.lines().any(|l| {
        l.split_once(' ').is_some_and(|(i, t)| {
            i == id && !t.is_empty() && (tip.starts_with(t) || t.starts_with(tip))
        })
    })
}

pub(super) fn record_batch_red(c: &Ctx, id: &str, tip: &str) -> std::io::Result<()> {
    let path = c.queue_file(RED_TIPS);
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&format!("{id} {tip}\n"));
    records::write_atomic(&path, &text).map_err(std::io::Error::other)
}

pub(super) fn forget_batch_red(c: &Ctx, id: &str, tip: &str) -> std::io::Result<()> {
    let path = c.queue_file(RED_TIPS);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let kept: String = text
        .lines()
        .filter(|l| {
            !l.split_once(' ').is_some_and(|(i, t)| {
                i == id && !t.is_empty() && (tip.starts_with(t) || t.starts_with(tip))
            })
        })
        .map(|l| format!("{l}\n"))
        .collect();
    records::write_atomic(&path, &kept).map_err(std::io::Error::other)
}

fn changes_recipe(w: &World, repo: &Path, base: &str, tip: &str) -> bool {
    let from = w
        .git
        .merge_base(repo, base, tip)
        .unwrap_or_else(|| base.to_string());
    let changed: Vec<String> = w
        .git
        .numstat(repo, &from, tip)
        .unwrap_or_default()
        .into_iter()
        .map(|(p, ..)| p)
        .collect();
    spira_config::recipe::touches(&changed)
}

fn tip_or_unpinned(tip: &str) -> &str {
    if tip.is_empty() {
        "unpinned"
    } else {
        tip
    }
}

fn open(
    w: &World,
    repo: Option<&str>,
    members_arg: &Text,
    name: Option<&str>,
    worktree: Option<&Path>,
) -> i32 {
    let label = "round open";
    let text = match read_text(w, members_arg) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let mut wanted: Vec<Member> = Vec::new();
    for m in parse_members(&text) {
        match wanted.iter().find(|h| h.id == m.id) {
            Some(h) if h.tip == m.tip => {}
            Some(h) => {
                w.err(format!(
                    "queue.sh {label}: {} is named at two tips ({} and {}) — a round holds one version of a bead; refused",
                    m.id,
                    tip_or_unpinned(&h.tip),
                    tip_or_unpinned(&m.tip)
                ));
                return USAGE;
            }
            None => wanted.push(m),
        }
    }
    if wanted.is_empty() {
        w.err(format!("queue.sh {label}: --members names no bead"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    reconcile_unrecorded(w, &c, &path);
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
        if idents(w, label, &[("member id", &m.id)]).is_err()
            || (!m.tip.is_empty() && idents(w, label, &[("member tip", &m.tip)]).is_err())
        {
            return FAIL;
        }
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else {
        return FAIL;
    };
    let open_now = live_records(w, &c);
    let limit = c.s.rounds_max.max(1);
    if open_now.len() >= limit {
        let held: Vec<String> = open_now
            .iter()
            .map(|(_, kv)| {
                format!(
                    "{} (phase {})",
                    kv.get("batch_id").unwrap_or("?"),
                    phase_of(w, kv.get("batch_id").unwrap_or(""))
                )
            })
            .collect();
        w.err(if limit == 1 {
            format!("queue.sh {label}: a round is already open for {repo_name} ({}) — land or abandon it first", held.join(", "))
        } else {
            format!("queue.sh {label}: {} rounds are already open for {repo_name} (limit {limit}): {} — land or abandon one first", open_now.len(), held.join(", "))
        });
        return FAIL;
    }
    for m in &wanted {
        if let Some((_, kv)) = open_now
            .iter()
            .find(|(_, kv)| kv.members().iter().any(|h| h.id == m.id))
        {
            w.err(format!("queue.sh {label}: {} is already a member of open round {} — rounds hold disjoint members; refused", m.id, kv.get("batch_id").unwrap_or("?")));
            return FAIL;
        }
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!(
            "queue.sh {label}: cannot resolve landing ref for {repo_name}"
        ));
        return FAIL;
    };
    let Some(landed_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh {label}: cannot resolve {base}"));
        return FAIL;
    };
    let parent = open_now
        .iter()
        .filter_map(|(_, kv)| {
            Some((
                kv.get("batch_id")?.to_string(),
                kv.get("head")?.to_string(),
                kv.get("opened")?.parse::<u64>().unwrap_or(0),
            ))
        })
        .max_by_key(|(_, _, at)| *at)
        .filter(|(_, head, _)| !head.is_empty());
    let base_sha = parent
        .as_ref()
        .map_or_else(|| landed_sha.clone(), |(_, head, _)| head.clone());
    if let Ok(Some(orphan)) = records::read_kv(&c.queue_file(stage::STAGED)) {
        let parent = orphan.get("parent").unwrap_or("?").to_string();
        if open_now
            .iter()
            .all(|(_, kv)| kv.get("batch_id") != Some(parent.as_str()))
        {
            stage::discard_behind(
                w,
                &c,
                &path,
                &parent,
                "round open cut a round of its own: the round it followed is gone",
            );
        }
    }

    let mut skips: Vec<String> = Vec::new();
    let mut admitted: Vec<Member> = Vec::new();
    for m in wanted {
        if let Some(by) = super::integrate::carried_by(w, &c, &m.id) {
            skips.push(format!(
                "{}: carried by integration {by} — lands with it, not admitted",
                m.id
            ));
            continue;
        }
        match w.lc.bead_row(&m.id) {
            None => skips.push(format!("{}: no lifecycle row (spira-lc could not say) — not admitted", m.id)),
            // SUBMITTED or CERTIFIED (law-a-round-is-feature-first-then-catch-all): the round's own
            // full suite is the certification, so a submitted tip need not pass a per-bead gate first.
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => {
                skips.push(format!("{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted", m.id, r.state))
            }
            Some(r) => match r.tip.clone().filter(|t| !t.is_empty()) {
                Some(tip) if r.ejected_red_tip.as_deref() == Some(tip.as_str()) => {
                    skips.push(format!("{}: {tip} is a recorded ejected-red tip — not admitted until the tip changes or `queue absolve {} --evidence-tip {tip}` withdraws a red that was not the tree's fault", m.id, m.id))
                }
                Some(tip) if tip.starts_with(&m.tip) && batch_red_at(&c, &m.id, &tip) => {
                    skips.push(format!("{}: {tip} already went red in a batch — not admitted until the tip changes", m.id))
                }
                Some(tip) if tip.starts_with(&m.tip) => match gate_red_at(&c, &m.id, &tip) {
                    Some(why) => skips.push(format!("{}: its gate is FAIL at {tip} ({why}) — not admitted", m.id)),
                    None => match r.blocked_by.first() {
                        Some(dep) => {
                            let state = w.lc.bead_row(dep).map(|d| d.state).unwrap_or_else(|| "unknown".into());
                            skips.push(format!("{}: depends on {dep} ({state}) — not admitted until it lands", m.id))
                        }
                        None => admitted.push(Member { id: m.id, tip }),
                    },
                },
                Some(tip) => skips.push(format!("{}: {} is not its row's tip {tip} — not admitted", m.id, m.tip)),
                None => skips.push(format!("{}: no submitted tip — not admitted", m.id)),
            },
        }
    }

    let open_recipe = open_now
        .iter()
        .find(|(_, kv)| {
            kv.members()
                .iter()
                .any(|h| changes_recipe(w, &path, &landed_sha, &h.tip))
        })
        .map(|(_, kv)| kv.get("batch_id").unwrap_or("?").to_string());
    let (mut recipe, ordinary): (Vec<Member>, Vec<Member>) = admitted
        .into_iter()
        .partition(|m| changes_recipe(w, &path, &landed_sha, &m.tip));
    let alone = open_recipe.is_none() && ordinary.is_empty();
    let admitted = if alone {
        recipe.drain(..1.min(recipe.len())).collect()
    } else {
        ordinary
    };
    for m in &recipe {
        skips.push(match &open_recipe {
            Some(open) => format!("{0}: solo: {0} changes the round recipe — waits for open recipe round {open}, not admitted", m.id),
            None => format!("{0}: solo: {0} changes the round recipe — rounds alone, not admitted with other members", m.id),
        });
    }

    let batch = name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{repo_name}-{}", w.clock.stamp()));
    let wt = worktree.map(Path::to_path_buf).unwrap_or_else(|| {
        let leaf = if open_now.is_empty() {
            format!(".round-{repo_name}")
        } else {
            format!(".round-{batch}")
        };
        c.s.run.join("worktree").join(leaf)
    });
    w.git.worktree_prune(&path);
    w.git.worktree_remove(&path, &wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &base_sha) {
        w.err(format!(
            "queue.sh {label}: cannot create the round worktree {}",
            wt.display()
        ));
        return FAIL;
    }
    let certified: BTreeMap<String, String> = admitted
        .iter()
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect();
    let mut merged: Vec<Member> = Vec::new();
    let mut unreturned = false;
    let mut collided: Vec<(Member, String)> = Vec::new();
    for m in admitted {
        if !super::simple::lint_passed(w, label, &path, &m.tip, false) {
            skips.push(format!(
                "{}: {} fails its compile-and-lint check — not admitted",
                m.id, m.tip
            ));
            continue;
        }
        if merge_member(w, &c, &wt, &m.id, &m.tip) {
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            if w.lib.base_conflict(&path, &landed_sha, &m.tip) {
                skips.push(match return_to_rework(w, &m) {
                    Ok(()) => format!("{}: conflicts with base — returned to rework", m.id),
                    Err(e) => {
                        unreturned = true;
                        format!("{}: conflicts with base ({e})", m.id)
                    }
                });
            } else {
                let onto = w
                    .git
                    .rev_parse(&wt, "HEAD")
                    .unwrap_or_else(|| base_sha.clone());
                let scratch = wt.with_file_name(format!(
                    "{}.rebase",
                    wt.file_name().and_then(|n| n.to_str()).unwrap_or("round")
                ));
                let upstream = w
                    .git
                    .merge_base(&path, &base_sha, &m.tip)
                    .unwrap_or_else(|| base_sha.clone());
                match w.git.rebase_onto(
                    &path,
                    &scratch,
                    &m.tip,
                    &upstream,
                    &onto,
                    &c.s.git_name,
                    &c.s.git_email,
                ) {
                    Ok(rebased) if merge_member(w, &c, &wt, &m.id, &rebased) => {
                        merged.push(Member {
                            id: m.id,
                            tip: rebased,
                        })
                    }
                    Ok(_) => {
                        w.git.merge_abort(&wt);
                        let why = format!("{}: conflicts with the round, and its rebase onto the round's head does not merge", m.id);
                        collided.push((m, why.clone()));
                        skips.push(why);
                    }
                    Err(paths) => {
                        let why = format!(
                            "{}: conflicts with the round, and with a rebase onto its head — paths: {}",
                            m.id,
                            if paths.is_empty() { "unknown".to_string() } else { paths.join(" ") }
                        );
                        collided.push((m, why.clone()));
                        skips.push(why)
                    }
                }
            }
        }
    }
    for s in &skips {
        w.out(format!("queue.sh {label}: skip — {s}"));
    }
    if let Some((blocked, _)) = collided.first() {
        let mut involved = super::integrate::colliding(w, &path, &base_sha, blocked, &merged);
        involved.push(blocked.clone());
        let source = format!(
            "Round open could not merge {} with the round that holds the others.",
            blocked.id
        );
        let evidence = collided
            .iter()
            .map(|(_, y)| y.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if super::integrate::file(
            w, label, &c, &path, &base_sha, &involved, &source, &evidence,
        )
        .is_some()
        {
            merged.retain(|m| !involved.iter().any(|i| i.id == m.id));
            if !merged.is_empty() {
                match assemble(w, &c, &path, &wt, &base_sha, &merged, &base_sha) {
                    Ok(_) => {}
                    Err(e) => {
                        w.err(format!("queue.sh {label}: cannot reassemble the round without the integrated members: {e}"));
                        w.git.worktree_remove(&path, &wt);
                        return FAIL;
                    }
                }
            }
        }
    }
    if unreturned {
        w.err(format!("queue.sh {label}: a member that conflicts with base could not be returned to rework — no round opened for {repo_name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    if merged.is_empty() {
        w.err(format!(
            "queue.sh {label}: no round — nothing admissible for {repo_name}"
        ));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let mut head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let cut = loop {
        let cut_members: Vec<Member> = merged
            .iter()
            .map(|m| Member {
                id: m.id.clone(),
                tip: certified
                    .get(&m.id)
                    .cloned()
                    .unwrap_or_else(|| m.tip.clone()),
            })
            .collect();
        let Err((rc, out)) = super::batch::cut_settled(
            w,
            &batch,
            &repo_name,
            &head,
            &base_sha,
            &csv(&cut_members),
            LC_ACTOR,
        ) else {
            break Ok(());
        };
        let refused = merged
            .iter()
            .position(|m| out.contains(&format!("Deliver refused unexpectedly for {}:", m.id)));
        let Some(i) = refused.filter(|_| merged.len() > 1) else {
            break Err((rc, out));
        };
        let dropped = merged.remove(i);
        let line = format!(
            "{}: spira-lc refused it at the cut — dropped from the round: {}",
            dropped.id,
            one_line(&out)
        );
        w.out(format!("queue.sh {label}: skip — {line}"));
        match assemble(w, &c, &path, &wt, &base_sha, &merged, &base_sha) {
            Ok(h) => head = h,
            Err(e) => {
                break Err((
                    rc,
                    format!("{out}; reassembling without {}: {e}", dropped.id),
                ))
            }
        }
    };
    if let Err((rc, out)) = cut {
        w.err(format!(
            "queue.sh {label}: spira-lc cut refused for {batch} (rc={rc}): {out}"
        ));
        converge_orphans(w);
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }

    let now = w.clock.now().to_string();
    let mut kv = Kv::default();
    kv.push("batch_id", &batch);
    kv.push("repo", &repo_name);
    kv.push("head", &head);
    kv.push("base", &base_sha);
    if let Some((p, _, _)) = &parent {
        kv.push("parent", p);
    }
    kv.push("members", &render_members(&merged));
    kv.push("worktree", &wt.display().to_string());
    kv.push("actor", &actor(w));
    kv.push("opened", &now);
    if save(w, label, &c, &kv).is_err() {
        let why = "round open could not record the round it cut";
        if let Err((rc, out)) = lc_cas(w, &batch, |s, v| {
            w.lc.abandon_batch(&batch, s, v, LC_ACTOR, why)
        }) {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for the unrecorded round {batch} (rc={rc}): {out} — its members stay BATCHED until requeue-orphans"));
        }
        converge_orphans(w);
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    mark_running(&c, &batch);
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-OPEN {now} repo={repo_name} batch={batch} head={head} members={}",
            csv(&merged)
        ),
    );
    w.out(format!("batch={batch}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    OK
}

fn status(w: &World, repo: Option<&str>) -> i32 {
    let label = "round status";
    let Ok(c) = resolve(w, label, repo) else {
        return FAIL;
    };
    let open = live_records(w, &c);
    let Some((_, kv)) = open.first() else {
        w.out(format!("round=none\nrepo={}", c.r.name));
        return OK;
    };
    let since = |kv: &Kv, k: &str| {
        kv.get(k)
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |t| w.clock.now().saturating_sub(t))
    };
    let batches: Vec<&str> = open
        .iter()
        .map(|(_, kv)| kv.get("batch_id").unwrap_or(""))
        .collect();
    w.out(format!(
        "rounds={}\nopen_batches={}",
        open.len(),
        batches.join(" ")
    ));
    w.out("round=open");
    w.out(format!(
        "phase={}",
        phase_of(w, kv.get("batch_id").unwrap_or(""))
    ));
    for k in ["batch_id", "repo", "head", "base", "members"] {
        w.out(format!("{k}={}", kv.get(k).unwrap_or("")));
    }
    if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
        w.out(format!("red={red}"));
    }
    w.out(format!("wall_secs={}", since(kv, "opened")));
    for (i, (_, kv)) in open.iter().enumerate().skip(1) {
        let n = i + 1;
        w.out(format!(
            "round{n}=open\nround{n}_phase={}",
            phase_of(w, kv.get("batch_id").unwrap_or(""))
        ));
        for k in ["batch_id", "repo", "head", "base", "members"] {
            w.out(format!("round{n}_{k}={}", kv.get(k).unwrap_or("")));
        }
        if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
            w.out(format!("round{n}_red={red}"));
        }
        w.out(format!("round{n}_wall_secs={}", since(kv, "opened")));
    }
    if let Ok(Some(st)) = records::read_kv(&c.queue_file(stage::STAGED)) {
        for (k, v) in [
            ("staged_batch", "batch_id"),
            ("staged_phase", "phase"),
            ("staged_head", "head"),
            ("staged_members", "members"),
        ] {
            w.out(format!("{k}={}", st.get(v).unwrap_or("")));
        }
    }
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
        } else if let Some(suite) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".result"))
        {
            let status = fs::read_to_string(&p)
                .ok()
                .and_then(|t| t.split_whitespace().next().map(String::from))
                .unwrap_or_default();
            found.push((suite.to_string(), status));
        }
    }
}

/// Names the suites whose verdict cannot be read: a word that is neither passing nor blocking
/// (an empty file included), or a corpus suite with no result at all. A red lint or fence ends the
/// pass before any suite runs, so it is the verdict and the missing suites are not a fault.
pub(crate) fn unjudgeable(wt: &Path, found: &[(String, String)]) -> Option<String> {
    let mut bad: Vec<String> = found
        .iter()
        .filter(|(_, s)| !PASSING.contains(&s.as_str()) && !BLOCKING.contains(&s.as_str()))
        .map(|(n, s)| format!("{n} (verdict {s:?})"))
        .collect();
    let ended_early = found.iter().any(|(n, s)| s == "red" && (n == LINT_RED || n.starts_with(FENCE_RED_PREFIX)));
    if let (Ok(rd), false) = (fs::read_dir(wt.join("spira")), ended_early) {
        for name in rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
        {
            if name.starts_with("test-")
                && name.ends_with(".sh")
                && !found.iter().any(|(n, _)| *n == name)
            {
                bad.push(format!("{name} (no result)"));
            }
        }
    }
    bad.sort();
    if bad.is_empty() {
        None
    } else {
        Some(format!(
            "round-vm left no readable verdict for: {}",
            bad.join(", ")
        ))
    }
}

/// The wall bounds the tests, not the landing: a run the wall killed after every suite reported
/// and the unit tests passed is judged on its results.
fn wall_complete(wt: &Path, found: &[(String, String)], err: &str) -> bool {
    !found.is_empty() && unjudgeable(wt, found).is_none() && err.contains("UNIT-TESTS: PASS")
}

fn over_cap_why(err: &str, cap: u64) -> String {
    let line = err
        .lines()
        .find_map(|l| l.split("OVER-CAP:").nth(1))
        .unwrap_or("")
        .trim();
    if line.is_empty() {
        format!("the suites and bare lane exceeded the {cap}s cap")
    } else {
        format!("INCOMPLETE, cut off by {line}")
    }
}

fn result_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(rd) = fs::read_dir(dir) {
        let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        dirs.extend(subs);
    }
    dirs
}

/// A pass cut off by a cap holds no verdict: testenv's own suite cap ends the batch
/// `verdict FAULT over-cap`, whatever exit round-vm reports, and round-vm's guard exits 124/137.
fn batch_over_cap(results: &Path, rc: i32, err: &str) -> bool {
    let verdict = result_dirs(results)
        .iter()
        .find_map(|d| fs::read_to_string(d.join("run.events")).ok())
        .and_then(|t| {
            t.lines()
                .filter_map(|l| l.split_once(' ').and_then(|(_, r)| r.strip_prefix("verdict ")))
                .next_back()
                .map(|v| v.trim().to_string())
        });
    verdict.as_deref() == Some("FAULT over-cap") || (matches!(rc, 124 | 137) && err.contains("OVER-CAP"))
}

fn suite_cap_s(results: &Path) -> Option<String> {
    result_dirs(results).iter().find_map(|d| {
        let t = fs::read_to_string(d.join("runner.meta")).ok()?;
        t.lines().find_map(|l| l.strip_prefix("suite_cap_s=").map(|v| v.trim().to_string()))
    })
}

/// The first compiler error in a failed build's output, with the `file:line` that follows it.
fn first_build_error(err: &str) -> Option<String> {
    let mut lines = err.lines();
    let head = lines.find(|l| {
        l.starts_with("error")
            && !l.starts_with("error: could not compile")
            && !l.starts_with("error: aborting")
    })?;
    let at = lines
        .next()
        .map(str::trim)
        .filter(|l| l.starts_with("-->"))
        .map(|l| l.trim_start_matches("-->").trim());
    Some(match at {
        Some(a) => format!("{head} at {a}"),
        None => head.to_string(),
    })
}

/// `error[E0425]: message at file:line` as `error[E0425] at file:line`.
fn reason_form(e: &str) -> String {
    let (head, at) = e.split_once(" at ").unwrap_or((e, ""));
    let code = head.split(':').next().unwrap_or(head);
    if at.is_empty() {
        code.to_string()
    } else {
        format!("{code} at {at}")
    }
}

/// Every rustc `error` diagnostic in `err`, whole: from its `error...` line to the blank line
/// that ends it, so no block is cut to a head or a tail.
fn error_blocks(err: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in err.lines() {
        let starts = l.starts_with("error")
            && !l.starts_with("error: could not compile")
            && !l.starts_with("error: aborting");
        if starts {
            inside = true;
        } else if l.trim().is_empty() || l.starts_with("warning") || l.starts_with("round-vm:") {
            inside = false;
        }
        if inside {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// A workspace build that failed with no compiler diagnostic and a known machine fault in its
/// output is the environment's failure, not the members'.
pub(crate) fn build_environment_cause(err: &str) -> Option<&'static str> {
    if err.contains("error[") || err.lines().any(|l| l.trim_start().starts_with("-->")) {
        return None;
    }
    const MARKERS: [(&str, &str); 9] = [
        ("No space left on device", "out of disk"),
        ("out of memory", "out of memory"),
        ("signal: 9", "killed (out of memory)"),
        ("Killed", "killed (out of memory)"),
        ("sccache is missing", "sccache missing from the image"),
        ("no cache home", "image records no cache home"),
        ("can't find crate for `std`", "toolchain component missing"),
        ("is not installed", "toolchain or component not installed"),
        ("command not found", "tool missing from the image"),
    ];
    MARKERS
        .iter()
        .find(|(m, _)| err.contains(m))
        .map(|(_, w)| *w)
}

/// What a failed workspace build leaves to read: the invocation, how it ended, how long it took,
/// and every compiler error whole (the output's tail when it carried none).
fn build_log(err: &str, rc: i32, wall: u64) -> String {
    let blocks = error_blocks(err);
    let body = if blocks.is_empty() {
        tail(err, 200)
    } else {
        blocks
    };
    format!(
        "command: cargo build -q --profile release --workspace --config profile.release.incremental=false\nexit: {rc}\nwall_secs: {wall}\n\n{body}\n"
    )
}

/// The marker the loop's pacing reads: a stale round image is a wait, not a pool fault.
pub const STALE_AMI_MARK: &str = "round AMI is stale";

fn stale_ami_cause(out: &RunOut) -> Option<String> {
    if out.rc == 0 || out.rc == 1 {
        return None;
    }
    let lines: Vec<&str> = out.err.lines().map(str::trim).collect();
    let from = lines.iter().rposition(|l| l.contains("acquire failed"))?;
    let cause = lines[from..].join(" ");
    cause
        .contains("round-ami-build")
        .then(|| bounded_text(&cause))
}

/// A round whose image recipe is stale waits for the Round AMI build for the current recipe,
/// triggering one when none is running; the wait is bounded by `round_ami_wait_secs`.
#[derive(Default)]
struct AmiWait {
    began: Option<u64>,
    triggered: bool,
    idle_polls: u32,
}

impl AmiWait {
    const POLL_SECS: u64 = 60;
    const IDLE_POLLS: u32 = 3;

    fn step(&mut self, w: &World, label: &str, c: &Ctx) -> bool {
        let now = w.clock.now();
        let began = *self.began.get_or_insert(now);
        if now.saturating_sub(began) >= c.s.round_ami_wait_secs {
            return false;
        }
        let Ok(home) = resolve(w, label, Some(&c.s.home_repo)) else {
            w.out(format!(
                "queue.sh {label}: not waiting for a round AMI: cannot resolve {}",
                c.s.home_repo
            ));
            return false;
        };
        let Some(home_path) = home.r.path.clone() else {
            w.out(format!(
                "queue.sh {label}: not waiting for a round AMI: {} has no path",
                c.s.home_repo
            ));
            return false;
        };
        let Some(running) = w.forge.ami_runs(&c.s.forge, &home_path) else {
            w.out(format!("queue.sh {label}: not waiting for a round AMI: the forge cannot be read for its runs"));
            return false;
        };
        if let Some(run) = running.first() {
            self.idle_polls = 0;
            w.out(format!("queue.sh {label}: waiting for round AMI {run}"));
        } else if !self.triggered {
            let Some(land) = home.r.landref.as_deref() else {
                return false;
            };
            if !w
                .forge
                .ami_dispatch(&c.s.forge, &home_path, ref_branch(land))
            {
                w.out(format!(
                    "queue.sh {label}: the forge refused the Round AMI build on {land}"
                ));
                return false;
            }
            self.triggered = true;
            w.out(format!(
                "queue.sh {label}: no round AMI build was running; triggered one"
            ));
        } else {
            self.idle_polls += 1;
            if self.idle_polls >= Self::IDLE_POLLS {
                return false;
            }
        }
        w.clock.sleep(Self::POLL_SECS); // waits-on: the round AMI forge run; cap round_ami_wait_secs
        true
    }
}

/// What the round's own recipe asks of the round image.
enum Image {
    /// The recipe is the release's: the image the pool already selects.
    Release,
    /// The recipe differs and an image is built from it.
    Candidate(String),
    /// The candidate's image build failed: a red of the build, never a pool fault.
    Failed(String),
    /// The wait for the build could not finish; the round is not judged.
    Unjudged(String),
}

const IMAGE_UNJUDGED: i32 = 3;

fn candidate_branch(batch: &str, sha: &str) -> String {
    format!(
        "spira-round-candidate/{batch}-{}",
        &sha[..sha.len().min(12)]
    )
}

/// A round whose head changes the recipe certifies on an image built from that head: the head
/// is pushed under a per-candidate branch, the Round AMI workflow is dispatched on it, and the
/// pass waits up to `round_ami_wait_secs` for the image. A recipe already imaged is not rebuilt.
fn candidate_image(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    wt: &Path,
    head: &str,
    batch: &str,
) -> Image {
    if !matches!(c.s.round_spill.as_str(), "ec2" | "ec2-always" | "ec2-only") {
        return Image::Release;
    }
    let (Some(tree), Some(release)) = (w.scripts.recipe_sha(Some(wt)), w.scripts.recipe_sha(None))
    else {
        return Image::Release;
    };
    if tree == release {
        return Image::Release;
    }
    match w.scripts.ami_for(&tree) {
        AmiLookup::Found(id) => return Image::Candidate(id),
        AmiLookup::Unreadable(e) => {
            return Image::Unjudged(format!("cannot read the round images: {e}"))
        }
        AmiLookup::Missing => {}
    }
    let Ok(home) = resolve(w, label, Some(&c.s.home_repo)) else {
        return Image::Unjudged(
            "cannot resolve the repository the Round AMI workflow runs in".into(),
        );
    };
    let (Some(home_path), Some((remote, _))) = (home.r.path.clone(), c.r.publish.clone()) else {
        return Image::Unjudged(
            "the repository has no publish remote to push the candidate image build to".into(),
        );
    };
    let branch = candidate_branch(batch, &tree);
    let began = w.clock.now();
    let mut dispatched = false;
    let mut idle = 0u32;
    let verdict = loop {
        match w.forge.ami_result(&c.s.forge, &home_path, &branch) {
            None => {
                break Image::Unjudged(
                    "the forge cannot be read for the candidate round image build".into(),
                )
            }
            Some(AmiBuild::Failure(run, steps)) => {
                let at = steps
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "<unknown step>".into());
                break Image::Failed(format!("round image build {run} failed at {at}"));
            }
            Some(AmiBuild::Success(run)) => match w.scripts.ami_for(&tree) {
                AmiLookup::Found(id) => break Image::Candidate(id),
                AmiLookup::Missing => {
                    break Image::Unjudged(format!(
                        "round image build {run} succeeded but no image carries recipe {tree}"
                    ))
                }
                AmiLookup::Unreadable(e) => {
                    break Image::Unjudged(format!("cannot read the round images: {e}"))
                }
            },
            Some(AmiBuild::Running(run)) => {
                idle = 0;
                w.out(format!(
                    "queue.sh {label}: waiting for the candidate round image build {run}"
                ));
            }
            Some(AmiBuild::NotStarted) if !dispatched => {
                if !w.lib.push(
                    path,
                    &remote,
                    &format!("{head}:refs/heads/{branch}"),
                ) {
                    break Image::Unjudged(format!("could not push {branch}"));
                }
                if !w.forge.ami_dispatch(&c.s.forge, &home_path, &branch) {
                    break Image::Unjudged(format!(
                        "the forge refused the Round AMI build on {branch}"
                    ));
                }
                dispatched = true;
                w.out(format!("queue.sh {label}: recipe {tree} has no image; dispatched the Round AMI build on {branch}"));
            }
            Some(AmiBuild::NotStarted) => {
                idle += 1;
                if idle >= AmiWait::IDLE_POLLS {
                    break Image::Unjudged(format!(
                        "the Round AMI build dispatched on {branch} never started"
                    ));
                }
            }
        }
        if w.clock.now().saturating_sub(began) >= c.s.round_ami_wait_secs {
            break Image::Unjudged(format!(
                "the candidate round image build did not finish within {}s",
                c.s.round_ami_wait_secs
            ));
        }
        w.clock.sleep(AmiWait::POLL_SECS); // waits-on: the Round AMI run on the candidate branch; cap round_ami_wait_secs
    };
    if !matches!(verdict, Image::Unjudged(_)) {
        let _ = w.lib.push(
            path,
            &remote,
            &format!(":refs/heads/{branch}"),
        );
    }
    verdict
}

const FIXTURE_FAULT_TAIL: usize = 8;

/// The fixture step's own failure, as testenv recorded it in the pulled results.
fn fixture_fault_cause(results: &Path) -> Option<String> {
    let text = fs::read_to_string(results.join("fixture-fault.out")).ok()?;
    let head = text.lines().next()?.strip_prefix("FIXTURE-FAULT: ")?;
    let body: Vec<&str> = text
        .lines()
        .skip(2)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let tail = body[body.len().saturating_sub(FIXTURE_FAULT_TAIL)..].join(" | ");
    Some(if tail.is_empty() {
        head.to_string()
    } else {
        format!("{head} [{tail}]")
    })
}

fn vm_fault_cause(err: &str) -> Option<String> {
    let line = err.lines().rev().find(|l| {
        let l = l.trim_start();
        l.starts_with("round-vm: IMAGE-BUILD-FAILED") || l.starts_with("round-vm: cannot")
    })?;
    Some(line.trim().to_string())
}

const EXCERPT_CAP: usize = 300;
const SHELL_CONTEXT: usize = 2;
pub(crate) const NO_FAILURE_FOUND: &str =
    "(no failure lines matched; the last 80 lines of the output follow)";

/// The failure itself, not the output's tail: cargo's `---- test stdout ----` blocks, failures
/// list, `test result: FAILED` and `error: N target(s) failed`; or a shell suite's FAIL / not ok
/// lines with their context. The tail, announced as such, only when nothing matches.
pub(crate) fn failure_excerpt(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut inside = false;
    let mut running: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("Running ") || t.starts_with("Doc-tests ") {
            running = Some(i);
        }
        let block = t.starts_with("---- ") && t.trim_end().ends_with(" ----");
        if block || (t == "failures:" && !inside) {
            if !inside {
                if let Some(r) = running.take() {
                    keep[r] = true;
                }
            }
            inside = true;
        }
        if t.starts_with("test result:") {
            if t.contains("FAILED") {
                keep[i] = true;
            }
            inside = false;
        } else if (t.starts_with("error: ") && t.contains("failed") && !t.starts_with("error: could not compile"))
            || (t.starts_with("error: could not compile") && t.contains("test"))
        {
            keep[i] = true;
        }
        if inside {
            keep[i] = true;
        }
    }
    let cargo = keep.iter().any(|k| *k);
    if !cargo {
        for (i, l) in lines.iter().enumerate() {
            if l.contains("FAIL") || l.trim_start().starts_with("not ok") {
                for k in keep
                    .iter_mut()
                    .take(lines.len().min(i + 1 + SHELL_CONTEXT))
                    .skip(i)
                {
                    *k = true;
                }
            }
        }
    }
    let picked: Vec<&str> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| *l)
        .collect();
    if picked.is_empty() {
        return format!("{NO_FAILURE_FOUND}\n{}", tail(text, 80));
    }
    if picked.len() > EXCERPT_CAP {
        return format!(
            "(failure excerpt cut to its last {EXCERPT_CAP} lines)\n{}",
            picked[picked.len() - EXCERPT_CAP..].join("\n")
        );
    }
    picked.join("\n")
}

/// Consecutive passes ending without a verdict, with a member in common, that make the member the
/// cause.
const FAULTED_PASSES_RED: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PassEnd {
    Faulted,
    Judged,
    Ejected,
}

impl PassEnd {
    fn word(self) -> &'static str {
        match self {
            PassEnd::Faulted => "fault",
            PassEnd::Judged => "judged",
            PassEnd::Ejected => "ejected",
        }
    }
}

fn ledger_of(c: &Ctx) -> PathBuf {
    rounds_dir(c).join(format!("pass-ledger-{}", c.r.name))
}

fn note_pass_end(c: &Ctx, ids: &[String], end: PassEnd) {
    use std::io::Write;
    if ids.is_empty() {
        return;
    }
    let _ = fs::create_dir_all(rounds_dir(c));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(ledger_of(c)) {
        let _ = writeln!(f, "{} {}", end.word(), ids.join(","));
    }
}

/// Passes in a row, newest first, that ended without a verdict with `id` aboard. A pass that
/// reached a verdict without `id` proves the machinery works and is skipped; a faulted pass
/// without it makes the fault the host's, so the run stops there.
fn faulted_run(c: &Ctx, id: &str) -> usize {
    let text = fs::read_to_string(ledger_of(c)).unwrap_or_default();
    let mut n = 0;
    for line in text.lines().rev() {
        let (end, ids) = line.split_once(' ').unwrap_or((line, ""));
        let aboard = ids.split(',').any(|m| m == id);
        match (aboard, end) {
            (true, "fault") => n += 1,
            (true, _) => break,
            (false, "fault") => break,
            (false, _) => {}
        }
    }
    n
}

fn pass_members(c: &Ctx, batch: &str) -> Vec<String> {
    record_of(c, batch).map(|(_, kv)| kv.members().into_iter().map(|m| m.id).collect()).unwrap_or_default()
}

fn lost_vm_pass(w: &World, label: &str, batch: &str, err: &str) -> i32 {
    let why = "vm-lost: the round VM died under the pass";
    warn_unrecorded(w, label, batch, "the incomplete pass", pass_verdict_event(w, batch, &Verdict::Incomplete(why), false));
    w.err(format!("queue.sh {label}: certify rc={VM_LOST}: INCOMPLETE reason={why}; the round is not judged\n{}", tail(err, 20)));
    VM_LOST
}

/// The pass of `batch` ended without a verdict: remember it, and eject each member that has now
/// been aboard for [`FAULTED_PASSES_RED`] such passes. When every member of a multi-member pass
/// qualifies, nothing separates them from the host, so none is ejected. Returns [`FAULT`] either way.
fn faulted_pass(w: &World, label: &str, c: &Ctx, batch: &str, why: &str) -> i32 {
    let ids = pass_members(c, batch);
    note_pass_end(c, &ids, PassEnd::Faulted);
    let suspects: Vec<(&String, usize)> =
        ids.iter().map(|id| (id, faulted_run(c, id))).filter(|(_, n)| *n >= FAULTED_PASSES_RED).collect();
    if suspects.len() == ids.len() && ids.len() > 1 {
        return FAULT;
    }
    for (id, n) in suspects {
        let reason = format!(
            "{id} was aboard {n} consecutive passes of {} that ended without a verdict, and no pass without it faulted: the member is the cause, not the infrastructure.\nThe latest pass: {why}",
            c.r.name
        );
        let rc = eject(w, batch, id, Some(&c.r.name), &Text::Arg(reason), UNIT_RED, true, false, true, "");
        if rc == OK {
            note_pass_end(c, std::slice::from_ref(id), PassEnd::Ejected);
        } else {
            w.err(format!("queue.sh {label}: could not eject {id}, {n} faulted passes running (rc={rc})"));
        }
    }
    FAULT
}

pub(super) fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub(super) fn certify(
    w: &World,
    batch: &str,
    repo: Option<&str>,
    attest: Option<&str>,
    attested_ran: Option<usize>,
    rehead: Option<&str>,
) -> i32 {
    let label = "round certify";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let _certify = match crate::lock::try_certify_lock(&c.s.queue_dir, &c.r.name, batch) {
        crate::lock::Acquire::Held(g) => g,
        crate::lock::Acquire::Busy => {
            let held = crate::lock::certify_holder(&c.s.queue_dir, &c.r.name, batch);
            w.err(format!(
                "queue.sh {label}: round {batch} is already being certified ({held}) — refused"
            ));
            return FAIL;
        }
        crate::lock::Acquire::Unopenable => {
            w.err(format!(
                "queue.sh {label}: cannot open the certify lock of round {batch} — refused"
            ));
            return FAIL;
        }
    };
    let mut over = None;
    let mut rc = certify_pass(w, label, &c, &path, batch, attest, attested_ran, rehead, &mut over);
    while let Some(unreached) = over.take() {
        let Ok(_g) = lock(w, label, &c) else {
            return FAIL;
        };
        let Ok(mut kv) = load(w, label, &c, batch) else {
            return FAIL;
        };
        let head = kv.get("head").unwrap_or("").to_string();
        let first = kv
            .get("over-cap")
            .and_then(|v| v.split_once(':'))
            .filter(|(h, _)| *h == head)
            .and_then(|(_, n)| n.parse::<usize>().ok());
        let Some(first) = first else {
            set(&mut kv, "over-cap", &format!("{head}:{unreached}"));
            if save(w, label, &c, &kv).is_err() {
                return FAIL;
            }
            w.err(format!("queue.sh {label}: FAULT over-cap round={batch} head={head} unreached={unreached} — no member is implicated; re-certifying once"));
            w.out(format!("fault=over-cap unreached={unreached}"));
            drop(_g);
            rc = certify_pass(w, label, &c, &path, batch, None, attested_ran, None, &mut over);
            continue;
        };
        w.err(format!("queue.sh {label}: CAPACITY FAULT over-cap twice round={batch} head={head} unreached={first},{unreached} — no member is implicated, nobody is ejected; the round stays open"));
        w.out(format!("fault=capacity unreached={first},{unreached}"));
        if kv.get("capacity-paged") != Some(head.as_str()) {
            let pass = w.lc.batch_pass(batch).map_or(0, |(n, _)| n);
            let results = rounds_dir(&c).join(format!("{batch}.results"));
            w.lib.notify(
                &c.s.mailbox,
                &c.r.name,
                &format!("Round {batch} over-cap twice on {head}"),
                &format!(
                    "Round {batch} of {} hit testenv's suite cap on both passes of {head}: unreached {first} then {unreached}, suite_cap_s={}, passes {} and {pass}. No member is implicated and nobody was ejected; the round stays open. Capacity or the cap is the Concierge's to tune. Results: {}",
                    c.r.name,
                    suite_cap_s(&results).unwrap_or_else(|| "?".into()),
                    pass.saturating_sub(1),
                    results.display()
                ),
            );
            set(&mut kv, "capacity-paged", &head);
            let _ = save(w, label, &c, &kv);
        }
        return FAULT;
    }
    rc
}

#[allow(clippy::too_many_arguments)]
fn certify_pass(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    attest: Option<&str>,
    attested_ran: Option<usize>,
    rehead: Option<&str>,
    over: &mut Option<usize>,
) -> i32 {
    let who = actor(w);
    if attest.is_some() && attested_ran.unwrap_or(0) == 0 {
        w.err(format!("queue.sh {label}: --attest needs --ran <n> with n > 0 — nothing ran; a vacuous pass is not a certificate"));
        return FAIL;
    }

    let (head, wt, run_id) = {
        let Ok(_g) = lock(w, label, c) else {
            return FAIL;
        };
        let Ok(mut kv) = load(w, label, c, batch) else {
            return FAIL;
        };
        let wt = worktree_of(&kv);
        let mut head = kv.get("head").unwrap_or("").to_string();
        if let Some(a) = attest {
            let at = w.git.rev_parse(path, a);
            let at_wt = w.git.rev_parse(&wt, "HEAD");
            if at.is_none() || at != at_wt {
                w.err(format!("queue.sh {label}: --attest {a} is not the round worktree's head ({}) — refused", at_wt.unwrap_or_else(|| "<none>".into())));
                return FAIL;
            }
            head = at.unwrap_or_default();
            set(&mut kv, "head", &head);
        } else if let Some(r) = rehead {
            let at = w.git.rev_parse(path, r);
            let at_wt = w.git.rev_parse(&wt, "HEAD");
            if at.is_none() || at != at_wt {
                w.err(format!("queue.sh {label}: --rehead {r} is not the round worktree's head ({}) — refused", at_wt.unwrap_or_else(|| "<none>".into())));
                return FAIL;
            }
            head = at.unwrap_or_default();
            set(&mut kv, "head", &head);
        } else {
            match carry_mends(w, label, c, path, &wt, batch, &mut kv) {
                Ok(Some(h)) => head = h,
                Ok(None) => {}
                Err(()) => return FAIL,
            }
            if w.git.rev_parse(&wt, "HEAD").as_deref() != Some(head.as_str()) {
                w.err(format!("queue.sh {label}: the round worktree {} is not at the round's head {head} — refused", wt.display()));
                return FAIL;
            }
        }
        // A round the lifecycle already holds GREEN was certified; what is missing (the
        // certificate, after a lost reply) is completed without running the pass again: a
        // verified result is never redone (law-a-lost-verified-result-is-data-loss; r-auto-22).
        if phase_of(w, batch) == "green" {
            if save(w, label, c, &kv).is_err() {
                return FAIL;
            }
            let ran = kv.get("ran").and_then(|r| r.parse().ok()).unwrap_or(0);
            return finish_green(
                w,
                label,
                c,
                path,
                batch,
                &head,
                &who,
                "re-certify of a GREEN round",
                (ran, 0),
            );
        }
        let run_id = format!("{batch}-{}", w.clock.now());
        let live = fs::read_to_string(handle_of(c, batch))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .is_some_and(|p| w.scripts.pass_alive(p));
        if phase_of(w, batch) == "certifying" && !live {
            let left = Verdict::Incomplete("the pass was left open: no run holds it");
            warn_unrecorded(
                w,
                label,
                batch,
                "the abandoned pass",
                pass_verdict_event(w, batch, &left, true),
            );
        }
        let started = start_pass(w, batch, &head, false);
        if let Err((rc, out)) = started {
            w.err(format!(
                "queue.sh {label}: spira-lc refused to start the round {batch} (rc={rc}): {out}"
            ));
            return FAIL;
        }
        let _ = fs::remove_file(preempt_marker(c, batch));
        kv.remove("red");
        if save(w, label, c, &kv).is_err() {
            return FAIL;
        }
        mark_running(c, batch);
        (head, wt, run_id)
    };

    let mut reds: Vec<String> = Vec::new();
    let mut ran = attested_ran.unwrap_or(0);
    let mut timings = Timings {
        suites_s: 0,
        build_s: 0,
    };
    if attest.is_none() && gate::own::declares_own_testenv(&wt) {
        let results = rounds_dir(c).join(format!("{batch}.results"));
        let _ = fs::remove_dir_all(&results);
        let _ = fs::create_dir_all(&results);
        let Some(entry) = gate::own::ci_entry(&wt) else {
            let why = format!("{} declares no ci_entry — a round runs the repository's own CI entry point on a round VM", gate::own::DECL);
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            w.err(format!("queue.sh {label}: {why}; the round is not judged"));
            return faulted_pass(w, label, c, batch, &why);
        };
        let began = w.clock.now();
        let out = w.scripts.repo_round_vm(
            &wt,
            &results,
            &entry,
            (batch, &c.r.name),
            c.s.round_wall_secs,
            &handle_of(c, batch),
        );
        let _ = fs::remove_file(handle_of(c, batch));
        if preempt_marker(c, batch).exists() {
            w.out(format!("queue.sh {label}: round {batch} was preempted; its pass is recorded by `round preempt`"));
            return PREEMPTED;
        }
        let wall = w.clock.now().saturating_sub(began);
        ran = 1;
        timings = Timings {
            suites_s: wall,
            build_s: 0,
        };
        if out.rc == 1 {
            let _ = fs::write(
                results.join(format!("{REPO_TESTENV_RED}.out")),
                format!("{}{}", out.out, out.err),
            );
            reds.push(REPO_TESTENV_RED.into());
        } else if out.rc == VM_LOST {
            return lost_vm_pass(w, label, batch, &out.err);
        } else if out.rc != 0 {
            let why = if matches!(out.rc, 124 | 137) {
                format!(
                    "testenv hung past its guard (elapsed={wall}s cap {}s)",
                    c.s.round_wall_secs
                )
            } else {
                format!("the repository's CI entry exited {}", out.rc)
            };
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            let shown = tail(&format!("{}{}", out.out, out.err), 40);
            w.err(format!("queue.sh {label}: {why}; the round is not judged\n{shown}"));
            return faulted_pass(w, label, c, batch, &format!("{why}\n{shown}"));
        }
    } else if attest.is_none() {
        let results = rounds_dir(c).join(format!("{batch}.results"));
        let _ = fs::remove_dir_all(&results);
        let Some(base) = c.r.landref.clone() else {
            w.err(format!("queue.sh {label}: cannot resolve the landing ref of {} — the round's lint has nothing to judge against", c.r.name));
            return FAIL;
        };
        let began = w.clock.now();
        let _ = fs::create_dir_all(rounds_dir(c));
        let image = candidate_image(w, label, c, path, &wt, &head, batch);
        let image_id = match &image {
            Image::Candidate(id) => Some(id.clone()),
            _ => None,
        };
        let run_vm = || {
            w.scripts.round_vm(
                &wt,
                &results,
                &base,
                (batch, &c.r.name),
                c.s.round_wall_secs,
                &handle_of(c, batch),
                image_id.as_deref(),
            )
        };
        let mut out = match &image {
            Image::Failed(why) => RunOut {
                rc: 4,
                out: String::new(),
                err: format!("build-error: {why}"),
            },
            Image::Unjudged(why) => RunOut {
                rc: IMAGE_UNJUDGED,
                out: String::new(),
                err: format!("round image: {why}"),
            },
            _ => run_vm(),
        };
        let mut ami = AmiWait::default();
        while image_id.is_none()
            && stale_ami_cause(&out).is_some()
            && !preempt_marker(c, batch).exists()
            && ami.step(w, label, c)
        {
            out = run_vm();
        }
        let template_vm = fs::read_to_string(handle_of(c, batch))
            .map(|h| h.trim().to_string())
            .unwrap_or_default();
        let _ = fs::remove_file(handle_of(c, batch));
        if preempt_marker(c, batch).exists() {
            w.out(format!("queue.sh {label}: round {batch} was preempted; its pass is recorded by `round preempt`"));
            return PREEMPTED;
        }
        let build_env = if out.rc == 4 && !matches!(image, Image::Failed(_)) {
            build_environment_cause(&out.err)
        } else {
            None
        };
        if out.rc == 4 && !matches!(image, Image::Failed(_)) {
            let wall = w.clock.now().saturating_sub(began);
            let _ = fs::create_dir_all(&results);
            let _ = fs::write(
                results.join("workspace-build.out"),
                build_log(&out.err, out.rc, wall),
            );
        }
        timings = pass_timings(&results, w.clock.now().saturating_sub(began));
        let mut found = Vec::new();
        suite_statuses(&results, &mut found, 0);
        ran = found.len();
        if batch_over_cap(&results, out.rc, &out.err) {
            let unreached = found.iter().filter(|(_, s)| s == "unreached").count();
            let cut = if out.err.contains("OVER-CAP") {
                format!(" ({})", over_cap_why(&out.err, c.s.round_wall_secs))
            } else {
                String::new()
            };
            let why = format!("over-cap: FAULT over-cap unreached={unreached}{cut}");
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            if !cut.is_empty() {
                w.err(format!("queue.sh {label}: {why}"));
            }
            *over = Some(unreached);
            return FAULT;
        }
        if out.rc == VM_LOST {
            return lost_vm_pass(w, label, batch, &out.err);
        }
        let fault = match out.rc {
            0 | 1 if found.is_empty() => Some("round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string()),
            0 | 1 => unjudgeable(&wt, &found),
            124 | 137 if !out.err.contains("OVER-CAP") && wall_complete(&wt, &found, &out.err) => None,
            4 if build_env.is_some() => Some(format!(
                "workspace-build environment failure: {} (template VM {}, recipe_sha {})",
                build_env.unwrap_or_default(),
                if template_vm.is_empty() { "unknown" } else { &template_vm },
                w.scripts.recipe_sha(Some(&wt)).unwrap_or_else(|| "unknown".into())
            )),
            4 => {
                reds.push(BUILD_RED.into());
                if let Image::Failed(why) = &image {
                    w.out(format!("build-error={why}"));
                } else if let Some(e) = first_build_error(&out.err) {
                    w.out(format!("build-error={e}"));
                    w.out(format!("red-reason=workspace-build: {}", reason_form(&e)));
                }
                None
            }
            IMAGE_UNJUDGED if matches!(image, Image::Unjudged(_)) => Some(out.err.clone()),
            124 | 137 if out.err.contains("OVER-CAP") => Some(over_cap_why(&out.err, c.s.round_wall_secs)),
            124 | 137 => Some(format!(
                "round-vm hung past its guard (guard=hang-guard elapsed={}s cap {}s)",
                w.clock.now().saturating_sub(began),
                c.s.round_wall_secs
            )),
            rc => Some(match (stale_ami_cause(&out), fixture_fault_cause(&results).or_else(|| vm_fault_cause(&out.err))) {
                (Some(cause), _) => format!("{STALE_AMI_MARK}: {cause}"),
                (None, Some(c)) => format!("round-vm exited {rc}: {c}"),
                (None, None) => format!("round-vm exited {rc}"),
            }),
        };
        if let Some(why) = fault {
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            let shown = tail(&out.err, 20);
            w.err(format!("queue.sh {label}: {why}; the round is not judged\n{shown}"));
            return faulted_pass(w, label, c, batch, &format!("{why}\n{shown}"));
        }
        reds.extend(
            found
                .iter()
                .filter(|(_, s)| BLOCKING.contains(&s.as_str()))
                .map(|(n, _)| n.clone()),
        );
        if out.rc == 1 && reds.is_empty() {
            reds.push(UNIT_RED.into());
            let failed = fs::read_to_string(results.join("unit-tests.out")).unwrap_or_default();
            let names: Vec<&str> = failed
                .lines()
                .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
                .collect();
            if !names.is_empty() {
                w.out(format!("unit-tests-failed={}", names.join(",")));
            }
            if let Some(cause) = failed
                .lines()
                .find_map(|l| l.strip_prefix("UNIT-TESTS-CAUSE: "))
            {
                w.out(format!("unit-tests-cause={cause}"));
            }
        }
        reds.sort();
        reds.dedup();
    }

    let Ok(_g) = lock(w, label, c) else {
        return FAIL;
    };
    let Ok(mut kv) = load(w, label, c, batch) else {
        return FAIL;
    };
    let already_green = reds.is_empty() && phase_of(w, batch) == "green";
    if !(phase_of(w, batch) == "certifying" || already_green)
        || kv.get("head") != Some(head.as_str())
    {
        w.err(format!("queue.sh {label}: the round {batch} changed while it was being certified — the result is discarded; certify again"));
        return FAIL;
    }
    if !reds.is_empty() {
        raise_unreported_reds(w, c, batch, &kv, &reds);
        let recorded = (if reds != [BUILD_RED] {
            suites_started(w, batch, false)
        } else {
            Ok(())
        })
        .and_then(|()| pass_verdict_event(w, batch, &Verdict::Red(&reds, timings), false));
        if let Err((rc, e)) = recorded {
            w.err(format!("queue.sh {label}: REFUSED — the red pass of round {batch} was not recorded on spira-lc (rc={rc}): {e}"));
            set_phase(w, &mut kv, "fault");
            let _ = save(w, label, c, &kv);
            return FAULT;
        }
        note_pass_end(c, &pass_members(c, batch), PassEnd::Judged);
        kv.remove("over-cap");
        set(&mut kv, "red", &reds.join(","));
        if save(w, label, c, &kv).is_err() {
            return FAIL;
        }
        mark_running(c, batch);
        stage::discard_behind(
            w,
            c,
            path,
            batch,
            &format!("round {batch} went red: {}", reds.join(",")),
        );
        let verdict = format!(
            "queue.sh {label}: round {batch} is RED at {head}: {}",
            reds.join(",")
        );
        w.out(verdict.clone());
        w.err(verdict);
        w.out(format!("red={}", reds.join(",")));
        if reds.iter().any(|r| r == REPO_TESTENV_RED) {
            let output = rounds_dir(c)
                .join(format!("{batch}.results"))
                .join(format!("{REPO_TESTENV_RED}.out"));
            w.out(format!("red-output={}", output.display()));
        }
        return FAIL;
    }

    // The pass is the evidence: its certificate is written before the lifecycle is asked, so no
    // lost or refused reply can discard what the corpus proved.
    if let Err(rc) = write_round_certificate(w, label, c, path, batch, &head, &who) {
        return rc;
    }
    let greened = suites_started(w, batch, false)
        .and_then(|()| pass_verdict_event(w, batch, &Verdict::Green(timings), false));
    // A write that timed out may have applied: the batch row is the arbiter, not the reply.
    let applied = greened.is_err() && phase_of(w, batch) == "green";
    if let (Err((rc, out)), false) = (greened, applied) {
        w.err(format!("queue.sh {label}: spira-lc refused the GREEN for {batch} (rc={rc}): {out} — the round certificate for {head} is kept; certify again to finish without re-running"));
        return FAIL;
    }
    note_pass_end(c, &pass_members(c, batch), PassEnd::Judged);
    kv.remove("over-cap");
    set(&mut kv, "ran", &ran.to_string());
    if save(w, label, c, &kv).is_err() {
        return FAIL;
    }
    let wall = timings.suites_s + timings.build_s;
    finish_green(
        w,
        label,
        c,
        path,
        batch,
        &head,
        &who,
        &run_id,
        (ran, wall),
    )
}

/// The (repo, tree) certificate a green full-corpus pass earns. Rewriting it is harmless.
fn write_round_certificate(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    head: &str,
    who: &str,
) -> Result<String, i32> {
    let Some(tree) = w.git.rev_parse(path, &format!("{head}^{{tree}}")) else {
        w.err(format!(
            "queue.sh {label}: cannot resolve {head}^{{tree}} — nothing can say what was judged"
        ));
        return Err(FAIL);
    };
    let cert = gate::cert::Cert {
        source: gate::cert::Source::Round,
        tree: tree.clone(),
        repo: c.r.name.clone(),
        rev: head.to_string(),
        branch: format!("round/{batch}"),
        by: who.to_string(),
        when: w.clock.stamp(),
        at: w.clock.now(),
        harness: "-".into(),
        suites: "full-corpus".into(),
    };
    let verdicts = gate::cert::verdicts_dir(w.var("SPIRA_VERDICTS").as_deref(), &c.s.run);
    if let Err(e) = gate::cert::write(&verdicts, &cert) {
        w.err(format!(
            "queue.sh {label}: cannot record the round certificate for {head}: {e}"
        ));
        return Err(FAIL);
    }
    Ok(tree)
}

/// Everything after a GREEN the lifecycle holds, shared by a fresh certify and a re-certify so
/// both end in the same state.
#[allow(clippy::too_many_arguments)]
fn finish_green(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    head: &str,
    who: &str,
    run_id: &str,
    (ran, wall): (usize, u64),
) -> i32 {
    let tree = match write_round_certificate(w, label, c, path, batch, head, who) {
        Ok(t) => t,
        Err(rc) => return rc,
    };
    // A green round IS the full-suite pass on its head (sp-x334k): publish refuses a head with no
    // full-suite local-pass record, and the round's full corpus is the run that earns it.
    if let Err(e) = spira_config::local_pass::record(
        &c.s.run,
        spira_config::local_pass::Kind::FullSuite,
        head,
        &format!("round {batch}"),
        &w.clock.now().to_string(),
    ) {
        // Not a reason to discard a green round: publish fails closed on the missing record and
        // names it, so the gap is loud at the one place it matters.
        w.err(format!("queue.sh {label}: WARNING — round {batch} is green but its full-suite local pass for {head} was not recorded ({e}); publish will refuse this head until it is"));
    }
    if let Ok(kv) = load(w, label, c, batch) {
        for m in kv.members() {
            for step in ROUND_STEPS {
                let _ = spira_config::proof::record_run(
                    &c.s.run,
                    &m.id,
                    &m.tip,
                    step,
                    0,
                    &format!("round {batch} green at {head}"),
                );
            }
        }
    }
    mark_running(c, batch);
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-GREEN {} repo={} batch={batch} head={head} tree={tree} run={run_id} ran={ran} wall={wall}s",
            w.clock.now(),
            c.r.name
        ),
    );
    w.out(format!(
        "queue.sh {label}: round {batch} is GREEN at {head} (tree {tree}) ran={ran} wall={wall}s"
    ));
    OK
}

/// Suites an eject forces on the recertification must be ones testenv runs for the round's tree;
/// a name nothing can satisfy would block the bead's resubmit for good.
fn known_forced_suites(w: &World, label: &str, wt: &Path, suites: &str) -> Result<(), i32> {
    let named: Vec<&str> = suites.split(',').filter(|s| !s.is_empty()).collect();
    if named.is_empty() {
        return Ok(());
    }
    let known = match gate::own::known_suites(wt) {
        Ok(k) => k,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the suites testenv knows for the round tree: {e} — nothing changed"));
            return Err(FAIL);
        }
    };
    let unknown: Vec<&str> = named
        .iter()
        .copied()
        .filter(|n| !is_round_step(n) && !known.iter().any(|k| k == n))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    w.err(format!("queue.sh {label}: --suites names {} which testenv does not know — nothing changed; known suites: {}; round steps: {}", unknown.join(","), known.join(" "), ROUND_STEPS.join(" ")));
    Err(FAIL)
}

#[allow(clippy::too_many_arguments)]
fn eject(
    w: &World,
    batch: &str,
    id: &str,
    repo: Option<&str>,
    reason: &Text,
    suites: &str,
    red: bool,
    harness_fault: bool,
    rebuild: bool,
    with: &str,
) -> i32 {
    let label = "round eject";
    if red && !harness_fault && suites.trim().is_empty() {
        w.err(format!("queue.sh {label}: --red names no failing check — pass --suites <csv> naming the suite or step that failed, so the resubmit can be held to proving it"));
        return USAGE;
    }
    let reason = match read_text(w, reason) {
        Ok(r) if !r.trim().is_empty() => r,
        Ok(_) => {
            w.err(format!(
                "queue.sh {label}: --reason is required — the bead carries it back to its builder"
            ));
            return USAGE;
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    let reason = match statute_gate(w, label, &reason) {
        Ok(Some(line)) => format!("{reason}\n\n{line}"),
        Ok(None) => reason,
        Err(rc) => return rc,
    };
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch), ("bead id", id)]).is_err() {
        return FAIL;
    }
    if !suites.is_empty()
        && suites
            .split(',')
            .try_for_each(|s| idents(w, label, &[("suite", s)]))
            .is_err()
    {
        return FAIL;
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else {
        return FAIL;
    };
    let Ok(mut kv) = load(w, label, &c, batch) else {
        return FAIL;
    };
    let members = kv.members();
    if !members.iter().any(|m| m.id == id) {
        let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
        w.err(format!(
            "queue.sh {label}: {id} is not a member of round {batch} (members: {})",
            ids.join(" ")
        ));
        return FAIL;
    }
    let wt = worktree_of(&kv);
    if let Err(rc) = known_forced_suites(w, label, &wt, suites) {
        return rc;
    }
    let old_head = kv.get("head").unwrap_or("").to_string();
    let base_sha = kv.get("base").unwrap_or("").to_string();
    let ejected_tip = members
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.tip.clone())
        .unwrap_or_default();
    let withs: Vec<&str> = with
        .split(',')
        .filter(|s| !s.is_empty() && *s != id)
        .collect();
    if let Some(bad) = withs.iter().find(|i| !members.iter().any(|m| m.id == **i)) {
        w.err(format!(
            "queue.sh {label}: --with names {bad}, which is not a member of round {batch}"
        ));
        return FAIL;
    }
    let stacked: Vec<Member> = members
        .iter()
        .filter(|m| {
            m.id != id
                && (withs.contains(&m.id.as_str())
                    || w.git
                        .merge_base(&path, &ejected_tip, &m.tip)
                        .is_some_and(|mb| !w.git.is_ancestor(&path, &mb, &base_sha)))
        })
        .cloned()
        .collect();
    let survivors: Vec<Member> = members
        .iter()
        .filter(|m| m.id != id && !stacked.iter().any(|s| s.id == m.id))
        .cloned()
        .collect();

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
    if let Err((rc, out)) = lc_eject_member(w, batch, id, LC_ACTOR, &why) {
        if new_head != old_head {
            w.git.worktree_remove(&path, &wt);
            let _ = w.git.worktree_add_detached(&path, &wt, &old_head);
        }
        w.err(format!("queue.sh {label}: spira-lc eject-member refused for {id} (rc={rc}): {out} — nothing changed"));
        return FAIL;
    }
    let cause = EjectCause::decide(red, harness_fault, suites);
    if red && suites.split(',').any(|s| s == UNIT_RED) {
        crate::telemetry::sink().push(
            &[spira_otel::metrics::Point::Count {
                name: "habit_unit_tests_red_in_round",
                value: 1,
            }],
            &[("repo", c.r.name.as_str()), ("bead", id)],
        );
    }
    let mut unreturned: Vec<&str> = Vec::new();
    for (bead, why_text, own_cause) in
        std::iter::once((id, reason.clone(), cause)).chain(stacked.iter().map(|m| {
            let why = if withs.contains(&m.id.as_str()) {
                format!("red only in combination with {id}; integrated with it")
            } else {
                format!("stacked on {id}")
            };
            (m.id.as_str(), why, cause)
        }))
    {
        if !w.lib.bead_reopen(bead, own_cause.as_str(), suites) {
            w.err(format!(
                "queue.sh {label}: cannot reopen {bead} (cause {})",
                own_cause.as_str()
            ));
            unreturned.push(bead);
        }
        if lc_return(w, bead, bead == id && own_cause == EjectCause::EjectRed).is_err()
            && !unreturned.contains(&bead)
        {
            unreturned.push(bead);
        }
        w.lib.release_claim(bead);
        if bead != id {
            let why = bounded_text(&why_text);
            if let Err((rc, out)) = lc_eject_member(w, batch, bead, LC_ACTOR, &why) {
                w.err(format!(
                    "queue.sh {label}: spira-lc eject-member refused for {bead} (rc={rc}): {out}"
                ));
            }
        }
        let mut comment = format!("Ejected from round {batch} in {}.\n\n{why_text}", c.r.name);
        comment.push_str("\n\nFix the failing issue and re-certify before rejoining the queue.");
        if !suites.is_empty() {
            comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
        }
        w.lib.comment(bead, &comment);
        landing_log(
            &c.s.run,
            &format!(
                "QUEUE ROUND-EJECT {} repo={} batch={batch} id={bead} cause={} reason={}",
                w.clock.now(),
                c.r.name,
                own_cause.as_str(),
                one_line(&why_text)
            ),
        );
        if bead != id {
            w.out(format!(
                "queue.sh {label}: ejected {bead} from round {batch} (stacked on {id})"
            ));
        }
    }

    if !withs.is_empty() {
        let mut involved: Vec<Member> = members.iter().filter(|m| m.id == id).cloned().collect();
        involved.extend(
            members
                .iter()
                .filter(|m| withs.contains(&m.id.as_str()))
                .cloned(),
        );
        let source = format!(
            "Round {batch} went red only on the combination of {id} with {}.",
            withs.join(", ")
        );
        super::integrate::file(w, label, &c, &path, &base_sha, &involved, &source, &reason);
    }

    retire_menders(w, &c, batch, &format!("{id} ejected"));
    if survivors.is_empty() {
        let emptied = bounded_text(&format!("round emptied: {id} ejected — {reason}"));
        if let Err((rc, out)) = lc_abandon(w, batch, LC_ACTOR, &emptied) {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for the emptied round {batch} (rc={rc}): {out}"));
        }
        drop_record(&c, batch);
        w.git.worktree_remove(&path, &wt);
        finish(
            &c,
            batch,
            &format!("emptied: {id} ejected, no member remains"),
        );
        stage::discard_behind(w, &c, &path, batch, &format!("round {batch} was emptied"));
        w.out(format!(
            "queue.sh {label}: ejected {id}; the round {batch} has no member left and is closed"
        ));
        if !unreturned.is_empty() {
            w.err(format!("queue.sh {label}: {} left round {batch} but did not return to REWORK: {} — see above", id, unreturned.join(" ")));
            return FAIL;
        }
        return OK;
    }

    let rebuilt = serde_json::json!({"PassRebuilt": {"head": new_head}}).to_string();
    warn_unrecorded(
        w,
        label,
        batch,
        "the rebuilt head",
        pass_event(w, batch, |s, _, _| (s == "ATTRIBUTING").then_some(rebuilt)),
    );
    set(&mut kv, "members", &render_members(&survivors));
    set(&mut kv, "head", &new_head);
    kv.remove("red");
    let ejected = stacked.iter().fold(
        format!("{}{id} ", kv.get("ejected").unwrap_or("")),
        |acc, m| format!("{acc}{} ", m.id),
    );
    set(&mut kv, "ejected", &ejected);
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, batch);
    w.out(format!(
        "queue.sh {label}: ejected {id} from round {batch}; {} member(s) remain",
        survivors.len()
    ));
    w.out(format!("head={new_head}"));
    w.out(format!("members={}", render_members(&survivors)));
    if !unreturned.is_empty() {
        w.err(format!(
            "queue.sh {label}: {} left round {batch} but did not return to REWORK: {} — see above",
            id,
            unreturned.join(" ")
        ));
        return FAIL;
    }
    OK
}

/// A recorded mend moves a member's tip, not the round's head: before a pass starts, the head is
/// rebuilt from the base with every member at its mended tip. `Ok(None)` when nothing was mended.
fn carry_mends(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    wt: &Path,
    batch: &str,
    kv: &mut crate::records::Kv,
) -> Result<Option<String>, ()> {
    let members = kv.members();
    let beads: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let recorded = w.lc.mend_tips(&beads).map_err(|e| {
        w.err(format!("queue.sh {label}: cannot read the recorded mends: {e}"));
    })?;
    let carried: Vec<Member> = members
        .iter()
        .map(|m| Member {
            tip: recorded.get(&m.id).cloned().unwrap_or_else(|| m.tip.clone()),
            ..m.clone()
        })
        .collect();
    if carried.iter().zip(&members).all(|(a, b)| a.tip == b.tip) {
        return Ok(None);
    }
    let old_head = kv.get("head").unwrap_or("").to_string();
    let base = kv.get("base").unwrap_or("").to_string();
    let new_head = assemble(w, c, path, wt, &base, &carried, &old_head).map_err(|why| {
        w.err(format!("queue.sh {label}: cannot rebuild the round with its mended tips: {why} — refused"));
    })?;
    let rebuilt = serde_json::json!({"PassRebuilt": {"head": new_head}}).to_string();
    warn_unrecorded(
        w,
        label,
        batch,
        "the rebuilt head",
        pass_event(w, batch, |s, _, _| (s == "ATTRIBUTING").then_some(rebuilt)),
    );
    set(kv, "members", &render_members(&carried));
    set(kv, "head", &new_head);
    w.out(format!(
        "queue.sh {label}: round {batch} rebuilt at {new_head} with its mended tips"
    ));
    Ok(Some(new_head))
}

/// Stops the round's running pass without letting it finish: TERM to the run's handle so
/// round-vm salvages the finished suites and releases the VM, `PassPreempted` on the batch,
/// the named members ejected with the evidence, the head rebuilt and checked, the next pass
/// started. Refuses, naming the phase, when no pass is running.
fn preempt(
    w: &World,
    batch: &str,
    repo: Option<&str>,
    eject_arg: &str,
    reason: &Text,
    suites: &str,
) -> i32 {
    let label = "round preempt";
    let reason = match read_text(w, reason) {
        Ok(r) if !r.trim().is_empty() => r,
        Ok(_) => {
            w.err(format!("queue.sh {label}: --reason is required — it is the evidence each ejected bead carries back"));
            return USAGE;
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    let ids: Vec<String> = eject_arg
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if ids.is_empty() {
        w.err(format!("queue.sh {label}: --eject names no bead"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    if ids
        .iter()
        .try_for_each(|i| idents(w, label, &[("bead id", i)]))
        .is_err()
        || suites
            .split(',')
            .filter(|s| !s.is_empty())
            .try_for_each(|s| idents(w, label, &[("suite", s)]))
            .is_err()
    {
        return FAIL;
    }

    let (wt, head, tips) = {
        let Ok(_g) = lock(w, label, &c) else {
            return FAIL;
        };
        let Ok(kv) = load(w, label, &c, batch) else {
            return FAIL;
        };
        let phase = phase_of(w, batch);
        let members = kv.members();
        if let Some(missing) = ids.iter().find(|i| !members.iter().any(|m| &m.id == *i)) {
            let named: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
            w.err(format!(
                "queue.sh {label}: {missing} is not a member of round {batch} (members: {})",
                named.join(" ")
            ));
            return FAIL;
        }
        let pid = fs::read_to_string(handle_of(&c, batch))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .filter(|p| w.scripts.pass_alive(*p));
        let Some(pid) = pid.filter(|_| phase == "certifying") else {
            w.err(format!("queue.sh {label}: round {batch} is {phase}, with no pass running — nothing to preempt"));
            return FAIL;
        };
        let _ = fs::create_dir_all(rounds_dir(&c));
        if let Err(e) = fs::write(preempt_marker(&c, batch), one_line(&reason)) {
            w.err(format!(
                "queue.sh {label}: cannot mark the pass as preempted: {e}"
            ));
            return FAIL;
        }
        w.scripts.pass_terminate(pid);
        let mut polls = 0;
        while w.scripts.pass_alive(pid) && polls < PREEMPT_POLLS {
            w.clock.sleep(1);
            polls += 1;
        }
        if w.scripts.pass_alive(pid) {
            let _ = fs::remove_file(preempt_marker(&c, batch));
            w.err(format!("queue.sh {label}: the pass of round {batch} (pid {pid}) did not stop after {PREEMPT_POLLS} polls — left running, not killed: a kill would lose its salvaged results"));
            return FAIL;
        }
        (
            worktree_of(&kv),
            kv.get("head").unwrap_or("").to_string(),
            members,
        )
    };

    let mut found = Vec::new();
    suite_statuses(
        &rounds_dir(&c).join(format!("{batch}.results")),
        &mut found,
        0,
    );
    let corpus = fs::read_dir(wt.join("spira"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
                .count()
        })
        .unwrap_or(0);
    let (done, total) = (found.len() as u32, (corpus.max(found.len())) as u32);
    let mut reds: Vec<String> = found
        .iter()
        .filter(|(_, s)| BLOCKING.contains(&s.as_str()))
        .map(|(n, _)| n.clone())
        .collect();
    reds.sort();
    reds.dedup();

    {
        let Ok(_g) = lock(w, label, &c) else {
            return FAIL;
        };
        let Ok(mut kv) = load(w, label, &c, batch) else {
            return FAIL;
        };
        if phase_of(w, batch) != "certifying" || kv.get("head") != Some(head.as_str()) {
            w.err(format!("queue.sh {label}: the round {batch} changed while its pass was stopping — nothing ejected"));
            return FAIL;
        }
        let recorded = lc_cas(w, batch, |s, v| {
            let (n, _) =
                w.lc.batch_pass(batch)
                    .ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
            let kind = serde_json::json!({"PassPreempted": {"n": n, "done": done, "total": total, "red_suites": reds}});
            w.lc.batch_event(batch, s, v, LC_ACTOR, &kind.to_string())
        });
        if let Err((rc, out)) = recorded {
            w.err(format!("queue.sh {label}: spira-lc refused PassPreempted for {batch} (rc={rc}): {out} — nothing ejected"));
            return FAIL;
        }
        set(&mut kv, "red", &reds.join(","));
        if save(w, label, &c, &kv).is_err() {
            return FAIL;
        }
        let _ = fs::remove_file(preempt_marker(&c, batch));
        mark_running(&c, batch);
    }
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-PREEMPT {} repo={} batch={batch} done={done}/{total} red={} eject={}",
            w.clock.now(),
            c.r.name,
            reds.join(","),
            ids.join(",")
        ),
    );
    w.out(format!(
        "queue.sh {label}: pass of round {batch} stopped at {done}/{total} suites, red so far: {}",
        if reds.is_empty() {
            "none".to_string()
        } else {
            reds.join(",")
        }
    ));

    for id in &ids {
        match record_of(&c, batch).map(|(_, kv)| kv) {
            Some(kv) if kv.members().iter().any(|m| &m.id == id) => {}
            Some(_) => {
                w.out(format!(
                    "queue.sh {label}: {id} already left round {batch} with an earlier eject"
                ));
                continue;
            }
            None => break,
        }
        let rc = eject(
            w,
            batch,
            id,
            repo,
            &Text::Arg(reason.clone()),
            suites,
            !suites.is_empty(),
            false,
            true,
            "",
        );
        if rc != OK {
            w.err(format!(
                "queue.sh {label}: eject of {id} failed (rc={rc}) — the next pass was not started"
            ));
            return rc;
        }
    }

    let Some((_, kv)) = record_of(&c, batch) else {
        w.out(format!(
            "queue.sh {label}: round {batch} has no member left; no pass to start"
        ));
        return OK;
    };
    let new_head = kv.get("head").unwrap_or("").to_string();
    let base = kv.get("base").unwrap_or("").to_string();
    let still_in: Vec<&str> = tips
        .iter()
        .filter(|m| !kv.members().iter().any(|k| k.id == m.id))
        .filter(|m| {
            w.git.is_ancestor(&path, &m.tip, &new_head) && !w.git.is_ancestor(&path, &m.tip, &base)
        })
        .map(|m| m.id.as_str())
        .collect();
    if !still_in.is_empty()
        || w.git.rev_parse(&worktree_of(&kv), "HEAD").as_deref() != Some(new_head.as_str())
    {
        w.err(format!("queue.sh {label}: the rebuilt head {new_head} of round {batch} still holds the tip of {} or the worktree is not at it — the next pass was not started", still_in.join(" ")));
        return FAIL;
    }
    w.out(format!(
        "queue.sh {label}: verified no ejected tip remains in head {new_head}"
    ));
    if !w.scripts.pass_restart(batch, &c.r.name) {
        w.err(format!("queue.sh {label}: cannot start the next pass of round {batch} — run `queue round certify {batch}`"));
        return FAIL;
    }
    w.out(format!(
        "queue.sh {label}: next pass of round {batch} started at {new_head}"
    ));
    OK
}

/// Activation restarts the store's server; a refusal (rc 3) is final, anything else is retried
/// until the store answers or the deadline passes.
const RECORD_DEADLINE_SECS: u64 = 60;

fn record_landing(w: &World, batch: &str, head: &str) -> Result<(), (i32, String)> {
    let deadline = w.clock.now() + RECORD_DEADLINE_SECS;
    loop {
        match lc_cas(w, batch, |_, v| w.lc.land_batch(batch, v, LC_ACTOR, head)) {
            Err((rc, out)) if rc != 3 => {
                if w.clock.now() >= deadline {
                    return Err((rc, out));
                }
                w.clock.sleep(1);
                while w.lc.probe().is_err() && w.clock.now() < deadline {
                    w.clock.sleep(1);
                }
            }
            done => return done,
        }
    }
}

fn unrecorded_marker(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.unrecorded"))
}

/// A round that landed on the ref but whose landing record was refused left a marker holding its
/// head; record it now if the row is still GREEN and the head is on the landing ref.
pub(super) fn reconcile_unrecorded(w: &World, c: &Ctx, path: &Path) {
    let Ok(rd) = fs::read_dir(rounds_dir(c)) else {
        return;
    };
    let base = c.r.landref.clone().unwrap_or_default();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(batch) = name.strip_suffix(".unrecorded") else {
            continue;
        };
        match w.lc.batch_state(batch) {
            Some((s, _)) if super::reconcile::terminal(&s) => {
                let _ = fs::remove_file(e.path());
            }
            Some((s, _)) if s == "GREEN" => {
                let head = fs::read_to_string(e.path()).unwrap_or_default();
                let head = head.trim();
                if head.is_empty() || !w.git.is_ancestor(path, head, &base) {
                    continue;
                }
                match record_landing(w, batch, head) {
                    Ok(()) => {
                        let _ = fs::remove_file(e.path());
                        w.out(format!("queue.sh: round {batch} was landed at {head} with no landing record — recorded now"));
                    }
                    Err((rc, out)) => w.err(format!("queue.sh: WARNING — landing record for {batch} still refused (rc={rc}): {}", out.trim())),
                }
            }
            _ => {}
        }
    }
}

fn land(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    let label = "round land";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    reconcile_unrecorded(w, &c, &path);
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else {
        return FAIL;
    };
    let Ok(mut kv) = load(w, label, &c, batch) else {
        return FAIL;
    };
    let phase = phase_of(w, batch);
    if phase != "green" {
        w.err(format!(
            "queue.sh {label}: round {batch} is {phase}, not green — certify it first"
        ));
        return FAIL;
    }
    if let Some(rc) = remerge_if_moved(w, label, &c, &path, batch, &mut kv) {
        return rc;
    }
    let head = kv.get("head").unwrap_or("").to_string();
    let members = kv.members();
    let wt = worktree_of(&kv);
    let bins = wt
        .join("target")
        .join("release")
        .is_dir()
        .then_some(wt.as_path());
    let (rc, pending) = land_local_deferring(
        w,
        Some(&c.r.name),
        &head,
        &Text::Arg(csv(&members)),
        bins,
        true,
        true,
    );
    let base = c.r.landref.clone().unwrap_or_default();
    if rc != OK && !w.git.is_ancestor(&path, &head, &base) {
        if let Some(p) = pending {
            p.discard();
        }
        return rc;
    }

    let recorded = match record_landing(w, batch, &head) {
        Ok(()) => true,
        Err((rc, out)) => {
            let why = if out.trim().is_empty() {
                "spira-lc gave no reason on stderr or stdout"
            } else {
                out.trim()
            };
            w.err(format!("queue.sh {label}: ALARM: {head} is landed and live but spira-lc refused the batch landing record for {batch} (rc={rc}): {why}"));
            let _ = fs::create_dir_all(rounds_dir(&c));
            let _ = write_atomic(&unrecorded_marker(&c, batch), &head);
            w.scripts.mail_operator(
                &format!("Round {batch} landed but has no batch landing record"),
                &format!("{head} is landed and live, but spira-lc refused the batch landing record for {batch} (rc={rc}): {why}\n\nThe batch stays GREEN on the lifecycle until recorded. The next round open or land records it once the head is on the landing ref; no action is needed unless that does not happen.\n"),
            );
            false
        }
    };
    retire_menders(w, &c, batch, "round landed");
    drop_record(&c, batch);
    w.git.worktree_remove(&path, &wt);
    finish(
        &c,
        batch,
        &format!("landed {head} members={}", csv(&members)),
    );
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-LAND {} repo={} batch={batch} head={head} members={}",
            w.clock.now(),
            c.r.name,
            csv(&members)
        ),
    );
    w.out(format!(
        "queue.sh {label}: round {batch} landed at {head} — {} member(s)",
        members.len()
    ));
    // Activation is the last step: the new release may retire keys the steps above still need.
    let rc = match pending.map(|p| p.activate(w)) {
        Some(super::deploy::Outcome::Fault(_)) => super::DEPLOY_FAULT,
        _ => rc,
    };
    if !recorded {
        w.err(format!("queue.sh {label}: round {batch} has NO batch landing record; the release is live — exiting non-zero"));
        return if rc == OK { FAIL } else { rc };
    }
    rc
}

/// A round stacked on an open round lands only after that round has: it holds while the parent is
/// open, is re-cut on the parent's head when that changed, and is rebuilt on the landing ref
/// (to certify again) when the parent ended without landing. `None` means no stacking obstacle.
fn settle_parent(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    kv: &mut Kv,
    base_now: &str,
) -> Option<i32> {
    let parent = kv.get("parent").filter(|p| !p.is_empty())?.to_string();
    let stacked_on = kv.get("base").unwrap_or("").to_string();
    let onto = match record_of(c, &parent) {
        Some((_, pkv)) => {
            let phead = pkv.get("head").unwrap_or("").to_string();
            if phead == stacked_on {
                w.err(format!("queue.sh {label}: round {batch} is stacked on round {parent}, which has not landed — land {parent} first; nothing changed"));
                return Some(FAIL);
            }
            phead
        }
        None if w.git.is_ancestor(path, &stacked_on, base_now) => {
            kv.remove("parent");
            return None;
        }
        None => {
            kv.remove("parent");
            base_now.to_string()
        }
    };
    let members = kv.members();
    let wt = worktree_of(kv);
    let fallback = kv.get("head").unwrap_or("").to_string();
    let new_head = match assemble(w, c, path, &wt, &onto, &members, &fallback) {
        Ok(h) => h,
        Err(why) => {
            w.err(format!("queue.sh {label}: round {batch} no longer merges onto {onto}: {why} — nothing changed; eject the member and certify again"));
            return Some(FAIL);
        }
    };
    set(kv, "head", &new_head);
    set(kv, "base", &onto);
    kv.remove("red");
    let moved = lc_cas(w, batch, |s, v| {
        w.lc.batch_event(batch, s, v, LC_ACTOR, "\"BaseMoved\"")
    });
    if let Err((rc, out)) = moved {
        w.err(format!("queue.sh {label}: spira-lc refused BaseMoved for {batch} (rc={rc}): {out} — the round worktree was rebuilt at {new_head}; certify it again"));
    }
    if save(w, label, c, kv).is_err() {
        return Some(FAIL);
    }
    mark_running(c, batch);
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-RESTACK {} repo={} batch={batch} head={new_head} base={onto}",
            w.clock.now(),
            c.r.name
        ),
    );
    w.err(format!("queue.sh {label}: round {batch} was stacked on round {parent}, which changed or ended without landing — rebuilt at {new_head} on {onto}; run `queue round certify {batch}`, it is not landed"));
    Some(FAIL)
}

/// A round is cut from the landing ref and lands only on top of it. When an earlier round has
/// landed since, this one is re-merged onto the new ref and goes back to certify on the merged
/// tree, whatever files it shares with what landed: disjoint files can still conflict in meaning.
/// `None` means the round may land now (its kv holds the head to land).
fn remerge_if_moved(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    kv: &mut Kv,
) -> Option<i32> {
    let base = c.r.landref.clone().unwrap_or_default();
    let base_now = w.git.rev_parse(path, &base)?;
    if let Some(rc) = settle_parent(w, label, c, path, batch, kv, &base_now) {
        return Some(rc);
    }
    let head = kv.get("head").unwrap_or("").to_string();
    if w.git.is_ancestor(path, &base_now, &head) {
        return None;
    }
    let old_base = kv.get("base").unwrap_or("").to_string();
    let wt = worktree_of(kv);
    let members = kv.members();
    let files = |a: &str, b: &str| {
        w.git.numstat(path, a, b).map(|v| {
            v.into_iter()
                .map(|(f, _, _)| f)
                .collect::<std::collections::BTreeSet<String>>()
        })
    };
    let (Ok(landed), Ok(mine)) = (files(&old_base, &base_now), files(&old_base, &head)) else {
        w.err(format!("queue.sh {label}: cannot list the files of round {batch} or of what landed since it was cut — refused, nothing changed"));
        return Some(FAIL);
    };
    let shared: Vec<&String> = landed.intersection(&mine).collect();
    let new_head = match assemble(w, c, path, &wt, &base_now, &members, &head) {
        Ok(h) => h,
        Err(why) => {
            w.err(format!("queue.sh {label}: round {batch} no longer merges onto {base} ({base_now}): {why} — nothing changed; eject the member and certify again"));
            return Some(FAIL);
        }
    };
    set(kv, "head", &new_head);
    set(kv, "base", &base_now);
    kv.remove("red");
    let moved = lc_cas(w, batch, |s, v| {
        w.lc.batch_event(batch, s, v, LC_ACTOR, "\"BaseMoved\"")
    });
    if let Err((rc, out)) = moved {
        w.err(format!("queue.sh {label}: spira-lc refused BaseMoved for {batch} (rc={rc}): {out} — the round worktree was rebuilt at {new_head}; certify it again"));
    }
    if save(w, label, c, kv).is_err() {
        return Some(FAIL);
    }
    mark_running(c, batch);
    landing_log(&c.s.run, &format!("QUEUE ROUND-REMERGE {} repo={} batch={batch} head={new_head} base={base_now} shared={}", w.clock.now(), c.r.name, shared.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(",")));
    w.err(format!(
        "queue.sh {label}: round {batch} was cut before {base} moved to {base_now} (files in common with what landed: {}) — re-merged to {new_head}; run `queue round certify {batch}`, it is not landed",
        if shared.is_empty() { "none".to_string() } else { shared.iter().take(5).map(|f| f.as_str()).collect::<Vec<_>>().join(" ") }
    ));
    Some(FAIL)
}

fn requeue_stranded(w: &World, label: &str, id: &str, tip: &str) -> Option<String> {
    let kind = serde_json::json!({"Requeued": {"tip": tip}}).to_string();
    let (mut state, mut version) = w.lc.bead_state(id)?;
    for attempt in 0..2 {
        if state != "IN_DELIVERY" {
            return Some(state);
        }
        let Err((rc, e)) = w.lc.bead_event(id, &state, &version, LC_ACTOR, &kind) else {
            return w.lc.bead_state(id).map(|(s, _)| s);
        };
        let reread = w.lc.bead_state(id);
        match reread {
            Some((s, v)) if attempt == 0 && (s != state || v != version) => {
                (state, version) = (s, v)
            }
            _ => {
                w.err(format!("queue.sh {label}: {id} is still IN_DELIVERY after the abandon and the requeue was refused (rc={rc}): {e}"));
                return None;
            }
        }
    }
    None
}

fn abandon(w: &World, batch: &str, repo: Option<&str>, reason: &Text) -> i32 {
    let label = "round abandon";
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.trim().is_empty() {
        w.err(format!("queue.sh {label}: --reason is required — pass --reason \"<why>\" so the abandon is auditable"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else {
        return FAIL;
    };
    let Ok(kv) = load(w, label, &c, batch) else {
        return FAIL;
    };
    let members = kv.members();
    let why = bounded_text(&reason);
    match lc_abandon(w, batch, LC_ACTOR, &why) {
        Ok(()) => w.out(format!("queue.sh {label}: {batch} abandoned on spira-lc")),
        Err((1, out)) if out.is_empty() => w.err(format!(
            "queue.sh {label}: spira-lc has no batch row for {batch}"
        )),
        Err((rc, out)) => {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for {batch} (rc={rc}): {out} — the round stays open"));
            return FAIL;
        }
    }
    for m in &members {
        let mut state =
            w.lc.bead_row(&m.id)
                .map(|r| r.state)
                .unwrap_or_else(|| "unknown".into());
        if state == "IN_DELIVERY" {
            state = requeue_stranded(w, label, &m.id, &m.tip).unwrap_or(state);
        }
        w.out(format!("queue.sh {label}: {}: {state}", m.id));
    }
    retire_menders(w, &c, batch, "round abandoned");
    drop_record(&c, batch);
    w.git.worktree_remove(&path, &worktree_of(&kv));
    finish(&c, batch, &format!("abandoned: {}", one_line(&reason)));
    stage::discard_behind(
        w,
        &c,
        &path,
        batch,
        &format!("round {batch} was abandoned: {}", one_line(&reason)),
    );
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ROUND-ABANDON {} repo={} batch={batch} reason={}",
            w.clock.now(),
            c.r.name,
            one_line(&reason)
        ),
    );
    w.out(format!(
        "queue.sh {label}: round {batch} abandoned for {}",
        c.r.name
    ));
    OK
}
