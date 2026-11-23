use super::*;
use crate::serve_loop::{Shared, REFUSED_VERB};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Condvar, Mutex};

/// The loop tests share the pool's process-wide refusal count, so they run one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn closed() -> Arc<Gate> {
        Arc::new(Gate { open: Mutex::new(false), changed: Condvar::new() })
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

struct Running {
    path: std::path::PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<usize>>,
    stopped: Arc<AtomicBool>,
    gates: Vec<Arc<Gate>>,
    _dir: testkit::TempDir,
}

impl Running {
    /// Opens `gate` when the server is dropped, so a failed test ends instead of hanging on work it holds.
    fn releasing(mut self, gate: &Arc<Gate>) -> Running {
        self.gates.push(gate.clone());
        self
    }

    fn stop(&mut self) -> usize {
        self.stopping.store(true, Ordering::SeqCst);
        self.thread.take().expect("running").join().expect("the loop ends")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.gates.iter().for_each(|g| g.open());
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The loop server on a real socket, answering with `work` over a pool of `workers` and a queue of `queue`.
fn running(work: Work, workers: usize, queue: usize, deadline: Duration, drain_bound: Duration) -> Running {
    let dir = testkit::TempDir::new("lc-loop-server");
    let path = dir.join("loop.sock");
    let listener = bind_socket(path.to_str().unwrap()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let door = Door { ledger: Ledger::new(Duration::from_secs(60)), work, deadlines: Deadlines { default: deadline } };
    let shared = Arc::new(Shared::new(Arc::new(door), runtime::offload::Pool::new(workers, queue), None));
    let (stopping, stopped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (flag, gone, socket) = (stopping.clone(), stopped.clone(), path.clone());
    let thread = std::thread::Builder::new()
        .name("lc-loop-test".into())
        .spawn(move || run_loop(listener, shared, &flag, drain_bound, move || {
            let _ = std::fs::remove_file(&socket);
            gone.store(true, Ordering::SeqCst);
        }))
        .unwrap();
    Running { path, stopping, thread: Some(thread), stopped, gates: Vec::new(), _dir: dir }
}

fn work_that(f: impl Fn(&Request) + Send + Sync + 'static) -> Work {
    Arc::new(move |req| {
        f(&req);
        Answer::out(0, format!("ran {}", req.argv.join(" ")))
    })
}

fn send(path: &std::path::Path, line: &str) -> UnixStream {
    let mut s = UnixStream::connect(path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(s, "{line}").unwrap();
    s
}

/// The one answer on `s`: its code and stdout; the connection must then end with nothing more.
fn one_answer(mut s: UnixStream) -> (i64, String) {
    let mut got = Vec::new();
    s.read_to_end(&mut got).expect("the loop answers");
    let text = String::from_utf8(got).unwrap();
    let (head, body) = text.split_once('\n').unwrap_or_else(|| panic!("no header line in {text:?}"));
    let head: serde_json::Value = serde_json::from_str(head).unwrap();
    assert_eq!(body.len() as u64, head["len"].as_u64().unwrap() + head["err_len"].as_u64().unwrap_or(0), "one answer and nothing after it: {text:?}");
    (head["exit_code"].as_i64().unwrap(), body.to_string())
}

#[test_id(t7a5hs0)]
#[test]
fn with_the_show_work_blocked_list_live_is_still_answered() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let gate = Gate::closed();
    let (started, began) = std::sync::mpsc::channel::<()>();
    let (held, started) = (gate.clone(), Mutex::new(started));
    let work = work_that(move |req| {
        if req.argv[0] == "show" {
            let _ = started.lock().unwrap().send(());
            held.wait();
        }
    });
    let server = running(work, 2, 4, Duration::from_secs(30), DRAIN_BOUND).releasing(&gate);
    let shown = send(&server.path, r#"["show","sp-aaa"]"#);
    began.recv().expect("the show work started");
    let listed = one_answer(send(&server.path, r#"["list","--live"]"#));
    assert_eq!(listed, (0, "ran list --live".to_string()));
    gate.open();
    assert_eq!(one_answer(shown), (0, "ran show sp-aaa".to_string()));
}

#[test_id(ty6elsd)]
#[test]
fn a_refused_queue_answers_every_request_once_and_counts_the_refusals() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let gate = Gate::closed();
    let held = gate.clone();
    let server = running(work_that(move |_| held.wait()), 1, 1, Duration::from_millis(300), DRAIN_BOUND).releasing(&gate);
    let before = runtime::offload::spilled_total("lc-loop");
    let clients: Vec<UnixStream> = (0..6).map(|n| send(&server.path, &format!(r#"["show","sp-{n}"]"#))).collect();
    let answers: Vec<(i64, String)> = clients.into_iter().map(one_answer).collect();
    gate.open();
    assert!(answers.iter().all(|a| *a == (2, spira_store::lc_call::DEADLINE_ANSWER.to_string())), "{answers:?}");
    assert!(runtime::offload::spilled_total("lc-loop") >= before + 4, "six requests over one worker and one queue place refused four");
}

#[test_id(t49uqze)]
#[test]
fn a_refused_request_is_resubmitted_when_a_job_completes_and_gets_its_real_answer() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let gate = Gate::closed();
    let (started, began) = std::sync::mpsc::channel::<()>();
    let (held, started) = (gate.clone(), Mutex::new(started));
    let work = work_that(move |_| {
        let _ = started.lock().unwrap().send(());
        held.wait();
    });
    let server = running(work, 1, 1, Duration::from_secs(30), DRAIN_BOUND).releasing(&gate);
    let before = runtime::offload::spilled_total("lc-loop");
    let first = send(&server.path, r#"["show","sp-1"]"#);
    began.recv().expect("the first job runs");
    let rest: Vec<UnixStream> = (2..=4).map(|n| send(&server.path, &format!(r#"["show","sp-{n}"]"#))).collect();
    while runtime::offload::spilled_total("lc-loop") < before + 2 {
        std::thread::yield_now();
    }
    gate.open();
    assert_eq!(one_answer(first).1, "ran show sp-1");
    let answers: Vec<String> = rest.into_iter().map(|s| one_answer(s).1).collect();
    assert_eq!(answers, ["ran show sp-2", "ran show sp-3", "ran show sp-4"]);
}

#[test_id(tbmapcq)]
#[test]
fn a_request_sent_one_byte_at_a_time_is_answered() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let server = running(work_that(|_| {}), 1, 1, Duration::from_secs(30), DRAIN_BOUND);
    let mut s = UnixStream::connect(&server.path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    for b in "{\"argv\":[\"show\",\"sp-bytes\"],\"len\":3}\nabc".bytes() {
        s.write_all(&[b]).unwrap();
        s.flush().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(one_answer(s), (0, "ran show sp-bytes".to_string()));
}

#[test_id(t39i51s)]
#[test]
fn no_proper_prefix_of_a_request_is_a_whole_request() {
    let wire = "{\"argv\":[\"show\",\"sp-x\"],\"len\":3}\nabc";
    for end in 0..wire.len() {
        assert!(matches!(crate::serve_loop::parse(&wire.as_bytes()[..end], false), crate::serve_loop::Parsed::Need), "{end} bytes");
    }
    assert!(matches!(crate::serve_loop::parse(wire.as_bytes(), false), crate::serve_loop::Parsed::Ready(..)));
}

#[test_id(tz0udm7)]
#[test]
fn a_verb_other_than_show_and_list_live_is_answered_exit_2_naming_the_two() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let server = running(work_that(|_| panic!("no work runs for a refused verb")), 1, 1, Duration::from_secs(30), DRAIN_BOUND);
    for line in [r#"["history","sp-x"]"#, r#"["list","--state","READY"]"#, r#"[]"#] {
        assert_eq!(one_answer(send(&server.path, line)), (2, REFUSED_VERB.to_string()), "{line}");
    }
}

#[test_id(t79dwui)]
#[test]
fn on_sigterm_the_loop_takes_its_socket_away_and_answers_what_it_cannot_wait_for() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut server = running(work_that(|_| {}), 1, 1, Duration::from_secs(30), Duration::from_millis(100));
    let mut idle = UnixStream::connect(&server.path).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    idle.write_all(b"[\"show\"").unwrap();
    let cut = server.stop();
    assert!(server.stopped.load(Ordering::SeqCst), "the stop hook ran");
    assert_eq!(cut, 1);
    let (code, out) = one_answer(idle);
    assert_eq!((code, out), (2, stopped_for_restart()));
}

#[test_id(t1vj3u8)]
#[test]
fn loop_socket_is_split_from_the_arguments_and_must_name_a_path() {
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(split_loop_socket(&words(&["a.sock", "--loop-socket", "b.sock"])), Ok((words(&["a.sock"]), Some("b.sock".to_string()))));
    assert_eq!(split_loop_socket(&words(&["a.sock"])), Ok((words(&["a.sock"]), None)));
    assert!(split_loop_socket(&words(&["--loop-socket"])).is_err());
    assert!(split_loop_socket(&words(&["--loop-socket", "a", "--loop-socket", "b"])).is_err());
}

#[test_id(ta2fg2i)]
#[test]
fn a_refused_span_job_is_counted_in_spilled_total() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let gate = Gate::closed();
    let (started, began) = std::sync::mpsc::channel::<()>();
    let (held, started) = (gate.clone(), Mutex::new(started));
    let work = work_that(move |_| {
        let _ = started.lock().unwrap().send(());
        held.wait();
    });
    let server = running(work, 1, 1, Duration::from_secs(30), DRAIN_BOUND).releasing(&gate);
    let _running = send(&server.path, r#"["show","sp-1"]"#);
    began.recv().expect("the first job runs");
    let _queued = send(&server.path, r#"["show","sp-2"]"#);
    let before = runtime::offload::spilled_total("lc-loop");
    assert_eq!(one_answer(send(&server.path, "not json")).0, 2, "a malformed line is answered without the pool");
    let until = Instant::now() + Duration::from_secs(10);
    while runtime::offload::spilled_total("lc-loop") == before {
        assert!(Instant::now() < until, "the span job the full queue refused was not counted");
        std::thread::yield_now();
    }
}
