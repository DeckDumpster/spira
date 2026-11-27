use super::*;

#[test_id(tdav8ge)]
#[test]
fn a_stopping_server_answers_what_is_queued_and_removes_its_path() {
    use super::accept_until_stopped;
    use std::io::BufReader;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    let dir = testkit::TempDir::new("lc-drain");
    let path = dir.join("sock");
    let path_str = path.to_str().unwrap().to_string();
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let mut first = UnixStream::connect(&path).unwrap();
    std::io::Write::write_all(&mut first, b"x\n").unwrap();
    let (began_tx, began) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let began_tx = std::sync::Mutex::new(began_tx);
    let mut queued = None;
    std::thread::scope(|scope| {
        let stopping = &stopping;
        let queued = &mut queued;
        let path = &path;
        scope.spawn(move || {
            began.recv().unwrap();
            stopping.store(true, Ordering::SeqCst);
            let mut late = UnixStream::connect(path).unwrap();
            std::io::Write::write_all(&mut late, b"y\n").unwrap();
            *queued = Some(late);
            release.send(()).unwrap();
        });
        let (released, began_tx) = (&released, &began_tx);
        accept_until_stopped(&listener, &path_str, scope, stopping, move |stream: UnixStream| {
            let _ = began_tx.lock().unwrap().send(());
            let _ = released.lock().unwrap().recv();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut BufReader::new(&stream), &mut line).unwrap();
            std::io::Write::write_all(&mut &stream, b"served\n").unwrap();
        });
    });
    let mut line = String::new();
    std::io::BufRead::read_line(&mut BufReader::new(&first), &mut line).unwrap();
    assert_eq!(line, "served\n", "the request taken before the stop is answered");
    let late = queued.expect("a connection was made after the flag was set");
    let mut line = String::new();
    std::io::BufRead::read_line(&mut BufReader::new(&late), &mut line).unwrap();
    assert_eq!(line, "served\n", "a connection queued at the stop is answered");
    assert_eq!(UnixStream::connect(&path).unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test_id(thcqxxx)]
#[test]
fn the_drain_bound_leaves_the_unit_its_stop_timeout() {
    assert_eq!(super::DRAIN_BOUND, std::time::Duration::from_secs(60));
    for unit in ["lc-serve.service", "spira-lc.service"] {
        let text = std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join("systemd").join(unit)).unwrap();
        let stop: u64 = text.lines().find_map(|l| l.strip_prefix("TimeoutStopSec=")).unwrap().parse().unwrap();
        assert!(std::time::Duration::from_secs(stop) > super::DRAIN_BOUND + std::time::Duration::from_secs(15), "{unit}: TimeoutStopSec={stop}");
    }
}

/// The one handler thread of a test is the only user of the world while the scope runs.
struct Lent<'a>(&'a World);

unsafe impl Sync for Lent<'_> {}

unsafe impl Send for Lent<'_> {}

/// `argv` through the server's real reader, answer and reply on a connection that
/// `accept_until_stopped` took, with the stop flag set while the handler has the request.
fn stopped_during(w: &World, argv: &[&str], stop: &std::sync::atomic::AtomicBool) -> (i32, String, String) {
    use std::os::unix::net::UnixListener;
    let dir = testkit::TempDir::new("lc-drain-hold");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
    let asked = std::thread::spawn(move || crate::client::exchange(client, &argv, None));
    let flights = super::Flights::default();
    let lent = Lent(w);
    std::thread::scope(|scope| {
        let (lent, flights) = (&lent, &flights);
        super::accept_until_stopped(&listener, path.to_str().unwrap(), scope, stop, move |stream| {
            let (w, content) = (lent.0, Content::default());
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let (mut m, mut b) = (w, w);
            let ans = super::answer(req, &mut m, &mut b, &content, &|| Err("no run dir here".to_string()));
            flights.serve(stream.try_clone().unwrap(), |_| {
                super::reply_then_settle(&stream, &ans, false, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
            });
        });
    });
    asked.join().unwrap()
}

#[test_id(tcynfo3)]
#[test]
fn a_hold_taken_before_the_stop_is_answered_and_durable() {
    let w = World::new();
    filed(&w, "sp-a");
    let stop = std::sync::atomic::AtomicBool::new(false);
    let got = stopped_during(&w, &["hold", "sp-a", "wait", "--reason", "x"], &stop);
    assert_eq!(got.0, 0, "{got:?}");
    assert_eq!(over(&w, &Content::default(), &["held", "sp-a", "wait"], None).0, 0);
}

/// `argv` on a connection taken by an accept loop on its own thread, `handle` serving it as
/// one of `flights`; a handler that never ends leaves the test free to fail.
struct Serving {
    flights: std::sync::Arc<super::Flights>,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    finished: std::sync::mpsc::Receiver<()>,
    asked: std::thread::JoinHandle<(i32, String, String)>,
    _dir: testkit::TempDir,
}

fn serving(argv: &[&str], handle: impl Fn(std::os::unix::net::UnixStream) + Send + Sync + 'static) -> Serving {
    use std::os::unix::net::{UnixListener, UnixStream};
    let dir = testkit::TempDir::new("lc-drain-serving");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let asked = std::thread::spawn(move || crate::client::exchange(client, &argv, None));
    let flights = std::sync::Arc::new(super::Flights::default());
    let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done, finished) = std::sync::mpsc::channel::<()>();
    {
        let (flights, stopping) = (flights.clone(), stopping.clone());
        std::thread::spawn(move || {
            std::thread::scope(|scope| super::accept_until_stopped(&listener, path.to_str().unwrap(), scope, &stopping, |stream| flights.serve(stream, &handle)));
            let _ = done.send(());
        });
    }
    Serving { flights, stopping, finished, asked, _dir: dir }
}

#[test_id(tl1081l)]
#[test]
fn a_handler_that_never_finishes_is_answered_by_name_at_the_drain_bound() {
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Mutex};
    let (hold, held) = mpsc::channel::<()>();
    let held = Mutex::new(held);
    let (began_tx, began) = mpsc::channel::<()>();
    let began_tx = Mutex::new(began_tx);
    let Serving { flights, stopping, finished, asked, _dir } = serving(&["held"], move |stream| {
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        super::read_request(&mut reader).unwrap();
        began_tx.lock().unwrap().send(()).unwrap();
        let _ = held.lock().unwrap().recv();
    });
    began.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    stopping.store(true, Ordering::SeqCst);
    let (verdict_tx, verdict) = mpsc::channel();
    let drained = std::thread::spawn(move || {
        let r = super::drain(&flights, &stopping, &finished, std::time::Duration::from_millis(200), || {});
        verdict_tx.send(r).unwrap();
    });
    let (n, m) = verdict.recv_timeout(std::time::Duration::from_secs(5)).expect("the drain returns instead of joining a handler that never finishes");
    assert_eq!((n, m), (1, 1));
    let (code, out, _) = asked.join().unwrap();
    assert_eq!(code, 2);
    assert!(out.contains("stopped for a restart"), "{out}");
    hold.send(()).unwrap();
    drained.join().unwrap();
}

#[test_id(txsus4t)]
#[test]
fn sigterm_sets_the_stop_flag_instead_of_ending_the_process() {
    extern "C" {
        fn raise(sig: i32) -> i32;
    }
    super::stop_on_sigterm();
    assert!(!super::STOPPING.load(std::sync::atomic::Ordering::SeqCst));
    // SAFETY: the handler installed above only stores to an atomic.
    unsafe { raise(15) };
    assert!(super::STOPPING.load(std::sync::atomic::Ordering::SeqCst));
}

#[test_id(ttfbbd2)]
#[test]
fn the_drain_calls_on_stop_once_before_it_waits_for_the_connections() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    let (done, finished) = mpsc::channel::<()>();
    let calls = Arc::new(AtomicUsize::new(0));
    let (verdict_tx, verdict) = mpsc::channel();
    let counted = calls.clone();
    std::thread::spawn(move || {
        let flights = super::Flights::default();
        let stopping = AtomicBool::new(true);
        let r = super::drain(&flights, &stopping, &finished, super::DRAIN_BOUND, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            done.send(()).unwrap();
        });
        let _ = verdict_tx.send(r);
    });
    let got = verdict.recv_timeout(std::time::Duration::from_secs(1)).expect("on_stop runs before the wait it ends");
    assert_eq!(got, (0, 0));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test_id(tsuq0xl)]
#[test]
fn a_held_watch_is_answered_when_the_server_is_told_to_stop() {
    use crate::control::store::Store;
    use runtime::ports::VirtualClock;
    use state_store::conformance::Interpose;
    use state_store::{Config, Contract, MemBackend, Ports};
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc};
    let contract = Contract::open(Interpose::new(MemBackend::new()), Ports::virtual_clock(&VirtualClock::new(1_000_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let store = Arc::new(Store::new(contract, 1_000));
    let head = store.contract().head().unwrap().to_string();
    let (waiting_tx, waiting) = mpsc::channel();
    store.contract().backend().after_nth_log(2, move || {
        let _ = waiting_tx.send(());
    });
    let served = store.clone();
    let Serving { flights, stopping, finished, asked, _dir } = serving(&["watch", "lifecycle/", "--from-rev", &head, "--wait-ms", "30000"], move |stream| {
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
        let (code, out) = crate::control::verbs::run(&served, &req.argv);
        super::write_raw(&stream, code, &out, "", false).unwrap();
    });
    waiting.recv_timeout(std::time::Duration::from_secs(5)).expect("the watch is held in its wait");
    stopping.store(true, Ordering::SeqCst);
    let (verdict_tx, verdict) = mpsc::channel();
    std::thread::spawn(move || {
        let r = super::drain(&flights, &stopping, &finished, super::DRAIN_BOUND, || super::end_watches(Some(Ok(store))));
        let _ = verdict_tx.send(r);
    });
    let got = verdict.recv_timeout(std::time::Duration::from_secs(1)).expect("the drain does not wait out the watch's hold");
    assert_eq!(got, (1, 0));
    let (code, out, _) = asked.join().unwrap();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("progress_rev"), "{out}");
}

#[test_id(t5ggz4r)]
#[test]
fn the_stop_lines_are_stamped_and_carry_the_drain_milliseconds() {
    assert_eq!(super::stopping_line(2, 0), format!("spira-lc serve: {} stopping: 2 in flight", spira_lc::sections::rfc3339(0)));
    let took = std::time::Duration::from_millis(37);
    assert_eq!(super::stopped_line(2, took, 1, 0), format!("spira-lc serve: {} stopped after draining 2 in 37 ms, answered 1 at the bound", spira_lc::sections::rfc3339(0)));
    assert!(spira_lc::sections::rfc3339(0).starts_with("1970-01-01T00:00:00"));
}
