//! `spira-lc serve --loop-socket`: the loop socket answers `show` and `list --live` as the main
//! socket does. This spawns the binary because the thing compared is a running server's two
//! listeners and its threads, which no in-process call can see (law-a-unit-test-calls-no-binary).

#[macro_use]
extern crate spira_test_id;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use state_store_sqlite::{Options, SqliteBackend};

const CLIENTS: usize = 50;
const PINNED_NOW: &str = "1800000000";

fn thread_names(pid: u32) -> Vec<String> {
    let tasks = std::path::Path::new(system_paths::PROC).join(pid.to_string()).join("task");
    let mut names: Vec<String> = std::fs::read_dir(tasks).expect("the server is alive").flatten().filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok()).map(|n| n.trim().to_string()).collect();
    names.sort();
    names
}

struct Server {
    child: std::process::Child,
    socket: std::path::PathBuf,
    loop_socket: std::path::PathBuf,
    pid: u32,
    _dir: testkit::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server() -> Server {
    let dir = testkit::TempDir::new("spira-lc-serve-loop");
    let run_dir = dir.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let store = dir.join("control.db");
    drop(SqliteBackend::open(&store, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap());
    let (socket, loop_socket) = (dir.join("lc.sock"), dir.join("loop.sock"));
    let (d, r, s, sock) = (dir.display().to_string(), run_dir.display().to_string(), store.display().to_string(), socket.display().to_string());
    let toml = spira_config::process::fixture_toml(
        &dir,
        &[("SPIRA_WORKSPACES", &d), ("SPIRA_RUN", &r), ("SPIRA_LC_STORE_PATH", &s), ("SPIRA_LC_SOCKET", &sock), ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9")],
    );
    std::fs::create_dir_all(dir.join("release/bin")).unwrap();
    let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
    let mut child = Command::new(testkit::bin_exe("spira-lc"))
        .arg("serve")
        .arg("--loop-socket")
        .arg(&loop_socket)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("SPIRA_NOW", PINNED_NOW)
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
    Server { child, socket, loop_socket, pid, _dir: dir }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// Everything the server writes to `request` before it closes the connection.
fn raw(socket: &std::path::Path, request: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(socket).expect("the socket accepts");
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    stream.write_all(request).unwrap();
    let mut got = Vec::new();
    stream.read_to_end(&mut got).expect("the server answers");
    got
}

fn with_a_bead() -> Server {
    let server = start_server();
    let made = spira_store::lc_call::exchange_once(spira_store::lc_call::connect(&server.socket).unwrap(), &argv(&["create-bead", "sp-loopa1", "--ready"]), None).expect("create-bead answers");
    assert_eq!(made.code, 0, "{}", made.text());
    server
}

#[test_id(tuhznce)]
#[test]
fn both_sockets_give_identical_bytes_for_show_found_show_absent_list_live_and_a_malformed_line() {
    let server = with_a_bead();
    let requests: [(&str, &[u8]); 4] = [
        ("show found", b"[\"show\",\"sp-loopa1\"]\n"),
        ("show absent", b"[\"show\",\"sp-absent9\"]\n"),
        ("list --live", b"[\"list\",\"--live\"]\n"),
        ("a malformed line", b"this is not json\n"),
    ];
    for (name, request) in requests {
        let (main, looped) = (raw(&server.socket, request), raw(&server.loop_socket, request));
        assert!(!main.is_empty(), "{name}: the main socket said nothing");
        assert_eq!(String::from_utf8_lossy(&looped), String::from_utf8_lossy(&main), "{name}: the loop socket answered differently");
    }
    let found = String::from_utf8(raw(&server.loop_socket, requests[0].1)).unwrap();
    assert!(found.starts_with("{\"exit_code\":0") && found.contains("sp-loopa1"), "show found must find the bead: {found}");
    let absent = String::from_utf8(raw(&server.loop_socket, requests[1].1)).unwrap();
    assert!(!absent.starts_with("{\"exit_code\":0"), "show absent must not succeed: {absent}");
}

#[test_id(tv1g8nf)]
#[test]
fn a_verb_the_loop_does_not_serve_is_answered_cannot_tell_naming_the_two_it_does() {
    let server = start_server();
    let reply = spira_store::lc_call::exchange_once(spira_store::lc_call::connect(&server.loop_socket).unwrap(), &argv(&["history", "sp-loopa1"]), None).expect("the loop answers");
    assert_eq!((reply.code, reply.out.as_str()), (2, "cannot tell: the loop serves show and list --live"));
}

#[test_id(tm1a9x4)]
#[test]
fn fifty_concurrent_loop_clients_leave_the_thread_names_as_they_were_at_ready() {
    let server = with_a_bead();
    let at_ready = thread_names(server.pid);
    assert!(at_ready.iter().any(|n| n == "lc-loop"), "no lc-loop thread at ready: {}", at_ready.join(", "));
    std::thread::scope(|scope| {
        for c in 0..CLIENTS {
            let socket = &server.loop_socket;
            scope.spawn(move || {
                for n in 0..5 {
                    let words: Vec<String> = if (c + n) % 2 == 0 { argv(&["show", "sp-loopa1"]) } else { argv(&["list", "--live"]) };
                    let stream = spira_store::lc_call::connect(socket).unwrap_or_else(|_| panic!("connect {c}/{n}"));
                    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                    spira_store::lc_call::exchange_once(stream, &words, None).unwrap_or_else(|_| panic!("no answer to {words:?}"));
                }
            });
        }
    });
    let after = thread_names(server.pid);
    assert_eq!(after, at_ready, "the loop's clients changed the server's threads");
}
