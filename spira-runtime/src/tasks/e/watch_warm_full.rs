//! # watch.warm_full — contract
//!
//! watchd row `warm-full` (`spira/watchers:173`), target `@SPIRA_HOME@/warm-full.sh watch --interval 120`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the script is not only a watcher — every pass first **empties idle warm slots** (`spira/warm-full.sh:205-220`, `find "$slot/" -mindepth 1 -delete`), so the task has a destructive act, not just an inbox line. Its children are `df`, `du`, `flock`, `find` as well as `lib.sh`.
//!
//! ## Tick
//!
//! - Cadence 120 s (`--interval 120`, `spira/watchers:173`; loop at `spira/warm-full.sh:259-276`). No waking events. Single-flight: one `watch.warm_full.observe` outstanding; a tick while one is in flight is dropped (the next `Timer` re-arms).
//!
//! ## Observe
//!
//! `watch.warm_full.observe` (host class) returns:
//!
//! ```text
//! Snapshot { base: Option<String>, pct: Option<u8>, slots: Vec<Slot> }
//! Slot { name, mib, held_by: Option<String> }   // held = flock -n on slot.N.lock fails
//! ```
//!
//! - `base` from `SPIRA_DWARF_WARM_BASE` (`warm-full.sh:235`); `None` or not a dir → tick is idle (`:236`).
//! - `pct` from `statvfs(base)` — today `df --output=pcent` (`:237`, `:240`).
//! - `mib` per slot from a directory walk — today `du -sm` (`:213`, `:244`).
//! - `held_by`: the slot lock's flock state and the holder name written into the lock (`:222-232`). The lock is the dwarf session's own claim, so this asks the component that decides (the session), not a derived guess.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)`, pure.
//!
//! - Carry `watch.warm_full.state` = `{ said: bool }` — today the `run/watchd/warm-full.said` latch (`:196`, set `:246`, cleared `:242`).
//! - Reap set: every slot not held whose `mib > 0` and (`pct >= 85` or `cap > 0 && mib > cap`), `cap = SPIRA_DWARF_WARM_CAP_MIB` (`:206`, `:214`). Emits one `watch.warm_full.reap` per slot.
//! - Because the script re-measures `pct` after reaping (`:240`), the crossing is judged on the **next** observe after a reap, not the same one; decide over a stale pct would announce a full tmpfs it just emptied. Rule: if any reap was emitted this tick, defer the crossing decision to the next tick.
//! - `pct < 85` → `said=false`, no line. `pct >= 85 && !said` → one inbox line, `said=true`. `pct >= 85 && said` → nothing.
//! - Fixtures: none exist in the release for warm-full (unverified: no `test-warm-full*.sh`); new fixtures: under-limit, crossing, standing, clear-and-re-cross, held slot never reaped, over-cap idle slot reaped below the limit.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.warm_full.observe` | host | 10 s | (probe, not journaled) | `base` unset/absent → empty snapshot, not an error |
//! | `watch.warm_full.reap` | host | 30 s | `warm_full/reap/<slot>/<observe event id>` | a slot whose lock it cannot take with `flock -n` at run time (a session took it after observe) — reports `held`, deletes nothing; a path outside `base` |
//! | `inbox.append` (shared) | host | 10 s | `warm_full/full/<crossing event id>` | — (runner: `spira-runtime/src/lib.rs:233` → `world.rs:731`) |
//!
//! Inbox line, exactly as today (`spira/warm-full.sh:201`, text `:245`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:warm-full] dwarf warm tmpfs <base> is <pct>% full (limit 85%): <slot>=<n>MiB …; the slots a session holds cannot be emptied (held by: slot.N=<who> …)
//! ```
//!
//! Today the script writes this line twice: to stdout → `run/watchd/warm-full.log` (`:199`, via the unit's `StandardOutput=append:`), and straight into `SPIRA_CONCIERGE_INBOX` (`:200-201`). The reap line `<iso> idle slot.N emptied (<n>MiB)` (`:216`) goes **only** to the log. Readers of `run/watchd/warm-full.log`: none by name in the release or `run/concierge-notes`; only watchd's generic row walkers (`watchd notify`, `drain`, `peek`, `tail`). No `warm-full.cursor` exists in `run/watchd/`, so nobody has ever drained it. The task drops the log write; the reap becomes a journaled `effect.succeeded` instead.
//!
//! Health (4th field `warm-full.sh health`, `:249-257`: `.health` file younger than 3× interval) is replaced by S13 `scheduler status`: last fired / lag for `watch.warm_full`.
//!
//! ## Silence
//!
//! An idle tick (base unset, or `pct < 85` with `said=false` and nothing to reap) emits the observe probe only: no `Emit`, no effect.
//!
//! ## Replaces
//!
//! - `spira-watch-warm-full-prod.service` and the `warm-full` row (`spira/watchers:172-173`).
//! - Children: `bash lib.sh` (`warm-full.sh:193`), `df` (`:237`, `:240`), `du` (`:213`, `:244`), `flock` (`:212`, `:226`), `find -delete` (`:215`), `tr`, `awk`.
//! - Files retired: `run/watchd/warm-full.{said,health,log,restarts}`.
//! - Config pinned (S10): `SPIRA_DWARF_WARM_BASE`, `SPIRA_DWARF_WARM_CAP_MIB` (default 6144, `spira/conf.d.index:1193`); the 85 % limit is a literal today (`:235`) and stays one.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and row. Per task: `Observe` mode (S4) shadows `reap` and `inbox.append` as `effect.shadowed` while the probe runs.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "watch.warm_full";
pub const STREAM: &str = "watch.warm_full";
pub const TICK: EventKindTag = "watch.warm_full.tick";
pub const STATE: EventKindTag = "watch.warm_full.state";
pub const OBSERVE: &str = "watch.warm_full.observe";
pub const REAP: &str = "watch.warm_full.reap";
pub const EFFECTS: &[&str] = &[OBSERVE, REAP, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_DWARF_WARM_BASE", "SPIRA_DWARF_WARM_CAP_MIB", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag (`spira/warm-full.sh:201`).
pub const INBOX_TAG: &str = "warm-full";
/// The fill limit, a literal today (`warm-full.sh:235`) and still one.
pub const LIMIT_PCT: u8 = 85;
/// `--interval 120`.
pub const CADENCE_MS: u64 = 120_000;
const OBSERVE_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub base: String,
    pub cap_mib: u64,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { base: cfg("SPIRA_DWARF_WARM_BASE")?, cap_mib: cfg_parse("SPIRA_DWARF_WARM_CAP_MIB")?, inbox: cfg("SPIRA_CONCIERGE_INBOX")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub name: String,
    pub mib: u64,
    /// The holder named in the slot's lock, when a session holds it.
    pub held_by: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub base: Option<String>,
    pub pct: Option<u8>,
    pub slots: Vec<Slot>,
}

/// Today's `run/watchd/warm-full.said` latch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: bool,
}

/// The reap set, then the crossing (deferred a tick when anything was reaped).
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.warm_full: not ported yet")
}

pub struct WatchWarmFull {
    pub config: Result<Config, String>,
}

impl WatchWarmFull {
    pub fn new(config: Config) -> Self {
        WatchWarmFull { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchWarmFull { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.warm_full/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchWarmFull {
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
