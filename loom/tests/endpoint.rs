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
async fn get_location(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.expect("a connection");
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: loom\r\nConnection: close\r\n\r\n").as_bytes()).await.expect("a request");
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.expect("a response");
    let text = String::from_utf8_lossy(&buf).into_owned();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).expect("a status");
    let loc = text.lines().find_map(|l| l.to_ascii_lowercase().starts_with("location:").then(|| l[9..].trim().to_string())).unwrap_or_default();
    (code, loc)
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
    assert_eq!(code, 503, "/ is the Now page, which says so when no snapshot exists, and never redirects");
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
        duckdb: "duckdb".to_string(),
        journalctl: "journalctl".to_string(),
        broker: fake_broker().0.to_string_lossy().into_owned(),
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
  "list --batches") echo '[]' ;;
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
    let (stuck_at, fine_at) = (page.find("/beads/sp-2cah6").expect("stuck row"), page.find("/beads/sp-fine").expect("healthy row"));
    assert!(stuck_at < fine_at, "the stuck bead is listed first");
    assert!(page.contains("seg s-CERTIFIED over"), "its bar is outlined");
    assert!(!page.contains("seg s-WORKING over"), "positive control: the healthy bar is not");

    let (status, detail) = get(addr, "/beads/sp-2cah6").await;
    assert_eq!(status, 200, "{detail}");
    assert!(detail.contains("refused: no-edge"));
    assert_eq!(get(addr, "/beads/sp-nope1").await.0, 404);
    assert_eq!(get(addr, "/beads/a%20b").await.0, 400);
    assert_eq!(get(addr, "/stuck/a%20b").await.0, 400);
    let (status, loc) = get_location(addr, "/stuck/sp-2cah6").await;
    assert_eq!((status, loc.as_str()), (301, "/beads/sp-2cah6"));
}

// ── /units ───────────────────────────────────────────────────────────────────

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn exec_row(unit: &str, ago_s: i64, result: &str, duration_ms: i64, inv: &str) -> String {
    format!(r#"{{"unit":"{unit}","t":{},"result":"{result}","duration_ms":{duration_ms},"invocation_id":"{inv}"}}"#, now_s() - ago_s)
}

const SHOW: &str = "Id=ok.service\nActiveState=inactive\nSubState=dead\nUnitFileState=static\n\n\
Id=ok.timer\nActiveState=active\nSubState=waiting\nUnitFileState=enabled\nUnit=ok.service\nNextElapseUSecRealtime=soon\nLastTriggerUSec=just now\n\n\
Id=dead.service\nActiveState=inactive\nSubState=dead\nUnitFileState=static\n\n\
Id=dead.timer\nActiveState=active\nSubState=waiting\nUnitFileState=enabled\nUnit=dead.service\nNextElapseUSecRealtime=n/a\nLastTriggerUSec=15h ago\n\n\
Id=never.service\nActiveState=inactive\nSubState=dead\nUnitFileState=static\n\n\
Id=flaky.service\nActiveState=failed\nSubState=failed\nUnitFileState=static\n\n\
Id=dur.service\nActiveState=inactive\nSubState=dead\nUnitFileState=static\n";

struct UnitsFixture {
    dir: testkit::TempDir,
    cfg: Config,
}

fn units_fixture(rows: &[String]) -> UnitsFixture {
    let dir = scratch("units");
    let rows_file = dir.join("rows.json");
    std::fs::write(&rows_file, format!("[{}]", rows.join(","))).unwrap();
    let show_file = dir.join("show.txt");
    std::fs::write(&show_file, SHOW).unwrap();
    let run = dir.join("run");
    std::fs::create_dir_all(run.join("tsd")).unwrap();
    std::fs::write(run.join("tsd").join("unit-run.jsonl"), b"{}\n").unwrap();
    let systemctl = dir.join("systemctl");
    script(
        &systemctl,
        &format!(
            "#!/bin/sh\ncase \"$2\" in\nlist-unit-files) printf 'ok.service static -\\nok.timer enabled -\\ndead.service static -\\ndead.timer enabled -\\nnever.service static -\\nflaky.service static -\\ndur.service static -\\ntmpl@.service static -\\n';;\nshow) cat {};;\nesac\n",
            show_file.display()
        ),
    );
    let duckdb = dir.join("duckdb");
    script(&duckdb, &format!("#!/bin/sh\ncat {}\n", rows_file.display()));
    let journalctl = dir.join("journalctl");
    script(&journalctl, "#!/bin/sh\ncase \"$*\" in *inv-bad*) printf 'starting\\nboom: it broke\\n';; esac\n");
    let mut cfg = ops_cfg(&run.to_string_lossy());
    cfg.systemctl = systemctl.to_string_lossy().into_owned();
    cfg.duckdb = duckdb.to_string_lossy().into_owned();
    cfg.journalctl = journalctl.to_string_lossy().into_owned();
    UnitsFixture { dir, cfg }
}

fn fixture_rows() -> Vec<String> {
    let mut rows = Vec::new();
    for h in 0..24 {
        rows.push(exec_row("ok.service", 600 + h * 3600, "success", 800, &format!("ok-{h}")));
    }
    for h in 15..40 {
        rows.push(exec_row("dead.service", h * 3600, "success", 800, &format!("dead-{h}")));
    }
    for i in 1..=10 {
        rows.push(exec_row("dur.service", i * 60, "success", i * 100, &format!("dur-{i}")));
    }
    for (i, r) in ["success", "success", "exit-code", "success"].iter().enumerate() {
        let inv = if *r == "success" { format!("flaky-{i}") } else { "inv-bad".into() };
        rows.push(exec_row("flaky.service", 3600 + i as i64 * 600, r, 100, &inv));
    }
    rows
}

async fn units_json(addr: SocketAddr) -> Value {
    let (code, body) = get(addr, "/api/units").await;
    assert_eq!(code, 200, "{body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"))
}

fn unit<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["units"].as_array().unwrap().iter().find(|u| u["unit"] == name).unwrap_or_else(|| panic!("{name} missing from {v}"))
}

#[tokio::test]
async fn units_lists_every_unit_with_rates_percentiles_and_the_stale_flag() {
    let fx = units_fixture(&fixture_rows());
    let addr = spawn(fx.cfg.clone()).await;
    let v = units_json(addr).await;
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");

    let names: Vec<&str> = v["units"].as_array().unwrap().iter().map(|u| u["unit"].as_str().unwrap()).collect();
    for n in ["ok.service", "dead.service", "never.service", "flaky.service", "dur.service"] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    assert!(!names.iter().any(|n| n.ends_with(".timer") || n.contains('@')), "timers fold into their service, templates are not units: {names:?}");

    let never = unit(&v, "never.service");
    assert_eq!((never["runs_24h"].as_i64(), never["runs_7d"].as_i64()), (Some(0), Some(0)));
    assert!(never["success_rate_7d"].is_null() && never["last_failure"].is_null() && never["strip"].as_array().unwrap().is_empty());

    let dur = unit(&v, "dur.service");
    assert_eq!((dur["p50_ms"].as_i64(), dur["p95_ms"].as_i64()), (Some(500), Some(1000)));

    let flaky = unit(&v, "flaky.service");
    assert_eq!(flaky["runs_24h"], 4);
    assert_eq!(flaky["success_rate_24h"].as_f64(), Some(0.75));
    assert_eq!(flaky["state"]["active"], "failed");
    assert_eq!(flaky["last_failure"]["result"], "exit-code");
    assert_eq!(flaky["last_failure"]["log"][1], "boom: it broke");
    assert_eq!(flaky["strip"].as_array().unwrap().iter().filter(|r| *r != "success").count(), 1);

    let ok = unit(&v, "ok.service");
    assert_eq!(ok["stale"], false, "a timer whose last success is ten minutes old is not stale");
    assert_eq!(ok["timer"]["active"], "active");
    assert_eq!(ok["runs_24h"], 24);

    let dead = unit(&v, "dead.service");
    assert_eq!(dead["stale"], true, "last success 15 h ago on an hourly timer");
    assert_eq!(dead["interval_s"], 3600);
    assert_eq!(v["units"][0]["unit"], "dead.service", "stale units sort first");
}

#[tokio::test]
async fn units_page_renders_the_same_rows_and_is_cached() {
    let fx = units_fixture(&fixture_rows());
    let addr = spawn(fx.cfg.clone()).await;
    let (code, html) = get(addr, "/units").await;
    assert_eq!(code, 200);
    for n in ["ok.service", "dead.service", "never.service", "flaky.service", "dur.service"] {
        assert!(html.contains(n), "{n} in the page");
    }
    assert!(html.contains("stale — last success") && html.contains("boom: it broke"), "{html}");
    assert!(html.contains("@media(max-width:639px)") && html.contains("class=\"u\" title=\"ok.service\"") || html.contains("class=u title=\"ok.service\""), "narrow viewports stack each family as a card: {html}");
    assert!(html.contains("<td class=sr><div class=strip>"), "{html}");

    let swapped = fx.dir.join("duckdb");
    script(&swapped, "#!/bin/sh\necho '[]'\n");
    let v = units_json(addr).await;
    assert_eq!(unit(&v, "dur.service")["runs_7d"], 10, "inside the cache window the held snapshot answers");
}

#[tokio::test]
async fn units_without_a_unit_run_file_still_lists_the_units_and_names_nothing_missing() {
    let fx = units_fixture(&[]);
    std::fs::remove_file(std::path::Path::new(&fx.cfg.run).join("tsd").join("unit-run.jsonl")).unwrap();
    let addr = spawn(fx.cfg.clone()).await;
    let v = units_json(addr).await;
    assert_eq!(v["errors"].as_array().unwrap().len(), 0);
    assert_eq!(unit(&v, "never.service")["runs_7d"], 0);
    assert_eq!(unit(&v, "dead.service")["stale"], true, "an active timer with no success at all is flagged");
}

#[tokio::test]
async fn units_reports_a_failing_source_instead_of_hiding_the_rows() {
    let mut fx = units_fixture(&fixture_rows());
    fx.cfg.duckdb = "/nonexistent/duckdb".into();
    let addr = spawn(fx.cfg.clone()).await;
    let v = units_json(addr).await;
    assert!(v["errors"][0].as_str().unwrap().contains("unit-run"), "{v}");
    assert!(unit(&v, "never.service")["runs_7d"] == 0);
}

#[tokio::test]
async fn units_reads_the_real_unit_run_file_through_duckdb() {
    if std::process::Command::new("duckdb").arg("--version").output().is_err() {
        eprintln!("duckdb is not on PATH: the real-query check runs in the test container");
        return;
    }
    let mut fx = units_fixture(&[]);
    let run = std::path::PathBuf::from(&fx.cfg.run);
    let line = |unit: &str, ago: i64, result: &str, ms: i64, inv: &str| {
        format!(r#"{{"family":"unit-run","unit":"{unit}","ts":"{}","result":"{result}","duration_ms":{ms},"invocation_id":"{inv}"}}"#, tsd::iso_utc((now_s() - ago) as u64))
    };
    let mut text = String::new();
    for i in 1..=10 {
        text.push_str(&line("dur.service", i * 60, "success", i * 100, &format!("d{i}")));
        text.push('\n');
    }
    text.push_str(&line("flaky.service", 120, "exit-code", 5, "inv-bad"));
    text.push('\n');
    text.push_str(&line("flaky.service", 9 * 86_400, "success", 5, "too-old"));
    text.push('\n');
    std::fs::write(run.join("tsd").join("unit-run.jsonl"), text).unwrap();
    fx.cfg.duckdb = "duckdb".into();
    let addr = spawn(fx.cfg.clone()).await;
    let v = units_json(addr).await;
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    let dur = unit(&v, "dur.service");
    assert_eq!((dur["runs_7d"].as_i64(), dur["p50_ms"].as_i64(), dur["p95_ms"].as_i64()), (Some(10), Some(500), Some(1000)));
    let flaky = unit(&v, "flaky.service");
    assert_eq!((flaky["runs_7d"].as_i64(), flaky["last_failure"]["result"].as_str()), (Some(1), Some("exit-code")), "the nine-day-old row is outside the window");
}

#[tokio::test]
async fn units_folds_per_instance_names_into_one_family_row() {
    let mut rows = fixture_rows();
    for i in 0..30 {
        let result = if i == 7 { "exit-code" } else { "success" };
        rows.push(exec_row(&format!("spira-aeon-builder-17920{i:05}.service"), 40_000 + i * 100, result, 500, "inv-bad"));
    }
    rows.push(exec_row("spira-aeon-ops-1792000001.service", 3_000, "success", 500, "ops-1"));
    rows.push(exec_row("libpod-0123456789abcdef.scope", 3_000, "success", 500, "pod-1"));
    rows.push(exec_row("gpg-agent.service", 3_000, "success", 500, "gpg-1"));
    rows.push(exec_row("blockdev@dev-sda.service", 3_000, "success", 500, "bd-1"));
    rows.push(exec_row("blockdev@dev-sdb.service", 2_000, "success", 500, "bd-2"));
    let fx = units_fixture(&rows);
    let addr = spawn(fx.cfg.clone()).await;
    let v = units_json(addr).await;

    let names: Vec<&str> = v["units"].as_array().unwrap().iter().map(|u| u["unit"].as_str().unwrap()).collect();
    assert!(!names.iter().any(|n| n.contains("17920") || n.contains("0123456789")), "instances are not rows: {names:?}");

    let b = unit(&v, "spira-aeon-builder");
    assert_eq!(b["kind"], "session");
    assert_eq!(b["instances"], 30);
    assert_eq!(b["runs_7d"], 30);
    assert_eq!(b["stale"], false, "a finished session is not a stopped unit");
    assert!(b["last_failure"]["unit"].as_str().unwrap().starts_with("spira-aeon-builder-17920"), "{b}");
    assert_eq!(unit(&v, "spira-aeon-ops")["runs_7d"], 1);
    assert_eq!(unit(&v, "libpod")["kind"], "session");
    assert_eq!(unit(&v, "gpg-agent")["kind"], "system");
    assert_eq!(unit(&v, "blockdev")["instances"], 2);

    let (_, html) = get(addr, "/units").await;
    assert!(html.contains("unit families"), "{html}");
    assert!(html.contains("<details>"), "system families are folded away");
}

#[tokio::test]
async fn lifecycle_round_rows_are_links_and_a_vm_round_serves_its_log() {
    use cockpit_ops::lcview::{BatchRow, Snapshot};
    let run = ops_run_dir();
    let log = run.join("r-vm.log");
    std::fs::write(&log, "suite one ok\nsuite two <b>red</b>\n").expect("a log");
    let now = now_s();
    let snap = Snapshot {
        now,
        forge_base: "https://forge.test/acme".into(),
        batches: vec![
            BatchRow { id: "b-70".into(), repo: "dumpstar".into(), pr: 70, state: "CI_RUNNING".into(), phase: "red".into(), opened_at: now - 60, run_url: "https://forge.test/acme/dumpstar/actions/runs/9".into(), members: vec!["sp-p0".into()], progress: Some(cockpit_ops::lcview::PassProgress { round: "b-70".into(), phase: "suites".into(), done: 1, total: 2, red: vec!["t".into()], updated_at: now, ..Default::default() }), ..Default::default() },
            BatchRow { id: "r-vm".into(), repo: "spira".into(), state: "CI_RUNNING".into(), phase: "certifying".into(), opened_at: now - 50, log: log.to_string_lossy().into_owned(), members: vec!["sp-a0".into()], ..Default::default() },
        ],
        ..Default::default()
    };
    std::fs::create_dir_all(run.join("lcview")).expect("lcview dir");
    std::fs::write(run.join("lcview").join("snapshot.json"), serde_json::to_string(&snap).unwrap()).expect("a snapshot");
    let addr = spawn(ops_cfg(run.to_str().unwrap())).await;
    let (code, body) = get(addr, "/lifecycle?fragment").await;
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("fragment json");
    let html = format!("{}{}", v["banner"], v["tree"]).replace("\\\"", "\"");
    for href in ["https://forge.test/acme/dumpstar/pull/70", "https://forge.test/acme/dumpstar/actions/runs/9", "/rounds/r-vm"] {
        assert!(html.contains(&format!("<a href=\"{href}\">")), "{href} in {html}");
    }
    let (code, page) = get(addr, "/rounds/r-vm").await;
    assert_eq!(code, 200, "{page}");
    assert!(page.contains("suite one ok") && page.contains("&lt;b&gt;red"), "{page}");
    assert_eq!(get(addr, "/rounds/nope").await.0, 404);
}


// ── Now and the shell ────────────────────────────────────────────────────────

const FIVE: [&str; 5] = ["/", "/rounds", "/stuck", "/units", "/lifecycle"];

fn write_snapshot(run: &std::path::Path, snap: &cockpit_ops::lcview::Snapshot) {
    std::fs::create_dir_all(run.join("lcview")).unwrap();
    std::fs::write(run.join("lcview").join("snapshot.json"), serde_json::to_string(snap).unwrap()).unwrap();
}

fn now_snapshot() -> cockpit_ops::lcview::Snapshot {
    use cockpit_ops::lcview::*;
    let now = now_s();
    let row = |id: &str, state: &str, since: i64| Row { id: id.into(), state: state.into(), since, updated_at: since, ..Default::default() };
    let mut s = Snapshot { now, release: "rel-abc".into(), world: "RUNNING".into(), ceiling: 4, forge_base: "https://forge.test/o".into(), ..Default::default() };
    s.rows = vec![row("sp-late1", "WORKING", now - 36_000), row("sp-done1", "LANDED", now - 600), row("sp-done2", "LANDED", now - 90_000)];
    s.meta.insert("sp-late1".into(), Meta { title: "the slow one".into(), ..Default::default() });
    s.meta.insert("sp-done1".into(), Meta { title: "just landed".into(), ..Default::default() });
    s.dwell = vec![
        DwellRow { bead_id: "sp-late1".into(), state: "WORKING".into(), entered_at: now - 36_000, p95_s: Some(3600) },
        DwellRow { bead_id: "sp-fine1".into(), state: "WORKING".into(), entered_at: now - 60, p95_s: Some(3600) },
    ];
    s.live = Some(vec![LiveAeon { unit: "spira-aeon-x.service".into(), pid: 7, started: now - 300, phase: "session".into() }]);
    s.batches = vec![BatchRow {
        id: "b-live1".into(), state: "CI_RUNNING".into(), repo: "dumpstar".into(), pr: 7, members: vec!["sp-m1".into(), "sp-m2".into()], ejected: vec!["sp-m2".into()],
        opened_at: now - 900, last_at: now - 60, run_url: "https://forge.test/run/1".into(), ..Default::default()
    }];
    s
}

#[tokio::test]
async fn now_shows_the_live_round_the_late_bead_the_failing_unit_and_what_landed() {
    let fx = units_fixture(&fixture_rows());
    let run = std::path::PathBuf::from(&fx.cfg.run);
    write_snapshot(&run, &now_snapshot());
    let addr = spawn(fx.cfg.clone()).await;

    let mut page = String::new();
    for _ in 0..50 {
        page = get(addr, "/").await.1;
        if page.contains("flaky.service") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(page.contains("href=\"/rounds/dumpstar/b-live1\""), "a card per live round: {page}");
    assert!(page.contains("href=\"/stuck/sp-late1\"") && page.contains("10.0h in WORKING"), "the bead past p95: {page}");
    assert!(!page.contains("/stuck/sp-fine1"), "positive control: a bead inside its p95 is not listed");
    assert!(page.contains("flaky.service") && page.contains("dead.service"), "failing and stale units: {page}");
    assert!(page.contains("aeons 1/4") && page.contains("rel-abc") && page.contains("world RUNNING"), "the top bar: {page}");
    assert!(page.contains("1 in 24 h") && page.contains("just landed") && !page.contains("sp-done2"), "landed in the last day only: {page}");

    let (code, round) = get(addr, "/rounds/dumpstar/b-live1").await;
    assert_eq!(code, 200, "{round}");
    assert!(round.contains("sp-m1") && round.contains("sp-m2") && round.contains("forge.test/o/dumpstar/pull/7") && round.contains("forge.test/run/1"), "{round}");
    assert_eq!(get(addr, "/rounds/dumpstar/nope").await.0, 404);
}

#[tokio::test]
async fn now_answers_from_cache_while_a_source_is_slow_and_says_it_is_loading() {
    let fx = units_fixture(&fixture_rows());
    script(&PathBuf::from(&fx.cfg.systemctl), "#!/bin/sh\nsleep 5\n");
    write_snapshot(&PathBuf::from(&fx.cfg.run), &now_snapshot());
    let addr = spawn(fx.cfg.clone()).await;
    for _ in 0..3 {
        let (code, page) = get(addr, "/").await;
        assert_eq!(code, 200, "{page}");
        assert!(page.contains("unit history is loading") && page.contains("/rounds/dumpstar/b-live1"), "{page}");
    }
}

#[tokio::test]
async fn every_page_sits_in_the_shell_and_the_tab_bar_reaches_all_five_sections_on_a_phone() {
    let mut fx = units_fixture(&fixture_rows());
    fx.cfg.lc = fake_lc(&fx.dir.to_path_buf());
    write_snapshot(&PathBuf::from(&fx.cfg.run), &now_snapshot());
    let addr = spawn(fx.cfg.clone()).await;
    for path in FIVE.iter().chain(&["/beads/sp-2cah6", "/rounds/dumpstar/b-live1"]) {
        let (code, page) = get(addr, path).await;
        assert_eq!(code, 200, "{path}");
        assert!(page.contains("width=device-width") || *path == "/lifecycle", "{path}: viewport");
        assert!(page.contains("<b>SPIRA</b>") && page.contains("refreshed"), "{path}: top bar");
        for target in FIVE {
            assert!(page.contains(&format!("<a href=\"{target}\"")), "{path}: tab {target}");
        }
        assert!(!page.contains("<!--lm-bar-->"), "{path}: the bar slot was filled");
        let css = page.split("@media(max-width:639px)").nth(1).unwrap_or_else(|| panic!("{path}: no phone rule"));
        assert!(css.contains(".lm-tabs{position:fixed;left:0;right:0;bottom:0") && css.contains("flex:1 1 0;min-width:0"), "{path}: bottom tab bar of five equal cells");
        assert!(css.contains("body{overflow-x:hidden"), "{path}: no horizontal overflow");
    }
}

static BROKER: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();

/// One fake broker for the process, answering recorded
/// `run-jobs` responses by run id and logging every call.
fn fake_broker() -> &'static (PathBuf, PathBuf) {
    BROKER.get_or_init(|| {
        let dir = PathBuf::from(testkit::TempDir::new("loom-broker").to_path_buf());
        std::fs::create_dir_all(&dir).expect("a broker dir");
        let (bin, log) = (dir.join("broker"), dir.join("calls.log"));
        let red = r#"{"run":{"id":9001,"url":"https://github.com/o/dumpstar/actions/runs/9001","status":"completed","conclusion":"failure","head_sha":"e6e8c19"},"jobs":[{"name":"lint","conclusion":"success","status":"completed","url":"https://github.com/o/dumpstar/actions/runs/9001/job/1","failed_step":null,"error_lines":[]},{"name":"gate","conclusion":"failure","status":"completed","url":"https://github.com/o/dumpstar/actions/runs/9001/job/2","failed_step":"svelte-check","error_lines":["Error: Type 'string' is not assignable to type 'number'. (ts)","svelte-check found 1 error"]}]}"#;
        let green = r#"{"run":{"id":9002,"url":"https://github.com/o/deckdumpster/actions/runs/9002","status":"completed","conclusion":"success","head_sha":"aa"},"jobs":[{"name":"gate","conclusion":"success","status":"completed","url":"https://github.com/o/deckdumpster/actions/runs/9002/job/3","failed_step":null,"error_lines":[]}]}"#;
        script(&bin, &format!("#!/bin/sh\necho \"$*\" >> {log}\ncase \"$3\" in\n*/9001) cat <<'EOF'\n{red}\nEOF\n;;\n*/9002) cat <<'EOF'\n{green}\nEOF\n;;\n*) echo boom >&2; exit 1;;\nesac\n", log = log.display()));
        (bin, log)
    })
}

fn batch_files(run: &PathBuf, repo: &str, pr: &str, id: &str, ci: &str, run_id: &str, step: &str) {
    let d = run.join("queue").join(repo);
    std::fs::create_dir_all(&d).expect("a repo queue dir");
    std::fs::write(d.join("open"), format!("pr={pr}\nhead=h\nbase=b\nmembers=sp-aaa:1111111111111 sp-bbb:2222222222222\nopened={}\nbranch=spira/queue/{id}\nbatch_id=spira/queue/{id}\n", now_s() - 600)).expect("open");
    std::fs::write(d.join("forge-state"), format!("batch_id=spira/queue/{id}\npr={pr}\npr_state=open\nci={ci}\nrun={run_id}\nstep={step}\nwall=5700\n")).expect("forge-state");
    std::fs::write(d.join("closed-pr60-20261014T000000Z"), "pr=60\nhead=h\nbase=b\nmembers=sp-old:3333333333333\nopened=1000\nbranch=spira/queue/20261014T000000Z\nbatch_id=spira/queue/20261014T000000Z\nreason=re-cut the same members\nactor=operator\n").expect("closed");
}

async fn forge_loom() -> (SocketAddr, testkit::TempDir) {
    let run = ops_run_dir();
    let p = PathBuf::from(run.to_path_buf());
    batch_files(&p, "dumpstar", "66", "20261015T000000Z", "red", "9001", "gate / svelte-check");
    batch_files(&p, "deckdumpster", "619", "20261015T010000Z", "green", "9002", "");
    batch_files(&p, "pokedumpster", "7", "20261015T020000Z", "pending", "666", "gate / deploy");
    (spawn(ops_cfg(&run.to_string_lossy())).await, run)
}

#[tokio::test]
async fn rounds_lists_forge_batches_with_ci_state_and_links_each_to_its_page() {
    let (addr, _run) = forge_loom().await;
    let (code, page) = get(addr, "/rounds").await;
    assert_eq!(code, 200);
    assert!(page.contains("PR #66") && page.contains("CI red"), "{page}");
    assert!(page.contains("PR #619") && page.contains("CI green"), "{page}");
    assert!(page.contains("href=\"/rounds/dumpstar/20261015T000000Z\""), "{page}");
    assert!(page.contains("PR #60") && page.contains("closed"), "closed batches are listed: {page}");
    assert!(page.contains("width=device-width"), "mobile viewport: {page}");
}

#[tokio::test]
async fn a_red_batch_page_names_the_failing_step_its_error_lines_the_pr_and_the_run() {
    let (addr, _run) = forge_loom().await;
    let (code, page) = get(addr, "/rounds/dumpstar/20261015T000000Z").await;
    assert_eq!(code, 200);
    assert!(page.contains("failed step: svelte-check"), "{page}");
    assert!(page.contains("svelte-check found 1 error") && page.contains("not assignable to type"), "{page}");
    assert!(page.contains("href=\"https://github.com/o/dumpstar/pull/66\"") && page.contains("PR #66"), "{page}");
    assert!(page.contains("href=\"https://github.com/o/dumpstar/actions/runs/9001\""), "{page}");
    assert!(page.contains("href=\"/stuck/sp-aaa\"") && page.contains("sp-bbb"), "{page}");
}

#[tokio::test]
async fn a_green_batch_page_has_no_failed_step_and_ci_detail_is_cached_per_batch() {
    let (addr, _run) = forge_loom().await;
    let (_, log) = fake_broker();
    let calls = |needle: &str| std::fs::read_to_string(log).unwrap_or_default().lines().filter(|l| l.contains(needle)).count();
    let before = calls("deckdumpster/9002");
    let (code, page) = get(addr, "/rounds/deckdumpster/20261015T010000Z").await;
    assert_eq!(code, 200);
    assert!(page.contains("success") && !page.contains("failed step"), "{page}");
    let (_, again) = get(addr, "/rounds/deckdumpster/20261015T010000Z").await;
    assert!(again.contains("pull/619"), "{again}");
    assert_eq!(calls("deckdumpster/9002") - before, 1, "the second view is served from the 60 s cache");
}

#[tokio::test]
async fn a_batch_with_no_ci_run_shows_the_local_check_detail_the_closed_outcome_and_a_pr_link() {
    let (addr, run) = forge_loom().await;
    let q = run.join("queue").join("dumpstar");
    std::fs::write(q.join("local-verdict"), "verdict=red\nat=1\ndetail=suite test-widgets red\n").unwrap();
    std::fs::write(q.join("forge-state"), "").unwrap();
    write_snapshot(&run, &cockpit_ops::lcview::Snapshot { forge_base: "https://forge.test/o".into(), ..Default::default() });
    let (_, page) = get(addr, "/rounds/dumpstar/20261015T000000Z").await;
    assert!(page.contains("red — suite test-widgets red"), "{page}");
    assert!(page.contains("href=\"https://forge.test/o/dumpstar/pull/66\""), "{page}");
    let (_, closed) = get(addr, "/rounds/dumpstar/20261014T000000Z").await;
    assert!(closed.contains("closed without merging — re-cut the same members"), "{closed}");
}

#[tokio::test]
async fn a_failing_broker_says_ci_detail_is_unavailable_and_still_serves_the_members() {
    let (addr, _run) = forge_loom().await;
    let (code, page) = get(addr, "/rounds/pokedumpster/20261015T020000Z").await;
    assert_eq!(code, 200);
    assert!(page.contains("CI detail unavailable") && page.contains("sp-aaa"), "{page}");
    let (code, _) = get(addr, "/rounds/pokedumpster/nope").await;
    assert_eq!(code, 404);
}

// ── /rounds ──────────────────────────────────────────────────────────────────

fn fixture_log(name: &str) -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).expect("a fixture")
}

fn batch_json(id: &str, state: &str, pr: i64, last_verdict: Option<&str>, passes: &str) -> String {
    let verdict = last_verdict.map(|v| format!("\"{v}\"")).unwrap_or("null".into());
    format!(
        "{{\"batch_id\":\"{id}\",\"repo\":\"spira\",\"state\":\"{state}\",\"pr\":{pr},\"phase\":\"suites\",\"progress\":{{\"done\":120,\"total\":470}},\"opened_at\":1,\"last_at\":4000,\
         \"head\":\"aaaaaaaaaaaaaaaa\",\"base\":\"bbbbbbbbbbbbbbbb\",\"run\":\"ec2-6ad1374f-6d7c5-0\",\
         \"members\":[{{\"batch_id\":\"{id}\",\"bead_id\":\"sp-ejected1\",\"outcome\":null,\"tip\":\"1111111111\",\"title\":\"the red one\"}},{{\"batch_id\":\"{id}\",\"bead_id\":\"sp-kept2\",\"outcome\":null,\"tip\":\"2222222222\",\"title\":\"the kept one\"}}],\
         \"ejected\":[{{\"batch_id\":\"{id}\",\"bead_id\":\"sp-ejected1\",\"reason\":\"red: test-acceptance-ci-scratch-queue.sh\"}}],\
         \"passes\":{passes},\"last_pass\":{{\"n\":2,\"verdict\":{verdict}}}}}"
    )
}

fn rounds_lc(dir: &PathBuf, red: &str, live: &str, forge: &str) -> String {
    let bin = dir.join("spira-lc");
    script(
        &bin,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'list --batches --window'*) echo '[{red},{live},{forge}]' ;;\n  'list --batches --batch r-auto-194') echo '[{red}]' ;;\n  'list --batches --batch r-auto-200') echo '[{live}]' ;;\n  'list --batches --batch none') echo '[]' ;;\n  *) echo 'bad' >&2; exit 1 ;;\nesac\n"
        ),
    );
    bin.to_string_lossy().into_owned()
}

const PASSES_194: &str = "[{\"n\":1,\"head\":\"h1\",\"started_at\":1,\"ended_at\":220,\"verdict\":\"red\",\"red_suites\":[\"test-acceptance-ci-scratch-queue.sh\"],\"build_s\":42,\"suites_s\":158,\"ejects\":1},{\"n\":2,\"head\":\"h2\",\"started_at\":300,\"ended_at\":560,\"verdict\":\"green\",\"build_s\":45,\"suites_s\":155,\"ejects\":0}]";

async fn rounds_fixture() -> (SocketAddr, testkit::TempDir) {
    let dir = scratch("rounds");
    let run = dir.join("run");
    let results = run.join("vm-results");
    std::fs::create_dir_all(&results).expect("vm-results");
    std::fs::write(results.join("cert-rauto194.log"), fixture_log("pass-red-2026-10-15.log")).unwrap();
    std::fs::write(results.join("cert-auto-r-auto-194-6161a0ecea28.log"), fixture_log("pass-recert-2026-10-15.log")).unwrap();
    let older = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::OpenOptions::new().write(true).open(results.join("cert-rauto194.log")).unwrap().set_modified(older).unwrap();
    std::fs::write(results.join("cert-rauto200.log"), "round-vm: EC2 instance type c7i.8xlarge for ec2-x\n  test-a.sh                        ok      3s\nround-vm: building the next suite\n").unwrap();
    let red = batch_json("r-auto-194", "LANDED", 0, Some("green"), PASSES_194);
    let live = batch_json("r-auto-200", "CI_RUNNING", 0, None, "[{\"n\":1,\"head\":\"h3\",\"started_at\":10}]");
    let forge = batch_json("pr-77", "CI_RUNNING", 77, None, "[]");
    let mut c = ops_cfg(&run.to_string_lossy());
    c.lc = rounds_lc(&dir.to_path_buf(), &red, &live, &forge);
    (spawn(c).await, dir)
}

#[tokio::test]
async fn the_rounds_list_names_every_round_and_links_only_the_vm_rounds() {
    let (addr, _dir) = rounds_fixture().await;
    let (code, page) = get(addr, "/rounds").await;
    assert_eq!(code, 200, "{page}");
    assert!(page.contains("/rounds/spira/r-auto-194") && page.contains("/rounds/spira/r-auto-200"), "{page}");
    assert!(page.contains("forge PR #77") && !page.contains("/rounds/spira/pr-77"), "a forge batch is listed, not linked: {page}");
    assert!(page.contains("ec2-6ad1374f-6d7c5-0") && page.contains("120/470"), "{page}");
}

#[tokio::test]
async fn a_re_certified_round_page_shows_both_passes_both_logs_and_the_red_suite_first() {
    let (addr, _dir) = rounds_fixture().await;
    let (code, page) = get(addr, "/rounds/spira/r-auto-194?log=cert-rauto194").await;
    assert_eq!(code, 200, "{page}");
    assert!(page.contains("ejected: red: test-acceptance-ci-scratch-queue.sh") && page.contains("/beads/sp-kept2"), "{page}");
    assert!(page.contains("cert-rauto194") && page.contains("cert-auto-r-auto-194-6161a0ecea28"), "both certification logs are listed: {page}");
    let (red_at, ok_at) = (page.find("test-acceptance-ci-scratch-queue.sh</td>").expect("the red suite"), page.find("test-ops-read-model.sh</td>").expect("a green suite"));
    assert!(red_at < ok_at, "reds first");
    assert!(page.contains("not ok 10 - publish-settle has nothing to publish") && page.contains("c7i.8xlarge"), "{page}");
    assert!(page.contains("bbbbbbbbbb → aaaaaaaaaa"), "{page}");

    let (_, recert) = get(addr, "/rounds/spira/r-auto-194").await;
    assert!(recert.contains("470 ran · 0 red"), "the newest log is the default: {recert}");
    assert!(!recert.contains("not ok 10 - publish-settle"), "positive control: the re-cert log has no red line");
}

#[tokio::test]
async fn the_round_json_carries_the_suite_table_and_the_member_outcomes() {
    let (addr, _dir) = rounds_fixture().await;
    let (code, body) = get(addr, "/api/rounds/spira/r-auto-194").await;
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["members"][0]["outcome"], "ejected: red: test-acceptance-ci-scratch-queue.sh");
    assert_eq!(v["members"][1]["outcome"], "landed");
    let logs = v["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["parsed"]["verdict"], "red");
    assert_eq!(logs[0]["parsed"]["suites"][0]["suite"], "test-acceptance-ci-scratch-queue.sh");
    assert_eq!(logs[1]["parsed"]["verdict"], "green");
}

#[tokio::test]
async fn a_live_round_tails_its_log_and_an_unknown_round_is_a_404() {
    let (addr, _dir) = rounds_fixture().await;
    let (code, page) = get(addr, "/rounds/spira/r-auto-200").await;
    assert_eq!(code, 200, "{page}");
    assert!(page.contains("setInterval") && page.contains("round-vm: building the next suite"), "{page}");
    let (code, tail) = get(addr, "/rounds/spira/r-auto-200/log").await;
    assert_eq!(code, 200);
    assert!(tail.contains("building the next suite"), "{tail}");
    let (_, done) = get(addr, "/rounds/spira/r-auto-194").await;
    assert!(!done.contains("setInterval"), "positive control: a finished round does not poll");
    assert_eq!(get(addr, "/rounds/spira/none").await.0, 404);
    assert_eq!(get(addr, "/rounds/spira/a%20b").await.0, 400);
}

#[tokio::test]
async fn a_failing_lifecycle_read_serves_the_last_good_rounds_page_marked_stale() {
    let dir = scratch("rounds-stale");
    let flag = dir.join("broken");
    let bin = dir.join("spira-lc");
    let red = batch_json("r-auto-194", "LANDED", 0, Some("green"), PASSES_194);
    script(&bin, &format!("#!/bin/sh\nif [ -e {} ]; then echo down >&2; exit 2; fi\necho '[{red}]'\n", flag.display()));
    let mut c = ops_cfg(&dir.join("run").to_string_lossy());
    c.lc = bin.to_string_lossy().into_owned();
    c.cache = Duration::from_millis(0);
    let addr = spawn(c).await;
    let (_, fresh) = get(addr, "/rounds").await;
    assert!(fresh.contains("r-auto-194") && !fresh.contains("last good read"), "{fresh}");
    std::fs::write(&flag, b"").unwrap();
    let (code, stale) = get(addr, "/rounds").await;
    assert_eq!(code, 200);
    assert!(stale.contains("r-auto-194") && stale.contains("last good read"), "{stale}");
}
