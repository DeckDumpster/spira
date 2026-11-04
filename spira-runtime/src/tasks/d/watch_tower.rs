//! # watch.tower — contract
//!
//! Replaces `spira-watchtower-prod` (`ExecStart=bin/watchtower`, the bare sweep: `watchtower/src/main.rs:220` → `sweep::run_sweep`), pattern P1. It gathers the pipeline's vital signs, writes the ops persona's prompt, and raises the threshold alarms.
//!
//! The other subcommands (`--throttle-check`, the sentinel's detectors, `pr_stall`) are not on this timer and are out of scope here.
//!
//! ## Tick
//!
//! - Every 30 min (`OnUnitActiveSec=30min`, `OnBootSec=3min`); single-flight.
//! - Today's `TimeoutStartSec` is 6 min (drop-in `50-timeout.conf`, an interim for load).
//! - No other wake. `persona.ops` reads what this writes; it does not wake on it today.
//!
//! ## Observe
//!
//! `watch.tower.observe` returns `SweepData` (`watchtower/src/sweep/collect.rs:168-399`). Field sources:
//!
//! | Field | Today | Becomes |
//! |---|---|---|
//! | halt / drain | `run/world.halted`, `run/world.draining` (`collect.rs:143-166,172-173`) | file read (host) |
//! | failed units, CPU throttle | `systemctl --user list-units --failed` (`failed_units.rs:30-33`); `systemctl show -p ControlGroup` + `cpu.stat` (`cpu_throttle.rs:104-107`, `collect.rs:185`) | `unit.facts` (S6) + cgroupfs |
//! | pipeline counters `SP_*` | `run/cockpit/cockpit.env` (`collect.rs:192-217`) | the cockpit projection (S7b) |
//! | last landing, WORKING / SUBMITTED rows | `spira-lc list --state X --snapshot` (`lc.rs:48-72`, `collect.rs:220,301`) | `LcClient` list (S7a) |
//! | gate wait / slow / silence | `run/gate.log` (`collect.rs:228-232`) | file read |
//! | yield | `bash yield.sh report` (`collect.rs:235-246`) | port into the task, or `(unverified)` keep |
//! | no-verdict streaks | `run/noverdict/*` (`collect.rs:268-287`) | file read |
//! | dwarves live / ready by rune | `bash -c '. lib.sh; spira_runes; dwarf_count; bd ready'` (`seams.rs:31-59`, `collect.rs:291`) + `run/dwarf-ledger.log` | ready counts projection (S7b) + `spira-claim` library |
//! | disk / mem / tmp | `df /`, `df /tmp`, `/proc/meminfo` (`collect.rs:313-314,413`) | statvfs + procfs |
//! | throttle stamp | `run/queue-throttled` (`collect.rs:318`) | file read |
//! | lapsed dwarves | `run/lapsed/` against `run/lapsed.swept` (`collect.rs:342`) | file read |
//! | suites menu, branch guard | `testenv suites status` / `bash suites.sh status`; `bash branch-guard.sh check` (`collect.rs:349-350,452-480`) | `(unverified)` library calls |
//! | release status (hotfix) | `release status` (`collect.rs:354-357`) | `release` library |
//!
//! Side effect hidden in observe: `cpu_throttle::gather` rewrites `run/cpu-throttle.state` (`cpu_throttle.rs:135`). The task moves that sample into carry, so the probe is read-only.
//!
//! ## Decide
//!
//! `decide(carry, data) -> (Carry, Acts)` is made of:
//!
//! - **`render::is_nominal`** (`sweep/render.rs:62-74`): lapsed = 0, snapshot fresh, no no-verdict, not draining, not throttled, no disk/mem/CPU breach, no slow probe, failed units known and empty.
//! - **`render::render`** (`render.rs:76…`), which produces the prompt.
//! - **The 13 escalation predicates** in `escalate::run` (`sweep/escalate.rs:21-36`).
//! - **The halt guard**: halted means no acts at all (`sweep/mod.rs:32-39`).
//!
//! Carry: `{ lapsed_marker, failed_units rows, cpu_throttle samples }`. Today these are `run/lapsed.swept`, `run/failed-units.state` and `run/cpu-throttle.state` (`cfg.rs:63-71`, `escalate.rs:63-64`). It is emitted as `watch.tower.state`.
//!
//! Fixtures to port:
//!
//! - `sweep/render.rs` (20 tests), `sweep/collect.rs` (18), `sweep/escalate.rs` (9)
//! - `spira/test-watchtower-lapse.sh`
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.tower.observe` (probe) | host | 330 s (6 min less 30 s, `main.rs:168-171`) | — | — |
//! | `watch.tower.prompt` | host | 5 s | `watch.tower:prompt:<tick>` | when halted |
//! | `watch.tower.alarm` | store | 30 s | `watch.tower:<reference>:<remind window>` | a cause the control plane suspends (`incident.rs:129-132`, `ctrl_gate.rs:13`); when halted |
//! | `watch.tower.drain_check` | store | 30 s | `watch.tower:drain-check:<tick>` | when not draining |
//! | `watch.tower.log` | host | 5 s | `watch.tower:log:<tick>` | — |
//!
//! - **`watch.tower.prompt`** writes `run/ops-sweep-prompt.txt` atomically (`mod.rs:45-79,92-98`): either `SWEEP:NOMINAL lapsed=<n> snap_age=<s>s nv_worst=<n>` or the rendered sweep.
//!   - **This file is `persona.ops`'s stdin** (`spira-ops-prod.service` `StandardInput=file:…/ops-sweep-prompt.txt`). The edge must become a carried value `persona.ops` reads, or stay this file. Decide with [[persona-ops]].
//! - **`watch.tower.alarm`** is one per escalation. It is today's `bash incident.sh alarm <title> -` (`incident.rs:107-148`), a deduplicated Concierge-inbox note, never a bead. References: `incident:failed-unit-<unit>`, `hel-oldest-unsent`, `hel-unadopted-refs`, `hotfix-<sha>`, `hel-batched-stranded`, `queue-batched-too-long`, `hel-closed-stranded`, `dedup-meter-nonzero`, `gate-silent`, `gate-slow`, `idle-while-ready:<rune>`, and the unreferenced `DRAINING: world.sh calls gated` (`escalate.rs:61-380`).
//!   - Failed-unit alarms first ask `bdq list --external-ref incident:failed-unit-<unit>` plus the lifecycle row (`escalate.rs:46-59`). That becomes the `content` verb plus `LcClient`, read in observe.
//!   - It becomes a library call into `incident` (plan §2). The inbox line format is incident's, `(unverified)` here.
//! - **`watch.tower.drain_check`** is today's `world drain-check` (`escalate.rs:100-108`).
//! - **`watch.tower.log`** keeps `<ISO> spira: watchtower: <msg>` (`log.rs:1-7`) in `run/watchtower.log` (unit stdout) for the swept/nominal/slow-probe lines (`mod.rs:33-77`).
//!
//! **Dropped:** `moot-sweep.sh --apply` (`escalate.rs:382-399`) is the same script `spira-moot-sweep-prod` runs every 15 min. It becomes [[asks-moot]] and is not repeated here.
//!
//! ## Silence
//!
//! On an idle (nominal) tick, today's code still rewrites the prompt and the lapse marker and logs `nominal`. The task emits the probe and one `watch.tower.prompt`, and only when the `SWEEP:NOMINAL` line changed from the last one. There is no alarm, no log and no state Emit unless the lapse marker moved.
//!
//! ## Replaces
//!
//! - `spira-watchtower-prod.service`, `spira-watchtower-prod.timer` and drop-in `50-timeout.conf`.
//! - Children of the sweep:
//!   - `systemctl` ×2 (`failed_units.rs:33`, `cpu_throttle.rs:105`)
//!   - `spira-lc` (`lc.rs:49`)
//!   - `bdq` (`escalate.rs:47`)
//!   - `bash` ×6: yield.sh, pipeline probe, suites.sh, branch-guard, incident.sh, moot-sweep.sh (`collect.rs:239,454,472`, `seams.rs:53`, `incident.rs:133`, `escalate.rs:388`)
//!   - `testenv` (`collect.rs:457`)
//!   - `release` (`collect.rs:356`)
//!   - `df` ×2 (`collect.rs:413`, `disk_mem.rs:45`)
//!   - `world drain-check` (`escalate.rs:104`)
//! - Keys pinned (`main.rs:167-210`): `SPIRA_WATCHTOWER_START_TIMEOUT_S`, `_PROBE_TIMEOUT_S`, `SPIRA_DB`, `SPIRA_HOME_REPO`, `SPIRA_ASK_LABEL`, `SPIRA_SNAP_STALE_S`, `SPIRA_QUEUE_THROTTLE_OVERRIDE`, plus the ~25 env-only keys read by `getenv` with defaults (`SPIRA_WATCH_GATE_*`, `SPIRA_DISK_WARN_PCT`, `SPIRA_MEM_WARN_MB`, `SPIRA_CPU_THROTTLE_*`, `SPIRA_IDLE_WHILE_READY_N`, …). S10 must register these, because today they bypass the toml (`main.rs:83-96`).
//!
//! Catalogue differs: the timed sweep forks no `gh`. gh is `pr_stall`'s (`pr_stall.rs:79-83`), a sentinel subcommand. `world` is the `drain-check` call, and `bash` ×6 includes `moot-sweep.sh`, which duplicates the moot-sweep unit.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Observe mode shadows the prompt, alarm and drain-check. The ops persona then reads a stale prompt, which is acceptable for one flip.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use super::pattern;

pub const NAME: &str = "watch.tower";
pub const STREAM: &str = "watch.tower";
pub const TICK: EventKindTag = "watch.tower.tick";
pub const STATE: EventKindTag = "watch.tower.state";
pub const OBSERVE: &str = "watch.tower.observe";
pub const PROMPT: &str = "watch.tower.prompt";
pub const ALARM: &str = "watch.tower.alarm";
pub const DRAIN_CHECK: &str = "watch.tower.drain_check";
pub const LOG: &str = "watch.tower.log";
pub const EFFECTS: &[&str] = &[OBSERVE, PROMPT, ALARM, DRAIN_CHECK, LOG];
/// The toml keys the sweep reads (`main.rs:167-210`). The ~25 env-only `getenv` keys the contract
/// lists (`SPIRA_WATCH_GATE_*`, `SPIRA_DISK_WARN_PCT`, …) are not registered and join when S10
/// registers them.
pub const KEYS: &[&str] = &["SPIRA_WATCHTOWER_START_TIMEOUT_S", "SPIRA_WATCHTOWER_PROBE_TIMEOUT_S", "SPIRA_DB", "SPIRA_HOME_REPO", "SPIRA_ASK_LABEL", "SPIRA_SNAP_STALE_S", "SPIRA_QUEUE_THROTTLE_OVERRIDE"];
/// `OnUnitActiveSec=30min`.
pub const CADENCE_MS: u64 = 30 * 60_000;
/// 6 min less 30 s (`main.rs:168-171`).
const OBSERVE_DEADLINE_MS: u64 = 330_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub start_timeout_s: u64,
    pub probe_timeout_s: u64,
    pub db: String,
    pub home_repo: String,
    pub ask_label: String,
    pub snap_stale_s: u64,
    pub queue_throttle_override: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            start_timeout_s: cfg_parse("SPIRA_WATCHTOWER_START_TIMEOUT_S")?,
            probe_timeout_s: cfg_parse("SPIRA_WATCHTOWER_PROBE_TIMEOUT_S")?,
            db: cfg("SPIRA_DB")?,
            home_repo: cfg("SPIRA_HOME_REPO")?,
            ask_label: cfg("SPIRA_ASK_LABEL")?,
            snap_stale_s: cfg_parse("SPIRA_SNAP_STALE_S")?,
            queue_throttle_override: cfg("SPIRA_QUEUE_THROTTLE_OVERRIDE")?,
        })
    }
}

/// `SweepData` (`watchtower/src/sweep/collect.rs:168-399`), one field per row of the contract's
/// source table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub draining: bool,
    /// `None` when the failed-unit list could not be read.
    pub failed_units: Option<Vec<String>>,
    pub cpu_throttle: BTreeMap<String, u64>,
    /// The cockpit's `SP_*` counters.
    pub counters: BTreeMap<String, String>,
    pub last_landing_at: Option<u64>,
    pub working: Vec<String>,
    pub submitted: Vec<String>,
    pub gate_wait_s: Option<u64>,
    pub gate_slow: bool,
    pub gate_silent_s: Option<u64>,
    pub yield_report: Option<String>,
    /// bead → no-verdict streak.
    pub noverdict: BTreeMap<String, u64>,
    /// rune → (dwarves live, ready).
    pub runes: BTreeMap<String, (u64, u64)>,
    pub disk_pct: Option<u8>,
    pub tmp_pct: Option<u8>,
    pub mem_avail_mb: Option<u64>,
    pub throttled: bool,
    pub lapsed: Vec<String>,
    pub snap_age_s: Option<u64>,
    pub suites_menu: Option<String>,
    pub branch_guard: Option<String>,
    pub hotfix: Option<String>,
    /// Failed units whose incident is already filed: unit → bead.
    pub filed_incidents: BTreeMap<String, String>,
}

/// `{ lapsed_marker, failed_units rows, cpu_throttle samples, prompt }`; `prompt` is the last
/// `SWEEP:NOMINAL` line, so an unchanged nominal sweep rewrites nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub lapsed_marker: Option<String>,
    pub failed_units: BTreeMap<String, u64>,
    pub cpu_throttle: BTreeMap<String, u64>,
    pub prompt: Option<String>,
}

/// `is_nominal`, `render`, the 13 escalation predicates and the halt guard.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.tower: not ported yet")
}

pub struct WatchTower {
    pub config: Result<Config, String>,
}

impl WatchTower {
    pub fn new(config: Config) -> Self {
        WatchTower { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchTower { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.tower/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchTower {
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
