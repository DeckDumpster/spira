//! `spira-lc serve` holds no more anonymous memory after a burst of reads over many connections
//! than it did when it came up. This spawns the binary because the thing measured is the server
//! process's own resident memory, which no in-process call can see (law-a-unit-test-calls-no-binary).

#[macro_use]
extern crate spira_test_id;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use spira_otel::fixture::{spans_in, Collector};
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
    _dir: testkit::TempDir,
}

/// Starts `spira-lc serve` over a seeded store, pushing spans to `collector`, and returns once it has said ready.
fn start_server(collector: Option<&Collector>) -> Server {
    let dir = testkit::TempDir::new("spira-lc-serve-memory");
    let run_dir = dir.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let store = dir.join("control.db");
    seed(&store);
    let socket = dir.join("lc.sock");
    let (d, r, s, sock) = (dir.display().to_string(), run_dir.display().to_string(), store.display().to_string(), socket.display().to_string());
    let mut set = vec![("SPIRA_WORKSPACES", d.as_str()), ("SPIRA_RUN", r.as_str()), ("SPIRA_LC_STORE_PATH", s.as_str()), ("SPIRA_LC_SOCKET", sock.as_str()), ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9")];
    if let Some(c) = collector {
        set.push(("SPIRA_OTEL_COLLECTOR_ENDPOINT", c.addr()));
    }
    let toml = spira_config::process::fixture_toml(&dir, &set);
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
    Server { child, socket, pid, _dir: dir }
}

#[test_id(tu38fgg)]
#[test]
fn a_burst_of_reads_over_fifty_connections_leaves_the_servers_anonymous_memory_where_it_was() {
    let Server { mut child, socket, pid, _dir } = start_server(None);
    let at_ready = rss_anon(pid);

    let deadline_answers = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let per = REQUESTS / CONNECTIONS;
        for c in 0..CONNECTIONS {
            let (socket, deadline_answers) = (&socket, &deadline_answers);
            scope.spawn(move || {
                for n in 0..per {
                    let argv: Vec<String> = if (c + n) % 2 == 0 {
                        vec!["ops-view".into(), "ops_dwell".into()]
                    } else {
                        vec!["list".into(), "--live".into()]
                    };
                    let stream = std::os::unix::net::UnixStream::connect(socket)
                        .unwrap_or_else(|_| panic!("connect {c}/{n}"));
                    stream
                        .set_read_timeout(Some(Duration::from_secs(30)))
                        .unwrap();
                    let mut closed = stream.try_clone().unwrap();
                    let reply = spira_store::lc_call::exchange_once(Box::new(spira_store::lc_call::UnixPort::from(stream)), &argv, None)
                        .unwrap_or_else(|_| panic!("no answer to {argv:?}"));
                    if reply.code != 0 && reply.text() == spira_store::lc_call::DEADLINE_ANSWER {
                        deadline_answers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    let Server { mut child, pid, .. } = start_server(None);
    let names = thread_names(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(names.iter().any(|n| n == "lc-accept"), "ready was said before the accept thread ran; threads: {}", names.join(", "));
}

/// A cap on a wait for the fixture's own signal.
const CAP: Duration = Duration::from_secs(30);

fn ask(server: &Server) {
    let stream = spira_store::lc_call::connect_within(&server.socket, Duration::from_secs(30)).expect("connect");
    spira_store::lc_call::exchange_once(stream, &["list".to_string(), "--live".to_string()], None).expect("an answer");
}

fn assert_no_span_file(server: &Server) {
    let files: Vec<String> = std::fs::read_dir(server._dir.join("run/otel"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("spira-lc.") && !n.starts_with("spira-lc.logs."))
        .collect();
    assert!(files.is_empty(), "a span file was written: {files:?}");
}

#[test_id(tr71tg4)]
#[test]
fn a_collector_holding_requests_unanswered_stalls_no_request_worker() {
    let collector = Collector::hanging();
    let server = start_server(Some(&collector));
    assert!(collector.wait_until(|p| !p.is_empty(), CAP), "the start-up span's push is held");
    ask(&server);
    let before = thread_names(server.pid).len();
    for _ in 0..30 {
        ask(&server);
    }
    let grown = thread_names(server.pid).len().saturating_sub(before);
    assert_no_span_file(&server);
    let Server { mut child, .. } = server;
    let _ = child.kill();
    let _ = child.wait();
    assert!(grown <= 4, "threads grew by {grown} over 30 requests while the collector answered nothing");
}

#[test_id(tyigr7q)]
#[test]
fn a_hundred_requests_are_pushed_about_once_a_second() {
    let collector = Collector::start();
    let server = start_server(Some(&collector));
    let started = std::time::Instant::now();
    for _ in 0..100 {
        ask(&server);
    }
    let listed = |p: &[spira_otel::fixture::Post]| spans_in(p).iter().filter(|s| s.name == "list").count();
    assert!(collector.wait_until(|p| listed(p) >= 100, CAP), "{} of 100 spans received", listed(&collector.posts()));
    let elapsed = started.elapsed().as_secs();
    let pushes = collector.posts().iter().filter(|p| p.path == "/v1/traces").count(); // not a path: OTLP URL path
    assert_no_span_file(&server);
    let Server { mut child, .. } = server;
    let _ = child.kill();
    let _ = child.wait();
    assert!(pushes as u64 <= elapsed + 2, "{pushes} pushes in {elapsed} s"); // clock-safe: measures pushes per elapsed second the test itself spent; no wait depends on it
}

#[test_id(tkdm6hh)]
#[test]
fn a_request_answered_before_sigterm_has_its_span_at_the_collector_when_the_server_exits() {
    let collector = Collector::start();
    let mut server = start_server(Some(&collector));
    ask(&server);
    // SAFETY: signals a child this test spawned.
    assert_eq!(unsafe { libc::kill(server.pid as i32, libc::SIGTERM) }, 0);
    server.child.wait().expect("the server exits");
    let names: Vec<String> = collector.spans().into_iter().map(|s| s.name).collect();
    assert!(names.iter().any(|n| n == "list"), "no span for the request in {names:?}");
    assert_no_span_file(&server);
}

#[test_id(tqh7bnx)]
#[test]
fn serve_with_an_empty_collector_endpoint_exits_2_naming_the_key() {
    let dir = testkit::TempDir::new("spira-lc-serve-no-collector");
    let (d, sock) = (dir.display().to_string(), dir.join("lc.sock").display().to_string());
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_WORKSPACES", &d), ("SPIRA_RUN", &d), ("SPIRA_LC_SOCKET", &sock), ("SPIRA_OTEL_COLLECTOR_ENDPOINT", "")]);
    std::fs::create_dir_all(dir.join("release/bin")).unwrap();
    // batch-job: a server that refuses to start exits at once
    let out = Command::new(testkit::bin_exe("spira-lc"))
        .arg("serve")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree"))
        .env("SPIRA_RELEASE", dir.join("release"))
        .env("HOME", dir.join("home"))
        .output()
        .expect("spira-lc serve runs");
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{said}");
    assert!(said.contains("spira.otel_collector_endpoint"), "{said}");
}
