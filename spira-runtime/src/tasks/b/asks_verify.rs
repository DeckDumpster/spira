//! # asks.verify — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/asks-verify.md`;
//! its Acceptance section is the test file `tests/it/task_b_asks_verify.rs`.
//!
//! ## Tick
//!
//! - Cadence **15 min** (unit: `OnBootSec=7min`, `OnUnitActiveSec=15min`, `AccuracySec=2min`;
//!   `Persistent=true` inert). First tick 7 min after boot.
//! - No other wake. Single-flight: a pass can take minutes (each check up to 120 s,
//!   `verify-asks.sh:59`), and a tick during one is dropped.
//!
//! ## Observe
//!
//! `asks.verify.observe` returns:
//!
//! ```text
//! Snapshot { asks: Vec<Ask{ id, works: Vec<String>, cmd }>, mislabelled: Vec<(id, kind, title)> }
//! ```
//!
//! - Source: **one** fetch of every bead, `spira-lc content list --all --limit 0 --json`
//!   (`cockpit/db.sh:49-55`, called at `verify-asks.sh:47`), guarded by `.beads` existing
//!   (`db.sh:34-38`, `verify-asks.sh:43-46`). In-process: `content list` over `LcClient` (S7a),
//!   narrowed server-side with `--label-any $SPIRA_ASK_LABEL` (the full list today is wasted;
//!   `db.sh:57-64` measured the cost).
//! - `asks`: ask-labelled, not closed, not epic, carrying `^\s*VERIFY:\s*(.+)$` in the
//!   description; `works` from `work-bead:<id>` labels (`verify-asks.sh:93-113`).
//! - `mislabelled`: ask-labelled open epics (`verify-asks.sh:126-141`).
//! - Unreachable store → snapshot error; decide does nothing (`verify-asks.sh:47-50`).
//!
//! The VERIFY command itself is **not** an observe: it is arbitrary operator shell and runs as
//! an effect (below), its exit code returning as that effect's outcome.
//!
//! ## Decide
//!
//! Two steps, both pure:
//!
//! 1. `decide(&Snapshot) -> Vec<Check{id, cmd}>` — one check per ask; plus one inbox line per
//!    mislabelled epic **only when the set changes** (carried `mislabelled_seen`), replacing
//!    the every-15-min `MISLABELLED` stdout (`verify-asks.sh:124-142`).
//! 2. On a check's outcome: `rc == 0` → `Close{id, reason: "Verified already done — its own
//!    VERIFY check now passes: <cmd> → exit 0. Evidence: <first 400 bytes>"}`
//!    (`verify-asks.sh:59-68`); then for each work bead not terminal (`LANDED|SUPERSEDED|
//!    DROPPED|DONE`, `:79`) a `Withdraw{work, actor: claude}` (`:75-87`). `rc != 0` → nothing.
//!
//! Carried state: `mislabelled_seen`.
//!
//! A `close`, `withdraw` or `note` lost at a restart is asked again as it was.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `asks.verify.check` | host | 120 s (`SPIRA_VERIFY_TIMEOUT`, `:59`) | `<ask id>/<hash(cmd)>/<tick>` | an empty command; it is read-only by convention only (`:24-26`) — a child `bash -c`, the one fork that stays |
//! | `asks.verify.close` | store | 15 s | `<ask id>/close` | an already-closed ask (lifecycle exit is quiet); actor fixed `claude` (`:67`) |
//! | `asks.verify.withdraw` | store | 15 s | `<work id>/withdraw/<ask id>` | terminal work bead; exit 1/3 quiet, other exit → warning line (`:81-85`) |
//! | `asks.verify.note` | host | 10 s | `mislabelled/<sorted ids hash>` | unchanged set; becomes `inbox.append` |
//!
//! `check` runs under the S3 `host` class cap; a 120 s check must not hold a `store` permit.
//!
//! ## Silence
//!
//! An idle tick (no ask carries VERIFY, or every check fails, and the mislabelled set is
//! unchanged) emits the observe probe and the check effects' outcomes only — no journaled
//! `Emit`, no inbox line. Today it prints a summary and the `(none yet …)` hint every pass
//! (`verify-asks.sh:144-145`); dropped. Open question: a check per ask per tick is itself
//! traffic — `check` outcomes should be `Probe`-class (unjournaled) when `rc != 0`.
//!
//! ## Replaces
//!
//! - `spira-verify-asks-prod.{service,timer}` (`CPUQuota=25%`, `Nice=10`,
//!   `TimeoutStartSec=600`). No watchd row.
//! - Children per pass: `. conf.sh` via `db.sh:24`; `spira-lc content list`; two `python3`;
//!   per ask `timeout 120 bash -c <cmd>`; on satisfied `spira-lc close`, `spira-lc state`,
//!   `spira-lc withdraw-ask`. **Catalogue lists `lib.sh, spira-lc, bd`: the code sources
//!   `conf.sh` (not `lib.sh`), uses no `bd`, and forks `python3` and `bash -c`.**
//! - Pinned keys: `SPIRA_DB`/`COCKPIT_DB` (`db.sh:27`), `SPIRA_ASK_LABEL`,
//!   `SPIRA_VERIFY_TIMEOUT` (env default 120, not registered; joins `Settings::KEYS`),
//!   `SPIRA_LC_BIN` (test seam, `db.sh:30`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and the script. `Observe` mode shadows `close` and
//! `withdraw`; `check` still runs (it is the read), matching the script without `--apply`.
//!

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::{fnv1a, EventKindTag};
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "asks.verify";
pub const STREAM: &str = "task.asks.verify";
pub const TICK: EventKindTag = "asks.verify.tick";
pub const STATE: EventKindTag = "asks.verify.state";

pub const OBSERVE: &str = "asks.verify.observe";
pub const CHECK: &str = "asks.verify.check";
pub const CLOSE: &str = "asks.verify.close";
pub const WITHDRAW: &str = "asks.verify.withdraw";
pub const NOTE: &str = "asks.verify.note";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECK, CLOSE, WITHDRAW, NOTE];

/// Work-bead states a withdraw skips.
pub const TERMINAL: &[&str] = &["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ASKS_VERIFY_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_ASK_LABEL", "SPIRA_VERIFY_TIMEOUT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 15_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ask {
    pub id: String,
    pub works: Vec<String>,
    /// The `VERIFY:` command.
    pub cmd: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub asks: Vec<Ask>,
    /// (id, kind, title) of ask-labelled open epics.
    pub mislabelled: Vec<(String, String, String)>,
    /// Lifecycle state of each work bead the asks name.
    pub work_states: BTreeMap<String, String>,
}

/// The `check` runner's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckOutcome {
    pub id: String,
    pub rc: i32,
    pub output: String,
}

/// Per ask (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Open,
    Checking,
    Satisfied,
    Closed,
    Withdrawn,
    StillOpen,
}

/// `state × event → state` per ask. `closed` without `satisfied` is refused naming `checking`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "check", Phase::Checking),
    (Phase::StillOpen, "check", Phase::Checking),
    (Phase::Checking, "rc:0", Phase::Satisfied),
    (Phase::Checking, "rc:nonzero", Phase::StillOpen),
    (Phase::Satisfied, "close:ok", Phase::Closed),
    (Phase::Closed, "withdraw:ok", Phase::Withdrawn),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub mislabelled_seen: BTreeSet<String>,
    pub asks: BTreeMap<String, Phase>,
    /// Each ask in flight as last observed: its command (the close reason quotes it) and the
    /// work beads, already filtered to the non-terminal ones, a satisfied close withdraws.
    pub checks: BTreeMap<String, Ask>,
}

pub struct AsksVerify {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Open => "open",
        Phase::Checking => "checking",
        Phase::Satisfied => "satisfied",
        Phase::Closed => "closed",
        Phase::Withdrawn => "withdrawn",
        Phase::StillOpen => "still_open",
    }
}

/// A `close` act's input and a `withdraw`'s.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub work: String,
    pub reason: String,
    pub cmd: String,
    pub timeout_secs: u64,
}

fn act(name: &str, key: String, j: &Job, deadline_ms: u64, probe: bool) -> Act {
    Act { name: name.into(), key, input: serde_json::to_string(j).unwrap_or_default(), deadline_ms, probe }
}

/// `verify-asks.sh`'s close reason, the evidence cut at 400 bytes on a char boundary.
pub fn close_reason(cmd: &str, output: &str) -> String {
    let mut end = output.len().min(400);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("Verified already done — its own VERIFY check now passes: {cmd} → exit 0. Evidence: {}", &output[..end])
}

/// One check per ask; on `rc == 0` a close then a withdraw per non-terminal work bead. The
/// check is a probe (an `rc != 0` is the common answer and journals nothing); an ask that
/// stays open re-checks each tick without a state change.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let timeout_secs: u64 = cfg.get("SPIRA_VERIFY_TIMEOUT").and_then(|v| v.trim().parse().ok()).unwrap_or(120);
    let mut next = carry.clone();
    let mut acts = vec![];
    match input {
        Input::Fire => {}
        Input::Observed(s) => {
            let seen: BTreeSet<String> = s.mislabelled.iter().map(|m| m.0.clone()).collect();
            if seen != carry.mislabelled_seen {
                let fresh: Vec<String> = s.mislabelled.iter().filter(|m| !carry.mislabelled_seen.contains(&m.0)).map(|(id, kind, title)| format!("MISLABELLED {id} ({kind}) — {title}: an {kind} cannot be answered; strip the ask label or write the question")).collect();
                if !fresh.is_empty() {
                    let note = Job { reason: fresh.join("\n"), ..Job::default() };
                    acts.push(act(NOTE, format!("mislabelled/{:016x}", fnv1a(&note.reason)), &note, 10_000, false));
                }
                next.mislabelled_seen = seen;
            }
            let present: BTreeSet<&str> = s.asks.iter().map(|a| a.id.as_str()).collect();
            next.asks.retain(|id, p| present.contains(id.as_str()) || matches!(p, Phase::Satisfied | Phase::Closed));
            next.checks.retain(|id, _| next.asks.contains_key(id) || present.contains(id.as_str()));
            for a in &s.asks {
                let phase = carry.asks.get(&a.id).copied().unwrap_or_default();
                if matches!(phase, Phase::Satisfied | Phase::Closed) {
                    continue;
                }
                let works = a.works.iter().filter(|w| !s.work_states.get(*w).is_some_and(|st| TERMINAL.contains(&st.as_str()))).cloned().collect();
                next.checks.insert(a.id.clone(), Ask { id: a.id.clone(), works, cmd: a.cmd.clone() });
                if phase == Phase::Open || phase == Phase::Withdrawn {
                    next.asks.insert(a.id.clone(), Phase::Checking);
                }
                acts.push(act(CHECK, format!("{}/{:016x}/{now_ms}", a.id, fnv1a(&a.cmd)), &Job { id: a.id.clone(), cmd: a.cmd.clone(), timeout_secs, ..Job::default() }, timeout_secs * 1000, true));
            }
        }
        Input::Outcome { effect, ok, body } if effect == CHECK => {
            let Ok(c) = serde_json::from_str::<CheckOutcome>(body) else { return Ok((next, acts)) };
            if !ok {
                return Ok((next, acts));
            }
            let phase = carry.asks.get(&c.id).copied().unwrap_or_default();
            if !matches!(phase, Phase::Checking | Phase::StillOpen) {
                return Err(format!("{NAME}: {} is in state {}, not checking; its check outcome is refused", c.id, phase_name(phase)));
            }
            if c.rc == 0 {
                let cmd = carry.checks.get(&c.id).map(|a| a.cmd.clone()).unwrap_or_default();
                next.asks.insert(c.id.clone(), Phase::Satisfied);
                acts.push(act(CLOSE, format!("{}/close", c.id), &Job { id: c.id.clone(), reason: close_reason(&cmd, &c.output), ..Job::default() }, 15_000, false));
            } else if phase == Phase::Checking {
                next.asks.insert(c.id.clone(), Phase::StillOpen);
            }
        }
        Input::Outcome { effect, ok, body } if effect == CLOSE => {
            let id = body.split(": ").next().unwrap_or_default().to_string();
            let phase = carry.asks.get(&id).copied().unwrap_or_default();
            if phase != Phase::Satisfied {
                return Err(format!("{NAME}: {id} is in state {}, not satisfied; a close is refused", phase_name(phase)));
            }
            if !ok {
                next.asks.insert(id, Phase::StillOpen);
                return Ok((next, acts));
            }
            next.asks.insert(id.clone(), Phase::Closed);
            for w in carry.checks.get(&id).map(|a| a.works.clone()).unwrap_or_default() {
                acts.push(act(WITHDRAW, format!("{w}/withdraw/{id}"), &Job { id: id.clone(), work: w, ..Job::default() }, 15_000, false));
            }
            if acts.is_empty() {
                next.asks.remove(&id);
                next.checks.remove(&id);
            }
        }
        Input::Outcome { effect, body, .. } if effect == WITHDRAW => {
            let id = body.split(['/', ':']).next().unwrap_or_default().to_string();
            let phase = carry.asks.get(&id).copied().unwrap_or_default();
            if phase != Phase::Closed {
                return Err(format!("{NAME}: {id} is in state {}, not closed; a withdraw is refused", phase_name(phase)));
            }
            next.asks.remove(&id);
            next.checks.remove(&id);
        }
        Input::Outcome { effect, .. } if effect == NOTE || effect == OBSERVE => {}
        Input::Outcome { effect, .. } => return Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Lost(a) if [CLOSE, WITHDRAW, NOTE].contains(&a.name.as_str()) => acts.push(a.clone()),
        Input::Lost(_) => {}
        Input::Ran(r) => return Err(format!("{NAME}: no unit is expected, got {r:?}")),
    }
    Ok((next, acts))
}

/// The `VERIFY:` command in a description: the first line that reads `VERIFY: <cmd>`.
pub fn verify_cmd(description: &str) -> Option<String> {
    description.lines().find_map(|l| l.trim_start().strip_prefix("VERIFY:").map(|c| c.trim().to_string()).filter(|c| !c.is_empty()))
}

/// `verify-asks.sh`'s two passes over one `content list --json` answer: the open, ask-labelled,
/// non-epic beads that carry a check, and the ask-labelled open epics.
pub fn parse_rows(doc: &serde_json::Value, label: &str) -> (Vec<Ask>, Vec<(String, String, String)>) {
    let rows = doc.as_array().or_else(|| doc["issues"].as_array()).cloned().unwrap_or_default();
    let (mut asks, mut bogus) = (vec![], vec![]);
    for r in rows {
        let labels: Vec<String> = r["labels"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if !labels.iter().any(|l| l == label) || spira_config::nonwork::row_closed(spira_config::nonwork::Kind::Ask, &r) {
            continue;
        }
        let id = r["id"].as_str().unwrap_or_default().to_string();
        let kind = r["issue_type"].as_str().unwrap_or_default();
        if kind == "epic" {
            bogus.push((id, kind.to_string(), r["title"].as_str().unwrap_or_default().chars().take(60).collect()));
            continue;
        }
        if let Some(cmd) = verify_cmd(r["description"].as_str().unwrap_or_default()) {
            let works = labels.iter().filter_map(|l| l.strip_prefix("work-bead:")).filter(|w| !w.is_empty()).map(str::to_string).collect();
            asks.push(Ask { id, works, cmd });
        }
    }
    (asks, bogus)
}

/// The observe: one `content list` of the open beads carrying the ask label, and the lifecycle
/// states of the work beads the asks name, both over the socket.
pub fn observe(e: &crate::Effects, label: &str) -> Result<String, String> {
    observe_on(&super::lc(e), label)
}

pub fn observe_on(lc: &spira_store::lc_call::LcClient, label: &str) -> Result<String, String> {
    let text = lc.answer(&["content", "list", "--limit", "0", "--json", "--label-any", label]).map_err(String::from)?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("content list: {e}"))?;
    let (asks, mislabelled) = parse_rows(&doc, label);
    let mut work_states = BTreeMap::new();
    if asks.iter().any(|a| !a.works.is_empty()) {
        let works: Vec<String> = asks.iter().flat_map(|a| a.works.iter().cloned()).collect();
        for row in spira_store::lc_state::list_ids_on(lc, &works)? {
            work_states.insert(row.bead_id, row.state);
        }
    }
    serde_json::to_string(&Snapshot { asks, mislabelled, work_states }).map_err(|e| e.to_string())
}

/// The runner of [`CLOSE`]. A close lost at a restart is asked again though it may have run,
/// so a refused close of an ask whose bead reads closed is success.
fn close(lc: &spira_store::lc_call::LcClient, j: &Job) -> Result<String, String> {
    let r = lc.call_stdin(&["close", j.id.as_str(), "--reason-file", "-", "--actor", "claude"], &j.reason).map_err(|why| format!("{}: {why}", j.id))?;
    if r.code == 0 || shows_closed(lc, &j.id) {
        Ok(j.id.clone())
    } else {
        Err(format!("{}: close exited {}: {}", j.id, r.code, r.why()))
    }
}

/// `content show`, not `show`: an ask has no lifecycle row.
fn shows_closed(lc: &spira_store::lc_call::LcClient, id: &str) -> bool {
    let Ok(text) = lc.answer(&["content", "show", id, "--json"]) else { return false };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let row = doc.as_array().and_then(|a| a.first()).unwrap_or(&doc);
    spira_config::nonwork::row_closed(spira_config::nonwork::Kind::Ask, row)
}

/// Every act's runner, by `spec.name`. The check is the one child that stays: the operator's
/// `bash -c <cmd>`, bounded, through `runtime::ports::Proc`.
pub fn run(e: &crate::Effects, spec: &runtime::EffectSpec) -> Result<String, String> {
    use runtime::ports::Proc;
    let j: Job = serde_json::from_str(&spec.input).unwrap_or_default();
    match spec.name.as_str() {
        CHECK => {
            if j.cmd.trim().is_empty() {
                return Err(format!("{}: an empty VERIFY command is refused", j.id));
            }
            let out = runtime::ports::SysProc.run(&runtime::ports::ProcSpec { prog: "bash".into(), args: vec!["-c".into(), j.cmd.clone()], stdin: vec![], deadline: std::time::Duration::from_secs(j.timeout_secs.max(1)) });
            let (rc, output) = match out {
                Ok(o) => (o.code.unwrap_or(124), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
                Err(why) => (124, why),
            };
            serde_json::to_string(&CheckOutcome { id: j.id, rc, output }).map_err(|e| e.to_string())
        }
        CLOSE => close(&super::lc(e), &j),
        WITHDRAW => {
            let client = super::lc(e);
            let lc = mail::lc::Routed { client: Ok(&client), unserved: &mail::lc::LcProgram };
            match mail::lc::lift_ask(&lc, &j.work, mail::lc::Lift::Withdraw, "claude") {
                None => Ok(format!("{}/{}", j.id, j.work)),
                Some(w) => Err(format!("{}/{}: {w}", j.id, j.work)),
            }
        }
        NOTE => {
            for line in j.reason.lines() {
                e.world.inbox_append(line)?;
            }
            Ok("noted".into())
        }
        other => Err(format!("{NAME}: no runner for {other}")),
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("mislabelled_seen"), runtime::rows::Part::Map { name: "asks", drop: &[] }, runtime::rows::Part::Map { name: "checks", drop: &[] }];

super::hosted!(AsksVerify);

#[cfg(test)]
mod tests {
    use super::*;
    use spira_store::lc_call::{fake, LcClient};
    use std::sync::Arc;

    fn ask(id: &str, status: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": status, "issue_type": "task", "labels": ["needs-x"], "description": "VERIFY: true"})
    }

    fn cfg() -> Cfg {
        Cfg::new()
    }

    fn checking(id: &str, cmd: &str, works: &[&str]) -> Carry {
        let ask = Ask { id: id.into(), works: works.iter().map(|w| w.to_string()).collect(), cmd: cmd.into() };
        Carry { asks: BTreeMap::from([(id.to_string(), Phase::Checking)]), checks: BTreeMap::from([(id.to_string(), ask)]), ..Carry::default() }
    }

    fn checked(id: &str, rc: i32) -> Input<Snapshot> {
        Input::Outcome { effect: CHECK.into(), ok: true, body: serde_json::to_string(&CheckOutcome { id: id.into(), rc, output: "evidence".into() }).unwrap() }
    }

    fn row(id: &str, kind: &str, labels: &[&str], description: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": "open", "issue_type": kind, "labels": labels, "description": description, "title": id})
    }

    // covers: UC-operator-channel-26
    #[test_id(tli5xfd)]
    #[test]
    fn a_passing_verify_closes_the_ask_with_a_reason_quoting_the_command() {
        let (next, acts) = decide(&cfg(), &checking("sp-a", "test -f /done", &[]), &checked("sp-a", 0), 0).unwrap();
        assert_eq!(next.asks["sp-a"], Phase::Satisfied);
        let close: Vec<&Act> = acts.iter().filter(|a| a.name == CLOSE).collect();
        assert_eq!(close.len(), 1, "{acts:?}");
        let job: Job = serde_json::from_str(&close[0].input).unwrap();
        assert_eq!(job.id, "sp-a");
        assert!(job.reason.contains("test -f /done") && job.reason.contains("exit 0"), "{}", job.reason);
    }

    // covers: UC-operator-channel-26
    #[test_id(trpj4ht)]
    #[test]
    fn a_failing_verify_leaves_the_ask_open_and_closes_nothing() {
        let (next, acts) = decide(&cfg(), &checking("sp-a", "false", &[]), &checked("sp-a", 1), 0).unwrap();
        assert_eq!(next.asks["sp-a"], Phase::StillOpen);
        assert!(acts.is_empty(), "{acts:?}");
    }

    // covers: UC-operator-channel-26
    #[test_id(tdzlclw)]
    #[test]
    fn a_closed_ask_withdraws_the_hold_on_each_work_bead_that_is_not_yet_terminal() {
        let snap = Snapshot { asks: vec![Ask { id: "sp-a".into(), works: vec!["sp-live".into(), "sp-landed".into()], cmd: "true".into() }], work_states: BTreeMap::from([("sp-live".into(), "WORKING".into()), ("sp-landed".into(), "LANDED".into())]), ..Snapshot::default() };
        let (carry, _) = decide(&cfg(), &Carry::default(), &Input::Observed(snap), 0).unwrap();
        let (carry, _) = decide(&cfg(), &carry, &checked("sp-a", 0), 0).unwrap();
        let (_, acts) = decide(&cfg(), &carry, &Input::Outcome { effect: CLOSE.into(), ok: true, body: "sp-a".into() }, 0).unwrap();
        let withdrawn: Vec<String> = acts.iter().filter(|a| a.name == WITHDRAW).map(|a| serde_json::from_str::<Job>(&a.input).unwrap().work).collect();
        assert_eq!(withdrawn, ["sp-live"]);
    }

    // covers: UC-operator-channel-26
    #[test_id(tzrvaks)]
    #[test]
    fn only_open_non_epic_asks_with_a_verify_line_are_checked_and_open_epics_are_reported() {
        let mut closed = row("sp-closed", "decision", &["needs-x"], "VERIFY: exit 0");
        closed["status"] = "closed".into();
        let doc = serde_json::Value::Array(vec![
            row("sp-ok", "decision", &["needs-x", "work-bead:sp-w"], "why\n\nVERIFY: exit 0"),
            row("sp-bare", "decision", &["needs-x"], "no check line here"),
            row("sp-epic", "epic", &["needs-x"], "VERIFY: exit 0"),
            row("sp-unlabelled", "decision", &["other"], "VERIFY: exit 0"),
            closed,
        ]);
        let (asks, bogus) = parse_rows(&doc, "needs-x");
        assert_eq!(asks, [Ask { id: "sp-ok".into(), works: vec!["sp-w".into()], cmd: "exit 0".into() }]);
        assert_eq!(bogus.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(), ["sp-epic"]);
    }

    // covers: UC-operator-channel-26
    #[test_id(tsqp7pp)]
    #[test]
    fn a_verify_that_never_returned_closes_nothing() {
        let timed_out = Input::Outcome { effect: CHECK.into(), ok: false, body: String::new() };
        let (next, acts) = decide(&cfg(), &checking("sp-a", "sleep 600", &[]), &timed_out, 0).unwrap();
        assert!(acts.is_empty(), "{acts:?}");
        assert_eq!(next.asks["sp-a"], Phase::Checking);
    }

    #[test_id(tbw1u9c)]
    #[test]
    fn the_observe_checks_the_open_asks_and_never_reads_the_whole_history() {
        let store = Arc::new(fake::Content::new(vec![ask("sp-open", "open"), ask("sp-done", "closed")]));
        let snap: Snapshot = serde_json::from_str(&observe_on(&LcClient::over(store.clone(), true), "needs-x").unwrap()).unwrap();
        assert_eq!(snap.asks.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["sp-open"]);
        assert!(store.requests().iter().all(|r| !spira_store::lc_call::reads_whole_history(r)), "{:?}", store.requests());
    }

    /// The content fake, with `close` refused.
    struct RefusedClose(fake::Content);

    impl spira_store::lc_call::Transport for RefusedClose {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, deadline: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            if argv.first().is_some_and(|a| a == "close") {
                return Ok(spira_store::lc_call::Reply { code: 3, out: String::new(), err: "refused\n".into(), read_ended: false });
            }
            self.0.exchange(argv, stdin, deadline)
        }
    }

    // covers: UC-operator-channel-26
    #[test_id(t0x96b3)]
    #[test]
    fn a_refused_close_succeeds_only_when_the_ask_shows_closed() {
        for (status, closed) in [("closed", true), ("open", false)] {
            let lc = LcClient::over(Arc::new(RefusedClose(fake::Content::new(vec![ask("sp-a", status)]))), true);
            let got = close(&lc, &Job { id: "sp-a".into(), reason: "r".into(), ..Job::default() });
            if closed {
                assert_eq!(got, Ok("sp-a".into()));
            } else {
                assert_eq!(got, Err("sp-a: close exited 3: refused".into()));
            }
        }
    }
}
