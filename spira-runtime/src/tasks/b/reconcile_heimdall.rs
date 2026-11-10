//! # reconcile.heimdall — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-heimdall.md`;
//! its Acceptance section is the test file `tests/it/task_b_reconcile_heimdall.rs`.
//!
//! **Scope (rulings of 2026-11-03, S15).** Only heimdall's in-scheduler checks move here. The
//! outside watch, the one that sees the scheduler itself wedged or absent, is the watchdog
//! watcher on the platform's timer, not this task. So the task reads the sentinel through
//! `scheduler status` and never forks `systemctl`; it does not judge its own scheduler.
//!
//! ## Tick
//!
//! - `Timer`, every 2 min. The unit uses `OnUnitActiveSec=2min`, `OnBootSec=420`, `AccuracySec=30s` and `TimeoutStartSec=60`.
//! - The first tick is at boot + 420 s. No event wakes it. It runs single-flight.
//!
//! ## Observe
//!
//! `reconcile.heimdall.observe` is a Probe. Its snapshot is the existing `classify::Observation` (`heimdall/src/classify.rs`; built at `main.rs:227-248`). Each field must come from the component that owns it:
//!
//! | Field | Today (`path:line`) | Hosted source |
//! |---|---|---|
//! | `sentinel_log` tail, mtime | file read, 256 KiB (`main.rs:185`, `gather.rs:22`) | the `sentinel.pass` task's own status (S13), not its log |
//! | `sentinel_timer` | `systemctl --user is-enabled/is-active` (`gather.rs:59-66,133`) | `scheduler status` → `sentinel.pass` armed/last-fired (S13) |
//! | `restart_alerts` | `systemctl list-units` + `show NRestarts` (`gather.rs:68-96`, `main.rs:192-203`) | `unit.facts` (S6) for the P6 units that remain, plus per-task effect-failure counts from S13 |
//! | `db_reachable`, `alert_rows` | `spira-lc condition list --owner heimdall`, timeout 10 s (`conditions.rs:70-92`, `main.rs:207-217`) | store verb `condition list` |
//! | `mirror` | stat of `raw/spira-beads/spira.jsonl` (`gather.rs:155`, `main.rs:161,188`) | the `sweep.export_beads` task's last outcome |
//! | `world_halted/draining` | run-dir stamps, with `date -d` forked to parse them (`gather.rs:180-205`) | the `world` status read, with time parsed in-process |
//! | `strands` | `$SPIRA_RUN/strands.json` (`main.rs:190`) | unchanged file read (`unverified` who writes it) |
//!
//! The write probe (`alerts::maintain_probe`, `alerts.rs:66-71`) calls `condition set heimdall-probe … --status False` on every pass. It stays part of observe: "the alert query is also the read probe" (`main.rs:205`).
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)` composes two functions that are already pure:
//!
//! - `judge::classify(&obs)` (`judge.rs:171`)
//! - `reconcile::step_keys(now, confirm=2, clear=2, refresh=3600, db_reachable, firing, prior)` (`reconcile.rs:93`)
//!
//! **Carry** is `heimdall::state::State`: `first_run`, per-key `KeyState{seen, unseen, state, first, flaps, refreshed, bead}`, restart baselines and `probe_id`. Today it is the file `$SPIRA_RUN/heimdall.state` (`main.rs:156,178-182,357`). It becomes a journaled `reconcile.heimdall.state` Emit, written only when it changes.
//!
//! **Fixtures** are the existing tests in `reconcile.rs`, `judge.rs`, `restart.rs` and `pass.rs`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.heimdall.raise` → `condition set <key> --type alert --status True --owner heimdall` (`alerts.rs:42-45`) | store | 10 s (`SPIRA_HEIMDALL_BD_TIMEOUT`, `main.rs:95`) | `heimdall/<key>/raise/<flaps>`, because the store counts flaps (`alerts.rs:40-41`) | any write while `db_reachable=false` (`reconcile.rs:108`) |
//! | `reconcile.heimdall.clear` → `condition clear <key>` (`alerts.rs:48-51`) | store | 10 s | `heimdall/<key>/clear/<flaps>` | a key that is not firing (a no-op success, `alerts.rs:49`) |
//! | `reconcile.heimdall.refresh` → `condition set` on a firing key after 3600 s | store | 10 s | `heimdall/<key>/refresh/<refreshed>` | a key closed by hand (`main.rs:318-320`) |
//! | `reconcile.heimdall.fallback` → write or remove `$SPIRA_RUN/heimdall.alerts.json` (`fallback.rs`, `main.rs:342-354`) | host | 2 s | the content hash of the doc | nothing |
//! | `reconcile.heimdall.heartbeat` → `$SPIRA_RUN/heimdall.status` (`heartbeat.rs`, `main.rs:363-364`) | host | 2 s | `at` | nothing |
//!
//! Today a timeout (exit 124) is classed as *saturated*, not unreachable (`conditions.rs:61-67`). An effect `timed_out` keeps that meaning.
//!
//! ## Silence
//!
//! A tick where nothing is firing and every counter is unchanged emits no store write.
//!
//! That fails two ways today:
//!
//! 1. The write probe writes on every pass, by design.
//! 2. `heimdall.status` is rewritten every pass and is read by `cockpit-collect/src/probes/mod.rs` (`unverified` line).
//!
//! The ruling: the probe stays an unjournaled Probe. The heartbeat moves into `scheduler status` (S13), and the file is dropped once the pane reads S13. Until then the heartbeat is the one allowed per-tick host write, and it is not journaled.
//!
//! ## Replaces
//!
//! **Units and logs:** `spira-heimdall-prod.{service,timer}`, `StandardOutput=append:$SPIRA_RUN/heimdall.log`.
//!
//! **Children per pass** (catalogue: "(param runner)"):
//!
//! - `systemctl` ×4+: is-enabled, is-active, list-units, show
//! - `spira-lc condition` ×2+: list, the probe set, plus one per raise or clear
//! - `date` ×1–N (`gather.rs:183`, `util.rs:147`)
//!
//! **Keys pinned:**
//!
//! - registered: `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_WIKI`, `SPIRA_EXPORTER`, `SPIRA_INSTANCE`, `SPIRA_TZ`, `SPIRA_HEIMDALL_RESTARTS`, `SPIRA_HEIMDALL_RESTART_WINDOW`
//! - ad hoc: `SPIRA_HEIMDALL_{BD_TIMEOUT,FALLBACK,SENTINEL_LOG,STRANDS,MIRROR,TAIL_BYTES,CONFIRM,CLEAR,REFRESH,PASS_STALE,STARVE_PASSES,MIRROR_STALE,GHOST_STALE}`, `SPIRA_SYSTEMCTL` (`main.rs:95-175`)
//!
//! All of these join `Settings::KEYS` (S10).
//!
//! ## Reverse
//!
//! Release rollback restores the unit. With the task in `Observe` mode, raise and clear are shadowed. The probe and heartbeat keep running because they are observe. Conditions that are already raised stay raised until the task acts again.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.heimdall";
pub const STREAM: &str = "task.reconcile.heimdall";
pub const TICK: EventKindTag = "reconcile.heimdall.tick";
pub const STATE: EventKindTag = "reconcile.heimdall.state";

pub const OBSERVE: &str = "reconcile.heimdall.observe";
pub const RAISE: &str = "reconcile.heimdall.raise";
pub const CLEAR: &str = "reconcile.heimdall.clear";
pub const REFRESH: &str = "reconcile.heimdall.refresh";
pub const FALLBACK: &str = "reconcile.heimdall.fallback";
pub const HEARTBEAT: &str = "reconcile.heimdall.heartbeat";
pub const EFFECTS: &[&str] = &[OBSERVE, RAISE, CLEAR, REFRESH, FALLBACK, HEARTBEAT];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_HEIMDALL_SECS";
/// The in-scheduler checks' keys only. `SPIRA_SYSTEMCTL`, `SPIRA_HEIMDALL_SENTINEL_LOG` and
/// `SPIRA_HEIMDALL_TAIL_BYTES` are not read: the sentinel is a sibling task read through
/// `scheduler status` (S13), and the scheduler itself is watched from outside by the watchdog
/// (S15), not by this task.
/// The scheduler's own control socket, where the observe reads `scheduler status` (S13).
pub const CONTROL_SOCKET_KEY: &str = "SPIRA_RUNTIME_SOCKET";
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    CONTROL_SOCKET_KEY,
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_WIKI",
    "SPIRA_EXPORTER",
    "SPIRA_INSTANCE",
    "SPIRA_TZ",
    "SPIRA_HEIMDALL_RESTARTS",
    "SPIRA_HEIMDALL_RESTART_WINDOW",
    "SPIRA_HEIMDALL_BD_TIMEOUT",
    "SPIRA_HEIMDALL_FALLBACK",
    "SPIRA_HEIMDALL_STRANDS",
    "SPIRA_HEIMDALL_MIRROR",
    "SPIRA_HEIMDALL_CONFIRM",
    "SPIRA_HEIMDALL_CLEAR",
    "SPIRA_HEIMDALL_REFRESH",
    "SPIRA_HEIMDALL_PASS_STALE",
    "SPIRA_HEIMDALL_STARVE_PASSES",
    "SPIRA_HEIMDALL_MIRROR_STALE",
    "SPIRA_HEIMDALL_GHOST_STALE",
];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: Some(CONTROL_SOCKET_KEY) };

/// A sibling task as `scheduler status` (S13) reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskStatus {
    pub armed: bool,
    pub last_fired_ms: Option<u64>,
    pub effect_failures: u64,
}

/// `classify::Observation`, each field from the component that owns it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// `sentinel.pass` from `scheduler status`, replacing its log tail and its timer.
    pub sentinel: TaskStatus,
    /// (unit, restarts in the window) from `unit.facts` for the P6 units that remain.
    pub restart_alerts: Vec<(String, u64)>,
    pub db_reachable: bool,
    /// Keys of the conditions the store holds raised with owner heimdall.
    pub alert_rows: Vec<String>,
    /// `sweep.export_beads`' last outcome age, replacing the mirror file's mtime.
    pub mirror_age_secs: Option<u64>,
    pub world_halted: bool,
    pub world_draining: bool,
    pub strands: Vec<String>,
}

/// Per key (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Quiet,
    Seen,
    Firing,
    Unseen,
    Cleared,
    Refreshed,
}

/// `state × event → state` per key. A raise while the store is unreachable is refused, naming
/// `unreachable`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Quiet, "seen", Phase::Seen),
    (Phase::Seen, "seen:confirm", Phase::Firing),
    (Phase::Seen, "unseen", Phase::Quiet),
    (Phase::Firing, "unseen", Phase::Unseen),
    (Phase::Firing, "refresh:due", Phase::Refreshed),
    (Phase::Refreshed, "seen", Phase::Firing),
    (Phase::Unseen, "seen", Phase::Firing),
    (Phase::Unseen, "unseen:clear", Phase::Cleared),
    (Phase::Cleared, "seen", Phase::Seen),
];

/// `heimdall::state::KeyState`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyState {
    pub phase: Phase,
    pub seen: u32,
    pub unseen: u32,
    pub first: Option<u64>,
    pub flaps: u32,
    pub refreshed: Option<u64>,
    pub bead: Option<String>,
}

/// `heimdall::state::State`, journaled only when it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub first_run: bool,
    pub keys: BTreeMap<String, KeyState>,
    pub restart_baselines: BTreeMap<String, u64>,
    pub probe_id: Option<String>,
}

pub struct ReconcileHeimdall {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// The sentinel task, as `scheduler status` names it.
pub const SENTINEL_TASK: &str = "sentinel.pass";
/// The export task whose last success is the mirror's age.
pub const EXPORT_TASK: &str = "sweep.export_beads";

fn num(cfg: &Cfg, key: &str) -> Result<i64, String> {
    cfg.get(key).and_then(|v| v.trim().parse::<i64>().ok()).filter(|v| *v > 0).ok_or_else(|| format!("{key} must be a positive integer"))
}

/// A threshold whose check is off when it is not a positive number: the check is not run,
/// rather than run on an invented value.
fn optional(cfg: &Cfg, key: &str) -> Option<i64> {
    cfg.get(key).and_then(|v| v.trim().parse::<i64>().ok()).filter(|v| *v > 0)
}

fn to_heimdall(k: &KeyState) -> heimdall::state::KeyState {
    let firing = matches!(k.phase, Phase::Firing | Phase::Unseen | Phase::Refreshed);
    heimdall::state::KeyState { state: if firing { "firing" } else { "clear" }.into(), seen: k.seen.into(), unseen: k.unseen.into(), since: 0, first: k.first.map_or(0, |ms| (ms / 1000) as i64), flaps: k.flaps.into(), bead: k.bead.clone().unwrap_or_default(), refreshed: k.refreshed.map_or(0, |ms| (ms / 1000) as i64) }
}

fn from_heimdall(h: &heimdall::state::KeyState, phase: Phase) -> KeyState {
    let ms = |s: i64| (s > 0).then_some(s as u64 * 1000);
    KeyState { phase, seen: h.seen.max(0) as u32, unseen: h.unseen.max(0) as u32, first: ms(h.first), flaps: h.flaps.max(0) as u32, refreshed: ms(h.refreshed), bead: Some(h.bead.clone()).filter(|b| !b.is_empty()) }
}

/// The readings `heimdall::hosted::classify` takes, from the snapshot. A strand is
/// `<bead>\t<first epoch secs>`, and counts once it is older than the ghost threshold.
fn readings(cfg: &Cfg, snap: &Snapshot, now: i64) -> heimdall::hosted::Readings {
    let ghost = optional(cfg, "SPIRA_HEIMDALL_GHOST_STALE");
    let strands = snap.strands.iter().filter_map(|s| {
        let (id, first) = s.split_once('\t').unwrap_or((s, "0"));
        let first: i64 = first.trim().parse().unwrap_or(0);
        ghost.filter(|g| first > 0 && now - first > *g).map(|_| id.to_string())
    });
    heimdall::hosted::Readings { now, sentinel_armed: snap.sentinel.armed, sentinel_last_fired: snap.sentinel.last_fired_ms.map(|ms| (ms / 1000) as i64), restart_alerts: snap.restart_alerts.clone(), mirror_age_secs: snap.mirror_age_secs, world_halted: snap.world_halted, world_draining: snap.world_draining, strands: strands.collect() }
}

/// What a raise or refresh writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    pub key: String,
    pub title: String,
    pub body: String,
}

/// What a clear writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clear {
    pub key: String,
    pub reason: String,
}

/// Each condition's deadline: `SPIRA_HEIMDALL_BD_TIMEOUT`'s 10 s.
pub const WRITE_DEADLINE_MS: u64 = 10_000;
/// The host writes' deadline.
pub const HOST_DEADLINE_MS: u64 = 2_000;

fn act(name: &str, key: String, input: &impl Serialize, deadline_ms: u64, probe: bool) -> Result<Act, String> {
    Ok(Act { name: name.into(), key, input: serde_json::to_string(input).map_err(|e| e.to_string())?, deadline_ms, probe })
}

/// `hosted::classify` then `hosted::step` (`reconcile::step_keys` with its counters saturated).
/// A raise is a journaled effect; a refresh rewrites an alert already raised and goes as a
/// memory-only probe, its time carried. With the store unreachable nothing is written to it:
/// a pass that would raise is refused naming `unreachable`, and any other writes the fallback
/// channel. A raise that fails is retried by the next refresh. The per-pass heartbeat file is
/// not written by the pass: its reading is `scheduler status` (S13); [`HEARTBEAT`] stays
/// runnable for the pane until it reads S13.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let snap = match input {
        Input::Observed(s) => s,
        Input::Fire | Input::Outcome { .. } => return Ok((carry.clone(), vec![])),
        Input::Ran(r) => return Err(format!("{NAME}: no unit is expected, got {r:?}")),
    };
    let (confirm, clear_n, refresh) = (num(cfg, "SPIRA_HEIMDALL_CONFIRM")?, num(cfg, "SPIRA_HEIMDALL_CLEAR")?, num(cfg, "SPIRA_HEIMDALL_REFRESH")?);
    let th = heimdall::hosted::Thresholds { pass_stale: num(cfg, "SPIRA_HEIMDALL_PASS_STALE")?, mirror_stale: optional(cfg, "SPIRA_HEIMDALL_MIRROR_STALE").unwrap_or(i64::MAX), restart_threshold: optional(cfg, "SPIRA_HEIMDALL_RESTARTS").unwrap_or(i64::MAX), restart_window: optional(cfg, "SPIRA_HEIMDALL_RESTART_WINDOW").unwrap_or(3600) };
    let now = (now_ms / 1000) as i64;
    let firing = heimdall::hosted::classify(&readings(cfg, snap, now), &th);
    let prior: BTreeMap<String, heimdall::state::KeyState> = carry.keys.iter().map(|(k, v)| (k.clone(), to_heimdall(v))).collect();
    if !snap.db_reachable {
        let would = heimdall::hosted::raises(&heimdall::hosted::step(now, confirm, clear_n, refresh, true, &firing, &prior));
        if !would.is_empty() {
            return Err(format!("{NAME}: the store is unreachable; the raise of {} is refused and waits for it", would.join(", ")));
        }
        let doc = heimdall::fallback::build(now, false, None, &firing);
        return Ok((carry.clone(), vec![act(FALLBACK, format!("heimdall/fallback/{now}"), &doc, HOST_DEADLINE_MS, true)?]));
    }
    let by_key: BTreeMap<&str, &heimdall::classify::Firing> = firing.iter().map(|f| (f.key.as_str(), f)).collect();
    let when = |s: i64| crate::watchers::iso8601(s.max(0) as u64);
    let mut next = Carry { keys: BTreeMap::new(), ..carry.clone() };
    let mut acts = vec![];
    for st in heimdall::hosted::step(now, confirm, clear_n, refresh, true, &firing, &prior) {
        let was = carry.keys.get(&st.key).map(|k| k.phase).unwrap_or_default();
        let is_firing = by_key.contains_key(st.key.as_str());
        let write = |k: &heimdall::state::KeyState| -> Write {
            let f = by_key.get(st.key.as_str());
            Write { key: st.key.clone(), title: f.map(|f| f.title.clone()).unwrap_or_default(), body: heimdall::hosted::body_at(&st.key, &when(k.first), k.flaps, f.map_or("", |f| f.evidence.as_str())) }
        };
        let ks = match st.trigger {
            heimdall::reconcile::Trigger::ConfirmRaise => {
                let k = st.on_success(now);
                acts.push(act(RAISE, format!("heimdall/{}/raise/{}", st.key, k.flaps), &write(&k), WRITE_DEADLINE_MS, false)?);
                from_heimdall(&k, Phase::Firing)
            }
            heimdall::reconcile::Trigger::ConfirmClear => {
                let k = st.on_success(now);
                acts.push(act(CLEAR, format!("heimdall/{}/clear/{}", st.key, k.flaps), &Clear { key: st.key.clone(), reason: heimdall::alerts::clear_reason_with(&when(k.first), k.flaps, clear_n) }, WRITE_DEADLINE_MS, false)?);
                from_heimdall(&k, Phase::Cleared)
            }
            heimdall::reconcile::Trigger::Refresh if !snap.alert_rows.is_empty() && !snap.alert_rows.contains(&st.key) => from_heimdall(&st.on_refresh_skip_closed(now), Phase::Firing),
            heimdall::reconcile::Trigger::Refresh => {
                let k = st.on_success(now);
                acts.push(act(REFRESH, format!("heimdall/{}/refresh/{}", st.key, k.refreshed), &write(&k), WRITE_DEADLINE_MS, true)?);
                from_heimdall(&k, Phase::Firing)
            }
            heimdall::reconcile::Trigger::None => {
                let phase = match (st.counters.state == "firing", is_firing) {
                    (true, true) => Phase::Firing,
                    (true, false) => Phase::Unseen,
                    (false, true) => Phase::Seen,
                    (false, false) => Phase::Quiet,
                };
                from_heimdall(&st.counters, phase)
            }
        };
        if ks.phase == Phase::Quiet && ks.flaps == 0 && was != Phase::Cleared {
            continue;
        }
        next.keys.insert(st.key.clone(), ks);
    }
    Ok((next, acts))
}

/// The observe probe: the sentinel and export tasks from `scheduler status` on `control`, the
/// store's heimdall conditions (and the write probe, which stays part of observe), the run
/// dir's halt and drain stamps and its strands file. A store that answers removes a fallback
/// file left by an outage. Restart counts wait on `NRestarts` in `UnitFacts` and are empty.
pub fn observe(lc: &spira_config::lc_call::LcClient, run_dir: &std::path::Path, control: &str, now_ms: u64) -> Result<String, String> {
    use heimdall::conditions::{Conditions, Failure, LcConditions};
    let status: Option<serde_json::Value> = runtime::socket::lc_call(std::path::Path::new(control), &["scheduler", "status"]).ok().and_then(|t| serde_json::from_str(&t).ok());
    let task = |name: &str| status.as_ref().and_then(|s| s["tasks"].as_array()?.iter().find(|t| t["name"] == name).cloned());
    let sentinel = match task(SENTINEL_TASK) {
        Some(t) => TaskStatus { armed: !t["next_due_at"].is_null(), last_fired_ms: t["last_fired_at"].as_u64(), effect_failures: u64::from(t["last_outcome"] == "failed") },
        // Not hosted, or no status to read: cannot tell, so nothing is judged stale.
        None => TaskStatus { armed: true, last_fired_ms: None, effect_failures: 0 },
    };
    let mirror_age_secs = task(EXPORT_TASK).filter(|t| t["last_outcome"] == "succeeded").and_then(|t| t["last_fired_at"].as_u64()).map(|at| now_ms.saturating_sub(at) / 1000);
    let ops = LcConditions { client: lc.clone() };
    let (db_reachable, alert_rows) = match ops.list() {
        Ok(rows) => (true, rows.into_iter().filter(|c| c.firing()).map(|c| c.key).collect()),
        Err(Failure::Saturated) => (true, vec![]),
        Err(Failure::Other) => (false, vec![]),
    };
    if db_reachable {
        heimdall::alerts::maintain_probe(&ops);
        heimdall::fallback::remove(&run_dir.join("heimdall.alerts.json"));
    }
    let strands = std::fs::read_to_string(run_dir.join("strands.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let strands = strands.iter().filter_map(|(k, v)| k.strip_prefix("ghost:").map(|id| format!("{id}\t{}", v["first"].as_i64().or_else(|| v["first"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0)))).collect();
    let snap = Snapshot { sentinel, restart_alerts: vec![], db_reachable, alert_rows, mirror_age_secs, world_halted: run_dir.join("world.halted").exists(), world_draining: run_dir.join("world.draining").exists(), strands };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

fn ops(lc: &spira_config::lc_call::LcClient) -> heimdall::conditions::LcConditions {
    heimdall::conditions::LcConditions { client: lc.clone() }
}

/// The runner of [`RAISE`] and [`REFRESH`]: `condition set <key> --status True`.
pub fn raise(lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    let w: Write = serde_json::from_str(input).map_err(|e| e.to_string())?;
    heimdall::alerts::alert_write(&ops(lc), &w.key, &w.title, &w.body).map_err(|f| format!("{}: {f:?}", w.key))
}

/// The runner of [`CLEAR`]: `condition clear <key>`; nothing firing is success.
pub fn clear(lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    use heimdall::conditions::Conditions;
    let c: Clear = serde_json::from_str(input).map_err(|e| e.to_string())?;
    ops(lc).clear(&c.key, "Cleared", &c.reason).map(|()| c.key.clone()).map_err(|f| format!("{}: {f:?}", c.key))
}

/// The runner of [`FALLBACK`]: the fallback document, written whole.
pub fn fallback(run_dir: &std::path::Path, input: &str) -> Result<String, String> {
    let doc: serde_json::Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
    heimdall::fallback::write(&run_dir.join("heimdall.alerts.json"), &doc).map(|()| "written".into()).map_err(|e| e.to_string())
}

/// The runner of [`HEARTBEAT`]: `heimdall.status`, until the pane reads `scheduler status`.
pub fn heartbeat(run_dir: &std::path::Path, input: &str) -> Result<String, String> {
    heimdall::heartbeat::write(&run_dir.join("heimdall.status"), input).map(|()| "written".into()).map_err(|e| e.to_string())
}

const ROW_PARTS: &[runtime::rows::Part] = &[];

super::hosted!(ReconcileHeimdall);
