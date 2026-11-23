//! `spira-lc serve` holds no more anonymous memory after a burst of reads over many connections
//! than it did when it came up. This spawns the binary because the thing measured is the server
//! process's own resident memory, which no in-process call can see (law-a-unit-test-calls-no-binary).

#[macro_use]
extern crate spira_test_id;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use state_store::{events_key, Backend, Object, Rev, Write};
use state_store_sqlite::{Options, SqliteBackend};

const CONNECTIONS: usize = 50;
const REQUESTS: usize = 500;
const SLACK: u64 = 64 << 20;

fn status_field(pid: u32, name: &str) -> u64 {
    let status = std::fs::read_to_string(std::path::Path::new(system_paths::PROC).join(pid.to_string()).join("status")).expect("the server is alive");
    let line = status.lines().find(|l| l.starts_with(name)).unwrap_or_else(|| panic!("{name}"));
    line.split_whitespace().nth(1).and_then(|n| n.parse().ok()).expect("a number")
}

fn thread_names(pid: u32) -> Vec<String> {
    let tasks = std::path::Path::new(system_paths::PROC).join(pid.to_string()).join("task");
    let mut names: Vec<String> = std::fs::read_dir(tasks).expect("the server is alive").flatten().filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok()).map(|n| n.trim().to_string()).collect();
    names.sort();
    names
}

fn rss_anon(pid: u32) -> u64 {
    status_field(pid, "RssAnon:") << 10
}

/// A control store whose log holds a fortnight of bulky audits.
fn seed(path: &std::path::Path) {
    let b = SqliteBackend::open(path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
    let now = spira_config::vtime::now_epoch() as i64;
    let bulk = "x".repeat(2_000);
    let states = ["READY", "WORKING", "SUBMITTED", "CERTIFIED"];
    let mut head = b.head().unwrap();
    let audits = 1_500u64;
    for n in 0..audits {
        let (from, to) = (states[(n % 4) as usize], states[((n + 1) % 4) as usize]);
        let at = now - 13 * 86_400 + (n as i64) * (13 * 86_400) / audits as i64;
        let a = json!({ "machine": "bead", "lc_key": format!("sp-{:04}", n % 300), "event": "Claim", "expect": from, "from_state": from, "to_state": to, "applied": true, "refusal": null, "evidence": { "bulk": bulk }, "actor": "a", "at": at });
        let events = json!({ "refused": false, "evidence": { "then": [a] } });
        let write = Write::Put { key: events_key(head + 1), value: Object::new(events).encode(), create_rev: Rev::new(head + 1, 0), prev: None };
        head = b.commit(head, &[write]).unwrap();
    }
}

struct Server {
    child: std::process::Child,
    socket: std::path::PathBuf,
    pid: u32,
    run_dir: std::path::PathBuf,
    _dir: testkit::TempDir,
}

/// Starts `spira-lc serve` over a seeded store and returns once it has said ready.
fn start_server(hold_spans: bool) -> Server {
    let dir = testkit::TempDir::new("spira-lc-serve-memory");
    let run_dir = dir.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    if hold_spans {
        let otel = spira_otel::dir_of(&run_dir);
        std::fs::create_dir_all(&otel).unwrap();
        let now = std::time::SystemTime::now();
        for t in [now, now + Duration::from_secs(86_400)] {
            let path = std::ffi::CString::new(otel.join(format!("spira-lc.{}.jsonl", spira_otel::utc_date(t))).into_os_string().into_encoded_bytes()).unwrap();
            // SAFETY: a valid NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0, "mkfifo");
        }
    }
    let store = dir.join("control.db");
    seed(&store);
    let socket = dir.join("lc.sock");
    let (d, r, s, sock) = (dir.display().to_string(), run_dir.display().to_string(), store.display().to_string(), socket.display().to_string());
    let toml = spira_config::process::fixture_toml(
        &dir,
        &[("SPIRA_WORKSPACES", &d), ("SPIRA_RUN", &r), ("SPIRA_LC_STORE_PATH", &s), ("SPIRA_LC_SOCKET", &sock), ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9")],
    );
    std::fs::create_dir_all(dir.join("release/bin")).unwrap();
    let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
    let mut child = Command::new(testkit::bin_exe("spira-lc"))
        .arg("serve")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("SPIRA_RELEASE", dir.join("release"))
        .env("HOME", dir.join("home"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spira-lc serve runs");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let mut said = String::new();
    while !line.contains(" ready after ") {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "the server stopped before it said ready: {said}");
        said.push_str(&line);
    }
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    let pid = child.id();
    Server { child, socket, pid, run_dir, _dir: dir }
}

#[test_id(tu38fgg)]
#[test]
fn a_burst_of_reads_over_fifty_connections_leaves_the_servers_anonymous_memory_where_it_was() {
    let Server {
        mut child,
        socket,
        pid,
        run_dir,
        _dir,
    } = start_server(true);
    let at_ready = rss_anon(pid);

    // Every span write blocks until a reader opens: each worker is held in its reply's aftermath
    // until every client has its first reply, and a client that waits for the close is then
    // served by a reused worker, whatever the disk's speed.
    let first_replies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deadline_answers = std::sync::atomic::AtomicUsize::new(0);
    let otel = spira_otel::dir_of(&run_dir);
    let held: Vec<_> = std::fs::read_dir(&otel)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(held.len(), 2);
    // Detached: these readers never see an end of file.
    for path in held {
        let first_replies = first_replies.clone();
        std::thread::spawn(move || {
            while first_replies.load(std::sync::atomic::Ordering::SeqCst) < CONNECTIONS {
                std::thread::yield_now();
            }
            // Read-write, so the reader outlives each writer's close and no later write blocks.
            if let Ok(mut fifo) = std::fs::File::options().read(true).write(true).open(path) {
                let _ = std::io::copy(&mut fifo, &mut std::io::sink());
            }
        });
    }
    std::thread::scope(|scope| {
        let per = REQUESTS / CONNECTIONS;
        for c in 0..CONNECTIONS {
            let (socket, first_replies, deadline_answers) =
                (&socket, &*first_replies, &deadline_answers);
            scope.spawn(move || {
                for n in 0..per {
                    let argv: Vec<String> = if (c + n) % 2 == 0 {
                        vec!["ops-view".into(), "ops_dwell".into()]
                    } else {
                        vec!["list".into(), "--live".into()]
                    };
                    let stream = spira_store::lc_call::connect(socket)
                        .unwrap_or_else(|_| panic!("connect {c}/{n}"));
                    stream
                        .set_read_timeout(Some(Duration::from_secs(30)))
                        .unwrap();
                    let mut closed = stream.try_clone().unwrap();
                    let reply = spira_store::lc_call::exchange_once(stream, &argv, None)
                        .unwrap_or_else(|_| panic!("no answer to {argv:?}"));
                    if reply.code != 0 && reply.text() == spira_store::lc_call::DEADLINE_ANSWER {
                        deadline_answers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    if n == 0 {
                        first_replies.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    let mut rest = Vec::new();
                    std::io::Read::read_to_end(&mut closed, &mut rest).unwrap_or_else(|e| {
                        panic!(
                            "the close of {c}/{n} after {:?}: {e}",
                            (
                                reply.code,
                                reply.text().chars().take(60).collect::<String>()
                            )
                        )
                    });
                }
            });
        }
    });
    let names = thread_names(pid);
    let count = |name: &str| names.iter().filter(|n| *n == name).count();
    let deadline_answers = deadline_answers.into_inner();
    assert_eq!(
        count("lc-accept"),
        1,
        "connection workers are named lc-conn: {}",
        names.join(", ")
    );
    assert!(
        count("lc-conn") <= CONNECTIONS,
        "{} lc-conn threads for {CONNECTIONS} concurrent clients: {}",
        count("lc-conn"),
        names.join(", ")
    );
    assert!(
        count("lc-run") <= CONNECTIONS + deadline_answers,
        "{} lc-run threads for {CONNECTIONS} clients and {deadline_answers} deadline answers",
        count("lc-run")
    );
    let after = rss_anon(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        after < at_ready + SLACK,
        "RssAnon was {at_ready} B at ready and {after} B after {} requests over {} connections",
        REQUESTS,
        CONNECTIONS
    );
}

#[test_id(tovs682)]
#[test]
fn the_ready_line_is_written_once_the_accept_thread_runs() {
    let Server { mut child, pid, .. } = start_server(false);
    let names = thread_names(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(names.iter().any(|n| n == "lc-accept"), "ready was said before the accept thread ran; threads: {}", names.join(", "));
}

fn otel_dir(server: &Server) -> std::path::PathBuf {
    server._dir.join("run/otel")
}

fn ask(server: &Server) {
    let stream = spira_store::lc_call::connect(&server.socket).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    spira_store::lc_call::exchange_once(stream, &["list".to_string(), "--live".to_string()], None).expect("an answer");
}

fn span_lines(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        out.extend(std::fs::read_to_string(e.path()).unwrap_or_default().lines().map(str::to_string));
    }
    out
}

fn wait_for_spans(dir: &std::path::Path, want: usize) {
    let mut polls = 0;
    while span_lines(dir).len() < want {
        polls += 1;
        assert!(polls < 1_000, "{} of {want} spans written", span_lines(dir).len());
        std::thread::sleep(Duration::from_millis(10)); // clock-safe: polls the span file, the signal waited on
    }
}

#[test_id(tr71tg4)]
#[test]
fn a_span_file_held_locked_stalls_no_request_worker_and_loses_no_span() {
    let server = start_server();
    let dir = otel_dir(&server);
    std::fs::create_dir_all(&dir).unwrap();
    let today = dir.join(format!("spira-lc.{}.jsonl", spira_otel::utc_date(std::time::SystemTime::now())));
    let held = std::fs::OpenOptions::new().create(true).append(true).open(&today).unwrap();
    // SAFETY: the descriptor is open for as long as `held`.
    assert_eq!(unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&held), libc::LOCK_EX) }, 0);
    let before = threads(server.pid);
    for _ in 0..30 {
        ask(&server);
    }
    let grown = threads(server.pid).saturating_sub(before);
    drop(held);
    wait_for_spans(&dir, 30);
    let Server { mut child, .. } = server;
    let _ = child.kill();
    let _ = child.wait();
    assert!(grown <= 4, "threads grew by {grown} over 30 requests while the span file was locked");
}

#[test_id(tyigr7q)]
#[test]
fn a_hundred_requests_close_the_span_file_for_writing_about_once_a_second() {
    use inotify::{Inotify, WatchMask};
    let server = start_server();
    let dir = otel_dir(&server);
    std::fs::create_dir_all(&dir).unwrap();
    let mut watch = Inotify::init().expect("inotify");
    watch.watches().add(&dir, WatchMask::CLOSE_WRITE).expect("watch the otel directory");
    // A reader that blocks: unread identical events coalesce, which would hide a close per span.
    let closes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = closes.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(events) = watch.read_events_blocking(&mut buf) {
            seen.fetch_add(events.count(), std::sync::atomic::Ordering::Relaxed);
        }
    });
    let started = std::time::Instant::now();
    for _ in 0..100 {
        ask(&server);
    }
    let elapsed = started.elapsed().as_secs();
    wait_for_spans(&dir, 100);
    let closes = closes.load(std::sync::atomic::Ordering::Relaxed);
    let Server { mut child, .. } = server;
    let _ = child.kill();
    let _ = child.wait();
    assert!(closes as u64 <= elapsed + 2, "{closes} close-for-write events in {elapsed} s"); // clock-safe: measures the span-file closes per elapsed second the test itself spent; no wait depends on it
}

#[test_id(tkdm6hh)]
#[test]
fn a_request_answered_before_sigterm_has_its_span_in_the_file_when_the_server_exits() {
    let server = start_server();
    let dir = otel_dir(&server);
    ask(&server);
    // SAFETY: signals a child this test spawned.
    assert_eq!(unsafe { libc::kill(server.pid as i32, libc::SIGTERM) }, 0);
    let Server { mut child, .. } = server;
    child.wait().expect("the server exits");
    assert!(span_lines(&dir).iter().any(|l| l.contains("\"list\"")), "no span for the request in {:?}", span_lines(&dir).len());
}
