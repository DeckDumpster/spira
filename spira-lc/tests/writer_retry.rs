//! A content write whose writer cannot be built fails naming the cause, and the next write builds
//! it again once the cause is gone. This spawns `spira-lc serve` because the cell that must not
//! keep the failure lives for the life of the server process (law-a-unit-test-calls-no-binary).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use state_store_sqlite::{Options, SqliteBackend};

fn closed_port() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port().to_string()
}

fn write_comment(socket: &std::path::Path) -> spira_store::lc_call::Reply {
    let argv: Vec<String> = ["content", "comments", "add", "sp-none", "hello"].iter().map(|s| s.to_string()).collect();
    let stream = spira_store::lc_call::connect(socket).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    spira_store::lc_call::exchange_once(stream, &argv, None).unwrap_or_else(|_| panic!("no answer"))
}

#[test]
fn a_content_write_builds_the_writer_again_once_its_password_file_exists() {
    let dir = testkit::TempDir::new("spira-lc-writer-retry");
    let run_dir = dir.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let store = dir.join("control.db");
    drop(SqliteBackend::open(&store, Options::default()).unwrap());
    let socket = dir.join("lc.sock");
    let beads = dir.join("beads");
    std::fs::create_dir_all(beads.join(".beads")).unwrap();
    std::fs::write(beads.join(".beads/metadata.json"), r#"{"dolt_database":"sptest_writer"}"#).unwrap();
    let secret = dir.join("writer.pw");
    let port = closed_port();
    let (bd, d, r, s, sock, pw) = (beads.display().to_string(), dir.display().to_string(), run_dir.display().to_string(), store.display().to_string(), socket.display().to_string(), secret.display().to_string());
    let toml = spira_config::process::fixture_toml(
        &dir,
        &[
            ("SPIRA_WORKSPACES", &d),
            ("SPIRA_RUN", &r),
            ("SPIRA_LC_STORE_PATH", &s),
            ("SPIRA_LC_SOCKET", &sock),
            ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"),
            ("SPIRA_DB", &bd),
            ("SPIRA_BEADS_SQL_HOST", "127.0.0.1"),
            ("SPIRA_BEADS_SQL_PORT", &port),
            ("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE", &pw),
        ],
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

    let first = write_comment(&socket);
    std::fs::write(&secret, "secret\n").unwrap();
    let second = write_comment(&socket);
    let _ = child.kill();
    let _ = child.wait();

    let reading = format!("reading {pw}");
    assert!(first.code != 0 && first.err.contains(&reading), "first write: code {} err {:?}", first.code, first.err);
    assert!(!second.err.contains(&reading), "the failed build was kept: {:?}", second.err);
}
