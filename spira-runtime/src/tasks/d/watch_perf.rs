//! # watch.perf — contract
//!
//! Replaces `spira-perf-watch-prod` (`perf-watch --probes spira/perf-probes --record run/perf-watch.last --slow-log run/perf-slow.log --health run/perf-watch.health`), pattern P1. Plan: [[scheduler-consolidation-2026-11-03]].
//!
//! ## Tick
//!
//! - Every 5 min (timer `OnUnitActiveSec=5min`, `OnBootSec=3min`); single-flight.
//! - The code adds its own floor: a pass younger than `SPIRA_READ_WATCH_INTERVAL` is skipped (`perf-watch/src/lib.rs:270-274`). In the task that floor is the timer interval; the key stays pinned so the two cannot disagree.
//! - Woken by nothing else.
//!
//! ## Observe
//!
//! `watch.perf.observe` returns `Snapshot { timings: [{name, micros, millis, phases?, error?}], callers: {caller → count} }`.
//!
//! - Probe list: `spira/perf-probes` (six named read paths: `lc-list`, `lc-show`, `lc-ops-view`, `work-list`, `claim-check`, `lc-ready`), parsed by `parse_probes` (`perf-watch/src/lib.rs:32`), loaded once at boot (`perf-watch/src/main.rs:126-129`).
//! - Each probe today is a **forked CLI** timed inside `systemd-run --user --scope bash -c` (`perf-watch/src/main.rs:27,55-56`), 5 s deadline (`main.rs:10,74-77`).
//! - Caller counts come from `spira-lc stats` (`main.rs:112`, parsed by `caller_counts`, `lib.rs:55`).
//! - **Open point.** The thing measured is the fork-and-socket path that S7 removes. After S7 the probe should time the in-process `LcClient` / projection read for the same six names, and `stats` becomes the store's `stats` socket method. The probe file's names are kept, so the metric series and alarm keys do not change. Until then the observe needs the `host` class to fork the probe argv, the one place a P1 task keeps a Spira child (unverified that W1 can allow-list it).
//!
//! ## Decide
//!
//! - `decide(carry, snap, now, cfg) -> (Carry, Out)`: today's `run_pass` (`lib.rs:262-301`) with the probing lifted out. That is `pass` (`lib.rs:145-161`), `alarms` (`lib.rs:111-121`), `top_callers` (`lib.rs:67`), remind dedup by `alarm_key` (`lib.rs:222,288-296`) and `slow_samples` (`lib.rs:130-142`).
//! - Carry is today's `--health` file, `Health { last_pass, alarms: key→printed_at, sample_at, sample }` (`lib.rs:170-219`), emitted as `watch.perf.state`. `--record` (`perf-watch.last`, `main.rs:153-156`) has no reader in the release outside its unit test (`spira/test-perf-watch-units.sh`). It becomes a field of the carry, not a file.
//! - Config: `budget_ms` = `SPIRA_READ_WATCH_BUDGET_MS`, `interval_secs` = `SPIRA_READ_WATCH_INTERVAL`, `remind_secs` = `SPIRA_READ_WATCH_REMIND` (`main.rs:130-134`).
//! - Fixtures: the unit tests in `perf-watch/src/lib.rs:341-541` port as-is: standing alarm reminds, a cleared alarm forgets, exactly-the-limit is not slow, an unrunnable probe is an alarm, a restarted counter, health round-trips.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.perf.observe` (probe) | store (host until S7) | 35 s (6 probes × 5 s + stats) | — (probe, not journaled) | a malformed probe file, at boot (`lib.rs:494`) |
//! | `inbox.append` (shared) | host | 10 s | `watch.perf:<alarm_key>:<printed_at>` | an empty line |
//! | `watch.perf.log` | host | 5 s | `watch.perf:log:<alarm_key>:<printed_at>` | a path outside `SPIRA_RUN` |
//! | `watch.perf.slow_sample` | host | 5 s | `watch.perf:slow:<probe>:<epoch>` | a path outside `SPIRA_RUN` |
//! | `watch.perf.metrics` | host | 5 s | `watch.perf:metrics:<epoch>` | nothing; a lost push is a gap in the series, not a failure |
//!
//! - **`inbox.append`** carries `<UTC ISO> [watch:perf-watch] <alarm>` (`inbox_line`, `lib.rs:226-239`, test `lib.rs:350-352`). The alarm is one of `PERF SLOW <name> <ms>ms (limit <L>ms) top callers: <c1, c2, c3|unknown>` or `PERF UNRUNNABLE <name>: <err>` (`lib.rs:116-117`). The shared runner adds the newline, so the task drops the trailing `\n` that `inbox_line` appends.
//! - **`watch.perf.log`** appends the bare alarm line to `run/perf-watch.log`, the log row `spira/watchers:122` that the unit's stdout fills today (`main.rs:144`).
//! - **`watch.perf.slow_sample`** appends `<epoch>\t<probe>\t<ms>\t<phases>\trest=<ms>\t<top callers>` to `run/perf-slow.log` (`lib.rs:128-141`, `main.rs:147-151`).
//! - **`watch.perf.metrics`** pushes `spira_perf_watch_probe_seconds{probe}` for every answered probe (`lib.rs:101-109`, `main.rs:152`).
//!
//! ## Silence
//!
//! An idle tick, where every probe is inside budget and no alarm is new or due for a reminder, emits the probe, one `watch.perf.metrics` push, and a `watch.perf.state` Emit. The Emit is needed because `last_pass` and the stats sample change every pass.
//!
//! S11's budget must allow that one Emit per tick, 12/h. The alternative is to carry the stats sample in memory (`Note`) and accept a lost baseline on restart. The caller list then reads `unknown` for one pass, which the code already handles (`lib.rs:480`).
//!
//! ## Replaces
//!
//! - `spira-perf-watch-prod.service` and `spira-perf-watch-prod.timer` (`CPUQuota=20%`, `TimeoutStartSec=240`).
//! - The log row `perf-watch|log|@SPIRA_RUN@/perf-watch.log` (`spira/watchers:122`) keeps its file as the read contract.
//! - Children:
//!   - `timeout 5 systemd-run --version` (`main.rs:37`)
//!   - `systemd-run --user --scope … bash -c` (`main.rs:55-56`)
//!   - the six probe CLIs: `spira-lc` ×4, `work`, `spira-claim` (`spira/perf-probes`)
//!   - `spira-lc stats` (`main.rs:112`)
//! - Keys pinned (S10): `SPIRA_READ_WATCH_BUDGET_MS`, `SPIRA_READ_WATCH_INTERVAL`, `SPIRA_READ_WATCH_REMIND`, `SPIRA_CONCIERGE_INBOX` (`main.rs:140`), the metrics endpoint (`main.rs:152`).
//!
//! Catalogue differs: perf-watch forks `spira-lc stats` and the probe CLIs, not `queue round`. `queue round status` is perf-happy-path's child (`main.rs:191`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task, Observe mode (S4) shadows the four write effects while the probe still runs.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use super::pattern;
use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "watch.perf";
pub const STREAM: &str = "watch.perf";
pub const TICK: EventKindTag = "watch.perf.tick";
pub const STATE: EventKindTag = "watch.perf.state";
pub const OBSERVE: &str = "watch.perf.observe";
pub const LOG: &str = "watch.perf.log";
pub const SLOW_SAMPLE: &str = "watch.perf.slow_sample";
pub const METRICS: &str = "watch.perf.metrics";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND, LOG, SLOW_SAMPLE, METRICS];
pub const KEYS: &[&str] = &["SPIRA_READ_WATCH_BUDGET_MS", "SPIRA_READ_WATCH_INTERVAL", "SPIRA_READ_WATCH_REMIND", "SPIRA_CONCIERGE_INBOX", "SPIRA_METRICS_ENDPOINT", "SPIRA_RUN"];
/// The tag `inbox_line` wrote (`perf-watch/src/lib.rs:226-239`).
pub const INBOX_TAG: &str = "perf-watch";
/// `OnUnitActiveSec=5min`.
pub const CADENCE_MS: u64 = 5 * 60_000;
const OBSERVE_DEADLINE_MS: u64 = 35_000;

/// The keys, pinned once at build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub budget_ms: u64,
    pub interval_secs: u64,
    pub remind_secs: u64,
    pub inbox: String,
    pub metrics_endpoint: String,
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            budget_ms: cfg_parse("SPIRA_READ_WATCH_BUDGET_MS")?,
            interval_secs: cfg_parse("SPIRA_READ_WATCH_INTERVAL")?,
            remind_secs: cfg_parse("SPIRA_READ_WATCH_REMIND")?,
            inbox: cfg("SPIRA_CONCIERGE_INBOX")?,
            metrics_endpoint: cfg("SPIRA_METRICS_ENDPOINT")?,
            run: cfg("SPIRA_RUN")?,
        })
    }
}

/// One probe's timing; `error` when the probe could not run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub name: String,
    pub micros: u64,
    pub millis: u64,
    #[serde(default)]
    pub phases: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timings: Vec<Timing>,
    pub callers: BTreeMap<String, u64>,
}

/// Today's `--health` file (`perf-watch/src/lib.rs:170-219`) plus the `--record` it replaces.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub last_pass: u64,
    /// alarm key → when it was last printed.
    pub alarms: BTreeMap<String, u64>,
    pub sample_at: u64,
    pub sample: BTreeMap<String, u64>,
    pub record: Vec<Timing>,
}

/// `run_pass` with the probing lifted out: the next carry and the writes, among them each
/// `inbox.append` line as `<UTC ISO> [watch:perf-watch] <alarm>`.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.perf: not ported yet")
}

pub struct WatchPerf {
    pub config: Result<Config, String>,
}

impl WatchPerf {
    pub fn new(config: Config) -> Self {
        WatchPerf { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchPerf { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.perf/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchPerf {
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
