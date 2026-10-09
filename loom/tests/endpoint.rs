//! The endpoint driven over a real socket, against a `spira-lc` it spawns as a real process.
//!
//! HERMETIC BY DEFAULT. The `spira-lc` these tests spawn is a shell script that answers
//! `ops-view ops_live` and `ops-view ops_recent` from canned files in the shape the store's
//! views emit (BIGINT columns as strings, `holds` as a JSON string, a row filed before titles
//! were mirrored with a null title). The real views are exercised against a real store by
//! `spira/test-ops-read-model.sh`.
//!
//! The fixture: sp-aaa ready, sp-bbb ready with a title needing escaping and two holds, sp-ccc
//! working, sp-ddd with no mirrored title yet, and sp-zzz landed an hour ago.
//!
//! Every assertion that something is ABSENT is paired with one that the same check can see
//! something present: an invocation counter is shown moving before its stillness means
//! anything, and the budget is shown admitting a query before it is shown refusing one.

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

const LIVE: &str = r#"[
 {"bead_id":"sp-aaa","state":"READY","holds":"[]","holder":null,"lease_until":null,"since":null,"updated_at":"1791480000","priority":"1","title":"open bead"},
 {"bead_id":"sp-bbb","state":"READY","holds":"[\"manual\",\"wait\"]","holder":null,"lease_until":null,"since":null,"updated_at":"1791480001","priority":"1","title":"beta \"quoted\" and a \\ backslash"},
 {"bead_id":"sp-ccc","state":"WORKING","holds":"[]","holder":"aeon-1","lease_until":"1791483000","since":null,"updated_at":"1791480002","priority":"2","title":"working bead"},
 {"bead_id":"sp-ddd","state":"SUBMITTED","holds":"[]","holder":null,"lease_until":null,"since":null,"updated_at":"1791480003","priority":null,"title":null}
]"#;
const RECENT: &str = r#"[
 {"bead_id":"sp-zzz","state":"LANDED","since":"1791476400","updated_at":"1791476401","priority":"1","title":"landed bead"}
]"#;

/// The first element holds the fixture's directory: keep it for as long as the path is used.
fn fixture_with(live: &str, recent: &str) -> (testkit::TempDir, String) {
    let dir = scratch("fake");
    std::fs::write(dir.join("live.json"), live).expect("the live answer");
    std::fs::write(dir.join("recent.json"), recent).expect("the recent answer");
    let lc = dir.join("spira-lc");
    script(
        &lc,
        &format!(
            "#!/bin/sh\n\
             [ \"$1\" = ops-view ] || {{ echo \"fake spira-lc: unexpected query: $*\" >&2; exit 2; }}\n\
             case \"$2\" in\n\
               ops_live) exec cat {d}/live.json;;\n\
               ops_recent) exec cat {d}/recent.json;;\n\
               *) echo \"fake spira-lc: unexpected view: $2\" >&2; exit 2;;\n\
             esac\n",
            d = dir.display()
        ),
    );
    let lc = lc.to_string_lossy().into_owned();
    (dir, lc)
}

fn fixture() -> (testkit::TempDir, String) {
    fixture_with(LIVE, RECENT)
}

/// An `spira-lc` that never answers inside any budget a test sets. `exec` so the process loom
/// kills on overrun is the sleeper itself, not a shell that would orphan it.
fn slow_lc() -> (testkit::TempDir, String) {
    let dir = scratch("slow");
    let lc = dir.join("spira-lc");
    script(&lc, "#!/bin/sh\nexec sleep 30\n");
    let lc = lc.to_string_lossy().into_owned();
    (dir, lc)
}

/// A shim that records every call and then execs the real one.
///
/// COUNTING INVOCATIONS, NEVER TIMING THEM. "The second request was fast" is satisfied by a
/// warm page cache, a lucky scheduler or a query that failed early; only the count answers
/// whether the process boundary was crossed.
fn counting_lc(real: &str) -> (testkit::TempDir, String, PathBuf) {
    let dir = scratch("shim");
    let counter = dir.join("calls");
    std::fs::write(&counter, b"").expect("an empty counter");
    let shim = dir.join("spira-lc");
    script(
        &shim,
        &format!("#!/bin/sh\nprintf 'x\\n' >> {}\nexec {} \"$@\"\n", counter.display(), real),
    );
    let shim = shim.to_string_lossy().into_owned();
    (dir, shim, counter)
}

fn calls(counter: &PathBuf) -> usize {
    std::fs::read_to_string(counter).map(|s| s.lines().count()).unwrap_or(0)
}

/// Every value is pinned AWAY from the shipped default, so an assertion here cannot be
/// satisfied by code that has the default written in rather than reading the key.
fn cfg(lc: &str, budget_ms: u64, cache_s: u64) -> Config {
    Config {
        lc: lc.to_string(),
        extra_path: vec![],
        budget: Duration::from_millis(budget_ms),
        cache: Duration::from_secs(cache_s),
        addr: String::new(),
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

fn find<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["beads"].as_array().expect("a bead array").iter().find(|b| b["id"] == id).unwrap_or_else(|| panic!("{id} missing from {v}"))
}

#[tokio::test]
async fn the_payload_is_the_lifecycle_views_shaped_for_the_page() {
    let (_dir, lc) = fixture();
    let addr = spawn(cfg(&lc, 20_000, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 200, "{v}");

    // PRESENT FIRST, then the landing the page reads as closed.
    assert_eq!(v["count"], 5);
    assert_eq!(find(&v, "sp-aaa")["status"], "open");
    assert_eq!(find(&v, "sp-ccc")["status"], "in_progress");
    assert_eq!(find(&v, "sp-ccc")["assignee"], "aeon-1");
    assert_eq!(find(&v, "sp-zzz")["status"], "closed");

    let beta = find(&v, "sp-bbb");
    assert_eq!(beta["title"], "beta \"quoted\" and a \\ backslash", "the splice still produces a document");
    assert_eq!(beta["holds"], serde_json::json!(["wait", "manual"]));
    assert_eq!(beta["priority"], 1);
    assert_eq!(find(&v, "sp-aaa")["updated_at"], "2026-10-08T17:20:00Z");

    let ddd = find(&v, "sp-ddd");
    assert_eq!(ddd["title"], "sp-ddd", "an unmirrored title shows the id");
    assert_eq!(ddd["priority"], 3);

    assert_eq!(v["edges"], serde_json::json!([]));
    assert_eq!(v["budget_ms"], 20_000);
    assert_eq!(v["cache_s"], 30);
    assert!(v["query_ms"].is_u64() && v["refresh_ms"].is_u64());
}

#[tokio::test]
async fn a_twelve_thousand_row_answer_renders_within_budget() {
    let rows: Vec<String> = (0..12_000)
        .map(|i| format!(r#"{{"bead_id":"sp-b{i}","state":"READY","holds":"[]","updated_at":"1791480000","priority":"2","title":"t{i}"}}"#))
        .collect();
    let (_dir, lc) = fixture_with(&format!("[{}]", rows.join(",")), "[]");
    let addr = spawn(cfg(&lc, 1_500, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["count"], 12_000);
    assert!(v["refresh_ms"].as_u64().unwrap() < 1_500);
}

#[tokio::test]
async fn two_requests_inside_the_window_cost_one_refresh() {
    let (_dir, lc) = fixture();
    let (_shim_dir, shim, counter) = counting_lc(&lc);
    let addr = spawn(cfg(&shim, 20_000, 60)).await;
    assert_eq!(calls(&counter), 0, "nothing runs before anybody looks");

    let (code, _) = json(addr).await;
    assert_eq!(code, 200);
    let cold = calls(&counter);
    assert_eq!(cold, 2, "one read per view answers the whole page");

    let (code, v) = json(addr).await;
    assert_eq!(code, 200);
    assert_eq!(calls(&counter), cold, "the second request inside the window crossed the process boundary again");
    assert!(v["age_ms"].as_u64().expect("an age") < 60_000);

    // THE POSITIVE CONTROL. The counter is shown MOVING under an expired window, so its
    // stillness above is a property of the cache rather than of a shim nobody wired up.
    let (_shim_dir, shim2, counter2) = counting_lc(&lc);
    let addr2 = spawn(cfg(&shim2, 20_000, 0)).await;
    let (code, _) = json(addr2).await;
    assert_eq!(code, 200);
    assert_eq!(calls(&counter2), 2);
    let (code, _) = json(addr2).await;
    assert_eq!(code, 200);
    assert_eq!(calls(&counter2), 4, "a zero-length window must refresh every time");
}

#[tokio::test]
async fn a_query_over_budget_is_refused_rather_than_served_late() {
    let (_dir, lc) = fixture();
    let (_slow_dir, slow) = slow_lc();
    let (_shim_dir, shim, counter) = counting_lc(&slow);

    let addr = spawn(cfg(&shim, 1_500, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 503, "an over-budget query must be refused: {v}");
    assert_eq!(v["error"], "over budget");
    assert_eq!(v["budget_ms"], 1_500);
    assert!(calls(&counter) >= 1, "it was refused because the query overran, not because nothing was tried");
    let (code, _) = json(addr).await;
    assert_eq!(code, 503, "nothing stale is served in its place");

    let addr2 = spawn(cfg(&lc, 20_000, 30)).await;
    let (code, v) = json(addr2).await;
    assert_eq!(code, 200, "{v}");
}

#[tokio::test]
async fn a_query_that_fails_is_reported_as_such_and_not_as_an_empty_graph() {
    let (_dir, lc) = fixture_with("cannot tell", "[]");
    let addr = spawn(cfg(&lc, 20_000, 30)).await;
    let (code, v) = json(addr).await;
    assert_eq!(code, 502, "{v}");
    assert_eq!(v["error"], "query failed");
    assert!(v["detail"].as_str().map(|d| !d.is_empty()).unwrap_or(false), "a failure must carry its reason: {v}");

    let addr = spawn(cfg("/nonexistent/spira-lc", 20_000, 30)).await;
    let (code, _) = json(addr).await;
    assert_eq!(code, 502);
}

#[tokio::test]
async fn the_static_page_and_its_scripts_are_served() {
    let (_dir, lc) = fixture();
    let addr = spawn(cfg(&lc, 20_000, 30)).await;
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
