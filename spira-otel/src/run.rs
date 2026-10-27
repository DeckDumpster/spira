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
pub const PHASES_PREFIX: &str = "PERFWATCH_PHASES=";

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
        if std::env::var_os(PHASES_ENV).is_some() {
            let mut phases = PHASES.lock().map(|p| p.clone()).unwrap_or_default();
            phases.push(("push".to_string(), pushing.elapsed().as_millis() as u64));
            eprintln!("{PHASES_PREFIX}{}", render_phases(&phases));
        }
    }

    /// One domain event of this component (a summons, a rejection, a rework), `kind` bounded by the caller.
    pub fn event(&self, kind: &str, labels: &[(&str, &str)]) {
        let mut all = vec![("component", self.component.as_str()), ("operation", self.operation.as_str()), ("kind", kind)];
        all.extend_from_slice(labels);
        self.sink.push(&[Point::Count { name: EVENTS, value: 1 }], &all);
    }

    /// A duration this component measured itself (claim-to-submit, a summon's wall), seconds.
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
    static ARMED: std::sync::Once = std::sync::Once::new();
    ARMED.call_once(|| {
        unsafe { atexit(at_exit) };
    });
    let sink = MetricsSink::new(endpoint, &format!("spira-{component}"), DEADLINE);
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

    #[test]
    fn an_unfinished_run_is_pushed_as_an_unrecorded_exit() {
        let (addr, h) = serve();
        Run::new(MetricsSink::new(Some(&addr), "spira-x", DEADLINE), "x").push(false, "exit_unrecorded".into());
        let body = h.join().unwrap();
        assert!(body.contains("exit_unrecorded") && body.contains("error"));
    }

    #[test]
    fn a_push_to_an_endpoint_that_never_answers_ends_at_the_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let t0 = Instant::now();
        let run = Run::new(MetricsSink::new(Some(&addr), "spira-x", DEADLINE), "x");
        run.finish(0);
        assert!(t0.elapsed() < Duration::from_secs(1), "a stalled endpoint held the command for {:?}", t0.elapsed());
        drop(l);
    }

    #[test]
    fn phases_render_in_the_order_they_were_recorded() {
        assert_eq!(render_phases(&[("reply".into(), 12), ("push".into(), 250)]), "reply=12,push=250");
    }

    #[test]
    fn finish_without_a_begin_pushes_nothing_and_does_not_panic() {
        finish(0);
    }
}
