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
                seen.lock().unwrap().push((Arc::as_ptr(&current) as usize, flights.len()));
                reply(&stream, "ok");
            })
        }, idle.clone());
        client.join().unwrap();
    });
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].0, seen[1].0, "the second connection has its own slot");
    assert_eq!(seen[1].1, 1, "the first connection left the drain's list");
    assert!(CURRENT.with(|c| c.borrow().is_none()));
}
