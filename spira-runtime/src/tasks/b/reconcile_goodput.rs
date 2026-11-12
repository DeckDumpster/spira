//! # reconcile.goodput — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-goodput.md`;
//! its Acceptance section is the test file `tests/it/task_b_reconcile_goodput.rs`.
//!
//! ## Tick
//!
//! - Cadence 60 s (timer `OnUnitActiveSec=60s`, `OnBootSec=2min`, `AccuracySec=5s`). Boot tick at +120 s.
//! - Woken also by `section.published` for any section (unverified: no such event exists today; the
//!   store's `watch` long-poll of S7b would supply it). Without it, cadence alone.
//! - Single-flight: one pass in flight per task. The store-side lock `locks/reconciler`
//!   (`desired-state/src/intent.rs:17`) stays as the fencing token every write carries; in-process
//!   single-flight does not replace it, because round-controller's intents also fence on it
//!   (unverified: `round-controller/src/intents/tests.rs` uses the engine).
//! - No `world guard`: the unit runs on the observability plane (`systemd/spira-reconciler-goodput.service`
//!   `[X-Spira] Plane=observability`). A halt is an input, not a skip (see Observe).
//!
//! ## Observe
//!
//! Probe `reconcile.goodput.observe` returns:
//!
//! ```text
//! Snapshot { halted: bool, section_snapshot: Value /* spira-lc section snapshot */, lock: LockAnswer }
//! ```
//!
//! - `section snapshot` — store verb (`reconciler-engine/src/goodput_pass.rs:223`), parsed by
//!   `assemble::Snapshot::parse` (`reconciler-engine/src/assemble.rs:52`). Holds sections, freshness,
//!   previous pass record, intents. No re-derivation: the ranking reads only section distances.
//! - `world.halted` — today `run_dir/world.halted` existence (`reconciler/src/goodput_pass.rs:58`),
//!   same stamp `world guard` reads (`spira-world/src/guard.rs:11`). A halt makes the pass
//!   `Mode::Halted`: it records and judges nothing, it does not skip (`goodput_pass.rs:147,242`).
//! - Lock acquisition is **not** an observe: it is the first act (below). The probe reads; it never
//!   takes the lock.
//!
//! ## Decide
//!
//! `decide(carry, snapshot, cfg) -> (Carry, Plan)`: `assemble` (`assemble.rs:181`) → `rank`
//! (`goodput_pass.rs:69`) → `select` (`reconciler-engine/src/select.rs:567`). Already pure; it reads
//! time only from `now_ms` passed in. Output: judgements, steps, would-steps, refusals, the
//! `PassRecord` (`goodput_pass.rs:250-268`).
//!
//! - Carry: the last pass record's identity `{n, list_rev, gaps, steps, judged, refused}` digest, so
//!   an unchanged pass is recognised (see Silence). Mode and binding come from the snapshot.
//! - Config (pinned, S10): `SPIRA_RECONCILER_PASS_TIMEOUT_SECS` (default 50,
//!   `spira/conf.d/SPIRA_RECONCILER_PASS_TIMEOUT_SECS`; lock TTL and pass deadline,
//!   `reconciler/src/goodput_pass.rs:56`), `SPIRA_RECONCILER_SHADOW_KINDS` (default
//!   `RoundController Lifecycle Alarms`, `goodput_pass.rs:57`; already in `spira-runtime/src/settings.rs:113`).
//! - Fixtures: `reconciler-engine/src/select/tests.rs` (9 tests), `assemble/tests.rs`,
//!   `spira-lc/src/sections/tests/goodput_pass.rs` (the engine on a rig store).
//!
//! ## Act
//!
//! The pass is a short machine on one lock: acquire → judge* → (put → get)* → record → release. Each
//! leg is an effect; the next leg is decided on the previous outcome, so no effect blocks the loop.
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.goodput.lock` (`lock acquire reconciler --ttl T --holder H`, `goodput_pass.rs:197`) | store | 5 s | holder = task invocation id (`reconciler/src/goodput_pass.rs:44`) | `locked` → `reconcile.goodput.standby` |
//! | `reconcile.goodput.standby` (`pass standby --holder H`, `:202`) | store | 5 s | holder | store error |
//! | `reconcile.goodput.judge` (`intent judge <key> --token`, `:155`) | store | 5 s | intent key + token | halted, shadowed kind, deadline passed (`:147`) |
//! | `reconcile.goodput.put` (`intent put <key> --token --json`, `:177`) then read-back `intent get` (`:183`) | store | 5 s | intent key + intent id | deadline (`:175`); store refusal code; `no-read-back` (`:186`) |
//! | `reconcile.goodput.record` (`pass record --token --json`, `:271`) | store | 5 s | pass n + token | deadline passed (`:270`) |
//! | `reconcile.goodput.release` (`lock release --token --holder`, `:211`) | store | 5 s | token | none; always attempted once the token was read |
//!
//! - Pass deadline: `TIMEOUT_SECS` from the lock grant, on the injected clock (`goodput_pass.rs:136`).
//! - Metrics (`reconciler/src/goodput_pass.rs:62-86`) become wide-event fields on the record outcome
//!   (`ctx.event().set`), not an effect.
//!
//! ## Silence
//!
//! Idle means: no judgement due, no step planned, no refusal, and the gaps equal the carried record.
//! Today every pass writes a lock pair and a pass record each minute, idle or not. Proposed: an idle
//! tick emits the probe and nothing else. **Open question**: alarms' dead-man reads the pass
//! record's age (`incident/src/alarms_pass.rs:379`, `SPIRA_RECONCILER_DEADMAN_SECS`=180); an idle
//! pass that stops recording would trip it. Either the record is kept as a heartbeat at
//! ≤ deadman/2 (every 90 s idle), or the dead-man reads `scheduler status` (S13) instead. Default:
//! record at most every 90 s when idle; record at once on any change.
//!
//! ## Replaces
//!
//! - `spira-reconciler-goodput-prod.service` + `.timer`; `bin/reconciler goodput-pass`
//!   (`reconciler/src/main.rs:17`). `reconciler arm-check` stays a CLI.
//! - Log `run/reconciler-goodput.log` (service template `StandardOutput=append:`).
//! - Children: `spira-lc` × (4 + judgements + 2·steps) per pass via `bounded`
//!   (`reconciler/src/goodput_pass.rs:28`) → `LcClient` (S7a).
//! - Keys pinned: `SPIRA_RECONCILER_PASS_TIMEOUT_SECS`, `SPIRA_RECONCILER_SHADOW_KINDS`,
//!   `SPIRA_LC_PASSWORD_FILE` (already in `spira-runtime/src/main.rs:42`), `INVOCATION_ID` (dropped:
//!   the holder becomes `reconcile.goodput/<tick event id>`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Without a release: set the task to `Observe` (S4); its
//! writes are journaled as `effect.shadowed`. Re-enabling the old timer while the task is in Act is
//! safe only because the store lock fences both (the loser stands by).
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.goodput";
pub const STREAM: &str = "task.reconcile.goodput";
pub const TICK: EventKindTag = "reconcile.goodput.tick";
pub const STATE: EventKindTag = "reconcile.goodput.state";

pub const OBSERVE: &str = "reconcile.goodput.observe";
pub const LOCK: &str = "reconcile.goodput.lock";
pub const STANDBY: &str = "reconcile.goodput.standby";
pub const JUDGE: &str = "reconcile.goodput.judge";
pub const PUT: &str = "reconcile.goodput.put";
pub const RECORD: &str = "reconcile.goodput.record";
pub const RELEASE: &str = "reconcile.goodput.release";
pub const EFFECTS: &[&str] = &[OBSERVE, LOCK, STANDBY, JUDGE, PUT, RECORD, RELEASE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_GOODPUT_SECS";
pub const PASS_TIMEOUT_KEY: &str = "SPIRA_RECONCILER_PASS_TIMEOUT_SECS";
pub const SHADOW_KINDS_KEY: &str = "SPIRA_RECONCILER_SHADOW_KINDS";
/// The idle heartbeat record is kept at most every deadman/2 (Silence, default).
pub const DEADMAN_KEY: &str = "SPIRA_RECONCILER_DEADMAN_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, PASS_TIMEOUT_KEY, SHADOW_KINDS_KEY, DEADMAN_KEY, "SPIRA_LC_PASSWORD_FILE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// The store's lock answer as the probe read it; the probe never takes the lock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockAnswer {
    pub held_by: Option<String>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub halted: bool,
    /// The `spira-lc section snapshot`, parsed by `assemble::Snapshot::parse`.
    pub section_snapshot: serde_json::Value,
    pub lock: LockAnswer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Locking,
    Standby,
    Judging,
    Stepping,
    Recording,
    Releasing,
}

/// `state × event → state` (T3). An effect asked in `stepping` once the pass deadline has passed
/// is refused naming `deadline`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Idle, "observed:work", Phase::Locking),
    (Phase::Locking, "lock:granted", Phase::Judging),
    (Phase::Locking, "lock:locked", Phase::Standby),
    (Phase::Standby, "standby:done", Phase::Idle),
    (Phase::Judging, "judge:done", Phase::Stepping),
    (Phase::Stepping, "put:done", Phase::Recording),
    (Phase::Recording, "record:done", Phase::Releasing),
    (Phase::Releasing, "release:done", Phase::Idle),
];

/// The identity of the last pass record, so an unchanged pass is recognised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PassDigest {
    pub n: u64,
    pub list_rev: u64,
    pub gaps: Vec<String>,
    pub steps: Vec<String>,
    pub judged: Vec<String>,
    pub refused: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last: PassDigest,
    pub recorded_at: Option<u64>,
    /// The lock grant's token and the pass deadline it set, while a pass is in flight.
    pub token: Option<String>,
    pub deadline_at: Option<u64>,
    /// This pass's identity on the lock, `reconcile.goodput/<tick ms>`, while a pass is in flight.
    pub holder: Option<String>,
    /// The in-flight pass saw the world halted: it records `Mode::Halted` and judges nothing.
    pub halted: bool,
}

pub struct ReconcileGoodput {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Locking => "locking",
        Phase::Standby => "standby",
        Phase::Judging => "judging",
        Phase::Stepping => "stepping",
        Phase::Recording => "recording",
        Phase::Releasing => "releasing",
    }
}

fn secs(cfg: &Cfg, key: &str) -> Result<u64, String> {
    cfg.get(key).and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v > 0).ok_or_else(|| format!("{key} must be a positive integer"))
}

fn shadow(cfg: &Cfg) -> Vec<String> {
    cfg.get(SHADOW_KINDS_KEY).map(|v| v.split_whitespace().map(String::from).collect()).unwrap_or_default()
}

fn act(name: &str, key: &str, input: &impl Serialize) -> Result<Act, String> {
    Ok(Act { name: name.into(), key: format!("{NAME}/{name}/{key}"), input: serde_json::to_string(input).map_err(|e| e.to_string())?, deadline_ms: LEG_DEADLINE_MS, probe: false })
}

/// Every leg's deadline: one store call (5 s), or for the judge and step legs a few in a row,
/// each bounded by the socket client's own 5 s.
pub const LEG_DEADLINE_MS: u64 = 30_000;

/// Whether the snapshot has work for a pass: a halt (Mode::Halted still records), a judgement or
/// step due, or open gaps and refusals that differ from the last record. A snapshot the strict
/// parser refuses is read for its due intents and stale sections alone, so an unreadable answer
/// never takes the lock every minute.
pub fn has_work(cfg: &Cfg, carry: &Carry, snap: &Snapshot, now_ms: u64) -> bool {
    if snap.halted {
        return true;
    }
    match reconciler_engine::goodput_pass::legs::preview(&snap.section_snapshot, now_ms, &shadow(cfg), false) {
        Ok(p) => p.judgements > 0 || p.steps > 0 || p.gaps != carry.last.gaps || p.refused != carry.last.refused,
        Err(_) => {
            let listed = |f: &str| snap.section_snapshot.get(f).and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
            listed("intents").iter().any(|i| i.get("due").and_then(serde_json::Value::as_bool) == Some(true)) || listed("sections").iter().any(|x| x.get("fresh").and_then(serde_json::Value::as_bool) == Some(false))
        }
    }
}

/// The legs' inputs, as the runners read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockIn {
    pub holder: String,
    pub ttl_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseIn {
    pub token: String,
    pub holder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepIn {
    pub leg: reconciler_engine::goodput_pass::legs::LegConfig,
    pub judged: reconciler_engine::goodput_pass::legs::Judged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordIn {
    pub token: String,
    pub record: reconciler_engine::pass_record::PassRecord,
}

fn digest(r: &reconciler_engine::pass_record::PassRecord) -> PassDigest {
    use reconciler_engine::goodput_pass::legs::refusal_key;
    PassDigest { n: r.n, list_rev: r.list_rev, gaps: r.gaps.iter().map(|g| g.section.clone()).collect(), steps: r.steps.iter().map(|s| format!("{}/{}", s.section, s.action)).collect(), judged: r.judged.iter().map(|j| j.intent.clone()).collect(), refused: r.refused.iter().map(refusal_key).collect() }
}

/// `assemble` → `rank` → `select`, one leg per outcome: lock, judge, step, record,
/// release. A leg's outcome after the pass deadline is refused naming it; the next observe
/// releases the lock of a pass that ran past it.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    use reconciler_engine::goodput_pass::legs::{Judged, LegConfig};
    let leg = |c: &Carry| -> Result<LegConfig, String> {
        Ok(LegConfig { token: c.token.clone().ok_or("no token is carried")?, holder: c.holder.clone().unwrap_or_default(), shadow: shadow(cfg), halted: c.halted, deadline_at_ms: c.deadline_at.unwrap_or(0) })
    };
    let idle = |c: &Carry| Carry { phase: Phase::Idle, token: None, deadline_at: None, holder: None, halted: false, ..c.clone() };
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(snap) => {
            if carry.phase != Phase::Idle {
                if carry.deadline_at.is_some_and(|d| now_ms >= d) && carry.token.is_some() {
                    let release = ReleaseIn { token: carry.token.clone().unwrap_or_default(), holder: carry.holder.clone().unwrap_or_default() };
                    return Ok((Carry { phase: Phase::Releasing, ..carry.clone() }, vec![act(RELEASE, &release.token, &release)?]));
                }
                if carry.deadline_at.is_none_or(|d| now_ms >= d) {
                    return Ok((idle(carry), vec![]));
                }
                return Ok((carry.clone(), vec![]));
            }
            if !has_work(cfg, carry, snap, now_ms) {
                return Ok((carry.clone(), vec![]));
            }
            let timeout = secs(cfg, PASS_TIMEOUT_KEY)?;
            let holder = format!("{NAME}/{now_ms}");
            let next = Carry { phase: Phase::Locking, holder: Some(holder.clone()), token: None, deadline_at: Some(now_ms + timeout * 1000), halted: snap.halted, ..carry.clone() };
            Ok((next, vec![act(LOCK, &holder, &LockIn { holder: holder.clone(), ttl_secs: timeout })?]))
        }
        Input::Outcome { effect, ok, body } => {
            let expected = match carry.phase {
                Phase::Locking => LOCK,
                Phase::Standby => STANDBY,
                Phase::Judging => JUDGE,
                Phase::Stepping => PUT,
                Phase::Recording => RECORD,
                Phase::Releasing => RELEASE,
                Phase::Idle => "",
            };
            if effect != expected {
                return Err(format!("{NAME}: in state {} no outcome of {effect} is expected", phase_name(carry.phase)));
            }
            let under_lock = matches!(carry.phase, Phase::Judging | Phase::Stepping | Phase::Recording);
            if under_lock && carry.deadline_at.is_some_and(|d| now_ms >= d) {
                return Err(format!("{NAME}: in state {} the pass deadline passed at {} ms; no further write is made", phase_name(carry.phase), carry.deadline_at.unwrap_or(0)));
            }
            let release = |c: &Carry| -> Result<(Carry, Vec<Act>), String> {
                let r = ReleaseIn { token: c.token.clone().unwrap_or_default(), holder: c.holder.clone().unwrap_or_default() };
                Ok((Carry { phase: Phase::Releasing, ..c.clone() }, vec![act(RELEASE, &r.token, &r)?]))
            };
            match carry.phase {
                Phase::Locking if *ok => {
                    let v: serde_json::Value = serde_json::from_str(body.trim()).map_err(|e| format!("{NAME}: the lock grant is not JSON: {e}"))?;
                    let token = v.get("token").and_then(serde_json::Value::as_str).ok_or_else(|| format!("{NAME}: the lock grant names no token"))?.to_string();
                    let next = Carry { phase: Phase::Judging, token: Some(token), ..carry.clone() };
                    let l = leg(&next)?;
                    Ok((next, vec![act(JUDGE, &l.token, &l)?]))
                }
                Phase::Locking if body.contains("locked") => Ok((Carry { phase: Phase::Standby, ..carry.clone() }, vec![act(STANDBY, carry.holder.as_deref().unwrap_or_default(), &LockIn { holder: carry.holder.clone().unwrap_or_default(), ttl_secs: 0 })?])),
                Phase::Locking | Phase::Standby => Ok((idle(carry), vec![])),
                Phase::Judging if *ok => {
                    let judged: Judged = serde_json::from_str(body).map_err(|e| format!("{NAME}: the judge leg's answer does not decode: {e}"))?;
                    let l = leg(carry)?;
                    if judged.steps.is_empty() {
                        return Ok((Carry { phase: Phase::Recording, ..carry.clone() }, vec![act(RECORD, &l.token.clone(), &RecordIn { token: l.token, record: judged.record })?]));
                    }
                    Ok((Carry { phase: Phase::Stepping, ..carry.clone() }, vec![act(PUT, &l.token.clone(), &StepIn { leg: l, judged })?]))
                }
                Phase::Stepping if *ok => {
                    let record: reconciler_engine::pass_record::PassRecord = serde_json::from_str(body).map_err(|e| format!("{NAME}: the step leg's answer does not decode: {e}"))?;
                    Ok((Carry { phase: Phase::Recording, ..carry.clone() }, vec![act(RECORD, &leg(carry)?.token, &RecordIn { token: leg(carry)?.token, record })?]))
                }
                Phase::Recording if *ok => {
                    let last = serde_json::from_str::<RecordIn>(body).map(|r| digest(&r.record)).unwrap_or_else(|_| carry.last.clone());
                    release(&Carry { last, recorded_at: Some(now_ms), ..carry.clone() })
                }
                Phase::Judging | Phase::Stepping | Phase::Recording => release(carry),
                Phase::Releasing => Ok((idle(carry), vec![])),
                Phase::Idle => unreachable!("an idle task expects no outcome, refused above"),
            }
        }
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {r:?}", phase_name(carry.phase))),
    }
}

use reconciler_engine::goodput_pass::Socket;

/// The runner of [`OBSERVE`]: the halt stamp and the store's `section snapshot`. It never
/// takes the lock.
pub fn observe(lc: &spira_store::lc_call::LcClient, run_dir: &std::path::Path) -> Result<String, String> {
    let section_snapshot: serde_json::Value = serde_json::from_str(lc.answer(&["section", "snapshot"]).map_err(String::from)?.trim()).map_err(|e| format!("section snapshot answered non-JSON: {e}"))?;
    serde_json::to_string(&Snapshot { halted: run_dir.join("world.halted").exists(), section_snapshot, lock: LockAnswer::default() }).map_err(|e| e.to_string())
}

/// The runner of [`LOCK`]: the grant's `{"token"}`, or a failure reading `locked`.
pub fn lock(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    use reconciler_engine::goodput_pass::legs::{acquire, Lock};
    let i: LockIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
    match acquire(&Socket(lc.clone()), &i.holder, i.ttl_secs)? {
        Lock::Granted(token) => Ok(serde_json::json!({ "token": token }).to_string()),
        Lock::Locked => Err("locked".into()),
    }
}

/// The runner of [`STANDBY`].
pub fn standby(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    let i: LockIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
    reconciler_engine::goodput_pass::legs::standby(&Socket(lc.clone()), &i.holder).map(|v| v.to_string())
}

/// The runner of [`JUDGE`]: the snapshot under the lock, the plan and the judgements.
pub fn judge(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    let l: reconciler_engine::goodput_pass::legs::LegConfig = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let judged = reconciler_engine::goodput_pass::legs::judge(&Socket(lc.clone()), &runtime::ports::WallClock, &l)?;
    serde_json::to_string(&judged).map_err(|e| e.to_string())
}

/// The runner of [`PUT`]: every planned step written and read back; the record with them.
pub fn put(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    let i: StepIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let (record, _) = reconciler_engine::goodput_pass::legs::step(&Socket(lc.clone()), &runtime::ports::WallClock, &i.leg, i.judged);
    serde_json::to_string(&record).map_err(|e| e.to_string())
}

/// The runner of [`RECORD`]: `pass record`; the answer echoes the record so the carry keeps
/// its digest.
pub fn record(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    let i: RecordIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
    reconciler_engine::goodput_pass::legs::record(&Socket(lc.clone()), &i.token, &i.record)?;
    Ok(input.to_string())
}

/// The runner of [`RELEASE`].
pub fn release(lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    let i: ReleaseIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
    reconciler_engine::goodput_pass::legs::release(&Socket(lc.clone()), &i.token, &i.holder).map(|()| "released".into())
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase"), runtime::rows::Part::Field("last"), runtime::rows::Part::Field("recorded_at"), runtime::rows::Part::Field("token"), runtime::rows::Part::Field("deadline_at"), runtime::rows::Part::Field("holder"), runtime::rows::Part::Field("halted")];

super::hosted!(ReconcileGoodput);
