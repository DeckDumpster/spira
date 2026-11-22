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
    let mut started = 0;
    std::thread::scope(|scope| {
        let (stopping, path) = (&stopping, &path);
        let client = scope.spawn(move || {
            for i in 0..50 {
                assert_eq!(ask(path, "x"), "ok\n", "request {i}");
            }
            stopping.store(true, Ordering::SeqCst);
        });
        started = accept_until_stopped(&listener, path.to_str().unwrap(), scope, stopping, |stream| {
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            reply(&stream, "ok");
        });
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
