//! The journal through spira-lc's socket. The hermetic tests stand a socket server on a
//! `MemJournal` that speaks the service's wire (one JSON argv line in; a header line and the body
//! out); the container-tier test runs the same suite against a real `spira-lc serve`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

use runtime::conformance::*;
use runtime::journal::{Journal, MemJournal};
use runtime::socket::SocketJournal;
use runtime::*;
use testkit::TempDir;

mod common;

fn answer(j: &MemJournal, argv: &[String]) -> (i32, String) {
    let a = |i: usize| argv[i].as_str();
    let ok = |r: Result<String>| r.map_or_else(|e| (2, e.0), |s| (0, s));
    let dec = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
    assert_eq!(a(0), "journal");
    match a(1) {
        "append" => ok(j.append(&serde_json::from_value(dec(a(2))).unwrap()).map(|o| o.to_string())),
        "read" => ok(j.read_from(a(2).parse().unwrap(), a(3).parse().unwrap()).map(|r| serde_json::to_string(&r).unwrap())),
        "applied" => ok(j.applied(a(2), &EventId(a(3).into())).map(|b| (b as u8).to_string())),
        "mark-applied" => ok(j.mark_applied(a(2), &EventId(a(3).into())).map(|_| String::new())),
        "cursor" => ok(j.cursor().map(|o| o.to_string())),
        "set-cursor" => ok(j.set_cursor(a(2).parse().unwrap()).map(|_| String::new())),
        "schedule" => ok(j.schedule(&serde_json::from_value(dec(a(2))).unwrap()).map(|_| String::new())),
        "due" => ok(j.pending_schedules().map(|r| serde_json::to_string(&r).unwrap())),
        "mark-fired" => ok(j.mark_fired(a(2)).map(|_| String::new())),
        "poison" => ok(j.poison(&serde_json::from_value(dec(a(2))).unwrap()).map(|_| String::new())),
        "poisoned" => ok(j.poisoned(&StreamKey(a(2).into())).map(|p| serde_json::to_string(&p).unwrap())),
        other => (3, format!("journal: unknown op {other:?}")),
    }
}

fn serve(listener: UnixListener, j: Arc<MemJournal>) {
    for conn in listener.incoming().flatten() {
        let mut line = String::new();
        if BufReader::new(&conn).read_line(&mut line).unwrap_or(0) == 0 {
            continue;
        }
        let argv: Vec<String> = serde_json::from_str(line.trim()).unwrap();
        let (code, body) = answer(&j, &argv);
        let mut conn: UnixStream = conn;
        let _ = writeln!(conn, "{}", serde_json::json!({"exit_code": code, "len": body.len()}));
        let _ = conn.write_all(body.as_bytes());
    }
}

fn spawn_service(dir: &TempDir) -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = dir.join(format!("lc-{}.sock", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst))).to_str().unwrap().to_string();
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || serve(listener, MemJournal::new()));
    path
}

fn children() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .map(|c| c.split_whitespace().count())
        .sum()
}

fn event(id: &str) -> Event {
    Event {
        id: EventId(id.into()),
        stream: StreamKey("s".into()),
        kind: EventKind::new("t", "body 'with' \\ quotes"),
        at: 5,
        cause: None,
        correlation: CorrelationId(id.into()),
    }
}

#[test]
fn the_whole_suite_through_the_socket() {
    let dir = TempDir::new("rt-socket");
    let rig = InProcessRig(|| {
        let path = spawn_service(&dir);
        Arc::new(SocketJournal::new(path)) as Arc<dyn Journal>
    });
    run_all(&rig);
}

#[test]
fn a_journal_append_and_read_launch_no_child_process() {
    let dir = TempDir::new("rt-socket-children");
    let j = SocketJournal::new(spawn_service(&dir));
    let before = children();
    let off = j.append(&event("e1")).unwrap();
    assert_eq!(j.append(&event("e1")).unwrap(), off, "append is idempotent on the event id");
    let read = j.read_from(0, 10).unwrap();
    assert_eq!(read, vec![(off, event("e1"))]);
    assert_eq!(children(), before, "the socket journal launches nothing");
}

#[test]
fn a_dead_socket_is_cannot_tell_naming_the_path() {
    let dir = TempDir::new("rt-socket-down");
    let path = dir.join("none.sock");
    let err = SocketJournal::new(&path).cursor().unwrap_err();
    assert!(err.0.starts_with("cannot tell") && err.0.contains(path.to_str().unwrap()), "{err}");
}

fn spawns_dolt(src: &str) -> bool {
    src.contains("DoltCli") || src.contains("\"dolt\"")
}

#[test]
fn no_dolt_launcher_is_reachable_from_the_library() {
    assert!(spawns_dolt("let c = Command::new(\"dolt\");"), "the matcher finds a planted offender");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for f in std::fs::read_dir(src).unwrap().flatten() {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!spawns_dolt(&text), "{} launches dolt", f.path().display());
    }
}

#[test]
#[ignore = "container tier: needs RUNTIME_LC_SOCKET (a spira-lc serve over a throwaway database) and RUNTIME_STORE_DIR (that database's directory, to empty the tables between scenarios)"]
fn the_whole_suite_through_a_real_spira_lc() {
    let socket = std::env::var("RUNTIME_LC_SOCKET").expect("RUNTIME_LC_SOCKET names the spira-lc socket");
    let dir = std::env::var("RUNTIME_STORE_DIR").expect("RUNTIME_STORE_DIR names the throwaway database directory");
    let rig = InProcessRig(move || {
        runtime::store::StoreJournal::new(common::DoltCli { dir: dir.clone().into() }).reset().expect("empty the runtime tables");
        Arc::new(SocketJournal::new(&socket)) as Arc<dyn Journal>
    });
    run_all(&rig);
}
