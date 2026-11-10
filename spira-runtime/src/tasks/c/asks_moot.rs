//! # asks.moot — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/asks-moot.md`;
//! its Acceptance section is the test file `tests/it/task_c_asks_moot.rs`.
//!
//! ## Tick
//!
//! - Cadence **15 min** (unit: `OnBootSec=5min`, `OnUnitActiveSec=15min`, `AccuracySec=2min`;
//!   `Persistent=true` inert). First tick 5 min after boot.
//! - No other wake. Single-flight.
//!
//! ## Observe
//!
//! `asks.moot.observe` returns:
//!
//! ```text
//! Snapshot { asks: Vec<Ask{ id, pred }> }
//! ```
//!
//! - Source: `spira-lc content list --all --limit 0 --label-any "insight,$SPIRA_ASK_LABEL,overseer" --json`
//!   (`cockpit/db.sh:71-79`, called at `moot-sweep.sh:36`), guarded by `.beads` existing
//!   (`db.sh:34-38`, `moot-sweep.sh:32-35`). In-process: `content list` over `LcClient` (S7a).
//! - Filter: ask-labelled, status exactly `open`, carrying `^\s*MOOT-WHEN:\s*(.+)$`
//!   (`moot-sweep.sh:70-87`). An ask with no MOOT-WHEN never enters the snapshot (invariant 1,
//!   `:17-19`).
//! - Unreachable store → snapshot error; nothing decided (`moot-sweep.sh:36-39`).
//!
//! ## Decide
//!
//! 1. `decide(&Snapshot) -> Vec<Check{id, pred}>`.
//! 2. On the check outcome: `rc == 0` → `Resolve{id, reason: "auto-resolved: MOOT-WHEN
//!    predicate cleared — <pred> → exit 0. Evidence: <first 300 bytes>"}` (`:52-58`);
//!    `rc == 124` → timeout, ask stays open, counted as error (`:64-66`); any other rc → still
//!    live (`:67-68`). A failed probe never reads as cleared (invariant 2, `:21-22`).
//!
//! No carried state. Fixtures: none in-tree for the script (no `test-moot-sweep*.sh` found);
//! `resolve.rs` unit tests cover the resolve half. T5 fixtures must be captured first.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `asks.moot.check` | host | 30 s (`SPIRA_MOOT_TIMEOUT`, `:50`) | `<ask id>/<hash(pred)>/<tick>` | empty predicate; a child `bash -c`, the one fork that stays |
//! | `asks.moot.resolve` | store | 15 s | `<ask id>/resolve` | an id that is a work bead (has a lifecycle row and is no ask, `resolve.rs:61-70`); cannot-tell on either check (`resolve.rs:56-59,70`); actor fixed `claude` (invariant 3, `:24-25`) |
//! | `asks.moot.withdraw` | store | 15 s | `<work id>/withdraw/<ask id>` | terminal work bead (`resolve_main.rs:61-65`); exit 1/3 quiet (`resolve.rs:97`) |
//!
//! `resolve` folds today's sequence: for an ask on the ask machine, `spira-lc close-ask --exit
//! withdrawn` (`resolve.rs:85-93`); for a legacy ask, `lifecycle_row::close` then
//! `withdraw-ask` per `work-bead:` label (`resolve.rs:72,94-103`). Splitting the per-work-bead
//! withdraw into its own effect makes each one retryable alone.
//!
//! ## Silence
//!
//! An idle tick (no open ask carries MOOT-WHEN, or every predicate is still live) emits the
//! observe and the check outcomes only — no `Emit`, no inbox line. The summary line
//! (`moot-sweep.sh:89`) is dropped. A **timeout** is not silent today in effect (it only
//! prints); proposed: a predicate that times out three ticks running emits one inbox line
//! (new behaviour, flag for Ryan).
//!
//! ## Replaces
//!
//! - `spira-moot-sweep-prod.{service,timer}` (`TimeoutStartSec=300`). No watchd row.
//! - Children per pass: `. conf.sh` via `db.sh:24`; `spira-lc content list`; `python3`; per ask
//!   `timeout 30 bash -c <pred>`; on cleared `resolve`, which forks `timeout 5 spira-lc
//!   content show`, `spira-lc show`, `spira-lc close`, `spira-lc close-ask|withdraw-ask`
//!   (`resolve_main.rs:17-70`). **Catalogue lists `lib.sh, bd`: the code sources `conf.sh`,
//!   uses no `bd`, and forks `python3` and the `resolve` binary.**
//! - Pinned keys: `SPIRA_DB`/`COCKPIT_DB`, `SPIRA_ASK_LABEL`, `SPIRA_MOOT_TIMEOUT` (env default
//!   30, unregistered), `SPIRA_RESOLVE_SH` (test seam, `:44`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and script. `Observe` mode shadows `resolve` and
//! `withdraw`; `check` still runs, matching the script without `--apply`.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use runtime::ports::{Proc, ProcSpec};
use spira_config::lc_call::LcClient;

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "asks.moot";
pub const STREAM: &str = "task.asks.moot";
pub const TICK: EventKindTag = "asks.moot.tick";
pub const STATE: EventKindTag = "asks.moot.state";

pub const OBSERVE: &str = "asks.moot.observe";
pub const CHECK: &str = "asks.moot.check";
pub const RESOLVE: &str = "asks.moot.resolve";
pub const WITHDRAW: &str = "asks.moot.withdraw";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECK, RESOLVE, WITHDRAW];

/// The `timeout` exit a predicate that ran out of time returns.
pub const TIMED_OUT_RC: i32 = 124;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ASKS_MOOT_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_ASK_LABEL", "SPIRA_MOOT_TIMEOUT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 15_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ask {
    pub id: String,
    /// The `MOOT-WHEN:` predicate.
    pub pred: String,
    /// `work-bead:` labels of a legacy ask.
    pub works: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub asks: Vec<Ask>,
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
    Cleared,
    Resolved,
    Withdrawn,
    Live,
    TimedOut,
}

/// `state × event → state` per ask. `resolved` from `timed_out` is refused naming `timed_out`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "check", Phase::Checking),
    (Phase::Live, "check", Phase::Checking),
    (Phase::TimedOut, "check", Phase::Checking),
    (Phase::Checking, "rc:0", Phase::Cleared),
    (Phase::Checking, "rc:124", Phase::TimedOut),
    (Phase::Checking, "rc:other", Phase::Live),
    (Phase::Cleared, "resolve:ok", Phase::Resolved),
    (Phase::Resolved, "withdraw:ok", Phase::Withdrawn),
];

/// None in the old script; the per-ask phase and consecutive timeouts (three running emit one
/// inbox line, proposed) are carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub asks: BTreeMap<String, Phase>,
    pub timeouts: BTreeMap<String, u32>,
    /// The predicate and work beads of each ask the pass holds, for the resolve reason and the
    /// withdraw that follow its check.
    pub held: BTreeMap<String, Ask>,
}

pub struct AsksMoot {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// What [`CHECK`] carries: the predicate run under `bash -c`, bounded at `timeout_s`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub pred: String,
    pub timeout_s: u64,
}

/// What [`RESOLVE`] carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolve {
    pub id: String,
    pub reason: String,
}

/// What [`WITHDRAW`] carries: the ask resolved and the work beads its hold is lifted from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Withdraw {
    pub ask: String,
    pub works: Vec<String>,
}

/// The evidence a reason quotes: the first 300 bytes of the predicate's output (`head -c 300`).
pub const EVIDENCE_BYTES: usize = 300;

/// The actor every moot resolve records (invariant 3).
pub const ACTOR: &str = "claude";

fn phase_name(p: Phase) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn act(name: &str, key: String, body: &impl Serialize, deadline_ms: u64) -> Result<Act, String> {
    Ok(Act { name: name.into(), key, input: serde_json::to_string(body).map_err(|e| e.to_string())?, deadline_ms, probe: false })
}

/// `bytes` cut to at most `n`, on a char boundary.
fn head(text: &str, n: usize) -> &str {
    let mut end = text.len().min(n);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One check per MOOT-WHEN ask; `rc == 0` resolves, 124 is a timeout, any other rc is live.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let mut next = carry.clone();
    match input {
        Input::Fire => Ok((next, vec![])),
        Input::Observed(s) => {
            let timeout_s: u64 = cfg.get("SPIRA_MOOT_TIMEOUT").and_then(|v| v.trim().parse().ok()).filter(|t| *t > 0).ok_or("SPIRA_MOOT_TIMEOUT must be a positive number of seconds")?;
            // An ask still resolving keeps its place; every other one the store no longer
            // lists as open with a predicate is done with.
            next.asks.retain(|id, p| matches!(p, Phase::Cleared | Phase::Resolved) && !s.asks.iter().any(|a| &a.id == id));
            next.held.retain(|id, _| next.asks.contains_key(id));
            next.timeouts.retain(|id, _| s.asks.iter().any(|a| &a.id == id));
            let mut acts = vec![];
            for a in s.asks.iter().filter(|a| !a.pred.trim().is_empty()) {
                next.asks.insert(a.id.clone(), Phase::Checking);
                next.held.insert(a.id.clone(), a.clone());
                acts.push(act(CHECK, format!("{}/{:016x}/{now_ms}", a.id, runtime::event::fnv1a(&a.pred)), &Check { id: a.id.clone(), pred: a.pred.clone(), timeout_s }, timeout_s * 1000 + 5_000)?);
            }
            Ok((next, acts))
        }
        Input::Outcome { ok: false, .. } => Ok((next, vec![])),
        Input::Outcome { effect, body, .. } if effect == CHECK => {
            let out: CheckOutcome = serde_json::from_str(body).map_err(|e| format!("{NAME}: a check outcome that does not parse: {e}"))?;
            let phase = carry.asks.get(&out.id).copied();
            if phase != Some(Phase::Checking) {
                return Err(format!("{NAME}: {} is in state {}, not checking; its check outcome is refused", out.id, phase.map_or("unknown".into(), phase_name)));
            }
            match out.rc {
                0 => {
                    next.timeouts.remove(&out.id);
                    next.asks.insert(out.id.clone(), Phase::Cleared);
                    let pred = carry.held.get(&out.id).map(|a| a.pred.as_str()).unwrap_or_default();
                    let reason = format!("auto-resolved: MOOT-WHEN predicate cleared — {pred} → exit 0. Evidence: {}", head(&out.output, EVIDENCE_BYTES));
                    Ok((next, vec![act(RESOLVE, format!("{}/resolve", out.id), &Resolve { id: out.id, reason }, 15_000)?]))
                }
                TIMED_OUT_RC => {
                    *next.timeouts.entry(out.id.clone()).or_default() += 1;
                    next.asks.insert(out.id, Phase::TimedOut);
                    Ok((next, vec![]))
                }
                _ => {
                    next.timeouts.remove(&out.id);
                    next.asks.insert(out.id, Phase::Live);
                    Ok((next, vec![]))
                }
            }
        }
        Input::Outcome { effect, body, .. } if effect == RESOLVE || effect == WITHDRAW => {
            let id = body.trim();
            let (want, to) = if effect == RESOLVE { (Phase::Cleared, Phase::Resolved) } else { (Phase::Resolved, Phase::Withdrawn) };
            let phase = carry.asks.get(id).copied();
            if phase != Some(want) {
                return Err(format!("{NAME}: {id} is in state {}; a {effect} outcome needs {}", phase.map_or("unknown".into(), phase_name), phase_name(want)));
            }
            next.asks.insert(id.to_string(), to);
            let works = carry.held.get(id).map(|a| a.works.clone()).unwrap_or_default();
            if to == Phase::Resolved && !works.is_empty() {
                return Ok((next, vec![act(WITHDRAW, format!("{id}/withdraw"), &Withdraw { ask: id.to_string(), works }, 15_000)?]));
            }
            Ok((next, vec![]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// The asks a `content list --json` answer holds that this task checks: ask-labelled, status
/// exactly `open`, carrying a `MOOT-WHEN:` line (moot-sweep.sh's filter). An ask without one
/// never enters the snapshot (invariant 1).
pub fn moot_asks(list_json: &str, ask_label: &str) -> Result<Vec<Ask>, String> {
    let doc: serde_json::Value = serde_json::from_str(list_json).map_err(|e| format!("content list did not answer JSON: {e}"))?;
    let rows = match &doc {
        serde_json::Value::Array(a) => a.clone(),
        other => other.get("issues").and_then(|i| i.as_array()).cloned().unwrap_or_default(),
    };
    let mut out = vec![];
    for r in rows {
        let labels: Vec<&str> = r.get("labels").and_then(|l| l.as_array()).map(|l| l.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        if !labels.contains(&ask_label) || !spira_config::nonwork::is_open(spira_config::nonwork::Kind::Ask, spira_config::nonwork::status_of(spira_config::nonwork::Kind::Ask, &r)) {
            continue;
        }
        let desc = r.get("description").and_then(|d| d.as_str()).unwrap_or_default();
        let Some(pred) = desc.lines().find_map(|l| l.trim_start().strip_prefix("MOOT-WHEN:").map(str::trim).filter(|p| !p.is_empty())) else { continue };
        let works = labels.iter().filter_map(|l| l.strip_prefix("work-bead:")).map(str::to_string).collect();
        out.push(Ask { id: r.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string(), pred: pred.to_string(), works });
    }
    Ok(out)
}

/// The runner of [`OBSERVE`]: the attention beads over the store's `content` door, filtered.
pub fn observe(lc: &LcClient) -> Result<String, String> {
    let (db, label) = (spira_config::process::cfg("SPIRA_DB")?, spira_config::process::cfg("SPIRA_ASK_LABEL")?);
    if db.trim().is_empty() {
        return Err("no beads database (SPIRA_DB) — checked nothing".into());
    }
    let labels = format!("insight,{label},overseer");
    let rows = lc.answer(&["content", "list", "--all", "--limit", "0", "--label-any", &labels, "--json"]).map_err(|e| format!("the beads database is not reachable — checked nothing: {e}"))?;
    serde_json::to_string(&Snapshot { asks: moot_asks(&rows, &label)? }).map_err(|e| e.to_string())
}

/// The runner of [`CHECK`]: `bash -c <pred>` through `proc`, bounded at its timeout; a deadline
/// passed is rc 124 and a program that could not run is 127, so a failed probe never reads 0.
pub fn check(proc: &dyn Proc, input: &str) -> Result<String, String> {
    let c: Check = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if c.pred.trim().is_empty() {
        return Err(format!("{}: an empty predicate is refused", c.id));
    }
    let spec = ProcSpec { prog: "bash".into(), args: vec!["-c".into(), c.pred.clone()], stdin: vec![], deadline: std::time::Duration::from_secs(c.timeout_s) };
    let (rc, output) = match proc.run(&spec) {
        Ok(o) => (o.code.unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) if e.contains("deadline") => (TIMED_OUT_RC, e),
        Err(e) => (127, e),
    };
    serde_json::to_string(&CheckOutcome { id: c.id, rc, output: head(&output, EVIDENCE_BYTES).to_string() }).map_err(|e| e.to_string())
}

/// `cockpit_ops::resolve::run`'s store, over the socket. The per-work-bead withdraw is left to
/// [`WITHDRAW`], so here it answers 1 (nothing to lift), which `run` treats as quiet.
struct Store<'a>(&'a LcClient);

impl cockpit_ops::resolve::Closer for Store<'_> {
    fn close(&self, _: &std::path::Path, id: &str, reason: &str) -> cockpit_ops::resolve::BdResult {
        match self.0.call_stdin(&["close", id, "--reason-file", "-", "--actor", ACTOR], reason) {
            Ok(r) if r.code == 0 => cockpit_ops::resolve::BdResult { success: true, combined: String::new() },
            Ok(r) => cockpit_ops::resolve::BdResult { success: false, combined: format!("spira-lc close exit {}: {}", r.code, r.why()) },
            Err(e) => cockpit_ops::resolve::BdResult { success: false, combined: e.to_string() },
        }
    }
    fn show_json(&self, _: &std::path::Path, id: &str) -> Result<String, String> {
        self.0.answer(&["content", "show", id, "--json"]).map_err(|e| format!("content show {id}: {e}"))
    }
    fn withdraw_ask(&self, _: &str) -> (i32, String) {
        (1, "withdrawn by asks.moot.withdraw".into())
    }
    fn has_lifecycle_row(&self, id: &str) -> Result<bool, String> {
        codes(self.0, &["show", id])
    }
    fn is_ask(&self, id: &str) -> Result<bool, String> {
        codes(self.0, &["show-ask", id])
    }
    fn withdraw_ask_row(&self, id: &str, reason: &str) -> (i32, String) {
        match self.0.call(&["close-ask", id, "--exit", "withdrawn", "--quote", reason, "--actor", ACTOR]) {
            Ok(r) => (r.code, r.text()),
            Err(e) => (spira_config::lc_call::CANNOT_TELL, e.to_string()),
        }
    }
}

/// Exit 0 is yes, 1 is no, anything else cannot tell.
fn codes(lc: &LcClient, argv: &[&str]) -> Result<bool, String> {
    let r = lc.call(argv).map_err(|e| e.to_string())?;
    match r.code {
        0 => Ok(true),
        1 => Ok(false),
        c => Err(format!("spira-lc {} exit {c}: {}", argv[0], r.why())),
    }
}

/// The runner of [`RESOLVE`]: `cockpit_ops::resolve::run` as `claude`; answers the ask id.
pub fn resolve(lc: &LcClient, input: &str) -> Result<String, String> {
    let r: Resolve = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let db = spira_config::process::cfg("SPIRA_DB")?;
    match cockpit_ops::resolve::run(&r.id, &r.reason, std::path::Path::new(&db), &Store(lc)) {
        cockpit_ops::resolve::Outcome::Closed(_) => Ok(r.id),
        cockpit_ops::resolve::Outcome::Failed(why) => Err(why),
    }
}

/// The runner of [`WITHDRAW`]: `withdraw-ask <work> claude` per work bead; exit 1 (no row) and
/// 3 (no ask hold, or a terminal row) are quiet. Answers the ask id.
pub fn withdraw(lc: &LcClient, input: &str) -> Result<String, String> {
    let w: Withdraw = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let mut failed = vec![];
    for work in &w.works {
        match lc.call(&["withdraw-ask", work, ACTOR]) {
            Ok(r) if matches!(r.code, 0 | 1 | 3) => {}
            Ok(r) => failed.push(format!("{work}: exit {}: {}", r.code, r.why())),
            Err(e) => failed.push(format!("{work}: {e}")),
        }
    }
    if failed.is_empty() {
        Ok(w.ask)
    } else {
        Err(format!("the ask hold was not lifted on {}", failed.join("; ")))
    }
}

crate::tasks::b::hosted!(AsksMoot);
