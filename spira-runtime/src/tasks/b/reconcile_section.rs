//! # reconcile.section — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-section.md`;
//! its Acceptance section is the test file `tests/it/task_b_reconcile_section.rs`.
//!
//! ## Tick
//!
//! - `Timer`, every 60 s (`OnUnitActiveSec=60s`, `OnBootSec=2min`, `AccuracySec=5s`, unit file).
//!   Boot tick at +120 s.
//! - Nothing else wakes it. Single-flight: one request in flight per tick. A tick that finds the
//!   previous effect still running re-arms and does nothing.
//! - The store's own floor `MIN_GAP_SECS = 50` (`spira-lc/src/control/sections/lifecycle.rs:28`)
//!   keeps a jittered timer from publishing twice. The task keeps the 60 s cadence and does not
//!   add a gate of its own.
//!
//! ## Observe
//!
//! There is no observe probe, because the decision is the store's. The verb reads, server-side:
//!
//! - the section's own record, `read_section(contract, section_key(lifecycle, …))` (`lifecycle.rs:197-201`);
//! - `store.records()` and `store.history(id, "bead")` for LANDED/CERTIFIED dwell (`lifecycle.rs:217-248`);
//! - the run dir's `world.halted` mtime as the only stop (`lifecycle.rs:337-344`);
//! - tsd families `cert-pass` (`tsd::pass::FAMILY`) and `round-attribution` (`lifecycle.rs:350-354`).
//!
//! Re-deriving these in the task would break law-a-watcher-asks-the-component-that-decides. The
//! task does not read them.
//!
//! ## Decide
//!
//! `decide(carry, now) -> Option<Publish>`, which always returns `Some`. The pure function that
//! matters is the store's: `below_floor`, `distance`, `certified_stays` and `round_events`
//! (`lifecycle.rs:144-178,107-121,360-404`) are already pure and covered by
//! `control/sections/lifecycle/tests.rs`. Carry: none.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.section.publish` → store verb `publish-section lifecycle` | store | 5 s (the socket client's `TIMEOUT`, `spira-lc/src/client.rs:18`) | `section/lifecycle@<tick-minute>`. The store makes the call idempotent by itself: `--expect-rev` on the current revision (`lifecycle.rs:308-310`), the 50 s gap, and the unchanged-within-5 m skip (`lifecycle.rs:304-307`) | a name other than `lifecycle` (`lifecycle.rs:47`); a store path that is unset or unreadable (`served.rs:268-279`); `cannot tell` on an unreadable run dir. An expect-rev conflict is a refusal, and the next tick retries it |
//!
//! Metrics (`spira_goodput_*`, `lifecycle.rs:181-193`) are pushed by lc-serve, not by the task.
//!
//! **Decision for Ryan.** The verb is a self-gated write, and 59 of every 60 answers are
//! `published:false`. Journaling each one as `effect.requested`/`succeeded` puts about 120
//! events an hour on this task alone. The two options:
//!
//! - **(a, default)** Run it as a `Probe` whose answer is discarded unless it reads
//!   `published:true`, in which case one `reconcile.section.published` Emit is journaled. Under
//!   S4 Observe mode the probe must then be suppressed too: it is a write, so "Probes run" does
//!   not hold for it.
//! - **(b)** Add a read-only `publish-section lifecycle --dry-run` to the store so the task can
//!   decide, then act only on a change.
//!
//! ## Silence
//!
//! On an idle tick the store answers `{"published":false,"reason":…}`. Nothing is journaled,
//! nothing is emitted, and no metric is recorded: the store skips its own `record`
//! (`lifecycle.rs:49`). Under (a), one in-memory Probe and its outcome, neither journaled.
//!
//! ## Replaces
//!
//! - The `spira-lifecycle-section-prod.{service,timer}` units, and
//!   `StandardOutput=append:$SPIRA_RUN/lifecycle-section.log`. No watchd row reads that log.
//! - Children: one `spira-lc` exec per minute. lc-serve's own reads are unchanged.
//! - Config keys pinned, all server-side and unchanged: `SPIRA_LC_STORE_PATH`, `SPIRA_RUN`,
//!   `SPIRA_METRICS_ENDPOINT` (`served.rs:259,279,282`). The task's only own setting is
//!   `tasks.reconcile_section.interval_ms = 60000`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. The task can also be set to `Observe`, which
//! under (a) means the probe is suppressed. Either way the section goes stale after its
//! `max_age` of 15 min (`lifecycle.rs:26`), and the reconciler's section reading shows that.
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.section";
pub const STREAM: &str = "task.reconcile.section";
pub const TICK: EventKindTag = "reconcile.section.tick";
pub const STATE: EventKindTag = "reconcile.section.state";
/// The one journaled fact, on a `published:true` answer (option (a)).
pub const PUBLISHED: EventKindTag = "reconcile.section.published";

/// The store verb `publish-section lifecycle`, run as the tick's probe (option (a)).
pub const PUBLISH: &str = "reconcile.section.publish";
pub const EFFECTS: &[&str] = &[PUBLISH];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_SECTION_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: PUBLISH, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// The store's answer to `publish-section lifecycle`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub published: bool,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Published,
}

/// `state × answer → state`: the only machine is the store's; the task records a publish.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "published:true", Phase::Published), (Phase::Published, "published:true", Phase::Published), (Phase::Published, "published:false", Phase::Published)];

/// None of its own beyond the last publish it recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub published_at: Option<u64>,
}

pub struct ReconcileSection {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// Always asks for one publish per tick (the tick's probe); records the first `published:true`
/// and nothing after it, so a steady store journals nothing. The publish time is the store's
/// (the section record carries it); the carry keeps no clock, so a replay records the same state.
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(s) if s.published && carry.phase == Phase::Idle => Ok((Carry { phase: Phase::Published, published_at: None }, vec![])),
        Input::Observed(_) => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} no outcome of {effect} is expected; the publish is the tick's probe", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {r:?}", phase_name(carry.phase))),
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Published => "published",
    }
}

/// The runner of [`PUBLISH`]: the store verb `publish-section lifecycle` over the socket; its
/// answer is the snapshot.
pub fn publish(lc: &spira_config::lc_call::LcClient) -> Result<String, String> {
    lc.answer(&["publish-section", "lifecycle"]).map_err(String::from)
}

super::hosted!(ReconcileSection);
