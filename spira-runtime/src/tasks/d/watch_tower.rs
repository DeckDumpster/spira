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
    /// The lapsed count could not be read (`Count::Unknown`): never nominal.
    #[serde(default)]
    pub lapsed_unknown: bool,
    /// `DiskMem::{disk_breach, mem_breach}` and `cpu_throttle_breach`, judged by the probe
    /// against the sweep's own thresholds.
    #[serde(default)]
    pub disk_breach: bool,
    #[serde(default)]
    pub mem_breach: bool,
    #[serde(default)]
    pub cpu_breach: bool,
    /// The probes that hit their deadline and rendered `?`.
    #[serde(default)]
    pub slow_probes: Vec<String>,
    /// `lapsed_now`, the marker the prompt write advances.
    #[serde(default)]
    pub lapsed_now: Option<String>,
    /// `render::render` of the gathered data: the prompt of a sweep that is not nominal.
    #[serde(default)]
    pub rendered: Option<String>,
    /// The threshold escalations (`escalate::thresholds`) the gathered data raises.
    #[serde(default)]
    pub escalations: Vec<watchtower::incident::Finding>,
}

/// `{ lapsed_marker, failed_units rows, cpu_throttle samples, prompt }`; `prompt` is the last
/// `SWEEP:NOMINAL` line, so an unchanged nominal sweep rewrites nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub lapsed_marker: Option<String>,
    pub failed_units: BTreeMap<String, u64>,
    pub cpu_throttle: BTreeMap<String, u64>,
    pub prompt: Option<String>,
    /// The failed units already escalated (`failed-units.state`'s third column).
    #[serde(default)]
    pub escalated: std::collections::BTreeSet<String>,
    /// Alarm reference → when it was last requested: `incident alarm`'s reminder interval, so a
    /// standing condition is raised once a window, not every tick.
    #[serde(default)]
    pub alarmed: BTreeMap<String, u64>,
}

/// `SPIRA_FAILED_UNITS_WARN_MINS`'s default (`main.rs`): env-only, not yet registered.
pub const FAILED_UNITS_WARN_MINS: i64 = 15;
/// `incident::alarm::REMINDER_S`: a condition already noted is silent this long.
pub const REMIND_S: u64 = 86_400;
const ACT_DEADLINE_MS: u64 = 5_000;
const STORE_DEADLINE_MS: u64 = 30_000;

/// What `watch.tower.prompt` writes: the prompt file's text and the lapse marker it advances.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    pub lapsed_now: Option<String>,
}

/// `render::is_nominal` over the snapshot: nothing lapsed, a fresh snapshot, no no-verdict
/// streak, not draining, not throttled, no disk/mem/CPU breach, no slow probe, and the failed
/// units known and empty.
pub fn is_nominal(s: &Snapshot, snap_stale_s: u64) -> bool {
    !s.lapsed_unknown
        && s.lapsed.is_empty()
        && s.snap_age_s.is_some_and(|a| a < snap_stale_s)
        && s.noverdict.values().all(|n| *n == 0)
        && !s.draining
        && !s.throttled
        && !s.disk_breach
        && !s.mem_breach
        && !s.cpu_breach
        && s.slow_probes.is_empty()
        && s.failed_units.as_ref().is_some_and(|v| v.is_empty())
}

fn spec(name: &str, key: String, input: String, deadline_ms: u64) -> EffectSpec {
    EffectSpec::keyed(name, key, input, deadline_ms)
}

/// `is_nominal`, `render`, the 13 escalation predicates and the halt guard.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    // A DELIBERATELY STOPPED WORLD MUST NOT FILE (`sweep/mod.rs:27-39`): nothing at all.
    if snap.halted {
        return (carry.clone(), vec![]);
    }
    let now = now_ms / 1000;
    let mut next = carry.clone();
    let mut acts = vec![];
    if is_nominal(snap, cfg.snap_stale_s) {
        let nv_worst = snap.noverdict.values().max().copied().unwrap_or(0);
        let line = format!("SWEEP:NOMINAL lapsed={} snap_age={}s nv_worst={nv_worst}\n", snap.lapsed.len(), snap.snap_age_s.unwrap_or(0));
        if carry.prompt.as_deref() != Some(line.as_str()) {
            acts.push(spec(PROMPT, format!("watch.tower:prompt:{now_ms}"), serde_json::to_string(&Prompt { text: line.clone(), lapsed_now: snap.lapsed_now.clone() }).unwrap_or_default(), ACT_DEADLINE_MS));
            next.prompt = Some(line);
        }
        next.failed_units.clear();
        next.escalated.clear();
        next.alarmed.retain(|_, at| now.saturating_sub(*at) < REMIND_S);
        return (next, acts);
    }
    let text = snap.rendered.clone().unwrap_or_else(|| format!("## Spira pipeline\n(sweep not rendered: {} failed unit(s), {} lapsed)\n", snap.failed_units.as_ref().map_or(0, Vec::len), snap.lapsed.len()));
    acts.push(spec(PROMPT, format!("watch.tower:prompt:{now_ms}"), serde_json::to_string(&Prompt { text, lapsed_now: snap.lapsed_now.clone() }).unwrap_or_default(), ACT_DEADLINE_MS));
    next.prompt = None;
    let slow = if snap.slow_probes.is_empty() { String::new() } else { format!(" (timed out: {})", snap.slow_probes.join("; ")) };
    acts.push(spec(LOG, format!("watch.tower:log:{now_ms}"), format!("watchtower: swept — {} lapsed, {} escalation(s){slow}", snap.lapsed.len(), snap.escalations.len()), ACT_DEADLINE_MS));
    let mut raise = |next: &mut Carry, f: watchtower::incident::Finding| {
        let reference = f.reference.clone().unwrap_or_else(|| f.title.clone());
        if next.alarmed.get(&reference).is_some_and(|at| now.saturating_sub(*at) < REMIND_S) {
            return;
        }
        let key = format!("watch.tower:{reference}:{}", now / REMIND_S);
        next.alarmed.insert(reference, now);
        acts.push(spec(ALARM, key, serde_json::to_string(&f).unwrap_or_default(), STORE_DEADLINE_MS));
    };
    // The failed-unit escalation (`escalate.rs:59-96`), its first-seen rows carried.
    if let Some(units) = &snap.failed_units {
        let (mut rows, mut escalated) = (BTreeMap::new(), std::collections::BTreeSet::new());
        for unit in units {
            let prev = carry.failed_units.get(unit).map(|t| watchtower::failed_units::StateRow { unit: unit.clone(), first_seen: *t as i64, escalated: carry.escalated.contains(unit) });
            let (row, decided) = watchtower::failed_units::decide(now as i64, unit, prev.as_ref(), FAILED_UNITS_WARN_MINS);
            if decided || (row.escalated && !snap.filed_incidents.contains_key(unit)) {
                raise(&mut next, watchtower::sweep::escalate::failed_unit_finding(&cfg.db, &cfg.home_repo, unit, row.first_seen, now as i64));
            }
            if row.escalated {
                escalated.insert(unit.clone());
            }
            rows.insert(unit.clone(), u64::try_from(row.first_seen).unwrap_or(0));
        }
        next.failed_units = rows;
        next.escalated = escalated;
    }
    for f in &snap.escalations {
        raise(&mut next, f.clone());
    }
    if snap.draining {
        acts.push(spec(DRAIN_CHECK, format!("watch.tower:drain-check:{now_ms}"), String::new(), STORE_DEADLINE_MS));
    }
    next.alarmed.retain(|_, at| now.saturating_sub(*at) < REMIND_S);
    (next, acts)
}

/// The probe: the sweep's own gather (`sweep::collect`) and its pure readings, in-process.
/// The children the gather still forks (systemctl, bash seams, df) are the crate's fork edges.
pub fn observe() -> Result<String, String> {
    use watchtower::sweep::collect::Drain;
    let cfg = watchtower::process::sweep_cfg()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let d = watchtower::sweep::collect(now, &cfg);
    let mut escalations = vec![];
    watchtower::sweep::escalate::thresholds(&d, &cfg, &mut |f| {
        escalations.push(f.clone());
        watchtower::incident::AlarmOutcome::Noted
    });
    let (lapsed, lapsed_unknown) = match d.lapsed.count {
        watchtower::lapsed::Count::Known(n) => ((0..n).map(|i| format!("lapsed#{i}")).collect(), false),
        watchtower::lapsed::Count::Unknown => (vec![], true),
    };
    let pct = |s: &str| s.trim_end_matches('%').parse::<u8>().ok();
    let snap = Snapshot {
        halted: d.halt.is_some(),
        draining: matches!(d.drain, Drain::Draining { .. }),
        failed_units: d.failed_units.clone(),
        gate_wait_s: d.gate_wait.oldest_wait.and_then(|w| u64::try_from(w).ok()),
        gate_slow: d.gate_slow.is_some(),
        noverdict: d.nv_worst_key.clone().map(|k| (k, u64::try_from(d.nv_worst).unwrap_or(0))).into_iter().collect(),
        disk_pct: pct(&d.disk.disk_disp),
        tmp_pct: pct(&d.tmp_pct),
        throttled: d.throttle.since.is_some(),
        lapsed,
        snap_age_s: d.snap_age.and_then(|a| u64::try_from(a).ok()),
        hotfix: d.release_status.clone(),
        lapsed_unknown,
        disk_breach: d.disk.disk_breach,
        mem_breach: d.disk.mem_breach,
        cpu_breach: d.cpu_throttle_breach,
        slow_probes: d.slow_probes.clone(),
        lapsed_now: Some(d.lapsed_now.clone()),
        rendered: Some(watchtower::sweep::render::render(&d)),
        escalations,
        ..Snapshot::default()
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

fn atomic_write(path: &std::path::Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// `run/ops-sweep-prompt.txt`, atomically (`persona.ops` reads it as its stdin), then the
/// lapse marker, as `run_sweep` wrote them.
pub fn prompt(input: &str) -> Result<String, String> {
    let p: Prompt = serde_json::from_str(input).map_err(|e| format!("{PROMPT}: {e}"))?;
    let cfg = watchtower::process::sweep_cfg()?;
    let path = cfg.prompt_file();
    atomic_write(&path, &p.text)?;
    if let Some(m) = &p.lapsed_now {
        std::fs::write(cfg.lapsed_marker(), format!("{m}\n")).map_err(|e| format!("{}: {e}", cfg.lapsed_marker().display()))?;
    }
    Ok(path.display().to_string())
}

/// One escalation as `incident alarm`: a deduplicated Concierge-inbox note, never a bead. A
/// cause the control plane suspends is refused there.
pub fn alarm(input: &str) -> Result<String, String> {
    let f: watchtower::incident::Finding = serde_json::from_str(input).map_err(|e| format!("{ALARM}: {e}"))?;
    match watchtower::incident::alarm_outcome(&watchtower::process::resolved_incident_sh(), &f) {
        watchtower::incident::AlarmOutcome::Noted => Ok(format!("noted {}", f.title)),
        watchtower::incident::AlarmOutcome::Deduped => Ok(format!("deduped {}", f.title)),
        watchtower::incident::AlarmOutcome::Failed => Err(format!("incident alarm failed for {}", f.title)),
    }
}

/// `world drain-check`: the drain's own cap and owner checks.
pub fn drain_check() -> Result<String, String> {
    use runtime::ports::Proc;
    let spec = runtime::ports::ProcSpec { prog: "world".into(), args: vec!["drain-check".into()], stdin: vec![], deadline: std::time::Duration::from_millis(STORE_DEADLINE_MS) };
    match runtime::ports::SysProc.run(&spec)? {
        o if o.code == Some(0) => Ok("drain checked".into()),
        o => Err(format!("world drain-check exited {:?}: {}", o.code, String::from_utf8_lossy(&o.stderr).trim())),
    }
}

/// `<ISO> spira: <msg>` appended to `run/watchtower.log` (`log.rs:1-7`).
pub fn log(input: &str) -> Result<String, String> {
    use std::io::Write;
    let path = std::path::Path::new(&cfg("SPIRA_RUN")?).join("watchtower.log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{} spira: {input}", watchtower::log::now_iso()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
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
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

/// The rows a restart keeps; what a Carry field is left out of is rebuilt by the first tick.
const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("prompt"), runtime::rows::Part::Map { name: "failed_units", drop: &[] }, runtime::rows::Part::Field("escalated"), runtime::rows::Part::Map { name: "alarmed", drop: &[] }];

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
