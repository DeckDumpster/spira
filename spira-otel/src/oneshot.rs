//! The standard run metrics of a one-shot component: one run, its duration and its outcome, pushed
//! when the process ends. `begin` at the top of `main`, leave through `exit` (or `end` before a
//! plain return); a component-specific count goes out through `event`.

use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::metrics::{MetricsSink, Point};

const DEADLINE: Duration = Duration::from_millis(250);

struct Active {
    sink: MetricsSink,
    component: String,
    operation: String,
    started: Instant,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

fn active() -> std::sync::MutexGuard<'static, Option<Active>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the run; `endpoint` is the resolved metrics endpoint, `None` or empty pushes nothing.
pub fn begin(endpoint: Option<&str>, component: &str, operation: &str) {
    *active() = Some(Active { sink: MetricsSink::new(endpoint, component, DEADLINE), component: component.into(), operation: operation.into(), started: Instant::now() });
}

/// Names the operation once it is known (the subcommand is parsed after `begin`).
pub fn operation(operation: &str) {
    if let Some(a) = active().as_mut() {
        a.operation = operation.into();
    }
}

/// Pushes `name` = `value` labelled with the run's component and operation and `labels`.
pub fn event(name: &str, value: u64, labels: &[(&str, &str)]) {
    let g = active();
    let Some(a) = g.as_ref() else { return };
    let mut all: Vec<(&str, &str)> = vec![("component", &a.component), ("operation", &a.operation)];
    all.extend_from_slice(labels);
    a.sink.push(&[Point::Count { name, value }], &all);
}

/// Ends the run and pushes its invocation, duration and outcome. A run ends once.
pub fn end(code: i32) {
    push_end(if code == 0 { ("ok", String::new()) } else { ("error", format!("exit_{code}")) });
}

/// `end` for a `main` that returns an `ExitCode`, which has no accessor and no `PartialEq`: ok or `exit_nonzero`.
pub fn finish(code: ExitCode) -> ExitCode {
    push_end(if format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS) { ("ok", String::new()) } else { ("error", "exit_nonzero".into()) });
    code
}

fn push_end((outcome, class): (&'static str, String)) {
    let Some(a) = active().take() else { return };
    let labels = [("component", a.component.as_str()), ("operation", a.operation.as_str()), ("outcome", outcome), ("error_class", class.as_str())];
    a.sink.push(
        &[Point::Count { name: "spira_component_invocations", value: 1 }, Point::Seconds { name: "spira_component_duration_seconds", value: a.started.elapsed().as_secs_f64() }],
        &labels,
    );
}

/// `end`, flush the std streams, then exit the process with `code` (the one exit boundary for binaries whose helpers exit mid-run).
pub fn exit(code: i32) -> ! {
    end(code);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: _exit only terminates the process; both std streams are flushed above.
    unsafe { libc::_exit(code) }
}
