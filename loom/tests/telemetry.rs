//! Every request to the endpoint is one run of component `loom`, labelled by the page asked for.

use loom::telemetry::{metered, operation};
use loom::{router, Config, Loom};
use spira_otel::metrics::fixture::Capture;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn get(addr: std::net::SocketAddr, path: &str) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: loom\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
}

#[test]
fn operations_are_a_bounded_set_of_pages() {
    assert_eq!(operation("/"), "now");
    assert_eq!(operation("/api/units"), "units");
    assert_eq!(operation("/stuck/sp-abc"), "stuck");
    assert_eq!(operation("/rounds/spira/sp-1/log"), "rounds");
    assert_eq!(operation("/wp-login.php"), "other");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_pushes_one_run_labelled_by_page_and_status_class() {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("loom-telemetry");
    let cfg = Config {
        lc: "/nonexistent/lc".into(),
        extra_path: vec![],
        budget: std::time::Duration::from_millis(500),
        cache: std::time::Duration::from_secs(1),
        addr: String::new(),
        run: tmp.path().display().to_string(),
        instance: "t".into(),
        systemctl: "/nonexistent/systemctl".into(),
        duckdb: "/nonexistent/duckdb".into(),
        broker: "/nonexistent/broker".into(),
        desired_dir: String::new(),
        units_install: "/nonexistent/units-install".into(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = metered(router(Arc::new(Loom::new(cfg))), Some(cap.addr.clone()));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    get(addr, "/no-such-page").await;
    tokio::task::spawn_blocking(move || cap.assert_run("loom", "other", "client_error")).await.unwrap();
}
