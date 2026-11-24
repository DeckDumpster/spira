use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};

fn ask(path: &std::path::Path, line: &str) -> String {
    let mut s = UnixStream::connect(path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    writeln!(s, "{line}").unwrap();
    let mut out = String::new();
    BufReader::new(&s).read_line(&mut out).unwrap();
    out
}

fn reply(stream: &UnixStream, text: &str) {
    let _ = writeln!(&mut &*stream, "{text}");
}

#[test_id(tw4k9mz)]
#[test]
fn sequential_requests_are_served_by_one_worker_thread() {
    let dir = testkit::TempDir::new("lc-workers");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let idle = Arc::new(AtomicUsize::new(0));
    let mut started = 0;
    std::thread::scope(|scope| {
        let (stopping, path) = (&stopping, &path);
        let waiting = idle.clone();
        let client = scope.spawn(move || {
            for i in 0..50 {
                assert_eq!(ask(path, "x"), "ok\n", "request {i}");
                while waiting.load(Ordering::SeqCst) == 0 {
                    std::thread::yield_now();
                }
            }
            stopping.store(true, Ordering::SeqCst);
        });
        started = accept_counting_idle(&listener, path.to_str().unwrap(), scope, stopping, |stream| {
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            reply(&stream, "ok");
        }, idle.clone());
        client.join().unwrap();
    });
    assert_eq!(started, 1, "idle traffic starts no thread beyond the first worker");
}

#[test_id(tr7q2np)]
#[test]
fn a_request_that_calls_back_into_the_socket_does_not_deadlock() {
    let dir = testkit::TempDir::new("lc-workers-reentrant");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let mut started = 0;
    std::thread::scope(|scope| {
        let (stopping, path) = (&stopping, &path);
        let client = scope.spawn(move || {
            assert_eq!(ask(path, "outer"), "outer+inner\n");
            stopping.store(true, Ordering::SeqCst);
        });
        started = accept_until_stopped(&listener, path.to_str().unwrap(), scope, stopping, move |stream| {
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            if line == "outer\n" {
                let inner = ask(path, "inner");
                reply(&stream, &format!("outer+{}", inner.trim()));
            } else {
                reply(&stream, "inner");
            }
        });
        client.join().unwrap();
    });
    assert_eq!(started, 2, "the nested request found every worker busy and started one");
}

#[test_id(tc5v8wd)]
#[test]
fn a_reused_worker_registers_each_connection_as_its_own_flight() {
    let dir = testkit::TempDir::new("lc-workers-flights");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let idle = Arc::new(AtomicUsize::new(0));
    let flights = Flights::default();
    let seen = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let (stopping, path, flights, seen) = (&stopping, &path, &flights, &seen);
        let waiting = idle.clone();
        let client = scope.spawn(move || {
            for _ in 0..2 {
                ask(path, "x");
                while waiting.load(Ordering::SeqCst) == 0 {
                    std::thread::yield_now();
                }
            }
            stopping.store(true, Ordering::SeqCst);
        });
        accept_counting_idle(&listener, path.to_str().unwrap(), scope, stopping, move |stream| {
            flights.serve(stream, |stream| {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let current = CURRENT.with(|c| c.borrow().clone()).expect("a connection's slot is set while it is served");
                seen.lock().unwrap().push((current, flights.len()));
                reply(&stream, "ok");
            })
        }, idle.clone());
        client.join().unwrap();
    });
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(!Arc::ptr_eq(&seen[0].0, &seen[1].0), "the second connection has its own slot");
    assert_eq!(seen[1].1, 1, "the first connection left the drain's list");
    assert!(CURRENT.with(|c| c.borrow().is_none()));
}

fn send_head(path: &std::path::Path, argv: &[&str], request_id: Option<&str>) -> String {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let head = spira_store::lc_call::request_head(&argv, None, &Default::default(), None, request_id).unwrap();
    ask(path, &head)
}

#[test_id(t90ix0r)]
#[test]
fn request_work_reuses_runner_threads_beside_parked_watches() {
    let dir = testkit::TempDir::new("lc-workers-runners");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let idle = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let work: super::Work = {
        let ran = ran.clone();
        Arc::new(move |req| {
            ran.lock().unwrap().insert(std::thread::current().id());
            if req.argv.iter().any(|a| a == "park") {
                parked_tx.send(()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
            }
            crate::callers::Answer { code: 0, stdout: "ok".into(), ..Default::default() }
        })
    };
    let door = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, personas: Default::default(), deadlines: super::Deadlines { default: Duration::from_secs(60) } });
    std::thread::scope(|scope| {
        let (stopping, path) = (&stopping, &path);
        let waiting = idle.clone();
        let client = scope.spawn(move || {
            let parked: Vec<_> = (0..2).map(|_| scope.spawn(move || send_head(path, &["list", "park"], None))).collect();
            for _ in 0..2 {
                parked_rx.recv().unwrap();
            }
            for i in 0..50 {
                let reply = if i % 2 == 0 { send_head(path, &["list"], None) } else { send_head(path, &["content", "note", "sp-a", "x"], Some(&format!("req-{i}"))) };
                assert!(reply.contains(r#""exit_code":0"#), "request {i}: {reply}");
                while waiting.load(Ordering::SeqCst) == 0 {
                    std::thread::yield_now();
                }
            }
            for _ in 0..2 {
                release_tx.send(()).unwrap();
            }
            for p in parked {
                p.join().unwrap();
            }
            stopping.store(true, Ordering::SeqCst);
        });
        let door = &door;
        accept_counting_idle(&listener, path.to_str().unwrap(), scope, stopping, move |stream| {
            super::serve_one(stream, None, Instant::now(), door)
        }, idle.clone());
        client.join().unwrap();
    });
    let distinct = ran.lock().unwrap().len();
    assert!(distinct <= 3, "{distinct} distinct threads ran the work: 2 parked and 1 reused for the sequential requests");
}

#[test_id(txwl72b)]
#[test]
fn a_value_the_serve_closure_returns_is_dropped_only_after_its_worker_counts_itself_idle() {
    struct Guard(Arc<AtomicUsize>, Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.1
                .store(self.0.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }
    let dir = testkit::TempDir::new("lc-workers-guard");
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let stopping = AtomicBool::new(false);
    let idle = Arc::new(AtomicUsize::new(0));
    let at_drop = Arc::new(AtomicUsize::new(usize::MAX));
    std::thread::scope(|scope| {
        let (stopping, path, waiting, dropped) = (&stopping, &path, idle.clone(), at_drop.clone());
        let client = scope.spawn(move || {
            ask(path, "x");
            while dropped.load(Ordering::SeqCst) == usize::MAX {
                std::thread::yield_now();
            }
            assert!(waiting.load(Ordering::SeqCst) >= 1);
            stopping.store(true, Ordering::SeqCst);
        });
        let (idle_seen, recorded) = (idle.clone(), at_drop.clone());
        accept_counting_idle(
            &listener,
            path.to_str().unwrap(),
            scope,
            stopping,
            move |stream| {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                reply(&stream, "ok");
                Guard(idle_seen.clone(), recorded.clone())
            },
            idle.clone(),
        );
        client.join().unwrap();
    });
    assert!(
        at_drop.load(Ordering::SeqCst) >= 1,
        "the guard dropped before its worker counted itself idle"
    );
}
