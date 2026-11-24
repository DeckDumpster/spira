//! `queue round`: the round lifecycle the batcher and the Concierge share, under queue.local.
//! A round is one record (`<queue_dir>/<repo>/round`), one worktree and one spira-lc batch;
//! the verbs differ from a hand's scratch scripts in that spira-lc, loom and round-duty can
//! all see the round (DESIGN.md §2.2).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::batch::lc_return;
use super::land::land_local_deferring;
use super::round_files;
use super::round_rerun;
use super::stage;
use super::{
    actor, batch_abandoned, converge_orphans, czar_ok, idents, lc_abandon, lc_cas,
    lc_eject_member, lock_held_by_caller, read_text, repo_path, require_lc, resolve, round_event,
    statute_gate, take_lock, Ctx, World, FAIL, OK, USAGE,
};
use crate::batches::BatchRecord;
use crate::cli::{Round, Text};
use crate::ident::bounded_text;
use crate::lock::Guard;
use crate::model::{parse_members, render_members, EjectCause, LandMode, Member};
use crate::ports::{ref_branch, AmiBuild, AmiLookup, ImageCheck, RunOut};
use crate::records::{self, one_line, write_atomic, Kv};
use spira_config::scoped_stop;
use spira_config::sendback::failure_excerpt;

const DENY_LIST: &str = "deny-list";

const ROUNDS_LANE: &str = "rounds";

/// certify's exit when the corpus could not be judged (the VM, the wall, the install), as
/// opposed to FAIL, which is a red round.
pub const FAULT: i32 = 4;

/// round-vm's exit, and certify's, when the round VM died under the pass: no verdict, no member
/// at fault, the pass is run again.
pub const VM_LOST: i32 = 75;
/// round-vm placed the pass on EC2 while the candidate recipe's image is still baking.
pub const IMAGE_PENDING: i32 = 78;

/// round-vm's exit, and certify's, when the VM answered ssh and never started the round: no
/// verdict, no member at fault, and the driver runs the pass once more on a fresh VM.
pub const VM_NO_OUTPUT: i32 = 77;

/// eject's and rebuild's exit when a pass of the round is still running: nothing changed, and the
/// caller issues the same verb again once no pass is running.
pub const PASS_RUNNING: i32 = 79;

pub(super) const BUILD_RED: &str = "workspace-build";
pub(super) const LINT_RED: &str = "spira-lint";
const FENCE_RED_PREFIX: &str = "fence-";
/// The fence that runs the round image's setup on the merged tree before any suite.
pub(super) const PREFLIGHT_RED: &str = "fence-round-image-preflight";
pub(super) const IMAGE_RED: &str = "fence-round-image";
pub(super) const UNIT_RED: &str = "unit-tests";
/// The round VM's two test phases (sp-cddf76), each leaving its red as a record: the unit phase
/// (tests inside src/) before any suite, the integration phase (tests/) beside them.
/// Either is red on the `unit-tests` step, the testenv suite that runs both for a tip.
pub(super) const TEST_PHASES: [(&str, &str); 2] = [
    ("unit.json", "unit-tests"),
    ("integration.json", "integration-tests"),
];

/// The step a named red is charged to: both test phases are red on `unit-tests`, and `lint` is
/// the round step `spira-lint`.
fn test_phase_step(suite: &str) -> &str {
    if TEST_PHASES.iter().any(|(_, stage)| *stage == suite) {
        UNIT_RED
    } else if suite == "lint" {
        LINT_RED
    } else {
        suite
    }
}

/// A test phase the round VM recorded red.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhaseRed {
    pub stage: String,
    pub before_suites: bool,
    pub failed: Vec<String>,
    pub cause: String,
    pub secs: u64,
    /// `<test> ran over <N> s, then passed|failed` / `did not finish`, per test libtest flagged slow.
    pub long_running: Vec<String>,
}

impl PhaseRed {
    /// The lines certify prints for the red: the failed tests by name, and why.
    pub(super) fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.failed.is_empty() {
            lines.push(format!("unit-tests-failed={}", self.failed.join(",")));
        }
        let when = if self.before_suites {
            "before the suites"
        } else {
            "beside the suites"
        };
        lines.push(format!(
            "unit-tests-cause={}: {} ({when}, after {}s)",
            self.stage, self.cause, self.secs
        ));
        lines
    }
}

/// Each test phase whose record in `results` is red, unit phase first; an absent, unreadable or
/// green record is no red.
pub(super) fn phase_reds(results: &std::path::Path) -> Vec<PhaseRed> {
    TEST_PHASES
        .iter()
        .filter_map(|(file, stage)| {
            let v: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(results.join(file)).ok()?).ok()?;
            (v["stage"] == *stage && v["verdict"] == "red").then(|| PhaseRed {
                stage: stage.to_string(),
                before_suites: v["ran_before_suites"].as_bool().unwrap_or(false),
                failed: v["failed"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                cause: v["cause"].as_str().unwrap_or("").to_string(),
                secs: v["secs"].as_u64().unwrap_or(0),
                long_running: v["long_running"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| {
                        let end = match l["end"].as_str()? {
                            "unfinished" => "did not finish".to_string(),
                            e => format!("then {e}"),
                        };
                        Some(format!("{} ran over {} s, {end}", l["test"].as_str()?, l["over_secs"].as_u64()?))
                    })
                    .collect(),
            })
        })
        .collect()
}

/// A round step, or a base fence the round runs before its suites (`fence-<name>`).
pub fn is_round_step(name: &str) -> bool {
    spira_config::round_step::is_step(name)
}

/// Whether a verb meets the round lease guard: on the caller's word, with the token it was
/// given, or `Inside` a verb that has already met it or that cut the round itself.
#[derive(Clone, Copy)]
pub(super) enum LeaseCheck<'a> {
    Check(Option<&'a str>),
    Inside,
}

impl LeaseCheck<'_> {
    fn guard(self, w: &World, label: &str, c: &Ctx, batch: &str) -> Result<(), i32> {
        match self {
            LeaseCheck::Check(token) => {
                super::lease::lease_guard(w, label, &c.r.name, batch, token)
            }
            LeaseCheck::Inside => Ok(()),
        }
    }
}
/// The one red a repository that judges itself reports: its testenv run as a whole.
pub(super) const REPO_TESTENV_RED: &str = "testenv";
pub(crate) const LC_ACTOR: &str = "queue.sh";
/// The actor of a round event the queue writes on its own, under no caller's verb.
const HOUSEKEEPING: &str = "queue";
const PASSING: [&str; 5] = ["ok", "skip", "disabled", "skip-req", "quarantined-red"];
pub const BLOCKING: [&str; 5] = ["red", "timeout", "unreached", "deferred", "fault"];

pub fn run(w: &World, r: &Round) -> i32 {
    match r {
        Round::Open {
            by,
            repo,
            members,
            name,
            worktree,
        } => open(
            w,
            by,
            repo.as_deref(),
            members,
            name.as_deref(),
            worktree.as_deref(),
        ),
        Round::Certify {
            by,
            batch,
            repo,
            attest,
            ran,
            rehead,
            host_only,
            lease_token,
        } => certify(
            w,
            by,
            batch,
            repo.as_deref(),
            attest.as_deref(),
            ran.as_deref().and_then(|r| r.parse().ok()),
            rehead.as_deref(),
            *host_only,
            LeaseCheck::Check(lease_token.as_deref()),
        ),
        Round::Eject {
            by,
            batch,
            id,
            repo,
            reason,
            suites,
            red,
            harness_fault,
            rebuild,
            with,
            lease_token,
        } => eject(
            w,
            by,
            batch,
            id,
            repo.as_deref(),
            reason,
            suites,
            *red,
            *harness_fault,
            *rebuild,
            with,
            LeaseCheck::Check(lease_token.as_deref()),
        ),
        Round::Land {
            by,
            batch,
            repo,
            lease_token,
        } => land(w, by, batch, repo.as_deref(), lease_token.as_deref()),
        Round::Abandon {
            by,
            batch,
            repo,
            reason,
            lease_token,
            cause,
        } => abandon(
            w,
            by,
            batch,
            repo.as_deref(),
            reason,
            lease_token.as_deref(),
            cause,
        ),
        Round::SweepLocks { repo, all } => sweep_locks(w, repo.as_deref(), *all),
        Round::Stop {
            repo,
            by,
            reason,
            until,
        } => stop(w, repo.as_deref(), by, reason, until),
        Round::Start { repo, by, reason } => start(w, repo.as_deref(), by, reason),
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
            by,
            evidence,
            evidence_tip,
            members,
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
            members,
            super::mend::HandArgs {
                by: by.clone(),
                evidence: evidence.clone(),
                evidence_tip: evidence_tip.clone(),
            },
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
            by,
            repo,
            members,
            name,
            worktree,
        } => stage::stage(
            w,
            by,
            repo.as_deref(),
            members,
            name.as_deref(),
            worktree.as_deref(),
        ),
        Round::StageTest { by, repo } => stage::stage_test(w, by, repo.as_deref()),
        Round::Promote { by, repo } => stage::promote(w, by, repo.as_deref()),
        Round::Discard { by, repo, reason } => stage::discard(w, by, repo.as_deref(), reason),
    }
}

#[derive(Clone, Copy)]
struct Timings {
    suites_s: u64,
    build_s: u64,
}

/// Where a pass ran and the decision that put it there, as the round VM recorded it beside
/// `runner.meta`; `changed` is `prev->now` when the round's previous pass ran elsewhere.
struct Runner {
    place: String,
    why: String,
    changed: Option<String>,
}

enum Verdict<'a> {
    Green(Timings, Option<&'a Runner>),
    Red(&'a [String], Timings, Option<&'a Runner>),
    Incomplete(&'a str),
}

/// One batch event chosen from the round's current (state, pass, phase); `None` sends nothing.
fn pass_event<F>(w: &World, batch: &str, kind: F) -> Result<(), (i32, String)>
where
    F: FnOnce(&str, u32, &str) -> Option<String>,
{
    pass_event_with(w, batch, &Ride::none(), kind)
}

/// [`pass_event`], the event carrying `ride`'s fields.
fn pass_event_with<F>(w: &World, batch: &str, ride: &Ride, kind: F) -> Result<(), (i32, String)>
where
    F: FnOnce(&str, u32, &str) -> Option<String>,
{
    lc_cas(w, batch, |s, v| {
        let (n, phase) =
            w.lc.batch_pass(batch)
                .ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        match kind(s, n, &phase) {
            Some(k) => ride.sent(w.lc.batch_event(batch, s, v, LC_ACTOR, &k, ride.fields())),
            None => Ok(()),
        }
    })
}

/// `strict` is a hand verb: it always sends the event, so the machine refuses a wrong-phase call
/// and names the phase. certify and `pass-start --resume` are not strict: they resume a pass already recorded.
/// A judged round (ATTRIBUTING) starts its next pass directly; PassRebuilt is for a rebuilt head.
fn start_pass(w: &World, batch: &str, head: &str, strict: bool) -> Result<(), (i32, String)> {
    start_pass_with(w, batch, head, strict, &Ride::none())
}

/// [`start_pass`], the event carrying `ride`'s fields.
fn start_pass_with(
    w: &World,
    batch: &str,
    head: &str,
    strict: bool,
    ride: &Ride,
) -> Result<(), (i32, String)> {
    pass_event_with(w, batch, ride, |s, n, _| {
        (strict || matches!(s, "OPEN" | "REBUILDING" | "ATTRIBUTING"))
            .then(|| serde_json::json!({"PassStarted": {"n": n + 1, "head": head}}).to_string())
    })
}

const STALE_TRIES: u32 = 5;

/// The round record's `pass-trees`: `<n>=<tree>` per pass, the tree the pass built from.
pub(super) const PASS_TREES: &str = "pass-trees";

/// The deny list the round's last pass ran with, kept beside its results, which the next pass wipes.
fn passed_deny_path(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.passed-deny-list"))
}

fn record_pass_tree(kv: &mut Kv, tree: &str) {
    let mut entries: Vec<String> = kv.get(PASS_TREES).unwrap_or("").split_whitespace().map(String::from).collect();
    entries.push(format!("{}={tree}", entries.len() + 1));
    set(kv, PASS_TREES, &entries.join(" "));
}

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
    pass_verdict_event_with(w, batch, verdict, strict, &Ride::none())
}

/// [`pass_verdict_event`], the verdict carrying `ride`'s fields.
fn pass_verdict_event_with(
    w: &World,
    batch: &str,
    verdict: &Verdict,
    strict: bool,
    ride: &Ride,
) -> Result<(), (i32, String)> {
    lc_cas(w, batch, |s, v| {
        if !strict && s != "CI_RUNNING" {
            return match (verdict, s) {
                (Verdict::Green(..), "GREEN") | (Verdict::Red(..) | Verdict::Incomplete(_), _) => {
                    Ok(())
                }
                (Verdict::Green(..), other) => {
                    Err((1, format!("the batch is {other}, not CI_RUNNING")))
                }
            };
        }
        let (n, _) =
            w.lc.batch_pass(batch)
                .ok_or((1, format!("spira-lc cannot say which pass {batch} is on")))?;
        let kind = match verdict {
            Verdict::Green(t, r) => {
                let mut body = serde_json::json!({"n": n, "suites_s": t.suites_s, "build_s": t.build_s});
                add_runner(&mut body, *r);
                serde_json::json!({ "PassGreen": body })
            }
            Verdict::Red(red, t, r) => {
                let mut body = serde_json::json!({"n": n, "red_suites": red, "suites_s": t.suites_s, "build_s": t.build_s});
                add_runner(&mut body, *r);
                serde_json::json!({ "PassRed": body })
            }
            Verdict::Incomplete(why) => {
                serde_json::json!({"PassIncomplete": {"n": n, "reason": why}})
            }
        };
        ride.sent(w.lc.batch_event(batch, s, v, LC_ACTOR, &kind.to_string(), ride.fields()))
    })
}

fn add_runner(body: &mut serde_json::Value, runner: Option<&Runner>) {
    let Some(r) = runner else { return };
    body["runner"] = serde_json::json!({"place": r.place, "why": r.why});
    if let Some(c) = &r.changed {
        body["runner_changed"] = serde_json::json!(c);
    }
}

/// The pass a `LateBoundary` refusal names: a boundary for a pass that already has its verdict,
/// which the machine left untouched, so the event is done rather than refused.
fn late_pass(refusal: &str) -> Option<u32> {
    let rest = &refusal[refusal.find("refused: Some(LateBoundary {")?..];
    let digits = rest[rest.rfind("pass: ")? + "pass: ".len()..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?;
    digits.parse().ok()
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
    let _g = match lock(w, label, &c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let Ok(kv) = load(w, label, &c, batch) else {
        return FAIL;
    };
    match f(&c, &kv) {
        Ok(()) => {
            w.out(format!("queue.sh {label}: recorded for {batch}"));
            OK
        }
        Err((_, out)) if late_pass(&out).is_some() => {
            let n = late_pass(&out).unwrap_or_default();
            w.out(format!(
                "queue.sh {label}: ignored: pass {n} already has its verdict"
            ));
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
    let label = "round pass-start";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() {
        return FAIL;
    }
    if let Ok(kv) = load(w, label, &c, batch) {
        let wt = worktree_of(&kv);
        let held = ejected_still_in_tree(w, &path, &wt, &kv);
        if !held.is_empty() {
            return refuse_ejected_in_tree(w, label, batch, &wt, &held);
        }
    }
    pass_verb(w, label, batch, repo, |_, kv| {
        start_pass(w, batch, kv.get("head").unwrap_or(""), !resume)
    })
}

fn suites_started_verb(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    pass_verb(w, "round suites-started", batch, repo, |_, _| {
        suites_started(w, batch, true)
    })
}

/// A suite went red mid-pass. A result arriving proves the suites phase began, so the phase is
/// advanced first; the event itself is always sent, and one for a pass already judged is ignored.
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
        let out = phase_evidence(&results, s, |o| tail(o, 80))
            .or_else(|| suite_output(&results, s, 0).map(|o| tail(&o, 80)))
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
        .or_else(|| phase_evidence(&results, suite, failure_excerpt))
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
            w.lc.create_bead_in(&id, &c.r.name);
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

/// Before a round's results directory is recreated, drops the old closed rounds' `*.results` of
/// this repo from `rounds`. When the open rounds cannot be listed nothing is removed.
fn prune_round_results(w: &World, c: &Ctx, batch: &str) {
    let mut open = match w.lc.open_batches(&c.r.name) {
        Ok(open) => open.into_iter().map(|b| b.id).collect::<Vec<_>>(),
        Err(e) => {
            w.err(format!("queue.sh: round results not pruned: the open rounds of {} cannot be listed: {e}", c.r.name));
            return;
        }
    };
    open.push(batch.to_string());
    c.s.results_retention.prune(
        "queue.sh",
        &rounds_dir(c),
        spira_config::vtime::now_epoch(),
        &spira_config::results_retention::rounds_keep(&c.r.name, &open),
    );
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
        retire_filed_mender(w, id, batch, &format!("Batch {batch} closed ({why}); the failure this mender bead was filed for ended with it."), &done);
    }
}

/// Retire one mender bead unless a session holds it or it carries work; `done` marks it retired.
pub(super) fn retire_filed_mender(w: &World, id: &str, batch: &str, reason: &str, done: &Path) {
    if w.lc.bead_row(id).is_some_and(|r| {
        r.holds.iter().any(|h| h == "ask")
            || r.tip.is_some()
            || matches!(r.state.as_str(), "SUBMITTED" | "CERTIFIED")
    }) {
        return;
    }
    match w.lib.bead_retire(id, reason) {
        Ok(()) => {
            let _ = write_atomic(done, "");
            w.out(format!("queue.sh: mender bead {id} retired — {batch}"));
        }
        Err(e) => w.err(format!("queue.sh: WARNING — mender bead {id} of {batch} could not be closed: {e}")),
    }
}

/// A failure settled by hand ends its mender bead without waiting for the batch to close.
pub(super) fn retire_failure_mender(w: &World, c: &Ctx, batch: &str, pass: u32, suite: &str, reason: &str) {
    let marker = mender_marker(&rounds_dir(c), batch, pass, suite);
    let done = marker.with_file_name(format!("{}.retired", marker.file_name().unwrap_or_default().to_string_lossy()));
    let Ok(id) = fs::read_to_string(&marker) else {
        return;
    };
    let id = id.trim();
    if !id.is_empty() && !done.exists() {
        retire_filed_mender(w, id, batch, reason, &done);
    }
}

/// Keeps the red evidence of an eject (the failing tests' names, each red suite's first FAIL line,
/// the mender's excerpt) and each red suite's whole `.out` beside it, under the send-back store,
/// which the next pass's wipe of the round's results does not reach, and names where it is kept
/// after the reason's first line.
fn with_kept_evidence(w: &World, c: &Ctx, batch: &str, reason: &str, suites: &str) -> String {
    let results = rounds_dir(c).join(format!("{batch}.results"));
    let mut text = format!("{reason}\n");
    let mut outs: Vec<(String, Vec<u8>)> = Vec::new();
    for s in suites.split(',').filter(|s| !s.is_empty()) {
        let fail = suite_output(&results, s, 0)
            .or_else(|| phase_evidence(&results, s, |o| o.to_string()))
            .and_then(|o| o.lines().find(|l| l.contains("FAIL") || l.trim_start().starts_with("not ok")).map(|l| l.trim().to_string()))
            .unwrap_or_else(|| "(no FAIL line in its output)".into());
        text.push_str(&format!("{s}: {fail}\n"));
        if let Some(e) = phase_evidence(&results, s, failure_excerpt) {
            text.push_str(&format!("{e}\n"));
        }
        let stages: Vec<String> = if s == UNIT_RED { phase_reds(&results).into_iter().map(|p| p.stage).collect() } else { vec![s.to_string()] };
        for stage in stages {
            if let Some(full) = suite_output(&results, &stage, 0) {
                let name = format!("{stage}.out");
                text.push_str(&format!("{stage}: full output kept as {name}\n"));
                outs.push((name, full.into_bytes()));
            }
        }
    }
    outs.insert(0, ("red-evidence".to_string(), text.into_bytes()));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let kept = w
        .env
        .config("SPIRA_SENDBACK_KEEP_DAYS")
        .and_then(|d| d.trim().parse::<u32>().map_err(|e| e.to_string()))
        .and_then(|days| spira_config::sendback::keep(&c.s.run, "eject", &outs, days, now))
        .map(|id| spira_config::sendback::root(&c.s.run).join(id).join("red-evidence"));
    let line = match kept {
        Ok(p) => format!("Red evidence kept at {}.", p.display()),
        Err(e) => format!("Red evidence not kept: {e}."),
    };
    match reason.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{line}\n{rest}"),
        None => format!("{reason}\n{line}"),
    }
}

/// For the `unit-tests` step, each red test phase's own output (`<stage>.out`) behind the names
/// of its failed tests; `None` when no phase record in `results` is red.
fn phase_evidence(results: &Path, suite: &str, excerpt: impl Fn(&str) -> String) -> Option<String> {
    if suite != UNIT_RED {
        return None;
    }
    let parts: Vec<String> = phase_reds(results)
        .iter()
        .map(|p| {
            let mut part = String::new();
            for l in &p.long_running {
                part.push_str(&format!("{l}\n"));
            }
            if !p.failed.is_empty() {
                part.push_str(&format!("{} failed: {}\n", p.stage, p.failed.join(", ")));
            }
            if let Some(o) = suite_output(results, &p.stage, 0) {
                part.push_str(&excerpt(&o));
            }
            part
        })
        .filter(|p| !p.trim().is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
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
        w.err("queue.sh round pass-verdict: --reason is required for an incomplete pass");
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
        "green" => Verdict::Green(t, None),
        "red" => Verdict::Red(&red, t, None),
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

const UNRECORDED: &str = "unrecorded";

/// The runner a pass's results name in `runner.place`; a results dir without it reads as
/// `unrecorded`, never as absent.
fn pass_runner(results: &Path) -> Runner {
    let text = fs::read_to_string(results.join("runner.place")).unwrap_or_default();
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    match get("place") {
        Some(place) => Runner {
            place,
            why: get("why").unwrap_or_else(|| "the pass recorded no placement decision".into()),
            changed: None,
        },
        None => Runner {
            place: UNRECORDED.into(),
            why: "the pass left no runner.place".into(),
            changed: None,
        },
    }
}

/// [`pass_runner`], naming the move when the round's previous pass ran on another runner.
fn pass_runner_in_round(w: &World, batch: &str, results: &Path) -> Runner {
    let mut r = pass_runner(results);
    let now = w.lc.batch_pass(batch).map(|(n, _)| n);
    let prev = now.and_then(|now| {
        w.lc.batch_passes(batch)
            .into_iter()
            .filter(|p| p["n"].as_u64().is_some_and(|n| (n as u32) < now))
            .max_by_key(|p| p["n"].as_u64())
            .and_then(|p| p["runner"]["place"].as_str().map(str::to_string))
    });
    r.changed = prev.filter(|p| *p != r.place && p != UNRECORDED && r.place != UNRECORDED).map(|p| format!("{p}->{}", r.place));
    r
}

fn runner_words(r: &Runner) -> String {
    let moved = r.changed.as_ref().map(|c| format!(" runner_changed={c}")).unwrap_or_default();
    format!(" runner={} ({}){moved}", r.place, r.why)
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

/// The queue lock, with every record of an ended round swept away under it.
pub(super) fn lock(w: &World, label: &str, c: &Ctx) -> Result<Option<Guard>, i32> {
    lock_sparing(w, label, c, None)
}

/// [`lock`], leaving the record of `spare` in place even if its round has ended.
fn lock_sparing(
    w: &World,
    label: &str,
    c: &Ctx,
    spare: Option<&str>,
) -> Result<Option<Guard>, i32> {
    let g = if lock_held_by_caller(w) {
        None
    } else {
        take_lock(w, label, c, "")?
    };
    sweep(w, c, spare);
    Ok(g)
}

/// The record of the batch a mender reports on: the open round of a queue.local repo, or the open
/// batch PR of a queue.forge one.
pub(super) fn load_failed_batch(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    if c.r.mode == LandMode::QueueLocal {
        return load(w, label, c, batch);
    }
    match super::forge_batch::read(w, c) {
        Ok(Some(o)) if o.kv.get("batch_id") == Some(batch) => Ok(o.kv),
        Ok(_) => load(w, label, c, batch),
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether {batch} is {}'s open batch: {e} — refused, nothing changed", c.r.name));
            Err(FAIL)
        }
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

/// The open rounds of the repo, oldest first: the control store's when the batch machine is on
/// it, the round files' until then. Err: the store could not say.
pub(crate) fn live_records(w: &World, c: &Ctx) -> Result<Vec<Kv>, String> {
    if w.lc.rounds_door().is_none() {
        return Ok(round_files::live_records(w, c, round_files::keep)
            .into_iter()
            .map(|(_, kv)| kv)
            .collect());
    }
    // A queue.forge repo's batches are batch PRs, not rounds.
    if c.r.mode != LandMode::QueueLocal {
        return Ok(Vec::new());
    }
    Ok(w.lc
        .open_batches(&c.r.name)?
        .into_iter()
        .filter(|b| ended(&b.kv).is_none())
        .map(|b| b.kv)
        .collect())
}

/// Why the queue ended round `kv` while the lifecycle had not (its abandon or its landing
/// record was refused); `None` while the queue holds it open. Such a round is no reader's open
/// round: it counts toward no limit and is no round's parent.
fn ended(kv: &Kv) -> Option<&str> {
    kv.get(ENDED).filter(|e| !e.is_empty())
}

const ENDED: &str = "ended";

/// The rounds of the repo the queue has ended and the lifecycle has not, oldest first; none
/// with the round files, which the queue removes.
fn ended_rounds(w: &World, c: &Ctx) -> Result<Vec<BatchRecord>, String> {
    if w.lc.rounds_door().is_none() || c.r.mode != LandMode::QueueLocal {
        return Ok(Vec::new());
    }
    Ok(w.lc
        .open_batches(&c.r.name)?
        .into_iter()
        .filter(|b| ended(&b.kv).is_some())
        .collect())
}

/// Names each round the queue ended that the lifecycle still holds, so it is never silently
/// open on one side and gone on the other.
fn report_ended(w: &World, label: &str, c: &Ctx) {
    match ended_rounds(w, c) {
        Ok(all) => {
            for b in all {
                w.err(format!("queue.sh {label}: round {} ended ({}) but spira-lc holds it {} — it is not open; round open and round land retry recording its end", b.id, ended(&b.kv).unwrap_or(""), b.state));
            }
        }
        Err(e) => w.err(format!(
            "queue.sh {label}: cannot tell which ended rounds spira-lc still holds for {}: {e}",
            c.r.name
        )),
    }
}

/// Retries the lifecycle end of each round the queue ended without one: an abandon refused for
/// an emptied round is asked again. A landing is recorded by [`reconcile_unrecorded`].
fn reconcile_ended(w: &World, c: &Ctx) {
    let all = match ended_rounds(w, c) {
        Ok(all) => all,
        Err(e) => {
            w.err(format!(
                "queue.sh: cannot tell which ended rounds spira-lc still holds for {}: {e}",
                c.r.name
            ));
            return;
        }
    };
    for b in all.iter().filter(|b| ended(&b.kv) != Some(END_LANDED)) {
        let cause = ended(&b.kv).unwrap_or("");
        match lc_abandon(w, &b.id, LC_ACTOR, &bounded_text(&format!("round ended: {cause}"))) {
            Ok(()) => {
                crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, &b.id);
                w.out(format!("queue.sh: round {} ended ({cause}) with its abandon refused — abandoned now", b.id));
            }
            Err((rc, out)) => w.err(format!("queue.sh: WARNING — the abandon of the ended round {} is still refused (rc={rc}): {}", b.id, one_line(&out))),
        }
    }
}

const END_LANDED: &str = "landed";

/// Round `batch`'s record while it is open; `Ok(None)` once it has ended or never was.
pub(super) fn record_of(w: &World, c: &Ctx, batch: &str) -> Result<Option<Kv>, String> {
    if w.lc.rounds_door().is_none() {
        return Ok(round_files::record_of(w, c, batch).map(|(_, kv)| kv));
    }
    Ok(w.lc
        .batch(&c.r.name, batch)?
        .filter(|b| !b.is_terminal() && ended(&b.kv).is_none())
        .map(|b| b.kv))
}

/// The record of a round the lifecycle has already abandoned, for the verb that finishes
/// closing it. Err: the store could not say.
fn abandoned_record(w: &World, c: &Ctx, batch: &str) -> Result<Option<Kv>, String> {
    if !batch_abandoned(w, batch) {
        return Ok(None);
    }
    if w.lc.rounds_door().is_none() {
        return Ok(round_files::ended_record(w, c, batch));
    }
    Ok(w.lc.batch(&c.r.name, batch)?.map(|b| b.kv))
}

pub(super) fn save_record(w: &World, c: &Ctx, kv: &Kv) -> Result<(), String> {
    if w.lc.rounds_door().is_none() {
        return round_files::save_record(w, c, kv);
    }
    w.lc.save_batch(&c.r.name, kv)
}

/// Round `batch` answers no reader from here on, ended for `cause`. On the store the machine's
/// terminal state says so; when the lifecycle refused that end, the record says it instead
/// ([`ended`]) until a retry records it. The round files are removed. The caller holds the
/// queue lock. Err: the end could not be written, and the round still reads as open.
pub(super) fn drop_record(w: &World, c: &Ctx, batch: &str, cause: &str) -> Result<(), String> {
    if w.lc.rounds_door().is_none() {
        round_files::drop_record(w, c, batch);
        return Ok(());
    }
    // A queue.forge repo's batches are batch PRs, not rounds.
    if c.r.mode != LandMode::QueueLocal {
        return Ok(());
    }
    let Some(b) = w.lc.batch(&c.r.name, batch)? else {
        return Ok(());
    };
    // The first end recorded stands: a landed round is never re-marked abandoned.
    if b.is_terminal() || ended(&b.kv).is_some() {
        return Ok(());
    }
    let mut kv = b.kv;
    set(&mut kv, ENDED, cause);
    set(&mut kv, "ended_at", &w.clock.now().to_string());
    w.lc.save_batch(&c.r.name, &kv)
}

/// [`drop_record`], its failure named on stderr.
fn drop_or_warn(w: &World, label: &str, c: &Ctx, batch: &str, cause: &str) {
    if let Err(e) = drop_record(w, c, batch, cause) {
        w.err(format!("queue.sh {label}: ALARM: round {batch} ended ({cause}) but its end could not be recorded: {e} — it still reads as open"));
    }
}

/// Closes out the rounds that ended without a verb closing them (worktree, running marker), but
/// `spare`'s. The caller holds the queue lock.
fn sweep(w: &World, c: &Ctx, spare: Option<&str>) {
    if w.lc.rounds_door().is_none() {
        return round_files::sweep(w, c, spare);
    }
    let Ok(open) = w.lc.open_batches(&c.r.name) else {
        return;
    };
    let mut kept: Vec<&BatchRecord> = open.iter().collect();
    let spared;
    if let Some(id) = spare.filter(|id| !open.iter().any(|b| b.id == *id)) {
        let Ok(found) = w.lc.batch(&c.r.name, id) else {
            return;
        };
        spared = found;
        kept.extend(spared.iter());
    }
    let held: Vec<PathBuf> = kept.iter().map(|b| worktree_of(&b.kv)).collect();
    let mut marked: Vec<String> = fs::read_dir(rounds_dir(c))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".running").map(str::to_string)
        })
        .filter(|id| Some(id.as_str()) != spare && !open.iter().any(|b| b.id == *id))
        .collect();
    marked.sort();
    for id in &marked {
        let Ok(found) = w.lc.batch(&c.r.name, id) else {
            return;
        };
        let Some(b) = found.filter(|b| b.is_terminal()) else {
            continue;
        };
        let wt = worktree_of(&b.kv);
        let has_tree = !wt.as_os_str().is_empty() && wt.exists();
        let own_tree = has_tree && !held.contains(&wt);
        if let Err(e) = w.scripts.stop_round_legs(&c.r.name, &b.id) {
            w.err(format!("queue.sh sweep: round {} ended but a leg still runs ({e}) — its tree is kept", b.id));
            continue;
        }
        if let (Some(repo), true) = (&c.r.path, own_tree) {
            w.git.worktree_remove(repo, &wt);
        }
        finish(
            w,
            c,
            &b.id,
            End::Abandoned("swept"),
            &format!("ended: {} on the lifecycle; record swept", b.state),
        );
    }
    let Some(repo) = &c.r.path else {
        return;
    };
    for b in open
        .iter()
        .filter(|b| ended(&b.kv).is_none() && Some(b.id.as_str()) != spare)
    {
        let wt = worktree_of(&b.kv);
        if wt.as_os_str().is_empty() || running_marker(c, &b.id).exists() || wt.exists() {
            continue;
        }
        close_abandoned(
            w,
            HOUSEKEEPING,
            "sweep",
            c,
            repo,
            &b.kv,
            "the round's worktree is missing, so it cannot certify",
            "worktree-missing",
        );
    }
}

pub(super) fn load(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    let live = live_records(w, c).map_err(|e| {
        w.err(format!(
            "queue.sh {label}: cannot tell which rounds are open for {}: {e}",
            c.r.name
        ));
        FAIL
    })?;
    if let Some(kv) = live.iter().find(|kv| kv.get("batch_id") == Some(batch)) {
        return Ok(kv.clone());
    }
    let open: Vec<String> = live
        .iter()
        .map(|kv| kv.get("batch_id").unwrap_or("?").to_string())
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

/// The queue's fields of a round on their way to the store with the lifecycle step they
/// accompany, so they commit in that step's txn. With the round files, or when no step carried
/// them (it was refused, or none was sent), [`Ride::save_rest`] writes them on their own.
pub(super) struct Ride<'a> {
    kv: Option<&'a Kv>,
    carried: Cell<bool>,
}

impl<'a> Ride<'a> {
    /// No fields: the step carries what the round already holds.
    pub(super) fn none() -> Self {
        Ride {
            kv: None,
            carried: Cell::new(false),
        }
    }

    pub(super) fn with(w: &World, kv: &'a Kv) -> Self {
        Ride {
            kv: w.lc.rounds_door().is_some().then_some(kv),
            carried: Cell::new(false),
        }
    }

    /// What a step is sent with: `--round` on the store, nothing with the round files.
    pub(super) fn fields(&self) -> Option<&'a Kv> {
        self.kv
    }

    /// `sent`, the answer to a step sent with [`Ride::fields`], recorded.
    pub(super) fn sent<T, E>(&self, sent: Result<T, E>) -> Result<T, E> {
        if sent.is_ok() && self.kv.is_some() {
            self.carried.set(true);
        }
        sent
    }

    /// Writes `kv`'s fields unless a step already committed them.
    pub(super) fn save_rest(&self, w: &World, label: &str, c: &Ctx, kv: &Kv) -> Result<(), i32> {
        if self.carried.get() {
            return Ok(());
        }
        save(w, label, c, kv)
    }
}

fn save(w: &World, label: &str, c: &Ctx, kv: &Kv) -> Result<(), i32> {
    save_record(w, c, kv).map_err(|e| {
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

fn running_marker(c: &Ctx, batch: &str) -> PathBuf {
    rounds_dir(c).join(format!("{batch}.running"))
}

pub(super) fn mark_running(c: &Ctx, batch: &str) {
    let _ = fs::create_dir_all(rounds_dir(c));
    let _ = fs::write(running_marker(c, batch), "");
}

/// Why a round ended: the cause of its `.result` line. The kinds are a closed set; a word a
/// kind carries is reduced to one token, so the line's second field always parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End<'a> {
    Landed,
    DeployFault,
    Red(&'a str),
    Fault(&'a str),
    Held(&'a str),
    Abandoned(&'a str),
}

/// The cause kinds, each as it opens the cause field.
pub const END_KINDS: [&str; 6] = [
    "landed",
    "deploy-fault",
    "red:",
    "fault:",
    "held:",
    "abandoned:",
];

/// A word reduced to one lower-case token of `[a-z0-9._-]`; empty becomes `unspecified`.
pub fn cause_word(w: &str) -> String {
    let t: String = w
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let t = t.trim_matches('-').to_string();
    if t.is_empty() {
        "unspecified".into()
    } else {
        t
    }
}

impl End<'_> {
    pub fn cause(&self) -> String {
        let with = |kind: &str, w: &str| format!("{kind}:{}", cause_word(w));
        match self {
            End::Landed => "landed".into(),
            End::DeployFault => "deploy-fault".into(),
            End::Red(step) => with("red", step),
            End::Fault(why) => with("fault", why),
            End::Held(by) => with("held", by),
            End::Abandoned(why) => with("abandoned", why),
        }
    }
}

/// Appends `<UTC ISO-8601> <cause> <detail>` to `rounds/<batch>.result`, the stamp from the world's clock.
pub(super) fn finish(w: &World, c: &Ctx, batch: &str, end: End, detail: &str) {
    use std::io::Write;
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{batch}.result")))
    {
        let _ = writeln!(
            f,
            "{} {} {}",
            tsd::iso_utc(w.clock.now()),
            end.cause(),
            one_line(detail)
        );
    }
    let _ = fs::remove_file(running_marker(c, batch));
}

/// `round sweep-locks`: removes the certify lock of every batch spira-lc reports terminal.
fn sweep_locks(w: &World, repo: Option<&str>, all: bool) -> i32 {
    let label = "round sweep-locks";
    let names = if all {
        match w.lib.repos() {
            Ok(n) => n,
            Err(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot list the registered repositories: {e}"
                ));
                return FAIL;
            }
        }
    } else {
        vec![repo.map(str::to_string).unwrap_or_default()]
    };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let mut rc = OK;
    for name in names {
        let repo = Some(name.as_str()).filter(|n| !n.is_empty());
        if all && resolve(w, label, repo).is_ok_and(|c| c.r.mode != LandMode::QueueLocal) {
            continue;
        }
        let Ok((c, _)) = local_ctx(w, label, repo) else {
            rc = FAIL;
            continue;
        };
        let (mut removed, mut kept) = (0, 0);
        for batch in crate::lock::certify_lock_batches(&c.s.queue_dir, &c.r.name) {
            let ended =
                w.lc.batch_state(&batch)
                    .is_some_and(|(s, _)| super::reconcile::terminal(&s));
            if ended && crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, &batch) {
                removed += 1;
            } else {
                kept += 1;
            }
        }
        w.out(format!(
            "queue.sh {label}: {}: removed {removed} certify lock(s) of ended rounds, kept {kept}",
            c.r.name
        ));
    }
    rc
}

pub(super) fn csv(ms: &[Member]) -> String {
    ms.iter().map(Member::render).collect::<Vec<_>>().join(",")
}

pub(super) fn worktree_of(kv: &Kv) -> PathBuf {
    PathBuf::from(kv.get("worktree").unwrap_or(""))
}

/// Removes `ending`'s worktree `wt` unless a round still open on the repo records the same path:
/// rounds share a path (`.round-<repo>` by default, the batcher's one tree), and an ended round's
/// tree is then the live round's. When the open rounds cannot be read the tree stays.
fn remove_unheld_tree(w: &World, c: &Ctx, path: &Path, wt: &Path, ending: &str) {
    let held = live_records(w, c).map(|open| {
        open.iter()
            .filter(|kv| kv.get("batch_id") != Some(ending))
            .map(worktree_of)
            .collect::<Vec<_>>()
    });
    match held {
        Ok(held) if !held.iter().any(|h| h == wt) => match w.scripts.stop_tree_holders(wt) {
            Ok(stopped) => {
                if !stopped.is_empty() {
                    w.out(format!("queue.sh: round {ending} ended; stopped {} still working in {}", stopped.join(", "), wt.display()));
                }
                w.git.worktree_remove(path, wt)
            }
            Err(e) => w.err(format!(
                "queue.sh: round {ending} ended; its worktree {} is kept — {e}",
                wt.display()
            )),
        },
        Ok(_) => w.out(format!(
            "queue.sh: round {ending} ended; its worktree {} is kept — an open round stands in it",
            wt.display()
        )),
        Err(e) => w.err(format!(
            "queue.sh: round {ending} ended; its worktree {} is kept — the open rounds cannot be read: {e}",
            wt.display()
        )),
    }
}

/// The round's tree rebuilt from `base_sha` with `members` merged in order, in `wt`. A
/// member that no longer merges leaves `wt` as it was (at `fallback`) and is the Err.
pub(super) fn merge_member(w: &World, c: &Ctx, wt: &Path, id: &str, tip: &str) -> bool {
    w.git.merge_no_ff(
        wt,
        &w.lib.land_subject(id),
        tip,
        &c.s.git_name,
        &c.s.git_email,
    )
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
    if let Err(e) = w.git.worktree_reset(path, wt, base_sha) {
        let _ = w.git.worktree_reset(path, wt, fallback);
        return Err(format!("cannot create the round worktree: {e}"));
    }
    for m in members {
        if !merge_member(w, c, wt, &m.id, &m.tip) {
            w.git.merge_abort(wt);
            let _ = w.git.worktree_reset(path, wt, fallback);
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

/// The round-recipe inputs (`spira_config::recipe::PATHS`) `tip` changes against `base`.
fn recipe_inputs_touched(w: &World, repo: &Path, base: &str, tip: &str) -> Vec<String> {
    let from = w
        .git
        .merge_base(repo, base, tip)
        .unwrap_or_else(|| base.to_string());
    w.git
        .numstat(repo, &from, tip)
        .unwrap_or_default()
        .into_iter()
        .map(|(p, ..)| p)
        .filter(|p| spira_config::recipe::touches(std::slice::from_ref(p)))
        .collect()
}

/// The testenv tag inputs `tip` changes against `base`.
fn tag_inputs_touched(w: &World, repo: &Path, base: &str, tip: &str) -> Vec<String> {
    let from = w.git.merge_base(repo, base, tip).unwrap_or_else(|| base.to_string());
    w.git
        .numstat(repo, &from, tip)
        .unwrap_or_default()
        .into_iter()
        .map(|(p, ..)| p)
        .filter(|p| spira_config::recipe::touches_tag_input(std::slice::from_ref(p)))
        .collect()
}

fn tag_inputs_touched_recipe(w: &World, repo: &Path, base: &str, tip: &str) -> bool {
    !recipe_inputs_touched(w, repo, base, tip).is_empty()
}

/// A member whose tip changes the testenv image tag and whose image neither this host's store
/// nor a round AMI holds: no pass could run it, and no pass builds one. Its skip names the tag
/// and the command that builds it by hand.
fn image_absent(w: &World, repo: &Path, wt: &Path, base: &str, m: &Member) -> Option<Skip> {
    if tag_inputs_touched(w, repo, base, &m.tip).is_empty() {
        return None;
    }
    let tag = match w.scripts.image_check(repo, &m.tip) {
        ImageCheck::Present => return None,
        ImageCheck::Absent(tag) => tag,
        ImageCheck::Unreadable(e) => {
            w.err(format!("queue.sh round open: cannot check {}'s testenv image ({e}) — admitted; the pass refuses an absent image", m.id));
            return None;
        }
    };
    // A tip that changes the recipe has no checkout here to hash, so its image cannot be looked up.
    let held_by_ami = !tag_inputs_touched_recipe(w, repo, base, &m.tip)
        && w.scripts
            .recipe_sha(Some(wt))
            .is_some_and(|sha| matches!(w.scripts.ami_for(&sha), AmiLookup::Found(_)));
    if held_by_ami {
        return None;
    }
    Some(skip(
        &m.id,
        "image-absent",
        format!(
            "{}: {} needs testenv image localhost/spira-testenv:{tag}, which neither this host nor a round AMI holds, and no round builds one — not admitted; build it from a checkout of {} with `{}`",
            m.id, m.tip, m.tip, spira_config::recipe::HAND_BUILD
        ),
    ))
}

/// One named member `round open` did not admit: its id, the kind of refusal (a word a caller
/// acts on, law-a-matcher-reads-code-not-prose) and the operator's line.
pub struct Skip {
    pub id: String,
    pub kind: &'static str,
    pub why: String,
}

fn skip(id: &str, kind: &'static str, why: String) -> Skip {
    Skip {
        id: id.to_string(),
        kind,
        why,
    }
}

/// The typed record a refused open prints on stdout for each member it did not admit, one per
/// line: `refused=<id> kind=<kind> why=<line>`. A caller re-cuts without these members.
pub fn refused_record(s: &Skip) -> String {
    format!("refused={} kind={} why={}", s.id, s.kind, s.why)
}

fn refuse_open(w: &World, label: &str, repo: &str, named: usize, skips: &[Skip]) -> i32 {
    w.err(format!(
        "queue.sh {label}: refused — {} of {named} named members cannot be admitted to a round for {repo}; no round opened (members already returned to rework, and integrations already filed, stay so):",
        skips.len()
    ));
    for s in skips {
        w.err(format!("queue.sh {label}:   {}", s.why));
        w.out(refused_record(s));
    }
    FAIL
}

/// Milliseconds each step of an open took, read off the world's clock.
struct Steps<'a> {
    w: &'a World<'a>,
    last: std::cell::Cell<u64>,
    done: std::cell::RefCell<Vec<String>>,
}

impl<'a> Steps<'a> {
    fn new(w: &'a World<'a>) -> Self {
        Steps { w, last: w.clock.now_ms().into(), done: Default::default() }
    }
    fn lap(&self, step: &str) {
        let at = self.w.clock.now_ms();
        let took = at.saturating_sub(self.last.replace(at));
        self.done.borrow_mut().push(format!("{step}:{took}"));
    }
    fn field(&self) -> String {
        self.done.borrow().join(",")
    }
}

fn open(
    w: &World,
    by: &str,
    repo: Option<&str>,
    members_arg: &Text,
    name: Option<&str>,
    worktree: Option<&Path>,
) -> i32 {
    let steps = Steps::new(w);
    let rc = open_timed(w, by, repo, members_arg, name, worktree, &steps);
    if rc != OK && !steps.done.borrow().is_empty() {
        w.err(format!("steps={}", steps.field()));
    }
    rc
}

fn open_timed(
    w: &World,
    by: &str,
    repo: Option<&str>,
    members_arg: &Text,
    name: Option<&str>,
    worktree: Option<&Path>,
    steps: &Steps,
) -> i32 {
    let label = "round open";
    let text = match read_text(w, members_arg) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members: {e}"));
            return USAGE;
        }
    };
    if let Text::Arg(a) = members_arg {
        if a.trim().contains(char::is_whitespace) {
            w.err(format!("queue.sh {label}: --members takes one comma-separated value, but {a:?} holds whitespace — a space-separated list names one member, not several; refused, nothing changed"));
            return USAGE;
        }
    }
    let mut wanted: Vec<Member> = Vec::new();
    let named = parse_members(&text);
    if let Some(bad) = named.iter().find(|m| m.tip.contains(':')) {
        w.err(format!("queue.sh {label}: {}:{} holds a second colon — one id:tip per member, separated by commas; refused, nothing changed", bad.id, bad.tip));
        return USAGE;
    }
    for m in named {
        match wanted.iter_mut().find(|h| h.id == m.id) {
            Some(h) if h.tip.starts_with(&m.tip) => {}
            Some(h) if m.tip.starts_with(&h.tip) => h.tip = m.tip,
            Some(h) => {
                w.err(format!(
                    "queue.sh {label}: {} is named at two tips ({} and {}) — a round holds one version of a bead; refused",
                    m.id,
                    h.tip, m.tip
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
    steps.last.set(w.clock.now_ms());
    reconcile_unrecorded(w, &c, &path);
    reconcile_ended(w, &c);
    steps.lap("reconcile");
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
    let locked = lock(w, label, &c);
    steps.lap("lock");
    let _g = match locked {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let open_now = match live_records(w, &c) {
        Ok(open) => open,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which rounds are open for {repo_name}: {e} — refused, nothing cut"));
            return FAIL;
        }
    };
    let limit = c.s.rounds_max.max(1);
    if open_now.len() >= limit {
        let held: Vec<String> = open_now
            .iter()
            .map(|kv| {
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
        if let Some(kv) = open_now
            .iter()
            .find(|kv| kv.members().iter().any(|h| h.id == m.id))
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
        .filter_map(|kv| {
            Some((
                kv.get("batch_id")?.to_string(),
                kv.get("head")?.to_string(),
                kv.get("opened")?.parse::<u64>().unwrap_or(0),
            ))
        })
        .max_by_key(|(_, _, at)| *at)
        .filter(|(_, head, _)| !head.is_empty());
    let mut below = parent.as_ref().map(|(p, _, _)| p.clone());
    let mut seen: Vec<String> = Vec::new();
    while let Some(p) = below.take().filter(|p| !seen.contains(p)) {
        let Some(kv) = open_now
            .iter()
            .find(|kv| kv.get("batch_id") == Some(p.as_str()))
        else {
            break;
        };
        if let Some(why) = unstackable(w, kv) {
            let newest = parent.as_ref().map_or("", |(n, _, _)| n.as_str());
            let via = if p == newest {
                String::new()
            } else {
                format!(", under the newest open round {newest},")
            };
            w.err(format!("queue.sh {label}: open round {p}{via} is {why} — a round is cut only on {base} or on a live round still certifying; refused, nothing changed"));
            return FAIL;
        }
        below = kv
            .get("parent")
            .filter(|q| !q.is_empty())
            .map(str::to_string);
        seen.push(p);
    }
    let base_sha = parent
        .as_ref()
        .map_or_else(|| landed_sha.clone(), |(_, head, _)| head.clone());
    if let Ok(Some(orphan)) = records::read_kv(&c.queue_file(stage::STAGED)) {
        let parent = orphan.get("parent").unwrap_or("?").to_string();
        if open_now
            .iter()
            .all(|kv| kv.get("batch_id") != Some(parent.as_str()))
        {
            stage::discard_behind(
                w,
                by,
                &c,
                &path,
                &parent,
                "round open cut a round of its own: the round it followed is gone",
            );
        }
    }

    let named_count = wanted.len();
    let mut skips: Vec<Skip> = Vec::new();
    let mut admitted: Vec<Member> = Vec::new();
    let named: Vec<String> = wanted.iter().map(|m| m.id.clone()).collect();
    let mut blockers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut provisional: Vec<String> = Vec::new();
    for m in wanted {
        if let Some(by) = super::integrate::carried_by(w, &c, &m.id) {
            skips.push(skip(
                &m.id,
                "integration",
                format!(
                    "{}: carried by integration {by} — lands with it, not admitted",
                    m.id
                ),
            ));
            continue;
        }
        match w.lc.bead_row(&m.id) {
            None => skips.push(skip(&m.id, "no-row", format!("{}: no lifecycle row (spira-lc could not say) — not admitted", m.id))),
            // SUBMITTED or CERTIFIED (law-a-round-is-feature-first-then-catch-all): the round's own
            // full suite is the certification, so a submitted tip need not pass a per-bead gate first.
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => {
                skips.push(skip(&m.id, "state", format!("{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted", m.id, r.state)))
            }
            Some(r) if r.state == "SUBMITTED" && submitted_hold(&r, &m.id, by).is_some() => {
                skips.push(skip(&m.id, "held", submitted_hold(&r, &m.id, by).unwrap_or_default()))
            }
            Some(r) => match r.tip.clone().filter(|t| !t.is_empty()) {
                Some(tip) if r.ejected_red_tip.as_deref() == Some(tip.as_str()) => {
                    skips.push(skip(&m.id, "ejected-red", format!("{}: {tip} is a recorded ejected-red tip — not admitted until the tip changes or `queue absolve {} --evidence-tip {tip}` withdraws a red that was not the tree's fault", m.id, m.id)))
                }
                Some(tip) if tip.starts_with(&m.tip) && batch_red_at(&c, &m.id, &tip) => {
                    skips.push(skip(&m.id, "batch-red", format!("{}: {tip} already went red in a batch — not admitted until the tip changes", m.id)))
                }
                Some(tip) if tip.starts_with(&m.tip) => match r.blocked_by.iter().find(|d| !named.contains(d)) {
                    Some(dep) => {
                        let state = w.lc.bead_row(dep).map(|d| d.state).unwrap_or_else(|| "unknown".into());
                        skips.push(skip(&m.id, "blocked", format!("{}: depends on {dep} ({state}) — not admitted until it lands", m.id)))
                    }
                    None if super::simple::lands_by_lint(w, &c.s, &c.r.name, &path, &base, &tip) => {
                        skips.push(skip(&m.id, "text-only", crate::text_only::skip_line(&m.id)))
                    }
                    None => {
                        if r.state == "SUBMITTED" {
                            provisional.push(m.id.clone());
                        }
                        blockers.insert(m.id.clone(), r.blocked_by.clone());
                        admitted.push(Member { id: m.id, tip })
                    }
                },
                Some(tip) => skips.push(skip(&m.id, "tip-moved", format!("{}: {} is not its row's tip {tip} — not admitted", m.id, m.tip))),
                None => skips.push(skip(&m.id, "no-tip", format!("{}: no submitted tip — not admitted", m.id))),
            },
        }
    }

    steps.lap("admit");
    if !skips.is_empty() {
        return refuse_open(w, label, &repo_name, named_count, &skips);
    }
    admitted = blocker_first(admitted, &blockers);

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
        steps.lap("worktree");
        w.err(format!(
            "queue.sh {label}: cannot create the round worktree {}",
            wt.display()
        ));
        return FAIL;
    }
    steps.lap("worktree");
    let certified: BTreeMap<String, String> = admitted
        .iter()
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect();
    let mut merged: Vec<Member> = Vec::new();
    let mut collided: Vec<(Member, String)> = Vec::new();
    for m in admitted {
        if !super::simple::lint_passed(w, label, &path, &m.tip, false) {
            skips.push(skip(
                &m.id,
                "lint",
                format!(
                    "{}: {} fails its compile-and-lint check — not admitted",
                    m.id, m.tip
                ),
            ));
            steps.lap(&format!("merge.{}", m.id));
            continue;
        }
        if let Some(sk) = image_absent(w, &path, &wt, &landed_sha, &m) {
            skips.push(sk);
            steps.lap(&format!("merge.{}", m.id));
            continue;
        }
        let member = m.id.clone();
        if merge_member(w, &c, &wt, &m.id, &m.tip) {
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            if let Some(paths) = w.git.base_conflict_paths(&path, &landed_sha, &m.tip) {
                let paths = if paths.is_empty() { "unknown".to_string() } else { paths.join(" ") };
                skips.push(skip(
                    &m.id,
                    "base-conflict",
                    match return_to_rework(w, &m) {
                        Ok(()) => {
                            w.lib.comment(
                                &m.id,
                                &format!("rebase-conflict: does not merge onto {landed_sha}\npaths: {paths}"),
                            );
                            format!("{}: conflicts with base — returned to rework — paths: {paths}", m.id)
                        }
                        Err(e) => format!("{}: conflicts with base ({e})", m.id),
                    },
                ));
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
                        skips.push(skip(&m.id, "round-conflict", why.clone()));
                        collided.push((m, why));
                    }
                    Err(paths) => {
                        let why = format!(
                            "{}: conflicts with the round, and with a rebase onto its head — paths: {}",
                            m.id,
                            if paths.is_empty() { "unknown".to_string() } else { paths.join(" ") }
                        );
                        skips.push(skip(&m.id, "round-conflict", why.clone()));
                        collided.push((m, why))
                    }
                }
            }
        }
        steps.lap(&format!("merge.{member}"));
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
        super::integrate::file(
            w, label, &c, &path, &base_sha, &involved, &source, &evidence,
        );
    }
    steps.lap("collide");
    if !skips.is_empty() {
        w.git.worktree_remove(&path, &wt);
        return refuse_open(w, label, &repo_name, named_count, &skips);
    }
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let now = w.clock.now().to_string();
    let record = |head: &str, merged: &[Member]| {
        let mut kv = Kv::default();
        kv.push("batch_id", &batch);
        kv.push("repo", &repo_name);
        kv.push("head", head);
        kv.push("base", &base_sha);
        if let Some((p, _, _)) = &parent {
            kv.push("parent", p);
        }
        kv.push("members", &render_members(merged));
        let provisional: Vec<&str> = merged.iter().map(|m| m.id.as_str()).filter(|id| provisional.iter().any(|p| p == id)).collect();
        if !provisional.is_empty() {
            kv.push("provisional", &provisional.join(" "));
        }
        kv.push("worktree", &wt.display().to_string());
        kv.push("actor", &actor(w));
        kv.push("opened", &now);
        kv
    };
    let kv;
    let carried;
    let cut = {
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
        kv = record(&head, &merged);
        let ride = Ride::with(w, &kv);
        let cut = ride.sent(super::batch::cut_settled(
            w,
            &batch,
            &repo_name,
            &head,
            &base_sha,
            &csv(&cut_members),
            LC_ACTOR,
            ride.fields(),
        ));
        carried = ride.carried.get();
        cut
    };
    let mut carried = carried;
    if let Err((rc, out)) = cut {
        let outcome = match cut_outcome(w, &c, &batch) {
            CutOutcome::Absent if rc == 2 && w.lc.rounds_door().is_some() => {
                CutOutcome::Unknown("the store does not show it yet".into())
            }
            seen => seen,
        };
        match outcome {
            CutOutcome::Exists(stored) => carried = stored,
            CutOutcome::Absent => {
                w.err(format!(
                    "queue.sh {label}: spira-lc cut refused for {batch} (rc={rc}): {out}"
                ));
                converge_orphans(w);
                w.git.worktree_remove(&path, &wt);
                return FAIL;
            }
            CutOutcome::Unknown(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot tell whether round {batch} was cut (rc={rc}): {out}; nor read it back: {e} — its worktree {} is kept; `queue round status` says whether it exists",
                    wt.display()
                ));
                return FAIL;
            }
        }
    }

    if !carried && save(w, label, &c, &kv).is_err() {
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
    steps.lap("record");
    round_event(
        &c.s.run,
        "open",
        &repo_name,
        &batch,
        by,
        w.clock.now(),
        &[("head", head.to_string()), ("members", csv(&merged)), ("steps", steps.field())],
    );
    w.out(format!("steps={}", steps.field()));
    w.out(format!("batch={batch}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    w.out(format!("base={base_sha}"));
    w.out(format!(
        "base_of={}",
        parent
            .as_ref()
            .map_or_else(|| base.clone(), |(p, _, _)| format!("round {p}"))
    ));
    w.out(format!(
        "base_chain={}",
        first_parent_chain(w, &path, &base_sha, &landed_sha)
    ));
    OK
}

enum CutOutcome {
    /// The round is on the store; `true` when it holds the queue's fields too.
    Exists(bool),
    Absent,
    Unknown(String),
}

/// A cut that returned an error may still have written the round: ask the store, a few times, so
/// the caller is never told "refused" for a round that exists.
fn cut_outcome(w: &World, c: &Ctx, batch: &str) -> CutOutcome {
    if w.lc.rounds_door().is_none() {
        return CutOutcome::Absent;
    }
    let mut last = String::new();
    for attempt in 0..super::DEADLINE_TRIES {
        if attempt > 0 {
            w.clock.sleep(u64::from(attempt));
        }
        match w.lc.batch(&c.r.name, batch) {
            Ok(Some(b)) => return CutOutcome::Exists(!worktree_of(&b.kv).as_os_str().is_empty()),
            Ok(None) => return CutOutcome::Absent,
            Err(e) => last = e,
        }
    }
    CutOutcome::Unknown(last)
}

/// Why a round cannot be stacked on, or `None` when it is live and still certifying: a red one
/// is about to lose members, so a tree cut on its head holds code that will not land.
fn unstackable(w: &World, kv: &Kv) -> Option<String> {
    if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
        return Some(format!("red ({red})"));
    }
    match w.lc.batch_state(kv.get("batch_id").unwrap_or("")) {
        Some((s, _))
            if matches!(
                s.as_str(),
                "OPEN" | "CI_RUNNING" | "REBUILDING" | "GREEN" | "LANDING" | "DEPLOYING"
            ) =>
        {
            None
        }
        Some((s, _)) => Some(s),
        None => Some("of a state spira-lc could not say".into()),
    }
}

/// `base` and its first parents, newest first, down to the first commit `landed` already holds.
fn first_parent_chain(w: &World, path: &Path, base: &str, landed: &str) -> String {
    let range: BTreeMap<String, String> = match w.git.log_range(path, &format!("{landed}..{base}"))
    {
        Ok(v) => v
            .into_iter()
            .map(|c| (c.sha, c.parents.into_iter().next().unwrap_or_default()))
            .collect(),
        Err(e) => return format!("{base} (cannot read the chain: {})", one_line(&e)),
    };
    let mut chain = vec![base.to_string()];
    while let Some(next) = range.get(chain.last().map_or("", String::as_str)) {
        if next.is_empty() || chain.len() > range.len() {
            break;
        }
        chain.push(next.clone());
    }
    chain.join(" ")
}

fn stop(w: &World, repo: Option<&str>, by: &str, reason: &Text, until: &str) -> i32 {
    let label = "round stop";
    let Ok(c) = resolve(w, label, repo) else {
        return FAIL;
    };
    let reason = match read_text(w, reason) {
        Ok(r) => one_line(&r),
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            return FAIL;
        }
    };
    let lanes = [ROUNDS_LANE.to_string()];
    match scoped_stop::pause_repo_typed(&c.s.run, &c.r.name, &lanes, by, &reason, until, w.clock.now(), tsd::stop::Reason::Operator, None) {
        Ok(s) => {
            w.out(format!(
                "queue.sh {label}: {} rounds stopped by {} until {} — {}; an open round is not touched, a landing does not lift this; lift with: queue round start {} --by <owner> --reason <why>",
                s.repo, s.owner, s.until, s.reason, s.repo
            ));
            OK
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            FAIL
        }
    }
}

fn start(w: &World, repo: Option<&str>, by: &str, reason: &Text) -> i32 {
    let label = "round start";
    let Ok(c) = resolve(w, label, repo) else {
        return FAIL;
    };
    let reason = match read_text(w, reason) {
        Ok(r) => one_line(&r),
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            return FAIL;
        }
    };
    let Some(held) = scoped_stop::stop_for(&c.s.run, &c.r.name, ROUNDS_LANE) else {
        w.out(format!(
            "queue.sh {label}: {} has no stop of its rounds in force",
            c.r.name
        ));
        return OK;
    };
    match scoped_stop::unpause_repo(&c.s.run, &c.r.name, &[ROUNDS_LANE.to_string()]) {
        Ok(_) => {
            let lift = tsd::stop::StopRecord {
                kind: tsd::stop::Kind::Release,
                reason: tsd::stop::Reason::Operator,
                actor: by.into(),
                repo: Some(c.r.name.clone()),
                bead: None,
                detail: reason.clone(),
            };
            let now = w.clock.now();
            if let Err(e) = lift.write(&c.s.run, &tsd::default_host(), &tsd::iso_utc(now)) {
                w.err(format!("queue.sh {label}: the stop is lifted but its record failed: {e}"));
            }
            w.out(format!(
                "queue.sh {label}: {} rounds started by {by} ({reason}); the stop was set by {} until {} — {}",
                c.r.name, held.owner, held.until, held.reason
            ));
            OK
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: {e}"));
            FAIL
        }
    }
}

/// Why a SUBMITTED member with a hold is not admitted: held ask, wait or poison always, and held
/// manual when the batcher names it; a person's hand cut is the way past a manual hold.
pub(super) fn submitted_hold(r: &crate::model::LcBeadRow, id: &str, by: &str) -> Option<String> {
    let kind = r.holds.iter().find(|h| h.as_str() != "manual" || by == "batcher")?;
    Some(format!("{id}: SUBMITTED and held {kind} — not admitted until it is unheld"))
}

/// `admitted` with each member after the members of the same list that block it, the rest in
/// the order given.
pub(super) fn blocker_first(admitted: Vec<Member>, blockers: &BTreeMap<String, Vec<String>>) -> Vec<Member> {
    let mut rest = admitted;
    let mut out: Vec<Member> = Vec::with_capacity(rest.len());
    while !rest.is_empty() {
        let ready = rest
            .iter()
            .position(|m| {
                blockers.get(&m.id).into_iter().flatten().all(|b| {
                    !rest.iter().any(|r| &r.id == b) || out.iter().any(|o| &o.id == b)
                })
            })
            .unwrap_or(0);
        out.push(rest.remove(ready));
    }
    out
}

fn attribution_lines(w: &World, batch: &str, phase: &str, prefix: &str) {
    if phase != "attributing" {
        return;
    }
    let Some(a) = w
        .lc
        .batch_progress(batch)
        .and_then(|p| p.get("attribution").cloned())
    else {
        w.out(format!("{prefix}attribution_probes=0"));
        return;
    };
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let ids = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let probes = a.get("probes").and_then(|x| x.as_u64()).unwrap_or(0);
    w.out(format!("{prefix}attribution_target={}", text(&a, "target")));
    match a.get("ceiling").and_then(|x| x.as_u64()) {
        Some(c) => w.out(format!("{prefix}attribution_probes={probes}/{c}")),
        None => w.out(format!("{prefix}attribution_probes={probes}")),
    }
    w.out(format!("{prefix}attribution_suspects={}", ids(&a, "suspects")));
    if let Some(l) = a.get("last").filter(|l| l.is_object()) {
        let why = text(l, "why");
        let why = if why.is_empty() {
            String::new()
        } else {
            format!(": {}", one_line(&why))
        };
        w.out(format!(
            "{prefix}attribution_last={} {} on {} → {}{why}",
            text(l, "job"),
            text(l, "build"),
            ids(l, "members"),
            text(l, "verdict")
        ));
    }
}

fn status(w: &World, repo: Option<&str>) -> i32 {
    let label = "round status";
    let Ok(c) = resolve(w, label, repo) else {
        return FAIL;
    };
    let open = match live_records(w, &c) {
        Ok(open) => open,
        Err(e) => {
            w.err(format!(
                "queue.sh {label}: cannot tell which rounds are open for {}: {e}",
                c.r.name
            ));
            return FAIL;
        }
    };
    report_ended(w, label, &c);
    if let Some(s) = scoped_stop::stop_for(&c.s.run, &c.r.name, ROUNDS_LANE) {
        w.out(format!(
            "stop=in-force\nstop_owner={}\nstop_reason={}\nstop_until={}",
            s.owner,
            one_line(&s.reason),
            one_line(&s.until)
        ));
    }
    let Some(kv) = open.first() else {
        w.out(format!("round=none\nrepo={}\nrounds=0\nopen_batches=", c.r.name));
        return OK;
    };
    let since = |kv: &Kv, k: &str| {
        kv.get(k)
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |t| w.clock.now().saturating_sub(t))
    };
    let batches: Vec<&str> = open
        .iter()
        .map(|kv| kv.get("batch_id").unwrap_or(""))
        .collect();
    w.out(format!(
        "rounds={}\nopen_batches={}",
        open.len(),
        batches.join(" ")
    ));
    w.out("round=open");
    let phase = phase_of(w, kv.get("batch_id").unwrap_or(""));
    w.out(format!("phase={phase}"));
    attribution_lines(w, kv.get("batch_id").unwrap_or(""), &phase, "");
    for k in ["batch_id", "repo", "head", "base", "members", "worktree"] {
        w.out(format!("{k}={}", kv.get(k).unwrap_or("")));
    }
    if let Some(p) = kv.get("provisional").filter(|p| !p.is_empty()) {
        w.out(format!("provisional={p}"));
    }
    if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
        w.out(format!("red={red}"));
    }
    w.out(format!("wall_secs={}", since(kv, "opened")));
    runner_lines(w, kv.get("batch_id").unwrap_or(""), "");
    for (i, kv) in open.iter().enumerate().skip(1) {
        let n = i + 1;
        let phase = phase_of(w, kv.get("batch_id").unwrap_or(""));
        w.out(format!("round{n}=open\nround{n}_phase={phase}"));
        attribution_lines(
            w,
            kv.get("batch_id").unwrap_or(""),
            &phase,
            &format!("round{n}_"),
        );
        for k in ["batch_id", "repo", "head", "base", "members", "worktree"] {
            w.out(format!("round{n}_{k}={}", kv.get(k).unwrap_or("")));
        }
        if let Some(p) = kv.get("provisional").filter(|p| !p.is_empty()) {
            w.out(format!("round{n}_provisional={p}"));
        }
        if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
            w.out(format!("round{n}_red={red}"));
        }
        w.out(format!("round{n}_wall_secs={}", since(kv, "opened")));
        runner_lines(w, kv.get("batch_id").unwrap_or(""), &format!("round{n}_"));
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

/// `pass<n>_runner` and `pass<n>_runner_why` for each pass of the round that recorded a runner.
fn runner_lines(w: &World, batch: &str, prefix: &str) {
    for p in w.lc.batch_passes(batch) {
        let (Some(n), Some(place)) = (p["n"].as_u64(), p["runner"]["place"].as_str()) else {
            continue;
        };
        w.out(format!("{prefix}pass{n}_runner={place}"));
        if let Some(why) = p["runner"]["why"].as_str() {
            w.out(format!("{prefix}pass{n}_runner_why={}", one_line(why)));
        }
    }
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

/// Whether the entry itself wrote an `ok` verdict to `ci.result`: it finished green, so a fault in
/// the run around it is the machinery's, not the candidate's.
fn ci_result_ok(results: &Path) -> bool {
    fs::read_to_string(results.join("ci.result"))
        .is_ok_and(|t| t.split_whitespace().next() == Some("ok"))
}

/// Names the suites whose verdict cannot be read: a word that is neither a non-blocking status
/// testenv writes (`ok`, `skip`, `disabled`, `skip-req`, `quarantined-red`) nor blocking
/// (an empty file included), or a corpus suite with no result at all. A red lint or fence ends the
/// pass before any suite runs, so it is the verdict and the missing suites are not a fault.
pub(crate) fn unjudgeable(wt: &Path, found: &[(String, String)]) -> Option<String> {
    let mut bad: Vec<String> = found
        .iter()
        .filter(|(_, s)| !PASSING.contains(&s.as_str()) && !BLOCKING.contains(&s.as_str()))
        .map(|(n, s)| format!("{n} (verdict {s:?})"))
        .collect();
    let ended_early = found
        .iter()
        .any(|(n, s)| s == "red" && (n == LINT_RED || n.starts_with(FENCE_RED_PREFIX)));
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
    // Both test phases (sp-cddf76): the unit phase passed before the suites, the integration phase beside them.
    !found.is_empty()
        && unjudgeable(wt, found).is_none()
        && err.contains("UNIT-TESTS: PASS")
        && err.contains("INTEGRATION-TESTS: PASS")
}

const FAULT_KIND_FILE: &str = "round-fault-kind";

/// The detail of an `over-cap` fault kind round-vm recorded in `results`: a pass cut off by a
/// cap is named by that file, never by what round-vm printed.
fn over_cap_detail(results: &Path) -> Option<String> {
    let text = fs::read_to_string(results.join(FAULT_KIND_FILE)).ok()?;
    Some(
        text.lines()
            .next()?
            .strip_prefix("over-cap")?
            .trim()
            .to_string(),
    )
}

fn over_cap_why(detail: &str, cap: u64) -> String {
    let line = detail.trim();
    if line.is_empty() {
        format!("the suites exceeded the {cap}s cap")
    } else {
        format!("INCOMPLETE, cut off by {line}")
    }
}

fn result_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(rd) = fs::read_dir(dir) {
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        dirs.extend(subs);
    }
    dirs
}

/// A pass cut off by a cap holds no verdict: testenv's own suite cap ends the batch
/// `verdict FAULT over-cap`, whatever exit round-vm reports, and round-vm's guard exits 124/137.
/// The last `verdict` line of the pass's `run.events`, if round-vm wrote one.
fn run_events_verdict(results: &Path) -> Option<String> {
    result_dirs(results)
        .iter()
        .find_map(|d| fs::read_to_string(d.join("run.events")).ok())
        .and_then(|t| {
            t.lines()
                .filter_map(|l| {
                    l.split_once(' ')
                        .and_then(|(_, r)| r.strip_prefix("verdict "))
                })
                .next_back()
                .map(|v| v.trim().to_string())
        })
}

fn batch_over_cap(results: &Path, rc: i32) -> bool {
    let verdict = run_events_verdict(results);
    verdict.as_deref() == Some("FAULT over-cap")
        || (matches!(rc, 124 | 137) && over_cap_detail(results).is_some())
}

fn suite_cap_s(results: &Path) -> Option<String> {
    result_dirs(results).iter().find_map(|d| {
        let t = fs::read_to_string(d.join("runner.meta")).ok()?;
        t.lines()
            .find_map(|l| l.strip_prefix("suite_cap_s=").map(|v| v.trim().to_string()))
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

/// The phase the round's `round-phase` marker names, with the marker's line.
fn round_phase_line(results: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(results.join("round-phase")).ok()?;
    let line = text.lines().next()?.trim().to_string();
    let phase = line.split_whitespace().find_map(|w| w.strip_prefix("phase="))?;
    Some((phase.to_string(), line))
}

/// The first red in pass order after the workspace build (which prints its own): lint and
/// fences, then the test phases, then suites. `(phase, "<step>: <first failing line>")`, the
/// phase being the failing test phase's own stage when the step is `unit-tests`.
fn first_red(results: &Path, reds: &[String], phases: &[PhaseRed]) -> Option<(String, String)> {
    if reds.iter().any(|r| r == BUILD_RED) {
        return None;
    }
    let early = |r: &&String| *r == LINT_RED || r.starts_with(FENCE_RED_PREFIX);
    let mut order: Vec<&String> = reds.iter().filter(early).collect();
    order.sort_by_key(|r| (r.as_str() != LINT_RED, r.as_str()));
    let phase_names: Vec<&str> = phases.iter().map(|p| p.stage.as_str()).collect();
    let rest = reds.iter().filter(|r| !early(r));
    order.extend(rest.clone().filter(|r| r.as_str() == UNIT_RED));
    order.extend(rest.filter(|r| r.as_str() != UNIT_RED));
    let first = order.first()?;
    let out = suite_output(results, first, 0).or_else(|| {
        let stage = phase_names.iter().find(|s| test_phase_step(s) == first.as_str())?;
        suite_output(results, stage, 0)
    });
    let of_step = || phases.iter().filter(|p| test_phase_step(&p.stage) == first.as_str());
    let phrase = of_step().find_map(|p| p.long_running.first());
    let line = match out {
        _ if phrase.is_some() => phrase?.clone(),
        Some(o) if early(first) => o.lines().find(|l| !l.trim().is_empty())?.to_string(),
        Some(o) => failure_excerpt(&o).lines().find(|l| !l.trim().is_empty() && *l != spira_config::sendback::NO_FAILURE_FOUND)?.to_string(),
        None => of_step().next().map_or_else(
            || round_phase_line(results).map_or_else(|| "red".to_string(), |(_, l)| l),
            |p| p.cause.clone(),
        ),
    };
    let phase = of_step()
        .find(|p| !p.long_running.is_empty())
        .or_else(|| of_step().next())
        .map_or_else(|| first.to_string(), |p| p.stage.clone());
    Some((phase, format!("{first}: {}", bounded_text(&line))))
}

fn first_red_reason(results: &Path, reds: &[String], phases: &[PhaseRed]) -> Option<String> {
    first_red(results, reds, phases).map(|(_, reason)| reason)
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
    /// The build is running in the background; only an EC2 placement waits for it.
    Pending { tree: String, head: String, batch: String, began: u64, stale: Option<String> },
}

const IMAGE_UNJUDGED: i32 = 3;

fn candidate_branch(recipe: &str) -> String {
    format!("spira-round-candidate/{recipe}")
}

fn bake_mark(recipe: &str, head: &str) -> String {
    format!("{recipe}:{head}")
}

fn bake_recorded(w: &World, label: &str, c: &Ctx, batch: &str, mark: &str) -> bool {
    load(w, label, c, batch)
        .map(|kv| kv.get("candidate-bake") == Some(mark))
        .unwrap_or(false)
}

fn record_bake(w: &World, label: &str, c: &Ctx, batch: &str, mark: &str) -> bool {
    let Ok(_g) = lock(w, label, c) else {
        return false;
    };
    let Ok(mut kv) = load(w, label, c, batch) else {
        return false;
    };
    set(&mut kv, "candidate-bake", mark);
    save(w, label, c, &kv).is_ok()
}

/// Push this head over the recipe branch, dispatch the bake, and record it as this round's.
#[allow(clippy::too_many_arguments)]
fn rebake(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    remote: &str,
    home_path: &Path,
    head: &str,
    branch: &str,
    mark: &str,
    batch: &str,
) -> Option<Image> {
    if !w.lib.push(path, remote, &format!("+{head}:refs/heads/{branch}")) {
        return Some(Image::Unjudged(format!("could not push {branch}")));
    }
    if !w.forge.ami_dispatch(&c.s.forge, home_path, branch) {
        return Some(Image::Unjudged(format!(
            "the forge refused the Round AMI build on {branch}"
        )));
    }
    if !record_bake(w, label, c, batch, mark) {
        return Some(Image::Unjudged("cannot record the candidate image build".into()));
    }
    None
}

/// A round whose head changes the recipe certifies on an image built from that head. One branch
/// per recipe hash carries the bake, so rounds of one recipe follow a run in flight instead of
/// dispatching another. The bake runs in the background (`Image::Pending`); only a pass placed
/// on EC2 waits for it, in `await_candidate_image`. A recipe already imaged is not rebuilt. A
/// failed bake is a red only when this round's record names it: a failure left by another head
/// is rebuilt with this head, never read back.
fn begin_candidate_image(
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
        AmiLookup::Found(id) => {
            if let Some((remote, _)) = &c.r.publish {
                let _ = w.lib.push(path, remote, &format!(":refs/heads/{}", candidate_branch(&tree)));
            }
            return Image::Candidate(id);
        }
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
    let branch = candidate_branch(&tree);
    let mark = bake_mark(&tree, head);
    let began = w.clock.now();
    let build = w.forge.ami_result(&c.s.forge, &home_path, &branch);
    let ours = || bake_recorded(w, label, c, batch, &mark);
    let foreign_failure = matches!(build, Some(AmiBuild::Failure(..))) && !ours();
    if !foreign_failure {
        if let Some(done) = build.as_ref().and_then(|b| finished_build(w, &tree, b)) {
            return release_branch(w, path, &remote, &branch, done);
        }
    }
    let pending = Image::Pending {
        tree: tree.clone(),
        head: head.to_string(),
        batch: batch.to_string(),
        began,
        stale: match &build {
            Some(AmiBuild::Failure(run, _)) => Some(run.clone()),
            _ => None,
        },
    };
    match build {
        None => Image::Unjudged("the forge cannot be read for the candidate round image build".into()),
        Some(AmiBuild::NotStarted) | Some(AmiBuild::Failure(..)) => {
            if let Some(v) = rebake(w, label, c, path, &remote, &home_path, head, &branch, &mark, batch) {
                return v;
            }
            w.out(format!("queue.sh {label}: recipe {tree} has no image; dispatched the Round AMI build on {branch}"));
            pending
        }
        Some(_) => pending,
    }
}

/// The verdict of a build that has ended; None while it has not.
fn finished_build(w: &World, tree: &str, build: &AmiBuild) -> Option<Image> {
    match build {
        AmiBuild::Failure(run, steps) => {
            let at = steps
                .first()
                .cloned()
                .unwrap_or_else(|| "<unknown step>".into());
            Some(Image::Failed(format!("round image build {run} failed at {at}")))
        }
        AmiBuild::Success(run) => Some(match w.scripts.ami_for(tree) {
            AmiLookup::Found(id) => Image::Candidate(id),
            AmiLookup::Missing => Image::Unjudged(format!(
                "round image build {run} succeeded but no image carries recipe {tree}"
            )),
            AmiLookup::Unreadable(e) => {
                Image::Unjudged(format!("cannot read the round images: {e}"))
            }
        }),
        _ => None,
    }
}

/// A judged build's branch is deleted; an unjudged one is kept for the next look.
fn release_branch(w: &World, path: &Path, remote: &str, branch: &str, verdict: Image) -> Image {
    if !matches!(verdict, Image::Unjudged(_)) {
        let _ = w.lib.push(path, remote, &format!(":refs/heads/{branch}"));
    }
    verdict
}

/// Waits for the bake `begin_candidate_image` left pending, up to `round_ami_wait_secs` from
/// when it was dispatched.
fn await_candidate_image(
    w: &World,
    label: &str,
    c: &Ctx,
    path: &Path,
    pending: Image,
) -> Image {
    let Image::Pending { tree, head, batch, began, mut stale } = pending else {
        return pending;
    };
    let branch = candidate_branch(&tree);
    let mark = bake_mark(&tree, &head);
    let (Ok(home), Some((remote, _))) = (resolve(w, label, Some(&c.s.home_repo)), c.r.publish.clone())
    else {
        return Image::Unjudged("cannot resolve the repository the Round AMI workflow runs in".into());
    };
    let Some(home_path) = home.r.path.clone() else {
        return Image::Unjudged("the Round AMI workflow's repository has no path".into());
    };
    let mut idle = 0u32;
    let mut rebaked = false;
    let verdict = loop {
        let build = w.forge.ami_result(&c.s.forge, &home_path, &branch);
        let Some(build) = build else {
            break Image::Unjudged("the forge cannot be read for the candidate round image build".into());
        };
        if matches!(&build, AmiBuild::Failure(run, _) if stale.as_ref() == Some(run)) {
            idle += 1;
            if idle >= AmiWait::IDLE_POLLS {
                break Image::Unjudged(format!(
                    "the Round AMI build dispatched on {branch} never started"
                ));
            }
            w.clock.sleep(AmiWait::POLL_SECS); // waits-on: the dispatched run to replace the failed one; cap IDLE_POLLS
            continue;
        }
        if !rebaked
            && matches!(build, AmiBuild::Failure(..))
            && !bake_recorded(w, label, c, &batch, &mark)
        {
            if let AmiBuild::Failure(run, _) = &build {
                stale = Some(run.clone());
            }
            if let Some(v) = rebake(w, label, c, path, &remote, &home_path, &head, &branch, &mark, &batch) {
                break v;
            }
            rebaked = true;
            w.out(format!("queue.sh {label}: the build on {branch} failed for another head; rebuilt recipe {tree} from this head"));
            continue;
        }
        if let Some(done) = finished_build(w, &tree, &build) {
            break done;
        }
        match build {
            AmiBuild::Running(run) => {
                idle = 0;
                w.out(format!(
                    "queue.sh {label}: waiting for the candidate round image build {run}"
                ));
            }
            _ => {
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
    release_branch(w, path, &remote, &branch, verdict)
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

/// What round-vm names a pass that ended with no verdict from, in `round-fault.out`: the
/// fault, the config key a settings refusal names, and the tail of the VM script's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoundFault {
    pub cause: String,
    pub settings_key: Option<String>,
    pub tail: String,
}

pub(super) const ROUND_FAULT_FILE: &str = "round-fault.out";

pub(super) fn round_fault(results: &Path) -> Option<RoundFault> {
    let text = fs::read_to_string(results.join(ROUND_FAULT_FILE)).ok()?;
    let mut lines = text.lines();
    let cause = lines
        .next()?
        .strip_prefix("ROUND-FAULT: ")?
        .trim()
        .to_string();
    let rest: Vec<&str> = lines.collect();
    let settings_key = rest
        .first()
        .and_then(|l| l.strip_prefix("settings-key: "))
        .map(|k| k.trim().to_string());
    let tail = rest
        .iter()
        .skip_while(|l| !l.starts_with("--- "))
        .skip(1)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    Some(RoundFault {
        cause,
        settings_key,
        tail,
    })
}

/// A setting the round VM refused is the fault of the member whose diff adds its key: present
/// at the member's tip and absent at the landing ref. Each such member is ejected with the
/// fault; when none is, the harness is named and no member is counted against.
#[allow(clippy::too_many_arguments)]
fn settings_refused(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    f: &RoundFault,
    key: &str,
) -> i32 {
    let Some(base) = c.r.landref.as_deref() else {
        w.err(format!("queue.sh {label}: cannot resolve the landing ref of {} to attribute {key} — the fault is counted against no member", c.r.name));
        return FAULT;
    };
    let members = match record_of(w, c, batch) {
        Ok(Some(kv)) => kv.members(),
        Ok(None) => Vec::new(),
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members of {batch} to attribute {key}: {e} — the fault is counted against no member"));
            return FAULT;
        }
    };
    let has = |rev: &str| {
        w.git.grep_files(path, rev, key, "**").iter().any(|f| {
            w.git
                .file_at(path, rev, f)
                .is_some_and(|t| names_key(&t, key))
        })
    };
    let holders: Vec<Member> = if has(base) {
        Vec::new()
    } else {
        members.into_iter().filter(|m| has(&m.tip)).collect()
    };
    // A member stacked on another carries that one's key at its tip without adding it.
    let adders: Vec<Member> = holders
        .iter()
        .filter(|m| {
            !holders
                .iter()
                .any(|o| o.id != m.id && o.tip != m.tip && w.git.is_ancestor(path, &o.tip, &m.tip))
        })
        .cloned()
        .collect();
    if adders.is_empty() {
        w.err(format!("queue.sh {label}: {key} was refused on the round VM and no member's diff adds it: the harness is the cause (the round VM's script or config does not provide it); no member is counted against"));
        w.out("fault-owner=harness");
        return FAULT;
    }
    for m in adders {
        let reason = format!("{}'s diff adds {key}, which the round VM refused: {}\nThe VM script's log, last lines:\n{}", m.id, f.cause, f.tail);
        let rc = eject(
            w,
            by,
            batch,
            &m.id,
            Some(&c.r.name),
            &Text::Arg(reason),
            UNIT_RED,
            true,
            false,
            true,
            "",
            LeaseCheck::Inside,
        );
        if rc == OK {
            note_pass_end(c, std::slice::from_ref(&m.id), PassEnd::Ejected);
            w.out(format!("fault-owner={}", m.id));
        } else {
            w.err(format!(
                "queue.sh {label}: could not eject {}, whose diff adds the refused {key} (rc={rc})",
                m.id
            ));
        }
    }
    FAULT
}

/// Whether `text` holds `key` as a whole word: `FOO_X` is not named by `FOO_X_ROOT`.
fn names_key(text: &str, key: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(key).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(word)
            && !text[i + key.len()..].chars().next().is_some_and(word)
    })
}

fn vm_fault_cause(err: &str) -> Option<String> {
    let line = err.lines().rev().find(|l| {
        let l = l.trim_start();
        l.starts_with("round-vm: IMAGE-BUILD-FAILED") || l.starts_with("round-vm: cannot")
    })?;
    Some(line.trim().to_string())
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
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_of(c))
    {
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

/// The members of open round `batch`'s pass; none once it has ended. Err: the store could not say.
pub(crate) fn pass_members(w: &World, c: &Ctx, batch: &str) -> Result<Vec<String>, String> {
    Ok(record_of(w, c, batch)?.map_or_else(Vec::new, |kv| ids_of(&kv)))
}

fn ids_of(kv: &Kv) -> Vec<String> {
    kv.members().into_iter().map(|m| m.id).collect()
}

fn lost_vm_pass(w: &World, label: &str, batch: &str, err: &str) -> i32 {
    let why = "vm-lost: the round VM died under the pass";
    warn_unrecorded(
        w,
        label,
        batch,
        "the incomplete pass",
        pass_verdict_event(w, batch, &Verdict::Incomplete(why), false),
    );
    w.err(format!("queue.sh {label}: certify rc={VM_LOST}: INCOMPLETE reason={why}; the round is not judged\n{}", tail(err, 20)));
    VM_LOST
}

const NO_OUTPUT_KEY: &str = "vm-no-output";

/// Whether the round's latest pass ended on a VM that never started it: the fault is the VM's, so
/// the round holds no red to weigh against its members.
fn vm_never_started(kv: &Kv) -> bool {
    kv.get(NO_OUTPUT_KEY)
        .and_then(|v| v.split_once(':'))
        .is_some_and(|(head, _)| Some(head) == kv.get("head"))
}

/// A pass whose VM produced no output is counted on the round, per head, and held against no
/// member. The second on one head is paged: a fresh VM did the same.
fn no_output_pass(w: &World, label: &str, c: &Ctx, batch: &str, err: &str) -> i32 {
    let why = format!("{NO_OUTPUT_KEY}: VM produced no output");
    warn_unrecorded(
        w,
        label,
        batch,
        "the incomplete pass",
        pass_verdict_event(w, batch, &Verdict::Incomplete(&why), false),
    );
    w.err(format!("queue.sh {label}: certify rc={VM_NO_OUTPUT}: INCOMPLETE reason={why}; the round is not judged, no member is implicated\n{}", tail(err, 20)));
    w.out(format!("fault={NO_OUTPUT_KEY}"));
    let _g = match lock(w, label, c) {
        Ok(g) => g,
        Err(_) => return VM_NO_OUTPUT,
    };
    let Ok(mut kv) = load(w, label, c, batch) else {
        return VM_NO_OUTPUT;
    };
    let head = kv.get("head").unwrap_or("").to_string();
    let n = kv
        .get(NO_OUTPUT_KEY)
        .and_then(|v| v.split_once(':'))
        .filter(|(h, _)| *h == head)
        .and_then(|(_, n)| n.parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    set(&mut kv, NO_OUTPUT_KEY, &format!("{head}:{n}"));
    if save(w, label, c, &kv).is_err() {
        return VM_NO_OUTPUT;
    }
    if n == 2 {
        w.lib.notify(
            &c.s.mailbox,
            &c.r.name,
            &format!("Round {batch} parked: its VM produced no output twice on {head}"),
            &format!(
                "Round {batch} of {} ended {n} passes running on {head} with a VM that answered ssh and never started the round, on a fresh VM each time. No member is implicated and nobody was ejected; the round is parked open. The VM's boot log is in the round's results ({}). The image or the provider is the cause.",
                c.r.name,
                rounds_dir(c).join(format!("{batch}.results")).display()
            ),
        );
    }
    VM_NO_OUTPUT
}

const INFRA_KEY: &str = "infra-fault";

/// A pass that never reached a suite (no VM, no boot, no checkout) holds no verdict, so it
/// touches no member and no pass ledger: the next certify places the pass again. The second on
/// one head is paged under the infrastructure cause.
fn infra_pass(w: &World, label: &str, c: &Ctx, batch: &str, why: &str) -> i32 {
    w.out(format!("fault={INFRA_KEY}"));
    let _g = match lock(w, label, c) {
        Ok(g) => g,
        Err(_) => return FAULT,
    };
    let Ok(mut kv) = load(w, label, c, batch) else {
        return FAULT;
    };
    let head = kv.get("head").unwrap_or("").to_string();
    let n = kv
        .get(INFRA_KEY)
        .and_then(|v| v.split_once(':'))
        .filter(|(h, _)| *h == head)
        .and_then(|(_, n)| n.parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    set(&mut kv, INFRA_KEY, &format!("{head}:{n}"));
    if save(w, label, c, &kv).is_err() {
        return FAULT;
    }
    if n == 2 {
        w.lib.notify(
            &c.s.mailbox,
            &c.r.name,
            &format!(
                "Round {batch} cannot be placed, no suite ran twice on {head}: {}",
                one_line(&bounded_text(why))
            ),
            &format!(
                "Round {batch} of {} ended {n} passes on {head} before any suite ran. No member is implicated and nobody was ejected; the pass is placed again on the next certify. The infrastructure is the cause:\n{}",
                c.r.name,
                bounded_text(why)
            ),
        );
    }
    FAULT
}

/// round-vm judged the pass but the queue cannot read it: the reader is at fault, so no member is
/// counted, ejected or ledgered, and the operator is told.
fn unread_verdict_pass(
    w: &World,
    label: &str,
    c: &Ctx,
    batch: &str,
    verdict: &str,
    why: &str,
    results: &Path,
) -> i32 {
    w.out("fault=unread-verdict");
    w.err(format!(
        "queue.sh {label}: round-vm judged the pass {verdict}; no member is counted"
    ));
    w.lib.notify(
        &c.s.mailbox,
        &c.r.name,
        &format!("Round {batch}: round-vm judged the pass {verdict} and the queue could not read it"),
        &format!(
            "{why}\nresults: {}\nNo member was ejected and nothing was written to the pass ledger.",
            results.display()
        ),
    );
    FAULT
}

/// The pass of `batch` ended without a verdict: remember it, and eject each member that has now
/// been aboard for [`FAULTED_PASSES_RED`] such passes. When every member of a multi-member pass
/// qualifies, nothing separates them from the host, so none is ejected. Returns [`FAULT`] either way.
fn faulted_pass(w: &World, by: &str, label: &str, c: &Ctx, batch: &str, why: &str) -> i32 {
    let ids = match pass_members(w, c, batch) {
        Ok(ids) => ids,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which members rode the faulted pass of {batch}: {e} — the fault is counted against no member"));
            return FAULT;
        }
    };
    note_pass_end(c, &ids, PassEnd::Faulted);
    let suspects: Vec<(&String, usize)> = ids
        .iter()
        .map(|id| (id, faulted_run(c, id)))
        .filter(|(_, n)| *n >= FAULTED_PASSES_RED)
        .collect();
    if suspects.len() == ids.len() && ids.len() > 1 {
        return FAULT;
    }
    for (id, n) in suspects {
        let reason = format!(
            "{id} was aboard {n} consecutive passes of {} that ended without a verdict, and no pass without it faulted: the member is the cause, not the infrastructure.\nThe latest pass: {why}",
            c.r.name
        );
        let rc = eject(
            w,
            by,
            batch,
            id,
            Some(&c.r.name),
            &Text::Arg(reason),
            UNIT_RED,
            true,
            false,
            true,
            "",
            LeaseCheck::Inside,
        );
        if rc == OK {
            note_pass_end(c, std::slice::from_ref(id), PassEnd::Ejected);
        } else {
            w.err(format!(
                "queue.sh {label}: could not eject {id}, {n} faulted passes running (rc={rc})"
            ));
        }
    }
    FAULT
}

/// Why a pass whose tree was refused did not build the tree the round was cut for, when the
/// landing ref moved off the round's head: the old base, the new one and the landing between.
fn base_moved_cause(w: &World, c: &Ctx, path: &Path, head: &str, err: &str) -> Option<String> {
    if !err.contains(spira_config::cut_hold::TREE_MISMATCH) {
        return None;
    }
    let new = w.git.rev_parse(path, c.r.landref.as_deref()?)?;
    if w.git.is_ancestor(path, &new, head) {
        return None;
    }
    let old = w.git.merge_base(path, &new, head)?;
    let landing = w
        .git
        .log_range(path, &format!("{old}..{new}"))
        .ok()
        .and_then(|l| l.into_iter().next())
        .map(|l| format!("{} {}", l.sha.chars().take(7).collect::<String>(), l.subject))
        .unwrap_or_else(|| "a landing no log names".into());
    Some(format!("base moved from {old} to {new} by {landing}"))
}

/// The base moved under a round whose pass found its tree not the expected one: rebuild the
/// round on the new base once, to be certified again. No member is charged with the pass.
fn rebuild_on_moved_base(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    why: &str,
) -> i32 {
    w.err(format!("queue.sh {label}: {why}; the round is not judged"));
    let _g = match lock(w, label, c) {
        Ok(g) => g,
        Err(rc) => return rc,
    };
    let Ok(mut kv) = load(w, label, c, batch) else {
        return FAIL;
    };
    match remerge_if_moved(w, by, label, c, path, batch, &mut kv) {
        Some((_, rc)) if rc == FAIL => super::LAND_BASE_MOVED,
        Some((_, rc)) => rc,
        None => FAIL,
    }
}

pub(super) use spira_config::sendback::tail;

#[allow(clippy::too_many_arguments)]
pub(super) fn certify(
    w: &World,
    by: &str,
    batch: &str,
    repo: Option<&str>,
    attest: Option<&str>,
    attested_ran: Option<usize>,
    rehead: Option<&str>,
    host_only: bool,
    lease: LeaseCheck,
) -> i32 {
    let label = "round certify";
    let Ok((c, path)) = local_ctx(w, label, repo) else {
        return FAIL;
    };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    if let Err(rc) = lease.guard(w, label, &c, batch) {
        return rc;
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
    let mut rc = certify_pass(
        w,
        by,
        label,
        &c,
        &path,
        batch,
        attest,
        attested_ran,
        rehead,
        host_only,
        &mut over,
    );
    while let Some(unreached) = over.take() {
        let _g = match lock(w, label, &c) {
            Ok(g) => g,
            // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
            Err(rc) => return rc,
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
            rc = certify_pass(
                w,
                by,
                label,
                &c,
                &path,
                batch,
                None,
                attested_ran,
                None,
                host_only,
                &mut over,
            );
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

/// Whether `head` is the recorded base with the recorded members merged in and nothing else:
/// every member tip is under it, and each commit it adds over the base that is not a merge belongs
/// to a member.
fn members_merged_on_base(w: &World, path: &Path, kv: &Kv, head: &str) -> bool {
    let base = kv.get("base").unwrap_or("");
    let members = kv.members();
    if base.is_empty()
        || members.is_empty()
        || !w.git.is_ancestor(path, base, head)
        || !members
            .iter()
            .all(|m| w.git.is_ancestor(path, &m.tip, head))
    {
        return false;
    }
    let Ok(added) = w.git.log_range(path, &format!("{base}..{head}")) else {
        return false;
    };
    added.iter().all(|c| {
        c.parents.len() > 1
            || members
                .iter()
                .any(|m| w.git.is_ancestor(path, &c.sha, &m.tip))
    })
}

const EJECTED_TIPS: &str = "ejected-tips";

/// The ejected members whose recorded tip is in the round worktree's `HEAD` and not in the base.
fn ejected_still_in_tree(w: &World, path: &Path, wt: &Path, kv: &Kv) -> Vec<(String, String)> {
    let Some(head) = w.git.rev_parse(wt, "HEAD") else {
        return Vec::new();
    };
    let base = kv.get("base").unwrap_or("");
    kv.get(EJECTED_TIPS)
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|p| p.split_once(':'))
        .filter(|(_, tip)| w.git.is_ancestor(path, tip, &head) && (base.is_empty() || !w.git.is_ancestor(path, tip, base)))
        .map(|(id, tip)| (id.to_string(), tip.to_string()))
        .collect()
}

fn refuse_ejected_in_tree(w: &World, label: &str, batch: &str, wt: &Path, held: &[(String, String)]) -> i32 {
    let named: Vec<String> = held.iter().map(|(id, tip)| format!("{id} ({tip})")).collect();
    w.err(format!(
        "queue.sh {label}: refused — the round worktree {} still holds ejected member(s) of {batch}: {}; no pass ran. Rebuild the worktree from the recorded members (the next certify adopts it), or `queue round abandon {batch}`",
        wt.display(),
        named.join(", ")
    ));
    FAIL
}

#[allow(clippy::too_many_arguments)]
fn certify_pass(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    attest: Option<&str>,
    attested_ran: Option<usize>,
    rehead: Option<&str>,
    host_only: bool,
    over: &mut Option<usize>,
) -> i32 {
    let who = actor(w);
    if attest.is_some() && attested_ran.unwrap_or(0) == 0 {
        w.err(format!("queue.sh {label}: --attest needs --ran <n> with n > 0 — nothing ran; a vacuous pass is not a certificate"));
        return FAIL;
    }

    let (head, wt, run_id, pass, rerun) = {
        let _g = match lock(w, label, c) {
            Ok(g) => g,
            // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
            Err(rc) => return rc,
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
            let at_wt = w.git.rev_parse(&wt, "HEAD");
            if at_wt.as_deref() != Some(head.as_str()) {
                match at_wt {
                    Some(at) if members_merged_on_base(w, path, &kv, &at) => {
                        w.out(format!("queue.sh {label}: the round worktree is at {at}, the recorded members merged on the recorded base; adopted as the round's head (was {head})"));
                        head = at;
                        set(&mut kv, "head", &head);
                    }
                    _ => {
                        w.err(format!("queue.sh {label}: the round worktree {} is not at the round's head {head} — refused", wt.display()));
                        return FAIL;
                    }
                }
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
                by,
                label,
                c,
                path,
                batch,
                &head,
                &who,
                "re-certify of a GREEN round",
                (ran, 0),
                &pass_runner_in_round(w, batch, &rounds_dir(c).join(format!("{batch}.results"))),
                None,
            );
        }
        let held = ejected_still_in_tree(w, path, &wt, &kv);
        if !held.is_empty() {
            return refuse_ejected_in_tree(w, label, batch, &wt, &held);
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
        let head_tree = w.git.rev_parse(&wt, "HEAD^{tree}");
        let rerun = (attest.is_none() && !gate::own::declares_own_testenv(&wt))
            .then(|| {
                round_rerun::step_to_rerun(&round_rerun::Prior {
                    trees: kv.get(PASS_TREES).unwrap_or(""),
                    head_tree: head_tree.as_deref(),
                    red: kv.get("red").unwrap_or(""),
                    results: &rounds_dir(c).join(format!("{batch}.results")),
                    passed_deny: &passed_deny_path(c, batch),
                    wt: &wt,
                })
            })
            .flatten()
            .map(|step| (step, kv.get(PASS_TREES).unwrap_or("").split_whitespace().count()));
        kv.remove("red");
        match head_tree {
            Some(tree) => record_pass_tree(&mut kv, &tree),
            None => w.err(format!("queue.sh {label}: WARNING — the tree of this pass of {batch} could not be read; the round record does not name it")),
        }
        let ride = Ride::with(w, &kv);
        let started = start_pass_with(w, batch, &head, false, &ride);
        if let Err((rc, out)) = started {
            w.err(format!(
                "queue.sh {label}: spira-lc refused to start the round {batch} (rc={rc}): {out}"
            ));
            return FAIL;
        }
        if ride.save_rest(w, label, c, &kv).is_err() {
            return FAIL;
        }
        mark_running(c, batch);
        let pass = w.lc.batch_pass(batch).map(|(n, _)| n);
        (head, wt, run_id, pass, rerun)
    };

    let mut reds: Vec<String> = Vec::new();
    let mut ran = attested_ran.unwrap_or(0);
    let mut timings = Timings {
        suites_s: 0,
        build_s: 0,
    };
    if attest.is_none() && gate::own::declares_own_testenv(&wt) {
        prune_round_results(w, c, batch);
        let results = rounds_dir(c).join(format!("{batch}.results"));
        let _ = fs::remove_dir_all(&results);
        let _ = fs::create_dir_all(&results);
        let declared = gate::own::ci_entry(&wt);
        let Some(entry) = declared.clone().filter(|e| e.image.is_some()) else {
            let why = match declared {
                Some(_) => format!("{} declares a ci_entry but no image — a round runs the repository's CI entry point in its declared image", gate::own::DECL),
                None => format!("{} declares no ci_entry — a round runs the repository's own CI entry point on a round VM", gate::own::DECL),
            };
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            w.err(format!("queue.sh {label}: {why}; the round is not judged"));
            return faulted_pass(w, by, label, c, batch, &why);
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
        let wall = w.clock.now().saturating_sub(began);
        ran = 1;
        timings = Timings {
            suites_s: wall,
            build_s: 0,
        };
        let mut out = out;
        if ci_result_ok(&results) && out.rc == 2 {
            out.rc = 0;
        }
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
            w.err(format!(
                "queue.sh {label}: {why}; the round is not judged\n{shown}"
            ));
            return faulted_pass(w, by, label, c, batch, &format!("{why}\n{shown}"));
        }
    } else if attest.is_none() {
        prune_round_results(w, c, batch);
        let results = rounds_dir(c).join(format!("{batch}.results"));
        let kept_deny = fs::read_to_string(results.join(DENY_LIST)).ok();
        let only_dir = rounds_dir(c).join(format!("{batch}.only"));
        if rerun.is_some() {
            let _ = fs::remove_dir_all(&only_dir);
            let _ = fs::create_dir_all(&only_dir);
        } else {
            let _ = fs::remove_dir_all(&results);
        }
        let Some(base) = c.r.landref.clone() else {
            w.err(format!("queue.sh {label}: cannot resolve the landing ref of {} — the round's lint has nothing to judge against", c.r.name));
            return FAIL;
        };
        let began = w.clock.now();
        let _ = fs::create_dir_all(rounds_dir(c));
        let deny_path = results.join(DENY_LIST);
        let snapshot = match kept_deny {
            Some(text) => Ok(text),
            None => w.scripts.deny_list(w.clock.now()),
        }
        .and_then(|text| {
            fs::create_dir_all(&results)
                .map_err(|e| format!("{}: {e}", results.display()))?;
            write_atomic(&passed_deny_path(c, batch), &text)?;
            write_atomic(&deny_path, &text)
        });
        if let Err(e) = snapshot {
            let why = format!("the deny list could not be resolved: {e}");
            warn_unrecorded(
                w,
                label,
                batch,
                "the incomplete pass",
                pass_verdict_event(w, batch, &Verdict::Incomplete(&bounded_text(&why)), false),
            );
            w.err(format!("queue.sh {label}: {why}; the round is not judged"));
            return faulted_pass(w, by, label, c, batch, &why);
        }
        let mut image = begin_candidate_image(w, label, c, path, &wt, &head, batch);
        let run_vm = |ami: Option<&str>, recipe: Option<&str>| {
            w.scripts.round_vm(
                &wt,
                if rerun.is_some() { &only_dir } else { &results },
                &base,
                (batch, &c.r.name),
                c.s.round_wall_secs,
                &handle_of(c, batch),
                ami,
                recipe,
                Some(&deny_path),
                host_only,
                rerun.as_ref().map(|(step, _)| step.as_str()),
            )
        };
        let run_on = |image: &Image| match image {
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
            Image::Candidate(id) => run_vm(Some(id), None),
            Image::Pending { tree, .. } => run_vm(None, Some(tree)),
            Image::Release => run_vm(None, None),
        };
        let mut out = run_on(&image);
        if out.rc == IMAGE_PENDING && matches!(image, Image::Pending { .. }) {
            image = await_candidate_image(w, label, c, path, image);
            out = run_on(&image);
        }
        let mut ami = AmiWait::default();
        while matches!(image, Image::Release)
            && stale_ami_cause(&out).is_some()
            && ami.step(w, label, c)
        {
            out = run_vm(None, None);
        }
        if let Some((step, _)) = &rerun {
            round_rerun::replace_step_results(&results, &only_dir, step);
            let _ = fs::remove_dir_all(&only_dir);
        }
        let template_vm = fs::read_to_string(handle_of(c, batch))
            .map(|h| h.trim().to_string())
            .unwrap_or_default();
        let _ = fs::remove_file(handle_of(c, batch));
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
        let capped = over_cap_detail(&results);
        if batch_over_cap(&results, out.rc) {
            let unreached = found.iter().filter(|(_, s)| s == "unreached").count();
            let cut = match &capped {
                Some(d) => format!(" ({})", over_cap_why(d, c.s.round_wall_secs)),
                None => String::new(),
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
        if out.rc == VM_NO_OUTPUT {
            return no_output_pass(w, label, c, batch, &out.err);
        }
        // UNIT FIRST (sp-cddf76): each test phase's red is a record, not an exit code. A unit red
        // ends the pass before any suite, so a pass with no suite results is judged red on it,
        // not as a machinery fault; an integration red is one more red beside the suites'.
        let phase_red = phase_reds(&results);
        let ended_by_unit = out.rc == 1 && phase_red.iter().any(|p| p.before_suites);
        let vm_named = round_fault(&results);
        for p in &phase_red {
            reds.push(UNIT_RED.into());
            for line in p.report() {
                w.out(line);
            }
        }
        let moved = base_moved_cause(w, c, path, &head, &out.err);
        let fault = match out.rc {
            _ if moved.is_some() => moved.clone(),
            1 if ended_by_unit => None,
            0 | 1 if found.is_empty() => Some(vm_named.as_ref().map_or_else(
                || "round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string(),
                |f| f.cause.clone(),
            )),
            0 | 1 => unjudgeable(&wt, &found),
            124 | 137 if capped.is_none() && wall_complete(&wt, &found, &out.err) => None,
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
            124 | 137 if capped.is_some() => Some(over_cap_why(capped.as_deref().unwrap_or_default(), c.s.round_wall_secs)),
            124 | 137 => Some(format!(
                "round-vm hung past its guard (guard=hang-guard elapsed={}s cap {}s)",
                w.clock.now().saturating_sub(began),
                c.s.round_wall_secs
            )),
            rc => Some(match (stale_ami_cause(&out), fixture_fault_cause(&results), &vm_named, vm_fault_cause(&out.err)) {
                (Some(cause), ..) => format!("{STALE_AMI_MARK}: {cause}"),
                (None, Some(c), ..) => format!("round-vm exited {rc}: {c}"),
                (None, None, Some(f), _) => f.cause.clone(),
                (None, None, None, Some(c)) => format!("round-vm exited {rc}: {c}"),
                (None, None, None, None) => format!("round-vm exited {rc}"),
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
            if moved.is_some() {
                return rebuild_on_moved_base(w, by, label, c, path, batch, &why);
            }
            let shown = tail(&out.err, 20);
            w.err(format!(
                "queue.sh {label}: {why}; the round is not judged\n{shown}"
            ));
            if let Some(v) = run_events_verdict(&results)
                .filter(|v| v.starts_with("GREEN") || v.starts_with("RED"))
            {
                return unread_verdict_pass(w, label, c, batch, &v, &why, &results);
            }
            let reached_no_suite = found.is_empty()
                && vm_named.is_none()
                && fixture_fault_cause(&results).is_none()
                && !matches!(out.rc, 0 | 1 | 4 | 124 | 137 | IMAGE_UNJUDGED);
            if reached_no_suite {
                return infra_pass(w, label, c, batch, &format!("{why}\n{shown}"));
            }
            if let Some(f) = vm_named.as_ref().filter(|f| f.cause == why) {
                w.out(format!("fault={}", one_line(&f.cause)));
                w.err(format!(
                    "queue.sh {label}: the VM script's log, last lines:\n{}",
                    f.tail
                ));
                if let Some(key) = &f.settings_key {
                    return settings_refused(w, by, label, c, path, batch, f, key);
                }
                return faulted_pass(w, by, label, c, batch, &format!("{why}\n{}", f.tail));
            }
            return faulted_pass(w, by, label, c, batch, &format!("{why}\n{shown}"));
        }
        reds.extend(
            found
                .iter()
                .filter(|(_, s)| BLOCKING.contains(&s.as_str()))
                .map(|(n, _)| n.clone()),
        );
        if out.rc == 1 && reds.is_empty() {
            reds.push(
                round_phase_line(&results)
                    .map(|(phase, _)| phase)
                    .filter(|p| p != BUILD_RED && p != LINT_RED && !p.starts_with(FENCE_RED_PREFIX))
                    .unwrap_or_else(|| UNIT_RED.to_string()),
            );
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
        if let Some(line) = first_red_reason(&results, &reds, &phase_red) {
            w.out(format!("red-reason={line}"));
        }
        reds.sort();
        reds.dedup();
    }

    let _g = match lock(w, label, c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
    };
    let Ok(kv) = load(w, label, c, batch) else {
        return FAIL;
    };
    let already_green = reds.is_empty() && phase_of(w, batch) == "green";
    // round-vm records a red that ended the pass before its suites (lint, a fence) itself.
    let judged_red = !reds.is_empty()
        && reds
            .iter()
            .all(|r| r == LINT_RED || r.starts_with(FENCE_RED_PREFIX))
        && phase_of(w, batch) == "attributing"
        && pass.is_some()
        && w.lc.batch_pass(batch).map(|(n, _)| n) == pass;
    if !(phase_of(w, batch) == "certifying" || already_green || judged_red)
        || kv.get("head") != Some(head.as_str())
    {
        w.err(format!("queue.sh {label}: the round {batch} changed while it was being certified — the result is discarded; certify again"));
        return FAIL;
    }
    if !reds.is_empty() {
        raise_unreported_reds(w, c, batch, &kv, &reds);
        let runner = pass_runner_in_round(w, batch, &rounds_dir(c).join(format!("{batch}.results")));
        let mut judged = kv.clone();
        judged.remove("over-cap");
        judged.remove(NO_OUTPUT_KEY);
        judged.remove(INFRA_KEY);
        set(&mut judged, "red", &reds.join(","));
        let ride = Ride::with(w, &judged);
        let recorded = (if reds != [BUILD_RED] {
            suites_started(w, batch, false)
        } else {
            Ok(())
        })
        .and_then(|()| {
            pass_verdict_event_with(w, batch, &Verdict::Red(&reds, timings, Some(&runner)), false, &ride)
        });
        if let Err((rc, e)) = recorded {
            w.err(format!("queue.sh {label}: REFUSED — the red pass of round {batch} was not recorded on spira-lc (rc={rc}): {e}"));
            return FAULT;
        }
        note_pass_end(c, &ids_of(&judged), PassEnd::Judged);
        if ride.save_rest(w, label, c, &judged).is_err() {
            return FAIL;
        }
        mark_running(c, batch);
        stage::discard_behind(
            w,
            by,
            c,
            path,
            batch,
            &format!("round {batch} went red: {}", reds.join(",")),
        );
        let mut fields = vec![
            ("red", reds.join(",")),
            ("runner", runner.place.clone()),
            ("runner_why", runner.why.clone()),
        ];
        if let Some(ch) = &runner.changed {
            fields.push(("runner_changed", ch.clone()));
        }
        let results = rounds_dir(c).join(format!("{batch}.results"));
        let (phase, reason) = first_red(&results, &reds, &phase_reds(&results))
            .unwrap_or_else(|| (reds[0].clone(), format!("{}: red", reds[0])));
        fields.push(("phase", phase));
        fields.push(("reason", one_line(&reason)));
        round_event(&c.s.run, "red", &c.r.name, batch, by, w.clock.now(), &fields);
        let verdict = format!(
            "queue.sh {label}: round {batch} is RED at {head}: {}{}",
            reds.join(","),
            runner_words(&runner)
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
        if let Some(red) = [PREFLIGHT_RED, IMAGE_RED].into_iter().find(|n| reds.iter().any(|r| r == n)) {
            drop(_g);
            eject_recipe_changers(w, by, (label, red), c, path, batch, &judged);
        }
        return FAIL;
    }

    // The pass is the evidence: its certificate is written before the lifecycle is asked, so no
    // lost or refused reply can discard what the corpus proved.
    if let Err(rc) = write_round_certificate(w, label, c, path, batch, &head, &who) {
        return rc;
    }
    let mut judged = kv.clone();
    judged.remove("over-cap");
    judged.remove(NO_OUTPUT_KEY);
    judged.remove(INFRA_KEY);
    set(&mut judged, "ran", &ran.to_string());
    let runner = pass_runner_in_round(w, batch, &rounds_dir(c).join(format!("{batch}.results")));
    let ride = Ride::with(w, &judged);
    let greened = suites_started(w, batch, false)
        .and_then(|()| pass_verdict_event_with(w, batch, &Verdict::Green(timings, Some(&runner)), false, &ride));
    // A write that timed out may have applied: the batch row is the arbiter, not the reply.
    let applied = greened.is_err() && phase_of(w, batch) == "green";
    if let (Err((rc, out)), false) = (greened, applied) {
        w.err(format!("queue.sh {label}: spira-lc refused the GREEN for {batch} (rc={rc}): {out} — the round certificate for {head} is kept; certify again to finish without re-running"));
        return FAIL;
    }
    note_pass_end(c, &ids_of(&judged), PassEnd::Judged);
    if ride.save_rest(w, label, c, &judged).is_err() {
        return FAIL;
    }
    let wall = timings.suites_s + timings.build_s;
    finish_green(
        w,
        by,
        label,
        c,
        path,
        batch,
        &head,
        &who,
        &run_id,
        (ran, wall),
        &runner,
        rerun.as_ref().map(|(step, of)| (step.as_str(), *of)),
    )
}

pub const EJECT_REFUSED: &str = "eject-refused=";

/// A red image preflight is the recipe change's: every member whose diff touches a round-recipe
/// input leaves the round, with the fence's output as the evidence, and the rest certify again.
/// With no such member the red is the harness's and nobody leaves. Prints `ejected=<csv>`, so the
/// round's driver knows the red is attributed, and `eject-refused=<id>: …` for an eject the
/// verb refused, after which no further member is tried.
fn eject_recipe_changers(
    w: &World,
    by: &str,
    (label, red): (&str, &str),
    c: &Ctx,
    path: &Path,
    batch: &str,
    kv: &Kv,
) {
    let base = kv.get("base").unwrap_or("");
    let changers: Vec<(String, Vec<String>)> = kv
        .members()
        .into_iter()
        .map(|m| {
            let touched = if red == IMAGE_RED {
                tag_inputs_touched(w, path, base, &m.tip)
            } else {
                recipe_inputs_touched(w, path, base, &m.tip)
            };
            (m.id, touched)
        })
        .filter(|(_, touched)| !touched.is_empty())
        .collect();
    if changers.is_empty() {
        w.out(format!("queue.sh {label}: {red} is red and no member of {batch} changes a round-recipe input — the red is the harness's; nobody is ejected"));
        return;
    }
    let results = rounds_dir(c).join(format!("{batch}.results"));
    let Some(output) = suite_output(&results, red, 0).filter(|o| !o.trim().is_empty())
    else {
        w.err(format!("queue.sh {label}: {red} is red but left no output in {} — nothing names the evidence, so nobody is ejected; attribute it by hand", results.display()));
        return;
    };
    let evidence = tail(&output, 40);
    let mut ejected = Vec::new();
    for (id, touched) in changers {
        // A changer stacked on one ejected before it has already left with it.
        match pass_members(w, c, batch) {
            Ok(ids) if !ids.contains(&id) => continue,
            Ok(_) => {}
            Err(e) => {
                w.err(format!("queue.sh {label}: cannot read the members of {batch} before ejecting {id}: {e} — not ejected"));
                continue;
            }
        }
        let reason = format!(
            "{red} went red in round {batch}, and {id} changes the round recipe ({}).\n\n{evidence}",
            touched.join(", ")
        );
        let rc = eject(
            w,
            by,
            batch,
            &id,
            Some(&c.r.name),
            &Text::Arg(reason),
            red,
            true,
            false,
            true,
            "",
            LeaseCheck::Inside,
        );
        if rc == OK {
            note_pass_end(c, std::slice::from_ref(&id), PassEnd::Ejected);
            ejected.push(id);
        } else {
            w.err(format!(
                "queue.sh {label}: could not eject {id} for the red {red} (rc={rc})"
            ));
            w.out(format!("{EJECT_REFUSED}{id}: eject for the red {red} refused (rc={rc})"));
            break;
        }
    }
    if !ejected.is_empty() {
        w.out(format!("ejected={}", ejected.join(",")));
    }
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
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    head: &str,
    who: &str,
    run_id: &str,
    (ran, wall): (usize, u64),
    runner: &Runner,
    rerun: Option<(&str, usize)>,
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
            for step in spira_config::round_step::names() {
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
    round_event(
        &c.s.run,
        "green",
        &c.r.name,
        batch,
        by,
        w.clock.now(),
        &[
            ("head", head.to_string()),
            ("tree", tree.to_string()),
            ("run", run_id.to_string()),
            ("ran", ran.to_string()),
            ("wall", format!("{wall}s")),
            ("runner", runner.place.clone()),
            ("runner_why", runner.why.clone()),
        ]
        .into_iter()
        .chain(runner.changed.iter().map(|c| ("runner_changed", c.clone())))
        .chain(rerun.into_iter().flat_map(|(step, of)| [("only", step.to_string()), ("of_pass", of.to_string())]))
        .collect::<Vec<_>>(),
    );
    w.out(format!(
        "queue.sh {label}: round {batch} is GREEN at {head} (tree {tree}) ran={ran} wall={wall}s{}{}",
        runner_words(runner),
        rerun.map(|(step, of)| format!(" only={step} of_pass={of}")).unwrap_or_default()
    ));
    OK
}

/// The round's whole ci_entry run is a result, not a suite: a tree that declares its own testenv
/// is recertified on that declaration's default tiers instead.
fn ci_entry_red_as_default_tiers(wt: &Path, suites: &str) -> Result<String, String> {
    let named: Vec<&str> = suites.split(',').filter(|s| !s.is_empty()).collect();
    if !named.contains(&REPO_TESTENV_RED) || !gate::own::declares_own_testenv(wt) {
        return Ok(suites.to_string());
    }
    let tiers = gate::own::default_tiers(wt).map_err(|e| format!("cannot read the default tiers for the round tree: {e}"))?;
    if tiers.is_empty() {
        return Err(format!("--suites names {REPO_TESTENV_RED} but the round tree's testenv declares no default tier to run in its place"));
    }
    let mut out: Vec<String> = Vec::new();
    for n in named {
        let each = if n == REPO_TESTENV_RED { tiers.clone() } else { vec![n.to_string()] };
        for t in each {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    Ok(out.join(","))
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
    w.err(format!("queue.sh {label}: --suites names {} which testenv does not know — nothing changed; known suites: {}; round steps: {}", unknown.join(","), known.join(" "), spira_config::round_step::names().join(" ")));
    Err(FAIL)
}

#[allow(clippy::too_many_arguments)]
fn eject(
    w: &World,
    by: &str,
    batch: &str,
    id: &str,
    repo: Option<&str>,
    reason: &Text,
    suites: &str,
    red: bool,
    harness_fault: bool,
    rebuild: bool,
    with: &str,
    lease: LeaseCheck,
) -> i32 {
    let label = "round eject";
    let mut named: Vec<&str> = Vec::new();
    for s in suites.split(',').map(test_phase_step) {
        if !named.contains(&s) {
            named.push(s);
        }
    }
    let suites = named.join(",");
    let suites = suites.as_str();
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
    if let Err(rc) = lease.guard(w, label, &c, batch) {
        return rc;
    }
    match w.lc.batch_state(batch) {
        Some((state, _)) if state == "CI_RUNNING" => {
            let (n, phase) = w.lc.batch_pass(batch).unwrap_or((0, String::new()));
            w.err(format!(
                "queue.sh {label}: pass {n} (phase {phase}) of round {batch} is running; {} — nothing changed",
                lifecycle::batch::PASS_RUNNING_EXIT
            ));
            return PASS_RUNNING;
        }
        Some(_) => {}
        None => {
            w.err(format!("queue.sh {label}: cannot tell whether a pass of round {batch} is running — nothing changed"));
            return FAIL;
        }
    }
    let _g = match lock(w, label, &c) {
        Ok(g) => g,
        // A busy lock keeps its own exit (LOCK_BUSY), so a caller retries on the code.
        Err(rc) => return rc,
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
    let mapped = match ci_entry_red_as_default_tiers(&wt, suites) {
        Ok(m) => m,
        Err(e) => {
            w.err(format!("queue.sh {label}: {e} — nothing changed"));
            return FAIL;
        }
    };
    let suites = mapped.as_str();
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
    let mut dependants: Vec<String> = vec![id.to_string()];
    while let Some(m) = members.iter().find(|m| {
        !dependants.contains(&m.id) && w.lc.bead_row(&m.id).is_some_and(|r| r.blocked_by.iter().any(|b| dependants.contains(b)))
    }) {
        dependants.push(m.id.clone());
    }
    dependants.remove(0);
    let stacked: Vec<Member> = members
        .iter()
        .filter(|m| {
            m.id != id
                && (withs.contains(&m.id.as_str())
                    || dependants.contains(&m.id)
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
                let intact = w.git.rev_parse(&wt, "HEAD").as_deref() == Some(old_head.as_str());
                let left = if intact {
                    "nothing changed".to_string()
                } else {
                    format!("the round worktree {} was removed and is not at the round's head {old_head}", wt.display())
                };
                w.err(format!("queue.sh {label}: cannot rebuild the round without {id}: {why} — {left}"));
                return FAIL;
            }
        }
    }

    let reason = with_kept_evidence(w, &c, batch, &reason, suites);
    let why = bounded_text(&reason);
    if let Err((rc, out)) = lc_eject_member(w, batch, id, LC_ACTOR, &why) {
        if new_head != old_head {
            let _ = w.git.worktree_reset(&path, &wt, &old_head);
        }
        if w.lc.batch_state(batch).is_some_and(|(state, _)| state == "CI_RUNNING") {
            let (n, phase) = w.lc.batch_pass(batch).unwrap_or((0, String::new()));
            w.err(format!(
                "queue.sh {label}: pass {n} (phase {phase}) of round {batch} is running; {} — nothing changed",
                lifecycle::batch::PASS_RUNNING_EXIT
            ));
            return PASS_RUNNING;
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
            } else if dependants.contains(&m.id) {
                format!("blocked on {id}")
            } else {
                format!("stacked on {id}")
            };
            (m.id.as_str(), why, cause)
        }))
    {
        if !w.lib.bead_reopen(bead, own_cause.as_str(), suites) {
            w.err(format!(
                "queue.sh {label}: bead_reopen did not complete for {bead} (cause {}); deciding on the lifecycle row",
                own_cause.as_str()
            ));
        }
        if lc_return(w, bead, bead == id && own_cause == EjectCause::EjectRed).is_err() {
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
        round_event(
            &c.s.run,
            "eject",
            &c.r.name,
            batch,
            by,
            w.clock.now(),
            &[
                ("id", bead.to_string()),
                ("cause", own_cause.as_str().to_string()),
                ("reason", one_line(&why_text)),
            ],
        );
        if bead != id {
            w.out(format!(
                "queue.sh {label}: ejected {bead} from round {batch} ({})",
                if dependants.contains(&bead.to_string()) { format!("blocked on {id}") } else { format!("stacked on {id}") }
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
        match lc_abandon(w, batch, LC_ACTOR, &emptied) {
            Ok(()) => {
                crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, batch);
            }
            Err((rc, out)) => w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for the emptied round {batch} (rc={rc}): {out} — the round is recorded ended; round open retries the abandon")),
        }
        drop_or_warn(w, label, &c, batch, "emptied");
        remove_unheld_tree(w, &c, &path, &wt, batch);
        finish(
            w,
            &c,
            batch,
            End::Abandoned("emptied"),
            &format!("{id} ejected, no member remains"),
        );
        stage::discard_behind(
            w,
            by,
            &c,
            &path,
            batch,
            &format!("round {batch} was emptied"),
        );
        w.out(format!(
            "queue.sh {label}: ejected {id}; the round {batch} has no member left and is closed"
        ));
        let voided = void_stacked(w, by, label, &c, &path, batch);
        if !unreturned.is_empty() {
            w.err(format!("queue.sh {label}: {} left round {batch} but did not return to REWORK: {} — see above", id, unreturned.join(" ")));
            return FAIL;
        }
        return voided.map_or_else(|rc| rc, |()| OK);
    }

    set(&mut kv, "members", &render_members(&survivors));
    let provisional: Vec<String> = kv.get("provisional").unwrap_or("").split_whitespace().map(str::to_string).filter(|p| survivors.iter().any(|m| &m.id == p)).collect();
    if provisional.is_empty() {
        kv.remove("provisional");
    } else {
        set(&mut kv, "provisional", &provisional.join(" "));
    }
    set(&mut kv, "head", &new_head);
    kv.remove("red");
    let ejected = stacked.iter().fold(
        format!("{}{id} ", kv.get("ejected").unwrap_or("")),
        |acc, m| format!("{acc}{} ", m.id),
    );
    set(&mut kv, "ejected", &ejected);
    let fresh = 1 + stacked.len();
    let unrecorded = ejected.split_whitespace().count() - fresh - kv.get("ejected_by").unwrap_or("").split_whitespace().count();
    let ejected_by = format!("{}{}{}", kv.get("ejected_by").unwrap_or(""), "- ".repeat(unrecorded), format!("{by} ").repeat(fresh));
    set(&mut kv, "ejected_by", &ejected_by);
    if !rebuild {
        let tips = stacked.iter().fold(
            format!("{}{id}:{ejected_tip} ", kv.get(EJECTED_TIPS).unwrap_or("")),
            |acc, m| format!("{acc}{}:{} ", m.id, m.tip),
        );
        set(&mut kv, EJECTED_TIPS, &tips);
    }
    let rebuilt = serde_json::json!({"PassRebuilt": {"head": new_head}}).to_string();
    let ride = Ride::with(w, &kv);
    warn_unrecorded(
        w,
        label,
        batch,
        "the rebuilt head",
        pass_event_with(w, batch, &ride, |s, _, _| {
            (s == "ATTRIBUTING").then_some(rebuilt)
        }),
    );
    if ride.save_rest(w, label, &c, &kv).is_err() {
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
        w.err(format!(
            "queue.sh {label}: cannot read the recorded mends: {e}"
        ));
    })?;
    let carried: Vec<Member> = members
        .iter()
        .map(|m| Member {
            tip: recorded
                .get(&m.id)
                .cloned()
                .unwrap_or_else(|| m.tip.clone()),
            ..m.clone()
        })
        .collect();
    if carried.iter().zip(&members).all(|(a, b)| a.tip == b.tip) {
        return Ok(None);
    }
    let old_head = kv.get("head").unwrap_or("").to_string();
    let base = kv.get("base").unwrap_or("").to_string();
    let new_head = assemble(w, c, path, wt, &base, &carried, &old_head).map_err(|why| {
        w.err(format!(
            "queue.sh {label}: cannot rebuild the round with its mended tips: {why} — refused"
        ));
    })?;
    set(kv, "members", &render_members(&carried));
    set(kv, "head", &new_head);
    let rebuilt = serde_json::json!({"PassRebuilt": {"head": new_head}}).to_string();
    let ride = Ride::with(w, kv);
    warn_unrecorded(
        w,
        label,
        batch,
        "the rebuilt head",
        pass_event_with(w, batch, &ride, |s, _, _| {
            (s == "ATTRIBUTING").then_some(rebuilt)
        }),
    );
    w.out(format!(
        "queue.sh {label}: round {batch} rebuilt at {new_head} with its mended tips"
    ));
    Ok(Some(new_head))
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

/// A refused record (rc 3) whose row another landing already moved to LANDED, with the head on
/// the landing ref, is a round recorded, not a record lost.
fn landed_elsewhere(w: &World, batch: &str, path: &Path, head: &str, base: &str) -> bool {
    matches!(w.lc.batch_state(batch), Some((s, _)) if s == "LANDED")
        && w.git.is_ancestor(path, head, base)
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
                    Err((3, _)) if landed_elsewhere(w, batch, path, head, &base) => {
                        let _ = fs::remove_file(e.path());
                    }
                    Err((rc, out)) => w.err(format!("queue.sh: WARNING — landing record for {batch} still refused (rc={rc}): {}", out.trim())),
                }
            }
            _ => {}
        }
    }
}

/// Why `round land` landed nothing: its exit ([`LandRefusal::exit`]) is what a lander acts on;
/// the word that starts its stderr is for the operator (law-a-matcher-reads-code-not-prose).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandRefusal {
    /// Stacked on a round that has not landed: it waits for it.
    Stacked,
    /// The base it was certified on can no longer become the landing ref: it is cut again.
    BaseMoved,
    /// Anything else: it is held as it is.
    Refused,
}

impl LandRefusal {
    /// The exit a lander classifies the refusal by: stacked and base-moved have their own, a
    /// plain refusal keeps the exit the refusing step gave (law-lifecycle-state-is-not-text).
    pub fn exit(self, rc: i32) -> i32 {
        match self {
            LandRefusal::Stacked => super::LAND_STACKED,
            LandRefusal::BaseMoved => super::LAND_BASE_MOVED,
            LandRefusal::Refused => rc,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            LandRefusal::Stacked => "stacked",
            LandRefusal::BaseMoved => "base-moved",
            LandRefusal::Refused => "refused",
        }
    }
}

/// `round land`'s stderr, held until the verb knows whether it refused, so that a refusal's
/// word is the first thing it writes.
struct HeldErr<'a> {
    io: &'a dyn crate::ports::Emit,
    err: std::cell::RefCell<String>,
}

impl crate::ports::Emit for HeldErr<'_> {
    fn out(&self, s: &str) {
        self.io.out(s);
    }
    fn err(&self, s: &str) {
        self.err.borrow_mut().push_str(s);
    }
}

fn land(w: &World, by: &str, batch: &str, repo: Option<&str>, token: Option<&str>) -> i32 {
    let held = HeldErr {
        io: w.io,
        err: Default::default(),
    };
    let r = land_round(&World { io: &held, ..*w }, by, batch, repo, token);
    let text = held.err.into_inner();
    match r {
        Ok(rc) => {
            if !text.is_empty() {
                w.io.err(&text);
            }
            rc
        }
        Err((why, rc)) if text.is_empty() => {
            w.io.err(&format!(
                "{}: queue.sh round land: {batch} not landed\n",
                why.word()
            ));
            why.exit(rc)
        }
        Err((why, rc)) => {
            w.io.err(&format!("{}: {text}", why.word()));
            why.exit(rc)
        }
    }
}

/// The head the landing log records for `batch`'s ROUND-LAND when it is on the landing ref: a
/// land driven again after it ran (its outcome unread) is answered, not refused for a round that
/// ended by landing.
fn landed_head(w: &World, c: &Ctx, path: &Path, batch: &str) -> Option<String> {
    if unrecorded_marker(c, batch).exists() {
        return None;
    }
    let log = super::landing_log_text(&c.s.run)?;
    let (repo, id) = (format!(" repo={} ", c.r.name), format!(" batch={batch} "));
    let head = log
        .lines()
        .filter(|l| l.starts_with("QUEUE ROUND-LAND ") && l.contains(&repo) && l.contains(&id))
        .filter_map(|l| l.split(" head=").nth(1)?.split(' ').next())
        .next_back()?;
    let base = c.r.landref.clone()?;
    w.git
        .is_ancestor(path, head, &base)
        .then(|| head.to_string())
}

/// `Err` is a refusal that landed nothing; `Ok` is the exit of a land that ran.
fn land_round(
    w: &World,
    by: &str,
    batch: &str,
    repo: Option<&str>,
    token: Option<&str>,
) -> Result<i32, (LandRefusal, i32)> {
    let label = "round land";
    let refused = |rc: i32| (LandRefusal::Refused, rc);
    let (c, path) = local_ctx(w, label, repo).map_err(refused)?;
    reconcile_unrecorded(w, &c, &path);
    reconcile_ended(w, &c);
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return Err(refused(FAIL));
    }
    if let Some(head) = landed_head(w, &c, &path, batch) {
        w.out(format!(
            "queue.sh {label}: round {batch} already landed at {head}"
        ));
        return Ok(OK);
    }
    LeaseCheck::Check(token)
        .guard(w, label, &c, batch)
        .map_err(refused)?;
    let g = lock(w, label, &c).map_err(refused)?;
    if let Some(head) = landed_head(w, &c, &path, batch) {
        w.out(format!(
            "queue.sh {label}: round {batch} already landed at {head}"
        ));
        return Ok(OK);
    }
    let mut kv = load(w, label, &c, batch).map_err(refused)?;
    let phase = phase_of(w, batch);
    if phase != "green" {
        w.err(format!(
            "queue.sh {label}: round {batch} is {phase}, not green — certify it first"
        ));
        return Err(refused(FAIL));
    }
    if let Some(r) = remerge_if_moved(w, by, label, &c, &path, batch, &mut kv) {
        return Err(r);
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
        None,
    );
    let base = c.r.landref.clone().unwrap_or_default();
    if rc != OK && !w.git.is_ancestor(&path, &head, &base) {
        if let Some(p) = pending {
            p.discard(w);
        }
        return Err(refused(rc));
    }

    let recorded = match record_landing(w, batch, &head) {
        Ok(()) => true,
        Err((3, _)) if landed_elsewhere(w, batch, &path, &head, &base) => {
            w.out(format!("queue.sh {label}: round {batch} was recorded LANDED by another landing; {head} is on {base}"));
            true
        }
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
    drop_or_warn(w, label, &c, batch, END_LANDED);
    remove_unheld_tree(w, &c, &path, &wt, batch);
    if recorded {
        crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, batch);
    }
    round_event(
        &c.s.run,
        "land",
        &c.r.name,
        batch,
        by,
        w.clock.now(),
        &[("head", head.to_string()), ("members", csv(&members))],
    );
    w.out(format!(
        "queue.sh {label}: round {batch} landed at {head} — {} member(s)",
        members.len()
    ));
    // Activation is the last step: the new release may retire keys the steps above still need.
    // The landing is recorded, so the queue lock goes first (over-broad-scopes #3).
    drop(g);
    let (rc, end) = match pending.map(|p| p.activate(w)) {
        Some(super::deploy::Outcome::Fault(_)) => (super::DEPLOY_FAULT, End::DeployFault),
        _ => (rc, End::Landed),
    };
    finish(
        w,
        &c,
        batch,
        end,
        &format!("{head} members={}", csv(&members)),
    );
    if !recorded {
        w.err(format!("queue.sh {label}: round {batch} has NO batch landing record; the release is live — exiting non-zero"));
        return Ok(if rc == OK { FAIL } else { rc });
    }
    Ok(rc)
}

/// The parent a stacked round waited on has ended: its certify lock goes once spira-lc says so.
fn settled_parent(w: &World, c: &Ctx, parent: &str) {
    if w.lc
        .batch_state(parent)
        .is_some_and(|(s, _)| super::reconcile::terminal(&s))
    {
        crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, parent);
    }
}

/// A round stacked on an open round lands only after that round has: it holds while the parent is
/// open, is re-cut on the parent's head when that changed, and is rebuilt on the landing ref
/// (to certify again) when the parent ended without landing. `None` means no stacking obstacle.
#[allow(clippy::too_many_arguments)]
fn settle_parent(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    kv: &mut Kv,
    base_now: &str,
) -> Option<(LandRefusal, i32)> {
    let parent = kv.get("parent").filter(|p| !p.is_empty())?.to_string();
    let stacked_on = kv.get("base").unwrap_or("").to_string();
    let onto = match record_of(w, c, &parent) {
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether round {parent}, which {batch} is stacked on, is still open: {e} — nothing changed"));
            return Some((LandRefusal::Refused, FAIL));
        }
        Ok(Some(pkv)) => {
            let phead = pkv.get("head").unwrap_or("").to_string();
            if phead == stacked_on {
                w.err(format!("queue.sh {label}: round {batch} is stacked on round {parent}, which has not landed — land {parent} first; nothing changed"));
                return Some((LandRefusal::Stacked, FAIL));
            }
            phead
        }
        Ok(None) if w.git.is_ancestor(path, &stacked_on, base_now) => {
            settled_parent(w, c, &parent);
            kv.remove("parent");
            return None;
        }
        Ok(None) => {
            settled_parent(w, c, &parent);
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
            return Some((LandRefusal::BaseMoved, FAIL));
        }
    };
    set(kv, "head", &new_head);
    set(kv, "base", &onto);
    kv.remove("red");
    let ride = Ride::with(w, kv);
    let moved = lc_cas(w, batch, |s, v| {
        ride.sent(w.lc.batch_event(batch, s, v, LC_ACTOR, "\"BaseMoved\"", ride.fields()))
    });
    if let Err((rc, out)) = moved {
        w.err(format!("queue.sh {label}: spira-lc refused BaseMoved for {batch} (rc={rc}): {out} — the round worktree was rebuilt at {new_head}; certify it again"));
    }
    if ride.save_rest(w, label, c, kv).is_err() {
        return Some((LandRefusal::Refused, FAIL));
    }
    mark_running(c, batch);
    round_event(
        &c.s.run,
        "restack",
        &c.r.name,
        batch,
        by,
        w.clock.now(),
        &[("head", new_head.to_string()), ("base", onto.to_string())],
    );
    w.err(format!("queue.sh {label}: round {batch} was stacked on round {parent}, which changed or ended without landing — rebuilt at {new_head} on {onto}; run `queue round certify {batch}`, it is not landed"));
    Some((LandRefusal::BaseMoved, FAIL))
}

/// A round is cut from the landing ref and lands only on top of it. When an earlier round has
/// landed since, this one is re-merged onto the new ref and goes back to certify on the merged
/// tree, whatever files it shares with what landed: disjoint files can still conflict in meaning.
/// `None` means the round may land now (its kv holds the head to land).
fn remerge_if_moved(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    batch: &str,
    kv: &mut Kv,
) -> Option<(LandRefusal, i32)> {
    let base = c.r.landref.clone().unwrap_or_default();
    let base_now = w.git.rev_parse(path, &base)?;
    if let Some(r) = settle_parent(w, by, label, c, path, batch, kv, &base_now) {
        return Some(r);
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
        return Some((LandRefusal::Refused, FAIL));
    };
    let shared: Vec<&String> = landed.intersection(&mine).collect();
    let new_head = match assemble(w, c, path, &wt, &base_now, &members, &head) {
        Ok(h) => h,
        Err(why) => {
            w.err(format!("queue.sh {label}: round {batch} no longer merges onto {base} ({base_now}): {why} — nothing changed; eject the member and certify again"));
            return Some((LandRefusal::BaseMoved, FAIL));
        }
    };
    set(kv, "head", &new_head);
    set(kv, "base", &base_now);
    kv.remove("red");
    let ride = Ride::with(w, kv);
    let moved = lc_cas(w, batch, |s, v| {
        ride.sent(w.lc.batch_event(batch, s, v, LC_ACTOR, "\"BaseMoved\"", ride.fields()))
    });
    if let Err((rc, out)) = moved {
        w.err(format!("queue.sh {label}: spira-lc refused BaseMoved for {batch} (rc={rc}): {out} — the round worktree was rebuilt at {new_head}; certify it again"));
    }
    if ride.save_rest(w, label, c, kv).is_err() {
        return Some((LandRefusal::Refused, FAIL));
    }
    mark_running(c, batch);
    round_event(
        &c.s.run,
        "remerge",
        &c.r.name,
        batch,
        by,
        w.clock.now(),
        &[
            ("head", new_head.to_string()),
            ("base", base_now.to_string()),
            (
                "shared",
                shared
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ],
    );
    w.err(format!(
        "queue.sh {label}: round {batch} was cut before {base} moved to {base_now} (files in common with what landed: {}) — re-merged to {new_head}; run `queue round certify {batch}`, it is not landed",
        if shared.is_empty() { "none".to_string() } else { shared.iter().take(5).map(|f| f.as_str()).collect::<Vec<_>>().join(" ") }
    ));
    Some((LandRefusal::BaseMoved, FAIL))
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

fn abandon(
    w: &World,
    by: &str,
    batch: &str,
    repo: Option<&str>,
    reason: &Text,
    token: Option<&str>,
    cause: &str,
) -> i32 {
    abandon_with(w, by, batch, repo, reason, token, cause, true)
}

/// A hand landing's void of the open round: the round's red is no reason to keep it, and a
/// leased round is taken under its holder's own token.
pub(super) fn abandon_for_landing(
    w: &World,
    by: &str,
    batch: &str,
    repo: &str,
    reason: &str,
    token: Option<&str>,
) -> i32 {
    abandon_with(
        w,
        by,
        batch,
        Some(repo),
        &Text::Arg(reason.to_string()),
        token,
        "hand-landing",
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn abandon_with(
    w: &World,
    by: &str,
    batch: &str,
    repo: Option<&str>,
    reason: &Text,
    token: Option<&str>,
    cause: &str,
    considered: bool,
) -> i32 {
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
    if let Err(rc) = LeaseCheck::Check(token).guard(w, label, &c, batch) {
        return rc;
    }
    let _g = match lock_sparing(w, label, &c, Some(batch)) {
        Ok(g) => g,
        Err(rc) => return rc,
    };
    let abandoned = match abandoned_record(w, &c, batch) {
        Ok(a) => a,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether round {batch} is recorded: {e} — refused, nothing changed"));
            return FAIL;
        }
    };
    let (kv, reason) = match abandoned {
        Some(_) if rounds_dir(&c).join(format!("{batch}.result")).exists() => {
            w.out(format!("queue.sh {label}: round {batch} is already abandoned and closed — nothing written"));
            return OK;
        }
        Some(kv) => (kv, reason),
        None => match load(w, label, &c, batch) {
            Ok(kv) if !considered => (kv, reason),
            Ok(kv) => match considered_abandon(w, label, &kv, reason) {
                Ok(reason) => (kv, reason),
                Err(rc) => return rc,
            },
            Err(rc) => {
                drop_or_warn(w, label, &c, batch, "abandoned");
                return rc;
            }
        },
    };
    close_abandoned(w, by, label, &c, &path, &kv, &reason, cause)
}

const ABANDON_CONSIDERED_ENV: &str = "QUEUE_ABANDON_CONSIDERED";

/// A red round of more than one member is ejected, not abandoned: an abandon throws away the
/// members the red does not implicate, and they pay a whole pass to certify again. An eject
/// clears the round's `red`, so a round red with no eject since refuses unless the caller sets
/// [`ABANDON_CONSIDERED_ENV`]; its why leads the reason, so a bounded reason never cuts it, and
/// the lifecycle records it on the round's abandon event. Returns the reason to record.
fn considered_abandon(w: &World, label: &str, kv: &Kv, reason: String) -> Result<String, i32> {
    let members = kv.members();
    let Some(red) = kv
        .get("red")
        .filter(|r| !r.is_empty() && members.len() > 1 && !vm_never_started(kv))
    else {
        return Ok(reason);
    };
    if let Some(why) = w.var(ABANDON_CONSIDERED_ENV) {
        return Ok(format!(
            "{ABANDON_CONSIDERED_ENV}: {} — {reason}",
            one_line(&why)
        ));
    }
    let batch = kv.get("batch_id").unwrap_or("");
    let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    if w.lc.batch_state(batch).is_some_and(|(s, _)| s == "CI_RUNNING") {
        w.err(format!(
            "queue.sh {label}: refused — round {batch} is red ({red}) with {} members ({}) and a pass is running, so eject is refused until the pass reports its verdict and the round is OPEN, ATTRIBUTING or GREEN; wait for that, or set {ABANDON_CONSIDERED_ENV}=\"<why>\" to abandon it whole, recorded on the round's events. Nothing changed",
            ids.len(),
            ids.join(" ")
        ));
        return Err(FAIL);
    }
    w.err(format!(
        "queue.sh {label}: refused — round {batch} is red ({red}) with {} members ({}) and nothing ejected since; an abandon discards the members the red does not implicate. Eject the culprit and let the rest certify: `queue round eject {batch} <id> --reason <why> --suites <csv> --red`; or set {ABANDON_CONSIDERED_ENV}=\"<why>\" to abandon it whole, recorded on the round's events. Nothing changed",
        ids.len(),
        ids.join(" ")
    ));
    Err(FAIL)
}

/// Every live round cut on `parent`'s head is abandoned with it: its tree holds the parent's
/// members, so a verdict on it judges code that will not land. Each is named. A child's lease
/// does not shield it: a pass its holder runs on that tree is void whoever holds it. The caller
/// holds the queue lock; `Err` when any could not be voided.
fn void_stacked(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    parent: &str,
) -> Result<(), i32> {
    let children: Vec<Kv> = match live_records(w, c) {
        Ok(open) => open
            .into_iter()
            .filter(|kv| kv.get("parent") == Some(parent))
            .collect(),
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell which rounds are stacked on the closed round {parent}: {e} — void them by hand"));
            return Err(FAIL);
        }
    };
    let mut rc = Ok(());
    for kv in children {
        let child = kv.get("batch_id").unwrap_or("?").to_string();
        w.out(format!("queue.sh {label}: voiding round {child}: it is stacked on round {parent}, which closed without landing"));
        let reason = format!("voided: stacked on round {parent}, which closed without landing");
        if close_abandoned(w, by, label, c, path, &kv, &reason, "voided") != OK {
            w.err(format!("queue.sh {label}: round {child} is stacked on the closed round {parent} and could not be voided — abandon it by hand"));
            rc = Err(FAIL);
        }
    }
    rc
}

/// A driver still holding the round's certify lock was not stopped by the abandon; it ends at its
/// next poll, when it reads the abandon marker.
fn name_surviving_driver(w: &World, label: &str, c: &Ctx, batch: &str) {
    if crate::lock::certify_held(&c.s.queue_dir, &c.r.name, batch) {
        let holder = crate::lock::certify_holder(&c.s.queue_dir, &c.r.name, batch);
        w.err(format!(
            "queue.sh {label}: WARNING — a driver of round {batch} could not be stopped ({}); it ends at its next poll",
            if holder.is_empty() { "holder unrecorded" } else { &holder }
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn close_abandoned(
    w: &World,
    by: &str,
    label: &str,
    c: &Ctx,
    path: &Path,
    kv: &Kv,
    reason: &str,
    cause: &str,
) -> i32 {
    let batch = kv.get("batch_id").unwrap_or("");
    let members = kv.members();
    spira_config::round_ended::mark_abandoned(&c.s.run, batch);
    let stopped = match w.scripts.stop_round_legs(&c.r.name, batch) {
        Ok(units) => units,
        Err(e) => {
            spira_config::round_ended::clear_abandoned(&c.s.run, batch);
            w.err(format!("queue.sh {label}: round {batch} still has a leg running ({e}) — the round stays open and its tree is kept"));
            return FAIL;
        }
    };
    let reason = &match stopped.is_empty() {
        true => reason.to_string(),
        false => {
            w.out(format!(
                "queue.sh {label}: stopped {} of round {batch}",
                stopped.join(" ")
            ));
            format!("{reason} — stopped {}", stopped.join(" "))
        }
    };
    let why = bounded_text(reason);
    match lc_abandon(w, batch, LC_ACTOR, &why) {
        Ok(()) => {
            w.out(format!("queue.sh {label}: {batch} abandoned on spira-lc"));
            name_surviving_driver(w, label, c, batch);
            crate::lock::remove_certify_lock(&c.s.queue_dir, &c.r.name, batch);
        }
        Err((1, out)) if out.is_empty() => w.err(format!(
            "queue.sh {label}: spira-lc has no batch row for {batch}"
        )),
        Err((rc, out)) => {
            spira_config::round_ended::clear_abandoned(&c.s.run, batch);
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
    retire_menders(w, c, batch, "round abandoned");
    drop_or_warn(w, label, c, batch, "abandoned");
    remove_unheld_tree(w, c, path, &worktree_of(kv), batch);
    finish(w, c, batch, End::Abandoned(cause), reason);
    stage::discard_behind(
        w,
        by,
        c,
        path,
        batch,
        &format!("round {batch} was abandoned: {}", one_line(reason)),
    );
    round_event(
        &c.s.run,
        "abandon",
        &c.r.name,
        batch,
        by,
        w.clock.now(),
        &[("reason", one_line(reason))],
    );
    w.out(format!(
        "queue.sh {label}: round {batch} abandoned for {}",
        c.r.name
    ));
    match void_stacked(w, by, label, c, path, batch) {
        Ok(()) => OK,
        Err(rc) => rc,
    }
}

#[cfg(test)]
mod over_cap_text {
    use super::over_cap_why;

    #[test_id(t7lbxyg)]
    #[test]
    fn an_over_cap_with_no_detail_speaks_of_the_suites_alone() {
        assert_eq!(over_cap_why("", 900), "the suites exceeded the 900s cap");
    }
}
