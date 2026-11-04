//! # watch.mend — contract
//!
//! Replaces `spira-mend-watch-prod`: `spira-lc mend-watch --seen run/mend-watch.seen`, then `ExecStartPost=spira-lc pause-sweep`, both appending stdout to `run/mend-watch.log`. Pattern P1. Part of [[scheduler-consolidation-2026-11-03]].
//!
//! Catalogue differs: the row lists one job with no children. The unit actually runs two jobs: the mender-bounds alarm, and then `pause-sweep` (`spira-lc/src/main.rs:362-363`), which lifts scoped stops. Both are `spira-lc` processes, and both write the same log. This contract carries both, as two effects.
//!
//! ## Tick
//!
//! - `Timer` every 120 s, first at boot + 120 s (the timer has `OnBootSec=2min` and `OnUnitActiveSec=2min`).
//! - Single-flight: a tick while the observe is in flight re-arms only.
//! - No other event wakes it. A later option is a wake on `mending` lifecycle events.
//!
//! ## Observe
//!
//! `watch.mend.observe` returns a `Snapshot { now, overdue: [(batch, pass, suite, deadline)], mends: [(bead, n)], stops: [(repo, bead, state, since, reached: Ok(bool)|Err(why))] }`. It is built from these sources:
//!
//! - **Overdue mends.** The store's `mending` rows that are `MENDING`, have `awaiting_ask IS NULL` and have `deadline <= now`. Today this is SQL over the store connection (`spira-lc/src/mend_watch.rs:65`). The target is the store's `mending list` verb (it is in the usage at `spira-lc/src/main.rs:438`) through `LcClient` (S7a).
//! - **Mends in the last 24 h, per bead.** `mend` rows with `at >= now-86400`, grouped by `bead_id` (`mend_watch.rs:66`). No store verb returns this count yet, so S7a has to add one read method (unverified whether `mending list` can carry it).
//! - **Read retries.** Each read is tried 4 times, 3 s apart (`mend_watch.rs:46-61`). Under the scheduler, the effect deadline and the retry policy on the observe replace that loop.
//! - **Scoped stops.** The `repo-*.json` records under `<SPIRA_RUN>/scoped-stops/` (`spira-config/src/scoped_stop.rs:32`, `:58-69`; the run dir comes from `spira-lc/src/pause_sweep.rs:10`). For each stop whose condition is `bead:ID=STATE`, the observe checks whether the bead's applied lifecycle events show it entering STATE since the stop was set (`pause_sweep.rs:14-21`). Today that is the `event` table. The target is the store's `history <key> --machine bead` verb.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)` is pure. It reuses this existing core:
//!
//! - `overdue_breach`, `mends_breach` and `fresh` (`spira-lc/src/mend_watch.rs:20-36`; limit `MAX_MENDS_PER_DAY = 2`, `:11`)
//! - `bead_condition` and `entered_state` (`spira-config/src/scoped_stop.rs:42-54`)
//!
//! **Carry** is `watch.mend.state` = `{ seeded, seen: BTreeSet<breach key> }`. Today it is the `--seen` file. That file is rewritten every pass to hold the current breach keys (`mend_watch.rs:84-89`), so a breach that clears and later returns is announced again. The carry keeps the same semantics: `seen := keys of this snapshot`.
//!
//! **Fixtures:**
//! - the four `mend_watch.rs` tests (`:97-124`): retry, over-two-mends, no repeat until cleared, overdue says how late
//! - the `scoped_stop` tests in `spira-config` (unverified count)
//! - `spira/test-mend-watch-units.sh`, which pins the unit and becomes T1
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.mend.observe` (probe) | store | 15 s (12 s of retries today, under TimeoutStartSec=60) | none (probe) | — |
//! | `watch.mend.log_append` | host | 5 s | `watch.mend:<breach key>:<tick at>` | an empty line; a key already in `carry.seen` |
//! | `watch.mend.lift_stop` | host (rewrites `run/scoped-stops/repo-<repo>.json` via `scoped_stop::unpause_repo`) | 5 s | `watch.mend:lift:<repo>:<stop since>` | a free-text or unparsable condition (`scoped_stop.rs:42-45`); a stop whose condition read failed. That stop is kept and logged, which is the same result as `CANNOT_TELL` at `pause_sweep.rs:25` |
//!
//! **Lines written to `run/mend-watch.log`**, which is the `mend-watch|log` read contract at `spira/watchers:166`. The task appends each one exactly as today, with no timestamp prefix:
//!
//! - `MEND-WATCH OVERDUE {batch}#{pass}#{suite} is MENDING {now-deadline}s past its deadline; the sweep did not return it` (`mend_watch.rs:22`)
//! - `MEND-WATCH MENDS {bead} was mended {n} times in 24h (limit 2)` (`mend_watch.rs:29`)
//! - `pause-sweep: lifted the stop on {repo}: its bead condition was met` and `pause-sweep: kept {repo}: {why}` (`pause_sweep.rs:23-24`). These lines reach the same log today because ExecStartPost shares the unit's StandardOutput.
//!
//! No `inbox.append` exists today. Escalation is `inbox.notify`'s job (see [[inbox-notify]]): it reads this log through its cursor.
//!
//! ## Silence
//!
//! An idle tick emits the observe probe and nothing else:
//!
//! - no breach and no stop met → no Emit, no effect
//! - breaches that are all already in `seen`, with an unchanged set → no Emit
//!
//! ## Replaces
//!
//! - **Units:** `spira-mend-watch-prod.service` and `spira-mend-watch-prod.timer`.
//! - **The `mend-watch|log` row stays.** It is a read contract (`spira/watchers:166`); its writer becomes this task.
//! - **Children forked:** `spira-lc mend-watch` and `spira-lc pause-sweep`, as process starts by systemd. Neither forks further.
//! - **Config pinned (S10):** `SPIRA_RUN`, for the scoped-stops directory (`spira-lc/src/lib.rs`'s `lifecycle_run_dir`, unverified key path). The log path and the `--seen` path become task settings.
//!
//! ## Reverse
//!
//! - Release rollback restores both the unit and the timer. The `.seen` file is absent after the move, so the first restored pass re-announces any standing breach once.
//! - Observe mode (S4) shadows `log_append` and `lift_stop`, and keeps the probe running.

use std::collections::BTreeSet;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;

pub const NAME: &str = "watch.mend";
pub const STREAM: &str = "watch.mend";
pub const TICK: EventKindTag = "watch.mend.tick";
pub const STATE: EventKindTag = "watch.mend.state";
pub const OBSERVE: &str = "watch.mend.observe";
pub const LOG_APPEND: &str = "watch.mend.log_append";
pub const LIFT_STOP: &str = "watch.mend.lift_stop";
pub const EFFECTS: &[&str] = &[OBSERVE, LOG_APPEND, LIFT_STOP];
pub const KEYS: &[&str] = &["SPIRA_RUN"];
/// `OnUnitActiveSec=2min`.
pub const CADENCE_MS: u64 = 120_000;
const OBSERVE_DEADLINE_MS: u64 = 15_000;
/// `MAX_MENDS_PER_DAY` (`spira-lc/src/mend_watch.rs:11`).
pub const MAX_MENDS_PER_DAY: u64 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { run: cfg("SPIRA_RUN")? })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overdue {
    pub batch: String,
    pub pass: u64,
    pub suite: String,
    pub deadline: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub repo: String,
    pub bead: String,
    pub state: String,
    pub since: u64,
    /// Whether the bead entered `state` since the stop was set, or why that could not be read.
    pub reached: Result<bool, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: u64,
    pub overdue: Vec<Overdue>,
    /// (bead, mends in the last 24 h).
    pub mends: Vec<(String, u64)>,
    pub stops: Vec<Stop>,
}

/// `seen := keys of this snapshot`, so a breach that clears and returns is announced again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seeded: bool,
    pub seen: BTreeSet<String>,
}

/// `overdue_breach`, `mends_breach`, `fresh`, then `bead_condition` / `entered_state` per stop.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.mend: not ported yet")
}

pub struct WatchMend {
    pub config: Result<Config, String>,
}

impl WatchMend {
    pub fn new(config: Config) -> Self {
        WatchMend { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchMend { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.mend/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchMend {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
