//! The real `unit.ran` source on a systemd backend waits on the bus socket: an end that reaches
//! the socket is submitted with no bus call from anyone, and one a concurrent call already pulled
//! off the socket is submitted too, each once. `every` is an hour, so only a wake can deliver.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{Sources, UnitRanSource, UNITS_STREAM};
use runtime::units::{encode_args, Arg};
use runtime::units::{SystemdUnits, UnitHandle, UnitRan, Units};
use tokio::sync::mpsc;
use unit_run::dbus::{decode, encode_empty_return, encode_signal, message_len, Conn, METHOD_CALL};

const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_IFACE: &str = "org.freedesktop.systemd1.Manager";

fn job_removed(unit: &str) -> Vec<u8> {
    let (sig, body) = encode_args(&[Arg::U32(5), Arg::S("/org/freedesktop/systemd1/job/5".into()), Arg::S(unit.into()), Arg::S("dependency".into())]);
    encode_signal(2, MANAGER_PATH, MANAGER_IFACE, "JobRemoved", &sig, &body)
}

/// The bus: answers every call with an empty return, and answers `ResetFailedUnit` with a
/// `JobRemoved` for `pulled.service` first, so the caller's read pulls the signal off the socket.
fn serve(mut bus: UnixStream) {
    std::thread::spawn(move || loop {
        let mut head = [0u8; 16];
        if bus.read_exact(&mut head).is_err() {
            return;
        }
        let total = message_len(&head).unwrap().unwrap();
        let mut rest = vec![0u8; total - 16];
        bus.read_exact(&mut rest).unwrap();
        let m = decode(&[head.to_vec(), rest].concat()).unwrap();
        if m.kind != METHOD_CALL {
            continue;
        }
        let mut out = Vec::new();
        if m.member.as_deref() == Some("ResetFailedUnit") {
            out.extend(job_removed("pulled.service"));
        }
        out.extend(encode_empty_return(3, m.serial));
        bus.write_all(&out).unwrap();
    });
}

async fn next(rx: &mut mpsc::Receiver<Submit>) -> UnitRan {
    let s = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.expect("an end arrives").expect("the source is running"); // clock-safe: caps a wait for a named signal
    assert_eq!(s.stream, UNITS_STREAM);
    UnitRan::from_event_kind(&s.kind).expect("a unit.ran")
}

#[test_id(tpb2t5n)]
#[tokio::test]
async fn an_end_on_the_bus_socket_is_submitted_without_a_bus_call_and_one_a_call_pulled_is_too() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut wire = theirs.try_clone().unwrap();
    serve(theirs);
    let clock: Arc<dyn Clock> = Arc::new(VirtualClock::new(0));
    let units = Arc::new(SystemdUnits::new(Conn::over(ours), clock.clone()).unwrap());
    let (tx, mut rx) = mpsc::channel(8);
    let mut sources = Sources::default();
    sources.register(Box::new(UnitRanSource::polled("unit.ran", units.clone(), clock, Duration::from_secs(3600)))).unwrap();
    let handles = sources.start(tx);

    wire.write_all(&job_removed("written.service")).unwrap();
    assert_eq!(next(&mut rx).await.handle, UnitHandle::named("written.service"));

    let caller = units.clone();
    tokio::task::spawn_blocking(move || caller.reset_failed(&UnitHandle::named("any.service")).unwrap()).await.unwrap();
    assert_eq!(next(&mut rx).await.handle, UnitHandle::named("pulled.service"));

    wire.write_all(&job_removed("last.service")).unwrap();
    assert_eq!(next(&mut rx).await.handle, UnitHandle::named("last.service"), "nothing was submitted twice in between");

    drop(rx);
    for h in handles {
        h.await.unwrap();
    }
}

fn thread_comms() -> Vec<String> {
    let dir = format!("{}/task", system_paths::PROC_SELF);
    std::fs::read_dir(dir).unwrap().filter_map(|t| std::fs::read_to_string(t.unwrap().path().join("comm")).ok()).map(|c| c.trim().to_string()).collect()
}


#[test_id(t1bp2x0)]
#[tokio::test]
async fn the_source_runs_on_the_scheduler_and_no_thread_carries_its_name() {
    const COMM: &str = "source-unit.ran";
    let (release, held) = std::sync::mpsc::channel::<()>();
    let planted = std::thread::Builder::new().name(COMM.into()).spawn(move || held.recv().unwrap()).unwrap();
    while !thread_comms().iter().any(|c| c == COMM) {
        std::thread::yield_now();
    }
    release.send(()).unwrap();
    planted.join().unwrap();
    // join returns before the kernel unhashes the task from /proc/self/task
    while thread_comms().iter().any(|c| c == COMM) {
        std::thread::yield_now();
    }

    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut wire = theirs.try_clone().unwrap();
    serve(theirs);
    let clock: Arc<dyn Clock> = Arc::new(VirtualClock::new(0));
    let units = Arc::new(SystemdUnits::new(Conn::over(ours), clock.clone()).unwrap());
    let (tx, mut rx) = mpsc::channel(8);
    let mut sources = Sources::default();
    sources.register(Box::new(UnitRanSource::polled("unit.ran", units, clock, Duration::from_secs(3600)))).unwrap();
    let handles = sources.start(tx);
    wire.write_all(&job_removed("up.service")).unwrap();
    next(&mut rx).await;
    assert!(!thread_comms().iter().any(|c| c == COMM), "{:?}", thread_comms());
    drop(rx);
    for h in handles {
        h.await.unwrap();
    }
}

/// A backend with no wake whose first drain fails and whose later ones report one end.
struct Flaky {
    calls: std::sync::atomic::AtomicUsize,
    wake_errors: bool,
}

impl runtime::units::UnitCompletions for Flaky {
    fn ran(&self) -> runtime::Result<Vec<UnitRan>> {
        Ok(vec![])
    }

    fn ended(&self) -> runtime::Result<Vec<(UnitRan, runtime::units::RanOutput)>> {
        match self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => Err(runtime::RuntimeError::new("planted fault")),
            1 => Ok(vec![(UnitRan { handle: UnitHandle::named("after.service"), exit: runtime::units::Exit::Code(0), duration_ms: 0 }, Default::default())]),
            _ => Ok(vec![]),
        }
    }

    fn wake(&self) -> runtime::Result<Option<runtime::units::Wake>> {
        if self.wake_errors {
            Err(runtime::RuntimeError::new("planted: no wake"))
        } else {
            Ok(None)
        }
    }
}

async fn failed_drain_is_retried_and_each_end_arrives_once(wake_errors: bool) {
    let clock: Arc<dyn Clock> = Arc::new(VirtualClock::new(0));
    let backend = Arc::new(Flaky { calls: Default::default(), wake_errors });
    let (tx, mut rx) = mpsc::channel(8);
    let mut sources = Sources::default();
    sources.register(Box::new(UnitRanSource::polled("unit.ran", backend, clock, Duration::from_millis(5)))).unwrap();
    let handles = sources.start(tx);
    assert_eq!(next(&mut rx).await.handle, UnitHandle::named("after.service"));
    drop(rx);
    for h in handles {
        h.await.unwrap();
    }
}


#[test_id(ta6voxj)]
#[tokio::test]
async fn a_backend_with_no_wake_is_drained_every_interval_and_a_failed_drain_is_retried() {
    failed_drain_is_retried_and_each_end_arrives_once(false).await;
}


#[test_id(tcrwlrk)]
#[tokio::test]
async fn a_wake_that_cannot_be_had_falls_back_to_draining_every_interval() {
    failed_drain_is_retried_and_each_end_arrives_once(true).await;
}
