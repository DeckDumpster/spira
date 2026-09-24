//! The endpoint against a REAL `bd` on a throwaway database, driven over a real socket.
//!
//! `spira/test-loom.sh` builds the fixture and runs this; it is the only intended entry
//! point, and these tests fail loudly rather than skipping when its environment is absent. A
//! suite that silently no-ops when its fixture is missing reports "0 failed" for a run that
//! checked nothing, which is indistinguishable from a pass in an exit code.
//!
//! Every assertion that something is ABSENT is paired with one that the same check can see
//! something present: a closed bead is in the fixture and must not appear, an invocation
//! counter is shown moving before its stillness means anything, and the budget is shown
//! admitting a query before it is shown refusing one.

use loom::{router, Config, Loom};
use serde_json::Value;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The fixture the shell suite built, and the real `bd` that answers for it.
fn fixture() -> (String, String) {
    let db = std::env::var("LOOM_TEST_DB").unwrap_or_default();
    let bd = std::env::var("LOOM_TEST_BD").unwrap_or_default();
    assert!(
        !db.is_empty() && !bd.is_empty(),
        "LOOM_TEST_DB and LOOM_TEST_BD are unset — run spira/test-loom.sh, which builds the \
         fixture database these tests read"
    );
    (db, bd)
}

static UNIQUE: AtomicU32 = AtomicU32::new(0);

/// A directory holding a `bd` that records every call and then execs the real one.
///
/// COUNTING INVOCATIONS, NEVER TIMING THEM. "The second request was fast" is satisfied by a
/// warm page cache, a lucky scheduler or a query that failed early; only the count answers
/// whether the process boundary was crossed.
fn counting_bd(real: &str) -> (String, PathBuf) {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("loom-shim-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a shim directory");
    let counter = dir.join("calls");
    std::fs::write(&counter, b"").expect("an empty counter");
    let shim = dir.join("bd");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf 'x\\n' >> {}\nexec {} \"$@\"\n",
            counter.display(),
            real
        ),
    )
    .expect("a shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("mode 755");
    (dir.to_string_lossy().into_owned(), counter)
}

fn calls(counter: &PathBuf) -> usize {
    std::fs::read_to_string(counter)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Every value is pinned AWAY from the shipped default, so an assertion here cannot be
/// satisfied by code that has the default written in rather than reading the key.
fn cfg(db: &str, shim: &str, budget_ms: u64, cache_s: u64) -> Config {
    Config {
        db: db.to_string(),
        extra_path: vec![shim.to_string()],
        budget: Duration::from_millis(budget_ms),
        cache: Duration::from_secs(cache_s),
        addr: String::new(),
        bd: "bd".to_string(),
        run: String::new(),
        instance: "test".to_string(),
        systemctl: "systemctl".to_string(),
    }
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

async fn json(addr: SocketAddr) -> (u16, Value) {
    let (code, body) = get(addr, "/api/beads").await;
    let v = serde_json::from_str(&body).unwrap_or_else(|e| panic!("body is not JSON ({e}): {body}"));
    (code, v)
}

#[tokio::test]
async fn the_payload_is_bounded_to_live_work_and_carries_typed_edges() {
    let (db, bd) = fixture();
    let (shim, _counter) = counting_bd(&bd);
    let addr = spawn(cfg(&db, &shim, 20_000, 30)).await;

    let (code, v) = json(addr).await;
    assert_eq!(code, 200, "{v}");

    let ids: Vec<&str> = v["beads"]
        .as_array()
        .expect("a bead array")
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    // PRESENT FIRST. Without this the absence below is also satisfied by an empty payload.
    assert!(ids.contains(&"sp-aaa"), "open bead missing from {ids:?}");
    assert!(ids.contains(&"sp-ccc"), "in-progress bead missing from {ids:?}");
    // The bound that makes a per-request read affordable: the closed bead is in the database
    // and must not be in the response.
    assert!(!ids.contains(&"sp-zzz"), "a closed bead was served: {ids:?}");
    assert_eq!(v["count"], 3);

    // The payload is RAW rows — the page derives its view model, so nothing here may be
    // pre-chewed, and the fields the page needs must survive.
    //
    // NAMED, NOT beads[0]. This read the first row of an unordered response and required
    // `labels` on it. bd omits an EMPTY labels array entirely, and three of the four fixture
    // beads declare `"labels":[]` — so the assertion passed or failed on which bead happened
    // to sort first, and on main it drew sp-ccc and went red. A suite whose verdict depends
    // on row order is not testing the splice it was written to test.
    let aaa = v["beads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "sp-aaa")
        .expect("sp-aaa in the payload");
    for field in ["id", "status", "labels", "updated_at", "issue_type", "priority"] {
        assert!(!aaa[field].is_null(), "{field} is missing from {aaa}");
    }
    // AND THE ABSENCE IS THE OTHER HALF OF THE CONTRACT. A bead with no labels is served with
    // no `labels` key at all, so the page must read it as absent rather than as an empty
    // list. Asserting it here is what stops someone "fixing" the line above by normalising
    // the payload in the server, which is exactly the pre-chewing this endpoint refuses.
    let ccc = v["beads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "sp-ccc")
        .expect("sp-ccc in the payload");
    assert!(
        ccc["labels"].is_null(),
        "a label-less bead must arrive without a labels key, not with an empty one: {ccc}"
    );

    // The body is spliced together from text that is already JSON, so a title needing escapes
    // is what proves the splice still produces a document rather than something that merely
    // starts like one.
    let beta = v["beads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "sp-bbb")
        .expect("the epic");
    assert_eq!(beta["title"], "beta \"quoted\" and a \\ backslash");

    let edges = v["edges"].as_array().expect("an edge array");
    assert_eq!(edges.len(), 2, "{edges:?}");
    // The edges are hoisted off the rows they arrived on, so the payload has exactly one
    // edge list — not a second, unfiltered one for the page to reach for by accident.
    assert!(
        v["beads"].as_array().unwrap().iter().all(|b| b.get("dependencies").is_none()),
        "a row still carries its own dependency array"
    );
    let kinds: Vec<&str> = edges.iter().filter_map(|e| e["type"].as_str()).collect();
    assert!(kinds.contains(&"blocks"), "{kinds:?}");
    assert!(kinds.contains(&"parent-child"), "{kinds:?}");
    // The type is what separates a real blocking chain from an epic's children.
    let blocks = edges.iter().find(|e| e["type"] == "blocks").expect("a blocking edge");
    assert_eq!(blocks["issue_id"], "sp-aaa");
    assert_eq!(blocks["depends_on_id"], "sp-ccc");

    // The meter is served, and it reports the configuration in force rather than the default.
    assert_eq!(v["budget_ms"], 20_000);
    assert_eq!(v["cache_s"], 30);
    assert_eq!(v["dropped_closed"], 0);
    // The fixture wires one edge INTO the closed bead, so this counter is seen carrying a
    // number rather than only ever reading zero. An edge to a bead that is not being served
    // cannot be drawn, and a graph quietly missing edges still looks like a graph.
    assert_eq!(v["dropped_edges"], 1);
    assert!(v["query_ms"].is_u64());
    assert!(v["refresh_ms"].is_u64());
}

#[tokio::test]
async fn two_requests_inside_the_window_cost_one_refresh() {
    let (db, bd) = fixture();

    let (shim, counter) = counting_bd(&bd);
    let addr = spawn(cfg(&db, &shim, 20_000, 60)).await;
    assert_eq!(calls(&counter), 0, "nothing runs before anybody looks");

    let (code, _) = json(addr).await;
    assert_eq!(code, 200);
    let cold = calls(&counter);
    assert_eq!(cold, 1, "one query answers the whole graph");

    let (code, v) = json(addr).await;
    assert_eq!(code, 200);
    assert_eq!(
        calls(&counter),
        cold,
        "the second request inside the window crossed the process boundary again"
    );
    assert!(v["age_ms"].as_u64().expect("an age") < 60_000);

    // THE POSITIVE CONTROL. The counter is shown MOVING under an expired window, so its
    // stillness above is a property of the cache rather than of a shim nobody wired up.
    let (shim2, counter2) = counting_bd(&bd);
    let addr2 = spawn(cfg(&db, &shim2, 20_000, 0)).await;
    let (code, _) = json(addr2).await;
    assert_eq!(code, 200);
    assert_eq!(calls(&counter2), 1);
    let (code, _) = json(addr2).await;
    assert_eq!(code, 200);
    assert_eq!(calls(&counter2), 2, "a zero-length window must refresh every time");
}

#[tokio::test]
async fn a_query_over_budget_is_refused_rather_than_served_late() {
    let (db, bd) = fixture();
    let (shim, counter) = counting_bd(&bd);

    // One millisecond is under the cost of spawning a process, let alone querying a database,
    // so the deadline fires on the machinery rather than on a threshold that might be met.
    let addr = spawn(cfg(&db, &shim, 1, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 503, "an over-budget query must be refused: {v}");
    assert_eq!(v["error"], "over budget");
    assert_eq!(v["budget_ms"], 1);
    assert_eq!(v["query"], "beads");
    // It was refused because the query overran, not because nothing was tried.
    assert_eq!(calls(&counter), 1);
    // And nothing stale is served in its place on the next look either.
    let (code, _) = json(addr).await;
    assert_eq!(code, 503);

    // THE POSITIVE CONTROL. The same fixture, the same shim, an honest budget: 200. Without
    // it a refusal proves only that the endpoint is broken.
    let (shim2, _) = counting_bd(&bd);
    let addr2 = spawn(cfg(&db, &shim2, 20_000, 30)).await;
    let (code, v) = json(addr2).await;
    assert_eq!(code, 200, "{v}");
}

#[tokio::test]
async fn a_query_that_fails_is_reported_as_such_and_not_as_an_empty_graph() {
    let (_db, bd) = fixture();
    let (shim, _) = counting_bd(&bd);
    // A database directory that does not exist. An empty response would read as "no live
    // work", which is the reading that looks like good news.
    let addr = spawn(cfg("/nonexistent/loom-has-no-database", &shim, 20_000, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 502, "{v}");
    assert_eq!(v["error"], "query failed");
    assert!(
        v["detail"].as_str().map(|d| !d.is_empty()).unwrap_or(false),
        "a failure must carry its reason: {v}"
    );
}

#[tokio::test]
async fn the_static_page_and_its_scripts_are_served() {
    let (db, bd) = fixture();
    let (shim, _) = counting_bd(&bd);
    let addr = spawn(cfg(&db, &shim, 20_000, 30)).await;

    // PRESENCE FIRST. The page must be there before its absence means anything.
    let (code, body) = get(addr, "/").await;
    assert_eq!(code, 200, "the page must be at /");
    assert!(body.contains("</html>"), "/ must serve an HTML document: {body:.200}");

    let (code, js) = get(addr, "/model.js").await;
    assert_eq!(code, 200, "the model script must be at /model.js");
    assert!(!js.is_empty(), "/model.js must not be empty");

    let (code, app) = get(addr, "/app.js").await;
    assert_eq!(code, 200, "the painter script must be at /app.js");
    assert!(!app.is_empty(), "/app.js must not be empty");

    // POSITIVE CONTROL for the absence assertions that follow. /api/beads is known to answer;
    // a route that 404s on every path would pass the absent-route checks above.
    let (code, _) = json(addr).await;
    assert_eq!(code, 200, "/api/beads must still answer");

    // AN ABSENT PATH MUST 404, not serve the page as a catch-all. A router that delivers the
    // page for every unknown URL hides broken links — a typo in the scripts' own relative path
    // would be served the page, not a 404 that names the problem.
    let (code, _) = get(addr, "/nonexistent.css").await;
    assert_eq!(code, 404, "/nonexistent.css must 404, not catch-all to the page");
}

// ── /api/ops ─────────────────────────────────────────────────────────────────
//
// This route never calls bd — it parses two small files under `run` and checks the halt
// stamp. `cfg()` above wires `db`/`bd` for /api/beads; /api/ops ignores both, so these tests
// only ever set `run`.

fn ops_cfg(run: &str) -> Config {
    Config {
        db: String::new(),
        extra_path: vec![],
        budget: Duration::from_millis(20_000),
        cache: Duration::from_secs(30),
        addr: String::new(),
        bd: "bd".to_string(),
        run: run.to_string(),
        instance: "test".to_string(),
        systemctl: "systemctl".to_string(),
    }
}

fn ops_run_dir() -> PathBuf {
    let n = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("loom-ops-{}-{n}", std::process::id()));
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
    // twice — the same encoding cockpit.sh's Python writer uses.
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
