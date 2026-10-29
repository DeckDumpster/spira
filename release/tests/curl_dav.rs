//! `CurlDav` against a live `sccache-dav`: the status mapping and the artifact protocol over
//! the real wire, where the unit tests use a directory-backed one.

use release::artifacts::{self, CurlDav, Dav, Fetched};
use sccache_dav::{router, telemetry::Tally, AppState};
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

fn serve(root: std::path::PathBuf, token: Option<&str>) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    let token = token.map(str::to_string);
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let state = Arc::new(AppState { root, token, tally: Arc::new(Tally::default()) });
            let _ = axum::serve(listener, router(state)).await;
        });
    });
    rx.recv().unwrap()
}

fn dav(addr: SocketAddr) -> CurlDav {
    CurlDav { endpoint: format!("http://{addr}/") }
}

#[test]
fn a_stored_file_is_a_hit_and_an_absent_one_is_a_miss_not_an_error() {
    let tmp = testkit::TempDir::new("curl-dav-status");
    let store = tmp.path().join("store");
    fs::create_dir_all(store.join("artifacts/k")).unwrap();
    fs::write(store.join("artifacts/k/MANIFEST"), "hello").unwrap();
    let d = dav(serve(store, None));
    let dest = tmp.path().join("got");
    assert_eq!(d.get_to("artifacts/k/MANIFEST", &dest), Ok(true));
    assert_eq!(fs::read_to_string(&dest).unwrap(), "hello");
    assert_eq!(d.get_to("artifacts/k/absent", &dest), Ok(false));
}

#[test]
fn a_refusing_or_unreachable_store_is_an_error_never_a_miss() {
    let tmp = testkit::TempDir::new("curl-dav-errors");
    let d = dav(serve(tmp.path().join("store"), Some("secret")));
    let dest = tmp.path().join("got");
    let e = d.get_to("artifacts/k/MANIFEST", &dest).unwrap_err();
    assert!(e.contains("401"), "{e}");
    let src = tmp.path().join("src");
    fs::write(&src, "x").unwrap();
    assert!(d.put_from("artifacts/k/MANIFEST", &src).is_err(), "a refused PUT is not stored");

    let sick = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sick_addr = sick.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut c in sick.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = c.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let e = dav(sick_addr).get_to("artifacts/k/MANIFEST", &dest).unwrap_err();
    assert!(e.contains("500"), "a failing store is an error, not a miss: {e}");

    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    assert!(dav(gone).get_to("artifacts/k/MANIFEST", &dest).is_err(), "connection refused is an error");
}

#[test]
fn a_build_published_over_the_wire_is_fetched_back_intact_and_damage_is_a_miss() {
    let tmp = testkit::TempDir::new("curl-dav-roundtrip");
    let store = tmp.path().join("store");
    let d = dav(serve(store.clone(), None));
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for n in ["one", "two"] {
        testkit::write_exe(bin.join(n), &format!("#!/bin/sh\necho {n}\n"));
    }
    let key = artifacts::key("tree", "tc 1.0");
    let (up, down) = (tmp.path().join("up"), tmp.path().join("down"));
    fs::create_dir_all(&up).unwrap();
    fs::create_dir_all(&down).unwrap();

    assert_eq!(artifacts::fetch(&d, &key, &tmp.path().join("early"), &down), Fetched::Miss("not stored".into()));
    artifacts::publish(&d, &key, "tree", "tc 1.0", &bin, &up).unwrap();
    let into = tmp.path().join("got");
    assert_eq!(artifacts::fetch(&d, &key, &into, &down), Fetched::Hit);
    assert_eq!(fs::read_to_string(into.join("two")).unwrap(), "#!/bin/sh\necho two\n");

    let tar = store.join("artifacts").join(&key).join(artifacts::TARBALL);
    let mut bytes = fs::read(&tar).unwrap();
    bytes[600] ^= 0xff;
    fs::write(&tar, bytes).unwrap();
    let again = tmp.path().join("again");
    assert!(matches!(artifacts::fetch(&d, &key, &again, &down), Fetched::Miss(_)));
    assert!(!again.exists(), "a damaged upload installs nothing");
}
