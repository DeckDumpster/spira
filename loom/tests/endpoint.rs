//! The endpoint driven over a real socket, against a `spira-lc` it spawns as a real process.
//!
//! Every assertion that something is ABSENT is paired with one that the same check can see
//! something present.

use loom::{router, Config, Loom};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static UNIQUE: AtomicU32 = AtomicU32::new(0);

fn scratch(kind: &str) -> testkit::TempDir {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    testkit::TempDir::new(&format!("loom-{kind}-{n}"))
}

/// testkit::write_exe, never `fs::write` then exec: a write descriptor held while another
/// test thread spawns a child makes the exec fail with ETXTBSY (testkit/DESIGN.md).
fn script(path: &PathBuf, body: &str) {
    testkit::write_exe(path, body);
}

async fn spawn(cfg: Config) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let addr = listener.local_addr().expect("its address");
    let app = router(Arc::new(Loom::new(cfg)));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// A GET over a real socket, so the routing and the status line are under test rather than a
/// handler called directly.
async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.expect("a connection");
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: loom\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .expect("a request");
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.expect("a response");
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("headers then a body");
    assert!(
        !head.to_ascii_lowercase().contains("transfer-encoding: chunked"),
        "the body is chunked and this client does not de-chunk: {head}"
    );
    let code = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status in {head}"));
    (code, body.to_string())
}

#[tokio::test]
async fn the_retired_list_view_is_gone_and_no_loom_route_reads_bd_listings() {
    let dir = scratch("retired");
    let counter = dir.join("calls");
    std::fs::write(&counter, b"").expect("an empty counter");
    let real = fake_lc(&dir.to_path_buf());
    let shim = dir.join("shim-lc");
    script(&shim, &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n", counter.display(), real));
    let mut c = ops_cfg("");
    c.lc = shim.to_string_lossy().into_owned();
    let addr = spawn(c).await;

    // POSITIVE CONTROL: the replacement page answers through the same shim.
    assert_eq!(get(addr, "/stuck").await.0, 200);
    for path in ["/api/beads", "/model.js", "/app.js"] {
        assert_eq!(get(addr, path).await.0, 404, "{path} must no longer be served");
    }
    let before = std::fs::read_to_string(&counter).unwrap();
    let (code, _) = get(addr, "/").await;
    assert_eq!(code, 303, "/ hands over to the stuck page");
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), before, "/ must not query the store");
    let calls = std::fs::read_to_string(&counter).unwrap();
    assert!(calls.contains("ops-gantt"), "the shim saw the stuck page's reads: {calls}");
    assert!(!calls.contains("list") && !calls.contains("ops_live") && !calls.contains("ops_recent"), "{calls}");
}

// ── /api/ops ─────────────────────────────────────────────────────────────────
//
// This route never reads the lifecycle store — it parses two small files under `run` and
// checks the halt stamp, so these tests only ever set `run`.

fn ops_cfg(run: &str) -> Config {
    Config {
        lc: String::new(),
        extra_path: vec![],
        budget: Duration::from_millis(20_000),
        cache: Duration::from_secs(30),
        addr: String::new(),
        run: run.to_string(),
        instance: "test".to_string(),
        systemctl: "systemctl".to_string(),
    }
}

fn ops_run_dir() -> testkit::TempDir {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir = testkit::TempDir::new(&format!("loom-ops-{n}"));
    std::fs::create_dir_all(&dir).expect("a run directory");
    dir
}

async fn ops_json(addr: SocketAddr) -> (u16, Value) {
    let (code, body) = get(addr, "/api/ops").await;
    let v = serde_json::from_str(&body).unwrap_or_else(|e| panic!("body is not JSON ({e}): {body}"));
    (code, v)
}

#[tokio::test]
async fn ops_round_trips_shell_quoting_and_hides_absent_keys() {
    let run = ops_run_dir();
    // SP_APOS decodes to two apostrophes — close-quote, backslash-apostrophe, open-quote,
    // twice — the same encoding cockpit-collect's merge (formerly cockpit.sh's shq()) uses.
    std::fs::write(
        run.join("cockpit.env"),
        "SP_PLAIN='hello world'\nSP_APOS=''\\'''\\'''\nSP_MISSING_CONTROL='present'\n",
    )
    .expect("a cockpit.env");
    std::fs::write(run.join("budget.env"), "").expect("an empty budget.env");

    let addr = spawn(ops_cfg(run.to_str().unwrap())).await;
    let (code, v) = ops_json(addr).await;
    assert_eq!(code, 200, "{v}");

    assert_eq!(v["SP_PLAIN"], "hello world");
    assert_eq!(v["SP_APOS"], "''");
    // POSITIVE CONTROL: SP_MISSING_CONTROL proves the lookup ran before SP_MISSING's absence
    // below means anything.
    assert_eq!(v["SP_MISSING_CONTROL"], "present");
    assert!(
        v.get("SP_MISSING").is_none(),
        "a key absent from cockpit.env must be absent from the JSON, not defaulted: {v}"
    );
    assert!(v["cockpit_env_age_s"].is_u64(), "{v}");
}

#[tokio::test]
async fn ops_reports_halted_only_from_the_stamp_not_the_snapshot() {
    let run = ops_run_dir();
    std::fs::write(run.join("cockpit.env"), "SP_PLAIN='x'\n").expect("a cockpit.env");
    std::fs::write(run.join("budget.env"), "").expect("an empty budget.env");

    let addr = spawn(ops_cfg(run.to_str().unwrap())).await;
    let (code, v) = ops_json(addr).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["halted"], false, "no world.halted stamp: {v}");

    // POSITIVE CONTROL: the same run dir with the stamp written — halted must flip. A
    // collector that renders a halted world as healthy because the banner read the snapshot
    // instead of the stamp is exactly the defect this route exists to avoid.
    std::fs::write(run.join("world.halted"), "2026-09-24T00:00:00Z\nwhy: acceptance\n")
        .expect("a halt stamp");
    let addr2 = spawn(ops_cfg(run.to_str().unwrap())).await;
    let (code, v) = ops_json(addr2).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["halted"], true, "{v}");
    assert_eq!(v["halted_why"], "acceptance", "{v}");
}

/// A fake `spira-lc` answering the three reads the stuck page makes, from canned JSON.
fn fake_lc(dir: &PathBuf) -> String {
    let bin = dir.join("spira-lc");
    script(
        &bin,
        r#"#!/bin/sh
now=$(date +%s)
case "$1 $2" in
  "ops-gantt "*) echo "{\"now\":$now,\"events\":[{\"seq\":1,\"bead_id\":\"sp-2cah6\",\"from_state\":\"SUBMITTED\",\"to_state\":\"CERTIFIED\",\"at\":$((now-72000))},{\"seq\":2,\"bead_id\":\"sp-fine\",\"from_state\":\"READY\",\"to_state\":\"WORKING\",\"at\":$((now-600))}]}" ;;
  "ops-view ops_dwell") echo "[{\"bead_id\":\"sp-2cah6\",\"state\":\"CERTIFIED\",\"holds\":\"[]\",\"priority\":1,\"title\":\"stuck one\",\"entered_at\":$((now-72000))},{\"bead_id\":\"sp-fine\",\"state\":\"WORKING\",\"holds\":\"[]\",\"priority\":2,\"title\":\"fine one\",\"entered_at\":$((now-600))}]" ;;
  "ops-view ops_dwell_p95") echo '[{"state":"CERTIFIED","p95_s":7200},{"state":"WORKING","p95_s":10800}]' ;;
  "ops-bead sp-2cah6") echo "{\"now\":$now,\"bead\":{\"state\":\"CERTIFIED\",\"holder\":null,\"holds\":\"[]\",\"title\":\"stuck one\"},\"events\":[{\"seq\":1,\"event\":\"Certify\",\"from_state\":\"SUBMITTED\",\"to_state\":\"CERTIFIED\",\"applied\":1,\"actor\":\"gate\",\"at\":$((now-72000))},{\"seq\":2,\"event\":\"Rework\",\"from_state\":\"CERTIFIED\",\"to_state\":\"CERTIFIED\",\"applied\":0,\"refusal\":\"no-edge\",\"actor\":\"x\",\"at\":$((now-60))}]}" ;;
  *) echo "no such bead" >&2; exit 1 ;;
esac
"#,
    );
    bin.to_string_lossy().into_owned()
}

#[tokio::test]
async fn the_stuck_page_leads_with_the_bead_past_its_p95_and_a_row_opens_its_timeline() {
    let dir = scratch("stuck");
    let mut c = ops_cfg("");
    c.lc = fake_lc(&dir.to_path_buf());
    let addr = spawn(c).await;

    let (status, page) = get(addr, "/stuck").await;
    assert_eq!(status, 200, "{page}");
    let (stuck_at, fine_at) = (page.find("/stuck/sp-2cah6").expect("stuck row"), page.find("/stuck/sp-fine").expect("healthy row"));
    assert!(stuck_at < fine_at, "the stuck bead is listed first");
    assert!(page.contains("seg s-CERTIFIED over"), "its bar is outlined");
    assert!(!page.contains("seg s-WORKING over"), "positive control: the healthy bar is not");

    let (status, detail) = get(addr, "/stuck/sp-2cah6").await;
    assert_eq!(status, 200, "{detail}");
    assert!(detail.contains("refused: no-edge"));
    assert_eq!(get(addr, "/stuck/sp-nope1").await.0, 404);
    assert_eq!(get(addr, "/stuck/a%20b").await.0, 400);
}
