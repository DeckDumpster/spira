//! The real `unit.ran` source (`tasks::g::unit_ran`): every run the unit manager reports ended is
//! submitted once, through `runtime::sources::UnitRanSource`, on the stream of the task its unit
//! name names ([`stream_of`]), or on `units` for a unit no booted task owns. On Linux the
//! manager is the systemd user manager over D-Bus; elsewhere it is the child-process backend,
//! which only knows the runs it started, so it must be the same instance the `unit.*` effects
//! run on. Replaces `unit-run watch` (`spira-unit-run-dbus-prod.service`).

use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::ports::{Clock, WallClock};
use runtime::sources::{Source, UnitRanSource};
use runtime::units::UnitCompletions;
use tokio::sync::mpsc;

use crate::tasks::g::unit_ran::NAME;

/// How often the source drains the manager's ended runs.
pub const DRAIN_EVERY: Duration = Duration::from_millis(500);

/// The source over `completions`: the instance the host's `unit.*` effects run on, so a
/// transient unit's captured output and every signal its connection subscribed to are drained
/// by the one reader. This is the form `main.rs` registers.
pub fn over(completions: Arc<dyn UnitCompletions>, clock: Arc<dyn Clock>) -> UnitRanSource {
    UnitRanSource::polled(NAME, completions, clock, DRAIN_EVERY).routed(stream_of)
}

/// The same source over the simulator's units, for a virtual-time driver: routed alike.
pub fn scripted(units: Arc<runtime::units::ScriptedUnits>, clock: Arc<dyn Clock>) -> UnitRanSource {
    UnitRanSource::scripted(NAME, units, clock).routed(stream_of)
}

/// The stream of the booted task named `task` (`tasks::all()`), where its `unit.ran` lands so
/// the commands its handler answers with land on its own stream too.
pub fn stream_of(task: &str) -> Option<String> {
    crate::tasks::all().find(|t| t.name == task).map(|t| t.stream().to_string())
}

/// The standalone D-Bus source: its own connection to the user manager. Returns at once when
/// `tx` is already closed, without connecting; a manager it cannot reach is said once and the
/// source ends (the host's status shows it gone).
pub fn run_dbus(tx: mpsc::Sender<Submit>) {
    if tx.is_closed() {
        return;
    }
    let clock: Arc<dyn Clock> = Arc::new(WallClock);
    match connect(clock.clone()) {
        Ok(c) => Box::new(over(c, clock)).run(tx),
        Err(e) => eprintln!("spira-runtime: source {NAME}: {e}"),
    }
}

#[cfg(target_os = "linux")]
fn connect(clock: Arc<dyn Clock>) -> Result<Arc<dyn UnitCompletions>, String> {
    Ok(Arc::new(runtime::units::SystemdUnits::connect(clock).map_err(|e| format!("user manager: {e}"))?))
}

#[cfg(not(target_os = "linux"))]
fn connect(_clock: Arc<dyn Clock>) -> Result<Arc<dyn UnitCompletions>, String> {
    Err("no systemd here: register the source over the host's ProcessUnits with sources::units::over".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::units::{Exit, ScriptedUnits, UnitHandle, UnitRan, Units, UnitSpec, UNIT_RAN};

    #[test]
    fn a_run_the_manager_ends_is_submitted_once_on_its_tasks_stream_or_on_units() {
        let task = crate::tasks::all().find(|t| t.name != "heartbeat").expect("a booted task");
        assert_eq!(stream_of(task.name).as_deref(), Some(task.stream()));
        assert_eq!(stream_of("planted.no_such_task"), None);
        let clock = Arc::new(VirtualClock::new(0));
        let units = Arc::new(ScriptedUnits::new(clock.clone()));
        units.script("planted.no_such_task", 1_000, Exit::Code(0));
        units.start(&UnitSpec { task: "planted.no_such_task".into(), key: "r1".into(), argv: vec!["true".into()], env: vec![], cwd: None }).unwrap();
        clock.advance(2_000);
        let (tx, mut rx) = mpsc::channel(4);
        let source = over(units.clone(), clock.clone());
        let h = std::thread::spawn(move || Box::new(source).run(tx));
        let got = rx.blocking_recv().expect("the run's end");
        drop(rx);
        h.join().unwrap();
        assert_eq!((got.stream.as_str(), got.kind.tag.as_str()), (runtime::sources::UNITS_STREAM, UNIT_RAN));
        let ran = UnitRan::from_event_kind(&got.kind).unwrap();
        assert_eq!(ran.handle, UnitHandle::of("planted.no_such_task", "r1").unwrap());
        assert_eq!(ran.key_for("planted.no_such_task").as_deref(), Some("r1"));
    }
}
