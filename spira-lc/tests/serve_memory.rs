//! `spira-lc serve` holds no more anonymous memory after a burst of reads over many connections
//! than it did when it came up. This spawns the binary because the thing measured is the server
//! process's own resident memory, which no in-process call can see (law-a-unit-test-calls-no-binary).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use state_store::{events_key, Backend, Object, Rev, Write};
use state_store_sqlite::{Options, SqliteBackend};

const CONNECTIONS: usize = 50;
const REQUESTS: usize = 500;
const SLACK: u64 = 64 << 20;

fn threads(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("the server is alive");
    let line = status.lines().find(|l| l.starts_with("Threads:")).expect("Threads");
    line.split_whitespace().nth(1).and_then(|n| n.parse().ok()).expect("count")
}

fn rss_anon(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("the server is alive");
    let line = status.lines().find(|l| l.starts_with("RssAnon:")).expect("RssAnon");
    line.split_whitespace().nth(1).and_then(|kb| kb.parse::<u64>().ok()).expect("kB") << 10
}

/// A control store whose log holds a fortnight of bulky audits.
fn seed(path: &std::path::Path) {
    let b = SqliteBackend::open(path, Options::default()).unwrap();
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

#[test]
fn a_burst_of_reads_over_fifty_connections_leaves_the_servers_anonymous_memory_where_it_was() {
    let dir = testkit::TempDir::new("spira-lc-serve-memory");
    let run_dir = dir.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
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
    let began = Instant::now();
    let mut said = String::new();
    while !line.contains(" ready after ") {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0 && began.elapsed() < Duration::from_secs(60), "the server never said ready: {said}");
        said.push_str(&line);
    }
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    let pid = child.id();
    let (at_ready, threads_at_ready) = (rss_anon(pid), threads(pid));

    let started = Instant::now();
    let per = REQUESTS / CONNECTIONS;
    std::thread::scope(|scope| {
        for c in 0..CONNECTIONS {
            let socket = &socket;
            scope.spawn(move || {
                for n in 0..per {
                    let argv: Vec<String> = if (c + n) % 2 == 0 { vec!["ops-view".into(), "ops_dwell".into()] } else { vec!["list".into(), "--live".into()] };
                    let stream = spira_store::lc_call::connect(socket).unwrap_or_else(|_| panic!("connect {c}/{n}"));
                    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                    spira_store::lc_call::exchange_once(stream, &argv, None).unwrap_or_else(|_| panic!("no answer to {argv:?}"));
                }
            });
        }
    });
    assert!(started.elapsed() < Duration::from_secs(60), "{} requests took {:?}", REQUESTS, started.elapsed());
    // A request past its deadline is answered and its worker runs on: wait for the workers to end.
    let began = Instant::now();
    while threads(pid) > threads_at_ready {
        assert!(began.elapsed() < Duration::from_secs(120), "the abandoned work never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = rss_anon(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(after < at_ready + SLACK, "RssAnon was {at_ready} B at ready and {after} B after {} requests over {} connections", REQUESTS, CONNECTIONS);
}
