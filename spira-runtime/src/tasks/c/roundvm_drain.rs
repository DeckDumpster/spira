//! # roundvm.drain — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/roundvm-drain.md`;
//! its Acceptance section is the test file `tests/it/task_c_roundvm_drain.rs`.
//!
//! ## Tick
//!
//! - **Today:** `OnCalendar=*-*-* *:0/5:00`, `AccuracySec=30s`, `Persistent=true` (unit file) — a
//!   calendar, not the 5 m interval the catalogue lists. **Code over catalogue, noted.**
//! - **Contract:** interval 5 min from the previous tick (P1). Alignment to wall `:00/:05` is not
//!   load-bearing: the decision compares the VM's ready age to a bound, so firing at :03 instead
//!   of :05 changes nothing. Taking the interval also avoids the S8 parser gap: `sim/src/units.rs:65-75`
//!   does not parse the `0/5` step syntax.
//! - **Catch-up:** none needed; an overdue tick is the next tick.
//! - **Wakes (proposed):** `round.finished` / `roundvm.released` would let the drain run as soon as a
//!   VM falls idle, but the bound is 30 min, so the tick alone suffices. Not added.
//! - **Single-flight:** a tick with a `release` in flight re-arms and does nothing.
//! - **Guard:** `ExecStartPre=world guard` today. The probe reports `world.halted` (stamp present,
//!   `spira-world/src/guard.rs:11-20`); `decide` acts on nothing while halted.
//!
//! ## Observe
//!
//! `roundvm.drain.observe` (probe, deadline 10 s) returns:
//!
//! ```text
//! Snapshot { world_halted: bool, ready: Option<{handle, ready_since}>, leased: Vec<handle>,
//!            refreshing_alive: bool, spill_mode: String, idle_bound_secs: u64 }
//! ```
//!
//! - The pool's state is owned by `round_vm::pool::Pool` under its file lock
//!   (`round-vm/src/pool.rs:116-129`, `$SPIRA_ROUND_VM_STATE_DIR/lock`). The probe asks the pool
//!   via `Pool::status_json` (`round-vm/src/pool.rs:1027`) — the same read `round-vm status --json`
//!   serves — not by parsing the state file.
//! - `spill_mode` = `SPIRA_ROUND_VM_SPILL` (`round-vm/src/cli.rs:98-99`); `idle_bound_secs` =
//!   `SPIRA_ROUND_VM_WARM_IDLE_SECS`, default 1800 (`cli.rs:110-113`). Pinned (S10).
//!
//! ## Decide
//!
//! `decide(snap) -> Option<(handle, why)>`, extracted from the closure in `Pool::drain_idle`
//! (`round-vm/src/pool.rs:873-905`): nothing if halted, no ready VM, the ready VM is leased, or a
//! refresh is alive (`:880-885`); release under `ec2-always|ec2-only` (`:886-891`, `cli.rs:98-99`);
//! otherwise release once `age >= idle` (`:892-903`). No carried state.
//! **Fixtures:** `pool.rs:1965` (`draining_never_touches_a_vm_a_live_round_holds`) and the other
//! `drain_idle` tests, ported to the snapshot form.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `roundvm.drain.release` | host | 600 s (unit `TimeoutStartSec`) | `roundvm.drain/<handle>` | a VM any lease holds (`pool.rs:805-807`, re-checked under the lock — the snapshot may be stale); a handle not ours (`DestroyError::NotOurs` is settled, not retried, `pool.rs:831-836`) |
//!
//! The runner is `Pool::release_inner(handle, me, true, factory)` (`pool.rs:796-836`): mark
//! `Released`/`Doomed` under the lock, then `destroy_verified`. The provider is PVE over
//! in-process HTTP (`round-vm/src/cli.rs:33-43`, `HttpTransport`) or EC2 through the `aws` CLI
//! child (`round-vm/src/ec2.rs:600-603`). **Catalogue says "(aws/pve)" children:** PVE is not a
//! child; `aws` is, and stays one per the plan.
//!
//! ## Silence
//!
//! The common tick: no ready VM, or one younger than 30 min → `decide` returns `None` → nothing
//! journaled beyond the probe. Today an idle run prints `warm: 0 released` to the journal
//! (`cli.rs:474-477`); that line goes.
//!
//! ## Replaces
//!
//! - Unit `spira-round-vm-drain-prod.service` / `.timer`; output to the journal.
//! - Children: `world guard` (ExecStartPre), `aws` (EC2 only; stays).
//! - Config keys pinned: `SPIRA_ROUND_VM_STATE_DIR` (`round-vm/src/config.rs:232`),
//!   `SPIRA_ROUND_VM_WARM_IDLE_SECS`, `SPIRA_ROUND_VM_SPILL`, the PVE env file (`config.rs`,
//!   `PveEnv::load`, `:356`). The unit's `SPIRA_HOME` is pinned to release `0ef4f0f93…/spira`, not
//!   `current` — the task reads `Settings`, so that drift disappears.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode shadows `release`; a VM then idles until the
//! next real drain (cost: one warm VM's hours). The pool lock means old unit and task can coexist
//! safely during a rollback window.
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "roundvm.drain";
pub const STREAM: &str = "task.roundvm.drain";
pub const TICK: EventKindTag = "roundvm.drain.tick";
pub const STATE: EventKindTag = "roundvm.drain.state";

pub const OBSERVE: &str = "roundvm.drain.observe";
pub const RELEASE: &str = "roundvm.drain.release";
pub const EFFECTS: &[&str] = &[OBSERVE, RELEASE];

/// An interval of 5 min, not today's `*:0/5` calendar (Tick).
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ROUNDVM_DRAIN_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_ROUND_VM_STATE_DIR", "SPIRA_ROUND_VM_WARM_IDLE_SECS", "SPIRA_ROUND_VM_SPILL"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ready {
    pub handle: String,
    /// Epoch ms.
    pub ready_since: u64,
}

/// `Pool::status_json`, the pool's own read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub world_halted: bool,
    pub ready: Option<Ready>,
    pub leased: Vec<String>,
    pub refreshing_alive: bool,
    pub spill_mode: String,
    pub idle_bound_secs: u64,
}

/// Stateless P1; the only machine is the pool's `Ready → Released` (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Releasing,
}

/// `state × event → state`. A release of a leased VM is refused naming the lease.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "ready:aged", Phase::Releasing), (Phase::Releasing, "release:outcome", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
}

pub struct RoundvmDrain {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Releasing => "releasing",
    }
}

/// The closure in `Pool::drain_idle` (`round_vm::pool::drain_choice`): at most one release.
///
/// An observation while a release is still `Releasing` returns the task to `idle` and asks for
/// nothing: the next tick decides afresh from the pool, whose lock re-checks every release, so
/// a release refused (a lease took the VM) never wedges the task.
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(_) if carry.phase == Phase::Releasing => Ok((Carry::default(), vec![])),
        Input::Observed(s) if s.world_halted => Ok((carry.clone(), vec![])),
        Input::Observed(s) => {
            let view = round_vm::pool::DrainView { ready: s.ready.as_ref().map(|r| (r.handle.clone(), r.ready_since / 1000)), leased: s.leased.clone(), refreshing_alive: s.refreshing_alive };
            match round_vm::pool::drain_choice(&view, round_vm::pool::spill_only_ec2(&s.spill_mode), s.idle_bound_secs, now_ms / 1000) {
                Some((handle, _why)) => Ok((Carry { phase: Phase::Releasing }, vec![Act { name: RELEASE.into(), key: format!("roundvm.drain/{handle}"), input: handle, deadline_ms: 600_000, probe: false }])),
                None => Ok((carry.clone(), vec![])),
            }
        }
        Input::Outcome { effect, ok: true, .. } if effect == RELEASE && carry.phase == Phase::Releasing => Ok((Carry::default(), vec![])),
        Input::Outcome { effect, ok: false, body } if effect == RELEASE && carry.phase == Phase::Releasing => Err(format!("{NAME}: in state releasing the release was refused: {body}")),
        // A release that outlived its tick answers after the task went back to idle.
        Input::Outcome { effect, .. } if effect == RELEASE => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} an outcome of {effect} is not expected", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// The runner of [`OBSERVE`]: the pool's own read under its lock, the world's halt stamp, and
/// the pinned spill mode and idle bound.
pub fn observe(run: &std::path::Path, spill_mode: &str, idle_secs: &str) -> Result<String, String> {
    let idle_bound_secs: u64 = idle_secs.trim().parse().map_err(|_| format!("SPIRA_ROUND_VM_WARM_IDLE_SECS must be whole seconds, got {idle_secs:?}"))?;
    let pool = round_vm::cli::pool_for(&round_vm::config::Config::load()?);
    let v = pool.drain_view()?;
    let snap = Snapshot {
        world_halted: run.join("world.halted").exists(),
        ready: v.ready.map(|(handle, since)| Ready { handle, ready_since: since * 1000 }),
        leased: v.leased,
        refreshing_alive: v.refreshing_alive,
        spill_mode: spill_mode.trim().to_string(),
        idle_bound_secs,
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The runner of [`RELEASE`]: `Pool::release_idle`, which re-checks the leases under the lock.
pub fn release(handle: &str) -> Result<String, String> {
    if handle.trim().is_empty() {
        return Err("no handle to release — refusing".into());
    }
    let pool = round_vm::cli::pool_for(&round_vm::config::Config::load()?);
    pool.release_idle(handle.trim(), &round_vm::cli::real_attempt)?;
    Ok(format!("Released {handle}"))
}

crate::tasks::b::hosted!(RoundvmDrain);
