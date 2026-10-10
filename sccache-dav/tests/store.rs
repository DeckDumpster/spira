//! Driven over a real socket (loom's own idiom: see loom/tests/endpoint.rs) with hand-rolled
//! HTTP, because the methods under test — PROPFIND, MKCOL — have no constructor on a typed
//! HTTP client this workspace already depends on, and the point of these tests is the wire
//! contract opendal's webdav client actually drives (confirmed by reading sccache 0.18.0's
//! and opendal-service-webdav 0.56.0's own source — see sccache-dav/DESIGN.md).

use sccache_dav::{router, AppState};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static UNIQUE: AtomicU32 = AtomicU32::new(0);

fn scratch() -> testkit::TempDir {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    testkit::TempDir::new(&format!("sccache-dav-{n}"))
}

async fn spawn(root: std::path::PathBuf, token: Option<&str>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let addr = listener.local_addr().expect("its address");
    let state = Arc::new(AppState { root, token: token.map(str::to_string) });
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

struct Resp {
    code: u16,
    headers: String,
    body: Vec<u8>,
}

async fn req(addr: SocketAddr, method: &str, path: &str, auth: Option<&str>, body: &[u8]) -> Resp {
    let mut s = TcpStream::connect(addr).await.expect("a connection");
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: sccache-dav\r\nConnection: close\r\n");
    if let Some(a) = auth {
        head.push_str(&format!("Authorization: {a}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).await.expect("a request head");
    if !body.is_empty() {
        s.write_all(body).await.expect("a request body");
    }
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.expect("a response");
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("headers then a body");
    let head_bytes = &buf[..split];
    let body_bytes = buf[split + 4..].to_vec();
    let headers = String::from_utf8_lossy(head_bytes).into_owned();
    let code = headers
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status in {headers}"));
    Resp { code, headers, body: body_bytes }
}

#[tokio::test]
async fn get_on_a_missing_key_is_404() {
    let dir = scratch();
    let addr = spawn(dir.path().to_path_buf(), None).await;
    let r = req(addr, "GET", "/a/b/c/abc123", None, b"").await;
    assert_eq!(r.code, 404, "{}", r.headers);
}

#[tokio::test]
async fn put_then_get_round_trips_the_sharded_key_sccache_actually_uses() {
    // sccache's own normalize_key: "abcdef" -> "a/b/c/abcdef" (src/cache/utils.rs). The
    // directories do not exist yet — PUT must create them, the way opendal's webdav_mkcol
    // walk expects a real store to.
    let dir = scratch();
    let addr = spawn(dir.path().to_path_buf(), None).await;
    let key = "/a/b/c/abcdef0123456789";
    let put = req(addr, "PUT", key, None, b"compiled-object-bytes").await;
    assert_eq!(put.code, 201, "{}", put.headers);
    let get = req(addr, "GET", key, None, b"").await;
    assert_eq!(get.code, 200, "{}", get.headers);
    assert_eq!(get.body, b"compiled-object-bytes");
    assert!(dir.path().join("a/b/c/abcdef0123456789").is_file(), "the file landed on disk, not just in memory");
}

#[tokio::test]
async fn propfind_depth0_reports_a_file_with_its_size_and_a_directory_as_a_collection() {
    let dir = scratch();
    std::fs::create_dir_all(dir.path().join("a/b")).expect("the directory");
    std::fs::write(dir.path().join("a/b/file"), b"12345").expect("the file");
    let addr = spawn(dir.path().to_path_buf(), None).await;

    let on_dir = req(addr, "PROPFIND", "/a/b", None, b"").await;
    assert_eq!(on_dir.code, 207, "{}", on_dir.headers);
    let dir_body = String::from_utf8_lossy(&on_dir.body).into_owned();
    assert!(dir_body.contains("<D:collection/>"), "{dir_body}");

    let on_file = req(addr, "PROPFIND", "/a/b/file", None, b"").await;
    assert_eq!(on_file.code, 207, "{}", on_file.headers);
    let file_body = String::from_utf8_lossy(&on_file.body).into_owned();
    assert!(file_body.contains("<D:getcontentlength>5</D:getcontentlength>"), "{file_body}");
    assert!(!file_body.contains("<D:collection/>"), "{file_body}");

    let missing = req(addr, "PROPFIND", "/nope", None, b"").await;
    assert_eq!(missing.code, 404, "{}", missing.headers);
}

#[tokio::test]
async fn mkcol_is_idempotent_and_makes_every_missing_ancestor() {
    let dir = scratch();
    let addr = spawn(dir.path().to_path_buf(), None).await;
    let first = req(addr, "MKCOL", "/x/y/z", None, b"").await;
    assert_eq!(first.code, 201, "{}", first.headers);
    assert!(dir.path().join("x/y/z").is_dir());
    // A second MKCOL on the same collection must not be refused (opendal's own client
    // accepts 405 here too, but this store always succeeds — see mkcol()'s doc comment).
    let second = req(addr, "MKCOL", "/x/y/z", None, b"").await;
    assert_eq!(second.code, 201, "{}", second.headers);
}

#[tokio::test]
async fn delete_removes_a_key_and_404s_the_second_time() {
    let dir = scratch();
    std::fs::write(dir.path().join("k"), b"v").expect("the file");
    let addr = spawn(dir.path().to_path_buf(), None).await;
    let first = req(addr, "DELETE", "/k", None, b"").await;
    assert_eq!(first.code, 204, "{}", first.headers);
    assert!(!dir.path().join("k").exists());
    let second = req(addr, "DELETE", "/k", None, b"").await;
    assert_eq!(second.code, 404, "{}", second.headers);
}

#[tokio::test]
async fn a_configured_token_is_required_and_checked_exactly() {
    let dir = scratch();
    std::fs::write(dir.path().join("k"), b"v").expect("the file");
    let addr = spawn(dir.path().to_path_buf(), Some("s3cr3t")).await;

    let none = req(addr, "GET", "/k", None, b"").await;
    assert_eq!(none.code, 401, "{}", none.headers);

    let wrong = req(addr, "GET", "/k", Some("Bearer nope"), b"").await;
    assert_eq!(wrong.code, 401, "{}", wrong.headers);

    let right = req(addr, "GET", "/k", Some("Bearer s3cr3t"), b"").await;
    assert_eq!(right.code, 200, "{}", right.headers);
}

#[tokio::test]
async fn path_traversal_is_refused_not_resolved() {
    let dir = scratch();
    // A sibling file outside the store root this request would otherwise escape to.
    std::fs::write(dir.path().join("secret"), b"nope").expect("a sibling file");
    let root = dir.path().join("store");
    std::fs::create_dir_all(&root).expect("the store root");
    let addr = spawn(root, None).await;

    let escape = req(addr, "GET", "/../secret", None, b"").await;
    assert_eq!(escape.code, 400, "{}", escape.headers);
}

#[test]
fn config_from_env_refuses_a_wildcard_bind_and_missing_vars() {
    // Exercised in-process (no server needed): the fail-closed checks in config_from_env.
    for (addr, root, token) in [
        (None, Some("/tmp/x"), None),
        (Some("0.0.0.0:9431"), Some("/tmp/x"), None),
        (Some("192.168.1.56:9431"), None, None),
    ] {
        let _env = testkit::env(&[("SCCACHE_DAV_ADDR", addr), ("SCCACHE_DAV_ROOT", root), ("SCCACHE_DAV_TOKEN", token)]);
        assert!(sccache_dav::config_from_env().is_err(), "{addr:?} {root:?}");
    }
    let _env = testkit::env(&[
        ("SCCACHE_DAV_ADDR", Some("192.168.1.56:9431")),
        ("SCCACHE_DAV_ROOT", Some("/tmp/sccache-dav-store")),
        ("SCCACHE_DAV_TOKEN", None),
    ]);
    let cfg = sccache_dav::config_from_env().expect("a valid config");
    assert_eq!(cfg.addr, "192.168.1.56:9431");
    assert!(cfg.token.is_none());
}

#[test]
fn listen_addrs_maps_config_to_listeners() {
    let lan = "192.168.1.56:9431";
    assert_eq!(sccache_dav::listen_addrs(lan, "").unwrap(), [lan]);
    assert_eq!(sccache_dav::listen_addrs(lan, "  ").unwrap(), [lan]);
    assert_eq!(sccache_dav::listen_addrs(lan, lan).unwrap(), [lan]);
    assert_eq!(sccache_dav::listen_addrs(lan, "100.64.0.9:9431").unwrap(), [lan, "100.64.0.9:9431"]);
    assert!(sccache_dav::listen_addrs(lan, "0.0.0.0:9431").is_err());
    assert!(sccache_dav::listen_addrs(lan, "*:9431").is_err());
}

fn put_aged(root: &std::path::Path, rel: &str, bytes: usize, age_secs: u64) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, vec![b'x'; bytes]).unwrap();
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
    std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
}

#[test]
fn eviction_removes_the_least_recently_used_until_under_the_cap() {
    let d = scratch();
    put_aged(d.path(), "a/oldest", 100, 3000);
    put_aged(d.path(), "b/middle", 100, 2000);
    put_aged(d.path(), "c/newest", 100, 1000);
    put_aged(d.path(), "c/inflight.tmp-77", 500, 9000);

    let ev = sccache_dav::evict_to_cap(d.path(), 250);

    assert_eq!((ev.files, ev.bytes, ev.remaining), (1, 100, 200));
    assert!(!d.path().join("a/oldest").exists());
    assert!(d.path().join("b/middle").exists() && d.path().join("c/newest").exists());
    assert!(d.path().join("c/inflight.tmp-77").exists(), "an in-flight PUT is never evicted");
    assert!(d.path().join("a").is_dir(), "shard directories stay");
}

#[test]
fn eviction_under_the_cap_deletes_nothing() {
    let d = scratch();
    put_aged(d.path(), "a/f", 100, 5000);
    assert_eq!(sccache_dav::evict_to_cap(d.path(), 100).files, 0);
    assert!(d.path().join("a/f").exists());
}

#[tokio::test]
async fn a_get_marks_the_entry_read_so_it_outlives_an_unread_newer_one() {
    let d = scratch();
    put_aged(d.path(), "a/hit", 100, 5000);
    put_aged(d.path(), "b/unread", 100, 1000);
    let addr = spawn(d.path().to_path_buf(), None).await;

    assert_eq!(req(addr, "GET", "/a/hit", None, b"").await.code, 200);
    sccache_dav::evict_to_cap(d.path(), 100);

    assert!(d.path().join("a/hit").exists(), "read just now");
    assert!(!d.path().join("b/unread").exists());
}
