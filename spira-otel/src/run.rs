//! The standard run metrics of a command-line component: one invocation counted, its duration
//! and its outcome, labelled by component and operation, pushed once at exit through the
//! fire-and-forget [`MetricsSink`]. `begin` at the top of `main`, `finish` or `exit` at every
//! way out; a component that never reaches either loses its sample, never its work.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::metrics::{MetricsSink, Point};

pub const RUNS: &str = "spira_component_runs";
pub const RUN_SECONDS: &str = "spira_component_run_seconds";
pub const EVENTS: &str = "spira_component_events";
/// What the push may add to the command's own wall time; a slower endpoint loses the sample.
pub const DEADLINE: Duration = Duration::from_millis(250);
/// Set by a caller timing this process (perf-watch): the phases are printed to stderr at exit.
pub const PHASES_ENV: &str = "PERFWATCH_PHASES";
/// Set by perf-watch on the probe: its start in epoch microseconds, so `begin` can record `spawn`, `load` and `config`.
pub const T0_ENV: &str = "PERFWATCH_T0_US";
pub const PHASES_PREFIX: &str = "PERFWATCH_PHASES=";
/// Printed on its own stderr line after the phases: the instant the probe wrote them, epoch microseconds.
pub const END_PREFIX: &str = "PERFWATCH_END_US=";

pub struct Run {
    sink: MetricsSink,
    component: String,
    operation: String,
    started: Instant,
}

impl Run {
    pub fn new(sink: MetricsSink, component: &str) -> Run {
        Run { sink, component: component.into(), operation: "run".into(), started: Instant::now() }
    }

    pub fn operation(&mut self, operation: &str) {
        self.operation = operation.into();
    }

    pub fn finish(&self, code: i32) {
        self.push(code == 0, if code == 0 { String::new() } else { format!("exit_{code}") });
    }

    fn push(&self, ok: bool, class: String) {
        let outcome = if ok { "ok" } else { "error" };
        let pushing = Instant::now();
        self.sink.push(
            &[Point::Count { name: RUNS, value: 1 }, Point::Seconds { name: RUN_SECONDS, value: self.started.elapsed().as_secs_f64() }],
            &[("component", &self.component), ("operation", &self.operation), ("outcome", outcome), ("error_class", &class)],
        );
        write_phases(&mut std::io::stderr(), phases_enabled(), Some(pushing.elapsed().as_millis() as u64), self.started.elapsed().as_millis() as u64, epoch_micros());
    }

    /// One domain event of this component (a calls, a rejection, a rework), `kind` bounded by the caller.
    pub fn event(&self, kind: &str, labels: &[(&str, &str)]) {
        let mut all = vec![("component", self.component.as_str()), ("operation", self.operation.as_str()), ("kind", kind)];
        all.extend_from_slice(labels);
        self.sink.push(&[Point::Count { name: EVENTS, value: 1 }], &all);
    }

    /// A duration this component measured itself (claim-to-submit, a call's wall), seconds.
    pub fn seconds(&self, name: &str, value: f64, labels: &[(&str, &str)]) {
        let mut all = vec![("component", self.component.as_str()), ("operation", self.operation.as_str())];
        all.extend_from_slice(labels);
        self.sink.push(&[Point::Seconds { name, value }], &all);
    }
}

static CURRENT: Mutex<Option<Run>> = Mutex::new(None);
static PHASES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// Records where `millis` of this run went (`reply`, say); named in the phases line a timed run prints.
pub fn phase(name: &str, millis: u64) {
    if let Ok(mut p) = PHASES.lock() {
        p.push((name.to_string(), millis));
    }
}

/// The phases before `begin`, from epoch microseconds: `spawn` (caller's stamp to the kernel's
/// process start), `load` (process start to `main`) and `config` (`main` to now). Without a
/// process start the first two cannot be told apart and are one `prelaunch`.
fn pre_begin_phases(t0_us: u64, proc_start_us: Option<u64>, main_us: u64, now_us: u64) -> Vec<(String, u64)> {
    let ms = |from: u64, to: u64| to.saturating_sub(from) / 1000;
    let config = ("config".to_string(), ms(main_us, now_us));
    match proc_start_us {
        Some(ps) => {
            let ps = ps.max(t0_us);
            vec![("spawn".into(), ms(t0_us, ps)), ("load".into(), ms(ps, main_us)), config]
        }
        None => vec![("prelaunch".into(), ms(t0_us, main_us)), config],
    }
}

/// Phases that happen before `begin`; the time since `begin` leaves them out.
const PRE_BEGIN: [&str; 4] = ["spawn", "load", "config", "prelaunch"];

/// This process's kernel start in epoch microseconds, or `None` when `/proc` cannot say. Boot
/// time is read as the realtime-minus-boottime difference: `/proc/stat` `btime` is whole seconds.
fn kernel_start_us() -> Option<u64> {
    let stat = std::fs::read_to_string(std::path::Path::new(system_paths::PROC_SELF).join("stat")).ok()?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let now = epoch_micros();
    let mut boot = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut boot) } != 0 {
        return None;
    }
    let since_boot_us = u64::try_from(boot.tv_sec).ok()? * 1_000_000 + u64::try_from(boot.tv_nsec).ok()? / 1000;
    proc_start_epoch_us(&stat, now.checked_sub(since_boot_us)?, u64::try_from(ticks).ok()?)
}

fn proc_start_epoch_us(self_stat: &str, boot_epoch_us: u64, ticks_per_sec: u64) -> Option<u64> {
    if ticks_per_sec == 0 {
        return None;
    }
    let after_comm = &self_stat[self_stat.rfind(')')? + 1..];
    let start_ticks: u64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    Some(boot_epoch_us + start_ticks * 1_000_000 / ticks_per_sec)
}

/// Writes the phases line a timed run prints at exit: the pre-`begin` phases and the recorded phases, then
/// `client` (the time since `begin` they and the push leave unaccounted), then `push` when made;
/// a second line stamps the end, so the caller can time the exit.
fn write_phases(out: &mut impl std::io::Write, enabled: bool, push: Option<u64>, since_begin_ms: u64, end_us: u64) {
    if !enabled {
        return;
    }
    let mut phases = PHASES.lock().map(|p| p.clone()).unwrap_or_default();
    let recorded: u64 = phases.iter().filter(|(n, _)| !PRE_BEGIN.contains(&n.as_str())).map(|(_, ms)| ms).sum();
    phases.push(("client".to_string(), since_begin_ms.saturating_sub(recorded).saturating_sub(push.unwrap_or(0))));
    if let Some(ms) = push {
        phases.push(("push".to_string(), ms));
    }
    let _ = writeln!(out, "{PHASES_PREFIX}{}", render_phases(&phases));
    let _ = writeln!(out, "{END_PREFIX}{end_us}");
}

pub fn epoch_micros() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

fn phases_enabled() -> bool {
    std::env::var_os(PHASES_ENV).is_some()
}

pub fn render_phases(phases: &[(String, u64)]) -> String {
    phases.iter().map(|(n, ms)| format!("{n}={ms}")).collect::<Vec<_>>().join(",")
}

fn with(f: impl FnOnce(&mut Run)) {
    if let Ok(mut g) = CURRENT.lock() {
        if let Some(r) = g.as_mut() {
            f(r);
        }
    }
}

extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
}

/// A process that leaves through a deep `process::exit` never reaches its `finish`; this pushes
/// that run as an error of class `exit_unrecorded`.
extern "C" fn at_exit() {
    let run = CURRENT.lock().ok().and_then(|mut g| g.take());
    if let Some(r) = run {
        r.push(false, "exit_unrecorded".into());
    }
}

/// Starts this process's run for `component`; `endpoint` is `SPIRA_METRICS_ENDPOINT` as the
/// caller resolved it, and `None` disables the push.
pub fn begin(component: &str, endpoint: Option<&str>) {
    begin_with(component, || endpoint.map(str::to_string));
}

/// `begin` for a caller that resolves the endpoint itself: `endpoint` runs after `main`'s entry
/// is stamped, so the time it takes is the `config` phase.
pub fn begin_with(component: &str, endpoint: impl FnOnce() -> Option<String>) {
    let main_us = epoch_micros();
    static ARMED: std::sync::Once = std::sync::Once::new();
    ARMED.call_once(|| {
        unsafe { atexit(at_exit) };
    });
    let endpoint = endpoint();
    let sink = MetricsSink::new(endpoint.as_deref(), &format!("spira-{component}"), DEADLINE);
    if phases_enabled() {
        let t0 = std::env::var(T0_ENV).ok().and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(t0) = t0 {
            for (n, ms) in pre_begin_phases(t0, kernel_start_us(), main_us, epoch_micros()) {
                phase(&n, ms);
            }
        }
    }
    if let Ok(mut g) = CURRENT.lock() {
        *g = Some(Run::new(sink, component));
    }
}

pub fn operation(operation: &str) {
    with(|r| r.operation(operation));
}

pub fn event(kind: &str, labels: &[(&str, &str)]) {
    with(|r| r.event(kind, labels));
}

pub fn seconds(name: &str, value: f64, labels: &[(&str, &str)]) {
    with(|r| r.seconds(name, value, labels));
}

/// Ends this run without pushing anything: a read on a critical path that pays no network call
/// when it succeeds. A failing run is still `finish`ed.
pub fn discard() {
    discard_to(&mut std::io::stderr(), phases_enabled(), epoch_micros());
}

fn discard_to(out: &mut impl std::io::Write, phases_enabled: bool, end_us: u64) {
    let run = CURRENT.lock().ok().and_then(|mut g| g.take());
    let since_begin = run.map_or(0, |r| r.started.elapsed().as_millis() as u64);
    write_phases(out, phases_enabled, None, since_begin, end_us);
}

/// Pushes the run's standard metrics; a second call, or one with no `begin`, pushes nothing.
pub fn finish(code: i32) {
    let run = CURRENT.lock().ok().and_then(|mut g| g.take());
    if let Some(r) = run {
        r.finish(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve() -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = c.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                if buf.len() >= i + 4 + len {
                    break;
                }
            }
            c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (addr, h)
    }

    #[test_id(t4bmeh0)]
    #[test]
    fn a_run_pushes_its_count_duration_and_outcome_under_its_labels() {
        let (addr, h) = serve();
        let mut run = Run::new(MetricsSink::new(Some(&addr), "spira-x", DEADLINE), "x");
        run.operation("pass");
        run.finish(3);
        let body = h.join().unwrap();
        for want in [RUNS, RUN_SECONDS, "component", "operation", "pass", "outcome", "error", "exit_3"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test_id(tjg3133)]
    #[test]
    fn an_unfinished_run_is_pushed_as_an_unrecorded_exit() {
        let (addr, h) = serve();
        Run::new(MetricsSink::new(Some(&addr), "spira-x", DEADLINE), "x").push(false, "exit_unrecorded".into());
        let body = h.join().unwrap();
        assert!(body.contains("exit_unrecorded") && body.contains("error"));
    }

    #[test_id(tm4s0k8)]
    #[test]
    fn the_exit_time_push_deadline_is_within_the_hot_read_budget() {
        assert!(DEADLINE <= Duration::from_millis(250));
    }

    #[test_id(tj9wik5)]
    #[test]
    fn phases_render_in_the_order_they_were_recorded() {
        assert_eq!(render_phases(&[("reply".into(), 12), ("push".into(), 250)]), "reply=12,push=250");
    }

    #[test_id(tfwywcq)]
    #[test]
    fn a_discarded_run_still_reports_its_recorded_phases_without_a_push_entry() {
        let _serial = PHASES_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        *PHASES.lock().unwrap() = vec![("store".into(), 30)];
        discard_to(&mut out, true, 7);
        assert_eq!(String::from_utf8(out).unwrap(), "PERFWATCH_PHASES=store=30,client=0\nPERFWATCH_END_US=7\n");
        let mut out = Vec::new();
        write_phases(&mut out, true, Some(7), 0, 9);
        assert_eq!(String::from_utf8(out).unwrap(), "PERFWATCH_PHASES=store=30,client=0,push=7\nPERFWATCH_END_US=9\n");
        PHASES.lock().unwrap().clear();
    }

    static PHASES_TESTS: Mutex<()> = Mutex::new(());

    fn line(start: Option<(u64, Option<u64>, u64, u64)>, recorded: &[(&str, u64)], push: u64, since_begin: u64) -> String {
        let _serial = PHASES_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *PHASES.lock().unwrap() = Vec::new();
        if let Some((t0, ps, main, now)) = start {
            for (n, ms) in pre_begin_phases(t0, ps, main, now) {
                phase(&n, ms);
            }
        }
        for (n, ms) in recorded {
            phase(n, *ms);
        }
        let mut out = Vec::new();
        write_phases(&mut out, true, Some(push), since_begin, 1);
        PHASES.lock().unwrap().clear();
        String::from_utf8(out).unwrap()
    }

    #[test_id(tzw59k5)]
    #[test]
    fn the_line_accounts_for_start_and_the_clients_own_time() {
        assert_eq!(line(Some((1_000_000, Some(1_100_000), 1_600_000, 1_610_000)), &[("reply", 30)], 2, 50), "PERFWATCH_PHASES=spawn=100,load=500,config=10,reply=30,client=18,push=2\nPERFWATCH_END_US=1\n");
    }

    #[test_id(tv1vdu5)]
    #[test]
    fn the_pre_begin_time_is_split_into_spawn_load_and_config() {
        let got = line(Some((1_000_000, Some(1_100_000), 1_600_000, 1_610_000)), &[], 0, 10);
        assert!(got.starts_with("PERFWATCH_PHASES=spawn=100,load=500,config=10,"), "{got}");
    }

    #[test_id(tse40yq)]
    #[test]
    fn without_a_kernel_start_the_pre_begin_time_is_prelaunch_and_config() {
        let got = line(Some((1_000_000, None, 1_600_000, 1_610_000)), &[], 0, 10);
        assert!(got.starts_with("PERFWATCH_PHASES=prelaunch=600,config=10,"), "{got}");
    }

    #[test_id(t708j7m)]
    #[test]
    fn the_kernel_start_is_boot_time_plus_the_starttime_ticks_past_a_command_with_parens() {
        let stat = "42 (a) b (c)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 1500 20 21";
        assert_eq!(proc_start_epoch_us(stat, 1_000_000_000 + 250_000, 100), Some(1_000_000_000 + 250_000 + 15_000_000), "a fractional boot time is kept");
        assert_eq!(proc_start_epoch_us("garbage", 1, 100), None);
        assert_eq!(proc_start_epoch_us(stat, 1, 0), None);
    }

    #[test_id(t81m3xh)]
    #[test]
    fn without_a_stamp_the_line_has_no_start_and_still_has_client() {
        assert_eq!(line(None, &[("reply", 30)], 2, 50), "PERFWATCH_PHASES=reply=30,client=18,push=2\nPERFWATCH_END_US=1\n");
    }

    #[test_id(t28b68b)]
    #[test]
    fn client_saturates_at_zero_when_the_phases_exceed_the_elapsed_time() {
        assert_eq!(line(None, &[("reply", 80)], 2, 50), "PERFWATCH_PHASES=reply=80,client=0,push=2\nPERFWATCH_END_US=1\n");
    }

    #[test_id(t7l9xot)]
    #[test]
    fn a_discarded_run_prints_nothing_with_phases_unset() {
        let mut out = Vec::new();
        discard_to(&mut out, false, 7);
        assert!(out.is_empty());
    }

    #[test_id(t3cmerb)]
    #[test]
    fn a_discarded_run_pushes_nothing_not_even_as_an_unrecorded_exit() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        begin("x", Some(&l.local_addr().unwrap().to_string()));
        discard();
        finish(0);
        at_exit();
        assert!(l.accept().is_err(), "no connection was made to the metrics endpoint");
    }

    #[test_id(tzyo20q)]
    #[test]
    fn finish_without_a_begin_pushes_nothing_and_does_not_panic() {
        finish(0);
    }
}
