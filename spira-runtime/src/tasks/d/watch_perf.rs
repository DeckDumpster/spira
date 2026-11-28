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
//! - Probe list: `spira/perf-probes` (seven named read paths: `lc-list`, `lc-show`, `lc-ops-view`, `work-list`, `claim-check`, `lc-ready`, `content-read`), parsed by `parse_probes` (`perf-watch/src/lib.rs:32`), loaded once at boot (`perf-watch/src/main.rs:126-129`).
//! - Each probe today is a **forked CLI** timed inside `systemd-run --user --scope bash -c` (`perf-watch/src/main.rs:27,55-56`), 5 s deadline (`main.rs:10,74-77`). `time_one` stamps `PERFWATCH_T0_US` before the fork so the probe's phases open with `spawn`, `load` and `config` (fork, exec and dynamic link, config until `begin`), and appends `exit` (the probe's `PERFWATCH_END_US` stamp to the reap), so the phases add up to the wall time.
//! - Caller counts come from `spira-lc stats` (`main.rs:112`, parsed by `caller_counts`, `lib.rs:55`).
//! - **Open point.** The thing measured is the fork-and-socket path that S7 removes. After S7 the probe should time the in-process `LcClient` / projection read for the same six names, and `stats` becomes the store's `stats` socket method. The probe file's names are kept, so the metric series and alarm keys do not change. Until then the observe needs the `host` class to fork the probe argv, the one place a P1 task keeps a Spira child (unverified that W1 can allow-list it).
//!
//! ## Decide
//!
//! - `decide(carry, snap, now, cfg) -> (Carry, Out)`: today's `run_pass` (`lib.rs:262-301`) with the probing lifted out. That is `pass` (`lib.rs:145-161`), `alarms` (`lib.rs:111-121`), `top_callers` (`lib.rs:67`), remind dedup by `alarm_key` (`lib.rs:222,288-296`) and `slow_samples` (`lib.rs:130-142`).
//! - Carry is today's `--health` file, `Health { last_pass, alarms: key→printed_at, sample_at, sample }` (`lib.rs:170-219`), emitted as `watch.perf.state`. `--record` (`perf-watch.last`, `main.rs:153-156`) has no reader in the release outside its unit test (`the perf-watch-units suite`). It becomes a field of the carry, not a file.
//! - Config: `budget_ms` = `SPIRA_READ_WATCH_BUDGET_MS`, `interval_secs` = `SPIRA_READ_WATCH_INTERVAL`, `remind_secs` = `SPIRA_READ_WATCH_REMIND` (`main.rs:130-134`).
//! - Fixtures: the unit tests in `perf-watch/src/lib.rs:341-541` port as-is: standing alarm reminds, a cleared alarm forgets, exactly-the-limit is not slow, an unrunnable probe is an alarm, a restarted counter, health round-trips.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.publish` (shared) | host | 10 s | `watch/perf-watch/<pass ms>/<fnv1a(input)>` | — |
//! | `watch.perf.observe` (probe) | store (host until S7) | 35 s (6 probes × 5 s + stats) | — (probe, not journaled) | a malformed probe file, at boot (`lib.rs:494`) |
//! | `inbox.append` (shared) | host | 10 s | `watch.perf:<alarm_key>:<printed_at>` | an empty line |
//! | `watch.perf.log` | host | 5 s | `watch.perf:log:<alarm_key>:<printed_at>` | a path outside `SPIRA_RUN` |
//! | `watch.perf.slow_sample` | host | 5 s | `watch.perf:slow:<probe>:<epoch>` | a path outside `SPIRA_RUN` |
//! | `watch.perf.metrics` | host | 5 s | `watch.perf:metrics:<epoch>` | nothing; a lost push is a gap in the series, not a failure |
//!
//! - **`watch.publish`** holds each standing alarm on `watch/perf-watch/<probe>`, whether or not this pass prints it. The value reads as the line last printed. It is cleared when a pass measures the probe and `settle` forgets the alarm; a probe cut at its deadline keeps what it held.
//! - **`inbox.append`** carries `<UTC ISO> [watch:perf-watch] <alarm>` (`inbox_line`, `lib.rs:226-239`, test `lib.rs:350-352`). The alarm is one of `PERF SLOW <name> <ms>ms (limit <L>ms) phases <p|unreported> [rest=<ms>ms; slowest: <phase> <ms>ms (<words>)]; top callers: <c1, c2, c3|unknown>`, `PERF UNRUNNABLE <name>: <err> (after <ms>ms; phases …)` (`lib.rs:116-117`), or `PERF TIMEOUT <name> timed out at <d>ms twice in a row (limit <L>ms); top callers: <c…>` when a probe was cut at its deadline on two passes running. The shared runner adds the newline, so the task drops the trailing `\n` that `inbox_line` appends.
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
//! Catalogue differs: perf-watch forks `spira-lc stats` and the probe CLIs, not `queue round`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task, Observe mode (S4) shadows the four write effects while the probe still runs.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use super::pattern;
use crate::watch_topics::{self, Published};
use crate::watchers::{carried, INBOX_APPEND, WATCH_PUBLISH};

pub const NAME: &str = "watch.perf";
pub const STREAM: &str = "watch.perf";
pub const TICK: EventKindTag = "watch.perf.tick";
pub const STATE: EventKindTag = "watch.perf.state";
pub const OBSERVE: &str = "watch.perf.observe";
pub const LOG: &str = "watch.perf.log";
pub const SLOW_SAMPLE: &str = "watch.perf.slow_sample";
pub const METRICS: &str = "watch.perf.metrics";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND, LOG, SLOW_SAMPLE, METRICS, WATCH_PUBLISH];
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
    #[serde(default)]
    pub timed_out_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timings: Vec<Timing>,
    pub callers: BTreeMap<String, u64>,
}

/// Today's `--health` file (`perf-watch/src/lib.rs:170-219`) plus the `--record` it replaces.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub last_pass: u64,
    /// alarm key → when it was last printed.
    pub alarms: BTreeMap<String, u64>,
    pub sample_at: u64,
    pub sample: BTreeMap<String, u64>,
    pub timed_out: BTreeSet<String>,
    pub record: Vec<Timing>,
    pub published: Published,
}

/// The carry as the library's health: no pass yet is a default carry, and no stats sample yet
/// is an empty one.
fn health_of(carry: &Carry) -> perf_watch::Health {
    perf_watch::Health {
        last_pass: (carry != &Carry::default()).then_some(carry.last_pass),
        alarms: carry.alarms.clone(),
        sample_at: (!carry.sample.is_empty()).then_some(carry.sample_at),
        sample: carry.sample.clone(),
        timed_out: carry.timed_out.clone(),
    }
}

fn to_lib(t: &Timing) -> perf_watch::Timing {
    perf_watch::Timing { name: t.name.clone(), millis: t.millis, micros: t.micros, error: t.error.clone(), phases: t.phases.clone(), timed_out_ms: t.timed_out_ms }
}

fn spec(name: &str, key: String, input: String, deadline_ms: u64) -> EffectSpec {
    EffectSpec::keyed(name, key, input, deadline_ms)
}

/// A probe's alarm is its condition: it reads as the line last printed, so the times of a
/// standing alarm this pass kept quiet about change nothing. A probe cut at its deadline measured
/// nothing: with no alarm for it this pass, what it held is not over.
fn conditions(published: &Published, pass: &perf_watch::Pass) -> BTreeMap<String, String> {
    let cut = pass.timings.iter().filter(|t| t.timed_out_ms.is_some()).filter_map(|t| Some((t.name.clone(), published.get(&t.name)?.text.clone())));
    let mut out: BTreeMap<String, String> = cut.collect();
    out.extend(pass.standing
        .iter()
        .map(|line| {
            let probe = perf_watch::alarm_probe(line).unwrap_or("all");
            let held = published.get(probe).filter(|h| !pass.lines.contains(line) && perf_watch::alarm_key(&h.text) == perf_watch::alarm_key(line));
            (probe.to_string(), held.map_or_else(|| line.clone(), |h| h.text.clone()))
        }));
    out
}

/// `run_pass` with the probing lifted out: the next carry and the writes, among them each
/// `inbox.append` line as `<UTC ISO> [watch:perf-watch] <alarm>`. The library's `settle` is the
/// decision; the metrics push is last, and the handler sends it as a memory-only probe.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let now = now_ms / 1000;
    let settings = perf_watch::Settings { budget_ms: cfg.budget_ms, interval_secs: cfg.interval_secs, remind_secs: cfg.remind_secs };
    let prior = health_of(carry);
    if !perf_watch::due(&settings, &prior, now) {
        return (carry.clone(), vec![]);
    }
    let cur = (!snap.callers.is_empty()).then(|| snap.callers.clone());
    let pass = perf_watch::settle(&settings, &prior, now, snap.timings.iter().map(to_lib).collect(), cur);
    let (published, change) = watch_topics::settle(INBOX_TAG, &carry.published, conditions(&carry.published, &pass), now_ms);
    let next = Carry { last_pass: now, alarms: pass.health.alarms, sample_at: pass.health.sample_at.unwrap_or(0), sample: pass.health.sample, timed_out: pass.health.timed_out, record: snap.timings.clone(), published };
    let mut acts: Vec<EffectSpec> = change.map(|c| watch_topics::effects(c, now_ms)).unwrap_or_default();
    for (i, l) in pass.lines.iter().enumerate() {
        acts.push(spec(INBOX_APPEND, format!("watch.perf:{now}:{i}"), perf_watch::inbox_line(now, l).trim_end().to_string(), WRITE_DEADLINE_MS));
        acts.push(spec(LOG, format!("watch.perf:log:{now}:{i}"), l.clone(), WRITE_DEADLINE_MS));
    }
    for (i, s) in pass.slow.iter().enumerate() {
        acts.push(spec(SLOW_SAMPLE, format!("watch.perf:slow:{i}:{now}"), s.trim_end().to_string(), WRITE_DEADLINE_MS));
    }
    acts.push(spec(METRICS, format!("watch.perf:metrics:{now}"), serde_json::to_string(&snap.timings).unwrap_or_default(), WRITE_DEADLINE_MS));
    (next, acts)
}

const WRITE_DEADLINE_MS: u64 = 5_000;
/// Each probe's own bound (`perf-watch/src/main.rs:10`).
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// The probe file the unit was started with, `spira/perf-probes` in the release.
fn probes() -> Result<Vec<perf_watch::Probe>, String> {
    let file = spira_config::resolve::locate_home_for_process()?.join("perf-probes");
    std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| perf_watch::parse_probes(&t)).map_err(|e| format!("perf-watch: {}: {e}", file.display()))
}

/// The phases line a probe printed to stderr, if it timed itself.
fn phases_of(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr).lines().rev().find_map(|l| l.strip_prefix(spira_otel::run::PHASES_PREFIX)).map(str::to_string)
}

/// The probe's phases with `exit` appended: the time from the probe stamping its end until
/// `reaped_us`. No end stamp leaves them as they were.
fn account(phases: Option<String>, stderr: &str, reaped_us: u64) -> Option<String> {
    let end_us = stderr.lines().rev().find_map(|l| l.strip_prefix(spira_otel::run::END_PREFIX)).and_then(|v| v.trim().parse::<u64>().ok());
    match (phases, end_us) {
        (Some(p), Some(end)) => Some(format!("{p},exit={}", reaped_us.saturating_sub(end) / 1000)),
        (p, _) => p,
    }
}

/// One probe timed end to end: a `spira-lc` read goes over the in-process client (the path
/// S7 left), any other probe is its program run through `proc` and cut at `deadline`. A call
/// that failed after at least its deadline was cut, not refused; the clock says so.
fn time_one(lc: &Result<spira_store::lc_call::LcClient, String>, argv: &[String], proc: &dyn runtime::ports::Proc, deadline: std::time::Duration) -> Timing {
    let t0 = std::time::Instant::now();
    let mut phases = None;
    let mut bound = deadline;
    let r: Result<(), String> = match (argv[0].as_str(), lc) {
        ("spira-lc", Ok(lc)) => {
            bound = spira_store::lc_call::DEADLINE;
            lc.call(&argv[1..]).map(|_| ()).map_err(|e| format!("{}: {e}", argv[0]))
        }
        ("spira-lc", Err(e)) => Err(format!("{}: {e}", argv[0])),
        _ => {
            let phases_on = format!("{}=1", spira_otel::run::PHASES_ENV);
            let t0_us = format!("{}={}", spira_otel::run::T0_ENV, spira_otel::run::epoch_micros());
            let args = [phases_on, t0_us, argv[0].clone()].into_iter().chain(argv[1..].iter().cloned()).collect();
            let spec = runtime::ports::ProcSpec { prog: "env".into(), args, stdin: vec![], deadline };
            proc.run(&spec).map_err(|e| format!("{}: {e}", argv[0])).map(|o| phases = account(phases_of(&o.stderr), &String::from_utf8_lossy(&o.stderr), spira_otel::run::epoch_micros()))
        }
    };
    let elapsed = t0.elapsed();
    let micros = elapsed.as_micros() as u64;
    let timed_out_ms = (r.is_err() && elapsed >= bound && !(argv[0] == "spira-lc" && lc.is_err())).then_some(bound.as_millis() as u64);
    Timing { name: String::new(), micros, millis: micros / 1000, phases, error: r.err(), timed_out_ms }
}

/// `watch.perf.observe`: every probe timed, then the store's caller counts.
pub fn observe(effects: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    effects.watch_track(&spec.input);
    let lc = spira_store::lc_call::LcClient::configured();
    let timings = probes()?.iter().map(|p| Timing { name: p.name.clone(), ..time_one(&lc, &p.argv, &runtime::ports::SysProc, PROBE_DEADLINE) }).collect();
    let callers = lc.as_ref().ok().and_then(|c| c.call(&["stats"]).ok()).map(|r| perf_watch::caller_counts(&r.out)).unwrap_or_default();
    serde_json::to_string(&Snapshot { timings, callers }).map_err(|e| e.to_string())
}

fn append(file: &str, line: &str) -> Result<String, String> {
    let run = Config::pinned()?.run;
    let path = std::path::Path::new(&run).join(file);
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// `watch.perf.log`: the bare alarm, to the `perf-watch` log row's file.
pub fn log(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    append("perf-watch.log", &spec.input)
}

/// `watch.perf.slow_sample`: one row of `run/perf-slow.log`.
pub fn slow_sample(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    append("perf-slow.log", &spec.input)
}

/// `watch.perf.metrics`: every answered probe's time, pushed; a lost push is a gap.
pub fn metrics(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    let timings: Vec<Timing> = serde_json::from_str(&spec.input).map_err(|e| e.to_string())?;
    perf_watch::push_timings(&spira_config::process::metrics_sink("perf-watch"), &timings.iter().map(to_lib).collect::<Vec<_>>());
    Ok(format!("{} probe(s)", timings.len()))
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
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("last_pass"), runtime::rows::Part::Map { name: "alarms", drop: &[] }, runtime::rows::Part::Field("sample_at"), runtime::rows::Part::Map { name: "sample", drop: &[] }, runtime::rows::Part::Field("timed_out"), runtime::rows::Part::Map { name: "published", drop: &[] }];

impl Handler for WatchPerf {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let held = watch_topics::carried(INBOX_TAG, &carried::<Carry>(view, &ev.stream, STATE).published);
            return pattern::tick(ev, OBSERVE, held, OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        if ev.kind.tag == EFFECT_TIMED_OUT {
            return watch_topics::again(view, ev, &carried::<Carry>(view, &ev.stream, STATE).published).map(Command::Effect).into_iter().collect();
        }
        // The push rides as a memory-only probe: a lost one is a gap in the series, and journaling
        // it would cost two events a pass on top of the state the pass must carry.
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
            .into_iter()
            .map(|c| match c {
                Command::Effect(s) if s.name == METRICS => Command::Probe(s),
                c => c,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fails;
    impl runtime::ports::Proc for Fails {
        fn run(&self, _: &runtime::ports::ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            Err("env exceeded its deadline".to_string())
        }
    }

    struct Records(std::sync::Mutex<Vec<runtime::ports::ProcSpec>>);
    impl runtime::ports::Proc for Records {
        fn run(&self, spec: &runtime::ports::ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            self.0.lock().unwrap().push(spec.clone());
            Err("recorded".to_string())
        }
    }

    #[test_id(tla8uk2)]
    #[test]
    fn a_forked_probe_is_given_its_start_stamp() {
        let proc = Records(Default::default());
        time_one(&Err("no store".to_string()), &["work", "list", "--json"].map(String::from), &proc, std::time::Duration::from_secs(1));
        let spec = proc.0.lock().unwrap().remove(0);
        let stamp = spec.args.iter().find_map(|a| a.strip_prefix("PERFWATCH_T0_US=")).expect("a T0 assignment");
        assert!(!stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()), "{stamp}");
        assert!(spec.args.contains(&"PERFWATCH_PHASES=1".to_string()));
    }

    #[test_id(t7xu3i2)]
    #[test]
    fn exit_is_the_time_from_the_probes_end_stamp_to_the_reap() {
        let phases = Some("spawn=100,load=400,config=100,reply=30,client=18,push=2".to_string());
        let got = account(phases.clone(), "PERFWATCH_PHASES=x\nPERFWATCH_END_US=1650000\n", 1_700_000).unwrap();
        assert_eq!(got, "spawn=100,load=400,config=100,reply=30,client=18,push=2,exit=50");
        let t = perf_watch::Timing { name: "work-list".into(), millis: 700, micros: 700_000, phases: Some(got), error: None, timed_out_ms: None };
        assert!(perf_watch::alarms(&[t], 500, &[])[0].contains("rest=0ms"));
        assert_eq!(account(phases.clone(), "no stamp\n", 1_700_000), phases);
    }

    fn probe(deadline: std::time::Duration) -> Timing {
        time_one(&Err("no store".to_string()), &["claim-check".to_string()], &Fails, deadline)
    }

    #[test_id(tmy8m71)]
    #[test]
    fn a_failed_probe_that_reached_its_deadline_is_timed_out_and_stays_an_error() {
        let t = probe(std::time::Duration::ZERO);
        assert_eq!(t.timed_out_ms, Some(0));
        assert!(t.error.is_some());
    }

    #[test_id(t8fnuig)]
    #[test]
    fn a_failed_probe_inside_its_deadline_is_not_timed_out() {
        let t = probe(std::time::Duration::from_secs(3600));
        assert_eq!(t.timed_out_ms, None);
        assert!(t.error.is_some());
    }

    #[test_id(tx36594)]
    #[test]
    fn timed_out_probes_survive_the_carry_rows() {
        let carry = Carry { last_pass: 5, timed_out: ["claim-check".to_string()].into(), ..Carry::default() };
        let body = serde_json::to_string(&carry).unwrap();
        let back = runtime::rows::join(&runtime::rows::split(&body, ROW_PARTS), ROW_PARTS);
        assert_eq!(serde_json::from_str::<Carry>(&back).unwrap().timed_out, carry.timed_out);
    }

    #[test_id(tvmboo0)]
    #[test]
    fn decide_pages_a_timeout_on_the_second_pass_and_not_the_first() {
        let cfg = Config { budget_ms: 500, interval_secs: 0, remind_secs: 1800, inbox: String::new(), metrics_endpoint: String::new(), run: String::new() };
        let snap = Snapshot { timings: vec![Timing { name: "claim-check".into(), micros: 5_000_000, millis: 5000, error: Some("x".into()), timed_out_ms: Some(5000), ..Timing::default() }], callers: BTreeMap::new() };
        let (first, acts) = decide(&Carry::default(), &snap, 1_000_000, &cfg);
        assert!(acts.iter().all(|a| a.name != INBOX_APPEND), "{acts:?}");
        let (_, acts) = decide(&first, &snap, 1_300_000, &cfg);
        assert!(acts.iter().any(|a| a.name == INBOX_APPEND && a.input.contains("PERF TIMEOUT claim-check timed out at 5000ms twice in a row")), "{acts:?}");
    }
}
