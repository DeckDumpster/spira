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
        metrics_endpoint: None,
        broker: fake_broker().0.to_string_lossy().into_owned(),
        desired_dir: String::new(),
        units_install: "/nonexistent/units-install".into(),
    }
}

fn ops_run_dir() -> testkit::TempDir {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir = testkit::TempDir::new(&format!("loom-ops-{n}"));
    std::fs::create_dir_all(&dir).expect("a run directory");
    dir
}

// ── /units ───────────────────────────────────────────────────────────────────

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}


// ── Now and the shell ────────────────────────────────────────────────────────

fn write_snapshot(run: &std::path::Path, snap: &cockpit_ops::lcview::Snapshot) {
    std::fs::create_dir_all(run.join("lcview")).unwrap();
    std::fs::write(run.join("lcview").join("snapshot.json"), serde_json::to_string(snap).unwrap()).unwrap();
}

#[tokio::test]
async fn the_lifecycle_api_says_a_child_of_a_parent_in_delivery_is_blocked_as_the_pane_does() {
    use cockpit_ops::lcview::{apply_verdicts, live_row, view, Snapshot};
    use spira_claim::verdicts::{Entry, Kind, VerdictFile};
    let now = now_s();
    let mut snap = Snapshot { now, ..Default::default() };
    for raw in [
        r#"{"bead_id":"sp-p","state":"IN_DELIVERY","holds":"[]","repo":"spira","blocked_by":[]}"#,
        r#"{"bead_id":"sp-p.1","state":"READY","holds":"[]","repo":"spira","blocked_by":[],"title":"the child"}"#,
    ] {
        let (row, meta) = live_row(&serde_json::from_str(raw).unwrap()).unwrap();
        snap.meta.insert(row.id.clone(), meta);
        snap.rows.push(row);
    }
    let file = VerdictFile {
        computed_at: spira_claim::verdicts::stamp(now),
        entries: vec![Entry { id: "sp-p.1".into(), verdict: Kind::Blocked, blocker: Some("sp-p".into()), runes: vec![] }],
    };
    apply_verdicts(&mut snap, &file);
    let pane_claim = |v: &cockpit_ops::lcview::View| v.machine.iter().find(|m| m.name == "READY").unwrap().beads.iter().find(|b| b.id == "sp-p.1").unwrap().claim.clone();
    assert_eq!(pane_claim(&view(&snap)), "blocked: sp-p");

    let run = ops_run_dir();
    write_snapshot(&run, &snap);
    let addr = spawn(ops_cfg(run.to_str().unwrap())).await;
    let (code, api) = get(addr, "/api/lifecycle").await;
    assert_eq!(code, 200, "{api}");
    let v: Value = serde_json::from_str(&api).expect("json");
    let ready = v["view"]["machine"].as_array().unwrap().iter().find(|m| m["name"] == "READY").unwrap();
    let bead = ready["beads"].as_array().unwrap().iter().find(|b| b["id"] == "sp-p.1").unwrap();
    assert_eq!(bead["claim"], "blocked: sp-p", "{api}");
    assert_eq!(v["view"]["next_count"], 0, "{api}");
}

static BROKER: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();

/// One fake broker for the process, answering recorded
/// `run-jobs` responses by run id and logging every call.
fn fake_broker() -> &'static (PathBuf, PathBuf) {
    BROKER.get_or_init(|| {
        let dir = testkit::TempDir::new("loom-broker").to_path_buf();
        std::fs::create_dir_all(&dir).expect("a broker dir");
        let (bin, log) = (dir.join("broker"), dir.join("calls.log"));
        let red = r#"{"run":{"id":9001,"url":"https://github.com/o/dumpstar/actions/runs/9001","status":"completed","conclusion":"failure","head_sha":"e6e8c19"},"jobs":[{"name":"lint","conclusion":"success","status":"completed","url":"https://github.com/o/dumpstar/actions/runs/9001/job/1","failed_step":null,"error_lines":[]},{"name":"gate","conclusion":"failure","status":"completed","url":"https://github.com/o/dumpstar/actions/runs/9001/job/2","failed_step":"svelte-check","error_lines":["Error: Type 'string' is not assignable to type 'number'. (ts)","svelte-check found 1 error"]}]}"#;
        let green = r#"{"run":{"id":9002,"url":"https://github.com/o/deckdumpster/actions/runs/9002","status":"completed","conclusion":"success","head_sha":"aa"},"jobs":[{"name":"gate","conclusion":"success","status":"completed","url":"https://github.com/o/deckdumpster/actions/runs/9002/job/3","failed_step":null,"error_lines":[]}]}"#;
        script(&bin, &format!("#!/bin/sh\necho \"$*\" >> {log}\ncase \"$3\" in\n*/9001) cat <<'EOF'\n{red}\nEOF\n;;\n*/9002) cat <<'EOF'\n{green}\nEOF\n;;\n*) echo boom >&2; exit 1;;\nesac\n", log = log.display()));
        (bin, log)
    })
}

// ── /rounds ──────────────────────────────────────────────────────────────────

