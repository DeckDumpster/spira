#[macro_use]
extern crate spira_test_id;
use spira_config::build::{find_on, server_unit_args, Store};
use std::process::Command;

#[test_id(tmeglch)]
#[test]
fn a_session_tmpdir_removed_under_the_server_does_not_break_a_compile() {
    let Some(sccache) = find_on(&std::env::var("PATH").unwrap_or_default(), "sccache") else {
        assert!(std::env::var_os("SPIRA_ALLOW_NO_SCCACHE").is_some(), "sccache is not on PATH: this test runs the real server and cannot run without it (set SPIRA_ALLOW_NO_SCCACHE=1 to skip)");
        return;
    };
    let t = testkit::TempDir::new("sccache-server-tmp");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string();
    let (cache, session, server_tmp) = (t.path().join("cache"), t.path().join("session-tmp-1"), t.path().join("run/sccache-server/tmp"));
    for d in [&session, &server_tmp] {
        std::fs::create_dir_all(d).unwrap();
    }
    let sc = sccache.to_str().unwrap();
    let base: Vec<(&str, &str)> = vec![("SCCACHE_DIR", cache.to_str().unwrap()), ("SCCACHE_SERVER_PORT", &port), ("SCCACHE_WEBDAV_ENDPOINT", "")];
    let store = Store { endpoint: "http://127.0.0.1:1".into(), key_prefix: "/".into() };

    let mut server = Command::new(sc);
    server.envs(base.iter().cloned()).env("TMPDIR", &session);
    for a in server_unit_args(&sccache, &store, &server_tmp) {
        if let Some((k, v)) = a.strip_prefix("--setenv=").and_then(|kv| kv.split_once('=')) {
            if !k.starts_with("SCCACHE_WEBDAV") {
                server.env(k, v);
            }
        }
    }
    let mut child = server.spawn().unwrap();
    let ready = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50)); // clock-safe: caps a wait for the server's port to accept
        std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).is_ok()
    });
    assert!(ready, "the server never answered");
    std::fs::remove_dir_all(&session).unwrap();

    let src = t.path().join("one.rs");
    std::fs::write(&src, "fn main() {}\n").unwrap();
    std::fs::create_dir_all(t.path().join("out")).unwrap();
    let o = Command::new(sc).arg("rustc").envs(base.iter().cloned()).env_remove("TMPDIR").current_dir(t.path()).args(["--edition=2021", "--crate-name", "one", "--crate-type", "lib", "--emit=dep-info,link", "--out-dir"]).arg(t.path().join("out")).arg(&src).output().unwrap();
    let _ = Command::new(sc).arg("--stop-server").envs(base.iter().cloned()).output();
    let _ = child.wait();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
