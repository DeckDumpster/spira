//! After systemd's SIGTERM the stopped `spira-lc serve` leaves a non-empty `-wal`, and
//! `compact` still compacts the store. This spawns the binary because the defect is the server
//! process exiting with its connections open, which no in-process call can show
//! (law-a-unit-test-calls-no-binary).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use state_store::{Backend, Object, Rev, Write};
use state_store_sqlite::{Options, SqliteBackend};

const ROWS: u64 = 20;

/// A control store with free pages and committed rows; the free pages need raw SQL beside the store.
#[allow(clippy::disallowed_methods)]
fn seed(path: &std::path::Path) {
    {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE junk(b BLOB)").unwrap();
        for _ in 0..400 {
            conn.execute("INSERT INTO junk VALUES (zeroblob(4000))", []).unwrap();
        }
        conn.execute_batch("DROP TABLE junk").unwrap();
    }
    let b = SqliteBackend::open(path, Options::default()).unwrap();
    let mut head = b.head().unwrap();
    for n in 0..ROWS {
        let write = Write::Put { key: format!("seed/{n}"), value: Object::new(serde_json::json!({ "n": n })).encode(), create_rev: Rev::new(head + 1, 0), prev: None };
        head = b.commit(head, &[write]).unwrap();
    }
}

struct Server {
    child: std::process::Child,
    store: std::path::PathBuf,
    _dir: testkit::TempDir,
}

/// Starts `spira-lc serve` over a seeded store and returns once it has said ready.
fn start_server() -> Server {
    let dir = testkit::TempDir::new("spira-lc-compact-after-stop");
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
    let mut said = String::new();
    while !line.contains(" ready after ") {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "the server stopped before it said ready: {said}");
        said.push_str(&line);
    }
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    Server { child, store, _dir: dir }
}


#[test_id(t1fe93r)]
#[test]
fn compact_folds_the_wal_a_sigterm_stopped_server_leaves_and_loses_no_row() {
    let Server { mut child, store, _dir } = start_server();
    let head_before = SqliteBackend::open(&store, Options::default()).ok().map(|_| ());
    assert!(head_before.is_none(), "the running server holds the opener lock");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    child.wait().unwrap();

    let wal = std::path::PathBuf::from(format!("{}-wal", store.display()));
    assert!(std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0), "a stopped server leaves a WAL");

    let report = state_store_sqlite::compact::compact(&store).unwrap();
    assert_eq!(report.auto_vacuum, 2);
    assert_eq!(report.free_bytes_after, 0);
    assert!(report.wal_bytes_before > 0);
    

    let b = SqliteBackend::open(&store, Options::default()).unwrap();
    let head = b.head().unwrap();
    for n in 0..ROWS {
        assert!(b.get(&format!("seed/{n}"), head).unwrap().is_some(), "seed/{n}");
    }
}
