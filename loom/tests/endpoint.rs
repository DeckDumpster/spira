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
    for path in FIVE.iter().chain(&["/stuck/sp-2cah6", "/rounds/dumpstar/b-live1"]) {
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
