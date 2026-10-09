//! Loom's read endpoints: `GET /api/beads` for the live beads graph and `GET /api/ops` for
//! the Spira ops dashboard (a mirror of the cockpit's health column, readable on a phone).
//!
//! WHAT IS AND IS NOT HERE. This serves the graph and nothing else — no layout, no
//! components, no buckets, no ranking. All of that measured 2 ms in the browser at the live
//! corpus and 60 ms at a hundred times it, so computing it here would buy nothing and cost a
//! rendering stack. The payload is the lifecycle store's live and recently landed rows, shaped
//! as the page reads them, and the page derives every view from that.
//!
//! WHY THERE IS A BUDGET AT ALL. A query per request is the simple choice, and it is the
//! right one only while it stays cheap. The meter ships with the mechanism rather than after
//! it: the query's wall time is recorded and served, and a query that overruns is REFUSED
//! rather than served late. Serving a stale snapshot instead would be kinder to one reader
//! and fatal to the design, because it would hide the one signal that says the simple choice
//! has stopped being adequate.
//!
//! THE BUDGET BOUNDS THE QUERY, and the work either side of it is reported next to it as
//! `refresh_ms`. Parsing most of a megabyte of JSON is not free, and a refresh that grew slow
//! by growing its payload rather than its query would otherwise be invisible — the number
//! nobody publishes is the one that grows.

pub mod beads;
pub mod ops;
pub mod stuck;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use beads::QueryError;
use ops::OpsSnapshot;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

// The static page and its two scripts, embedded at compile time. This makes the binary
// self-contained: one thing to install, one thing to start, no separate asset directory to
// keep in sync. The tradeoff is a recompile on any page change; at these file sizes that is
// under a second.
const PAGE_HTML: &str = include_str!("../static/loom.html");
const MODEL_JS: &str = include_str!("../static/model.js");
const APP_JS: &str = include_str!("../static/app.js");

#[derive(Clone, Debug)]
pub struct Config {
    /// The `spira-lc` to run, from `SPIRA_LC_BIN`.
    pub lc: String,
    /// Prepended to the child's PATH, from the harness's configured `SPIRA_PATH`.
    pub extra_path: Vec<String>,
    /// The deadline on one lifecycle read.
    pub budget: Duration,
    /// How long a parsed snapshot is held. Bounding the cost by TIME rather than by viewer is
    /// what makes ten open tabs cost one query instead of ten. It is a cache and not a
    /// background job: nothing runs when nobody is looking.
    pub cache: Duration,
    /// Where the server listens.
    pub addr: String,
    /// The runtime directory holding cockpit.env and the world stamps.
    /// From `SPIRA_RUN`. Empty means no ops endpoint data.
    pub run: String,
    /// Spira instance name, used to construct the sentinel timer unit name.
    /// From `SPIRA_INSTANCE`, default "prod".
    pub instance: String,
    /// The `systemctl` binary to use for sentinel-active checks. Overridable in tests.
    pub systemctl: String,
    /// The `spira-lc` the stuck page reads the lifecycle read model through.
    pub lc: String,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = spira_config::resolve::locate_home_for_process().map_err(|e| format!("loom: {e}"))?;
        Ok(Config {
            lc: spira_config::lifecycle_row::lc_bin(),
            extra_path: cfg("SPIRA_PATH")
                .map_err(|e| format!("loom: {e}"))?
                .split(':')
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect(),
            budget: Duration::from_millis(cfg_parse::<u64>("SPIRA_LOOM_BUDGET_MS").map_err(|e| format!("loom: {e}"))?),
            cache: Duration::from_secs(cfg_parse::<u64>("SPIRA_LOOM_CACHE_S").map_err(|e| format!("loom: {e}"))?),
            addr: cfg("SPIRA_LOOM_ADDR").map_err(|e| format!("loom: {e}"))?,
            run: cfg("SPIRA_RUN").map_err(|e| format!("loom: {e}"))?,
            instance: spira_config::resolve::resolve_instance(&env, &home).map_err(|e| format!("loom: {e}"))?,
            // SPIRA_SYSTEMCTL is not a registered config key — test-only override, left as env.
            systemctl: std::env::var("SPIRA_SYSTEMCTL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "systemctl".to_string()),
            // SPIRA_LC_BIN is not a registered config key — a bare binary found on PATH.
            lc: std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".to_string()),
        })
    }
}

/// One parsed read of the graph, and what it cost to take.
///
/// The rows are kept as SERIALISED JSON rather than as parsed values. A response is then a
/// short header spliced onto text that already exists, instead of re-serialising most of a
/// megabyte on every request for a payload that has not changed.
struct Snapshot {
    taken: Instant,
    generated_at_ms: u128,
    beads_json: String,
    edges_json: String,
    count: usize,
    edge_count: usize,
    query_ms: u128,
    refresh_ms: u128,
}

pub struct Loom {
    cfg: Config,
    /// The lock is held ACROSS a refresh, which is what makes concurrent readers cost one
    /// query rather than one each: the second waits and then finds the snapshot the first
    /// took. That wait is reported as `wait_ms`, because serialising every reader behind one
    /// query is the corner this simple design would run out of first, and it should be seen
    /// arriving rather than deduced afterwards.
    cell: Mutex<Option<Arc<Snapshot>>>,
    /// The ops snapshot cache. A 5-second TTL rather than the beads cache's configurable one:
    /// two small file reads and one systemctl call are much cheaper than a bd query.
    ops_cell: Mutex<Option<Arc<OpsSnapshot>>>,
}

impl Loom {
    pub fn new(cfg: Config) -> Loom {
        Loom {
            cfg,
            cell: Mutex::new(None),
            ops_cell: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    async fn refresh(&self) -> Result<Snapshot, QueryError> {
        let began = Instant::now();
        let c = &self.cfg;

        // Both views are read together: each is a few indexed lookups on the held store
        // connection, and the budget bounds the slower of the two.
        let (live, recent) = tokio::join!(
            beads::ops_view(&c.lc, &c.extra_path, "live", "ops_live", c.budget),
            beads::ops_view(&c.lc, &c.extra_path, "recent", "ops_recent", c.budget),
        );
        let ((live, live_ms), (recent, recent_ms)) = (live?, recent?);
        let query_ms = live_ms.max(recent_ms);
        let rows: Vec<Value> = live
            .iter()
            .filter_map(|r| beads::page_row(r, false))
            .chain(recent.iter().filter_map(|r| beads::page_row(r, true)))
            .collect();

        Ok(Snapshot {
            taken: Instant::now(),
            generated_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            count: rows.len(),
            edge_count: 0,
            edges_json: "[]".to_string(),
            beads_json: serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string()),
            query_ms,
            refresh_ms: began.elapsed().as_millis(),
        })
    }

    /// The whole response body: the meter, then the graph.
    fn body(&self, s: &Snapshot, wait_ms: u128) -> String {
        let meta = json!({
            // Epoch milliseconds, not a formatted timestamp. There is no timezone to get
            // wrong and no date library to carry, and the page turns it into a local time
            // in one call.
            "generated_at_ms": s.generated_at_ms as u64,
            "age_ms": s.taken.elapsed().as_millis() as u64,
            "cache_s": self.cfg.cache.as_secs(),
            "budget_ms": self.cfg.budget.as_millis() as u64,
            "wait_ms": wait_ms as u64,
            "refresh_ms": s.refresh_ms as u64,
            "query_ms": s.query_ms as u64,
            "count": s.count,
            "edge_count": s.edge_count,
            "dropped_closed": 0,
            "dropped_edges": 0,
        });
        let mut out = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_string());
        out.pop(); // the closing brace; the two big arrays are spliced in as text
        out.push_str(",\"beads\":");
        out.push_str(&s.beads_json);
        out.push_str(",\"edges\":");
        out.push_str(&s.edges_json);
        out.push('}');
        out
    }

    /// Serve the ops dashboard, refreshing first if the held snapshot has aged out (5 s TTL).
    pub async fn serve_ops(&self) -> (StatusCode, String) {
        const OPS_CACHE: Duration = Duration::from_secs(5);
        let mut held = self.ops_cell.lock().await;
        let fresh = held
            .as_ref()
            .map(|s| s.taken.elapsed() < OPS_CACHE)
            .unwrap_or(false);
        if !fresh {
            let snap = OpsSnapshot::take(
                &self.cfg.run,
                &self.cfg.instance,
                &self.cfg.systemctl,
            )
            .await;
            *held = Some(Arc::new(snap));
        }
        let s = held.as_ref().expect("a snapshot was just stored");
        (StatusCode::OK, s.body.clone())
    }

    /// Serve the graph, refreshing first if the held snapshot has aged out.
    pub async fn serve(&self) -> (StatusCode, String) {
        let asked = Instant::now();
        let mut held = self.cell.lock().await;
        let wait_ms = asked.elapsed().as_millis();

        let fresh = held
            .as_ref()
            .map(|s| s.taken.elapsed() < self.cfg.cache)
            .unwrap_or(false);
        if !fresh {
            match self.refresh().await {
                Ok(s) => *held = Some(Arc::new(s)),
                Err(e) => {
                    // THE STALE SNAPSHOT IS NOT SERVED IN ITS PLACE. A refusal that
                    // quietly degrades to old data is a refusal nobody ever sees, and this
                    // one is the whole reason the meter exists. The aged-out snapshot is
                    // kept rather than discarded so the next request retries the refresh
                    // instead of finding an empty cell and treating a slow box as an empty
                    // graph.
                    let (status, detail) = match &e {
                        QueryError::OverBudget { name, budget_ms } => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            json!({
                                "error": "over budget",
                                "query": name,
                                "budget_ms": budget_ms,
                            }),
                        ),
                        QueryError::Failed { name, detail } => (
                            StatusCode::BAD_GATEWAY,
                            json!({"error": "query failed", "query": name, "detail": detail}),
                        ),
                    };
                    return (status, detail.to_string());
                }
            }
        }
        let s = held.as_ref().expect("a refresh either stored or returned");
        (StatusCode::OK, self.body(s, wait_ms))
    }
}

async fn beads_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = loom.serve().await;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        // No intermediate cache in front of a snapshot that already carries its own age. A
        // browser holding a response for its own reasons would make `age_ms` a lie, and the
        // age is how a reader tells a live graph from a frozen one.
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

async fn ops_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = loom.serve_ops().await;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

fn static_response(content_type: &'static str, body: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(body.into())
        .expect("a static response with a valid type")
}

async fn page_route() -> Response {
    static_response("text/html; charset=utf-8", PAGE_HTML)
}

async fn model_js_route() -> Response {
    static_response("text/javascript; charset=utf-8", MODEL_JS)
}

async fn app_js_route() -> Response {
    static_response("text/javascript; charset=utf-8", APP_JS)
}

/// The lifecycle-lens ops view (sp-lpw5ol): the snapshot the cockpit's `lc-view` pane wrote on
/// its last pass, rendered from the same `View` the pane drew — so the phone page and the pane
/// cannot disagree. Read from `<run>/lcview/snapshot.json`; never runs `bd`.
fn lifecycle_snapshot(loom: &Loom) -> Result<(cockpit_ops::lcview::Snapshot, i64), String> {
    let path = std::path::Path::new(&loom.config().run).join("lcview").join("snapshot.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e} — is the lc-view pane running?", path.display()))?;
    let snap: cockpit_ops::lcview::Snapshot = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    Ok((snap.clone(), now - snap.now))
}

/// Older than this, the page says the collector has stopped rather than showing old numbers as live.
const LIFECYCLE_STALE_S: i64 = 60;

async fn lifecycle_page_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match lifecycle_snapshot(&loom) {
        Ok((snap, age)) => {
            let v = cockpit_ops::lcview::view(&snap);
            (StatusCode::OK, cockpit_ops::lcview::render_html(&v, (age > LIFECYCLE_STALE_S).then_some(age), 10))
        }
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=10><p>lifecycle view unavailable: {e}</p>")),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

async fn lifecycle_api_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match lifecycle_snapshot(&loom) {
        Ok((snap, age)) => {
            let v = cockpit_ops::lcview::view(&snap);
            (StatusCode::OK, serde_json::json!({ "age_s": age, "stale": age > LIFECYCLE_STALE_S, "view": v }).to_string())
        }
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "error": e }).to_string()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

async fn lc_json(loom: &Loom, args: &[&str]) -> Result<Value, String> {
    let cfg = loom.config();
    let mut cmd = tokio::process::Command::new(&cfg.lc);
    cmd.args(args)
        .env("PATH", beads::child_path(&cfg.extra_path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(cfg.budget, cmd.output())
        .await
        .map_err(|_| format!("spira-lc {} over its {} ms budget", args.join(" "), cfg.budget.as_millis()))?
        .map_err(|e| format!("could not run {}: {e}", cfg.lc))?;
    if !out.status.success() {
        return Err(format!("spira-lc {} exited {}: {}", args.join(" "), out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("spira-lc {}: {e}", args.join(" ")))
}

fn now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn html_response(status: StatusCode, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

fn unavailable(what: &str, e: &str) -> Response {
    html_response(StatusCode::SERVICE_UNAVAILABLE, format!("<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=10><p>{what} unavailable: {}</p>", stuck::esc(e)))
}

async fn stuck_rows(loom: &Loom) -> Result<(i64, Vec<stuck::Row>), String> {
    let (gantt, dwell, p95) = tokio::join!(lc_json(loom, &["ops-gantt"]), lc_json(loom, &["ops-view", "ops_dwell"]), lc_json(loom, &["ops-view", "ops_dwell_p95"]));
    let (gantt, dwell, p95) = (gantt?, dwell?, p95?);
    let rows = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let now = stuck::num(&gantt["now"]).unwrap_or_else(now_s);
    Ok((now, stuck::build(now, &rows(&gantt["events"]), &rows(&dwell), &stuck::p95_map(&rows(&p95)))))
}

async fn stuck_page_route(State(loom): State<Arc<Loom>>) -> Response {
    match stuck_rows(&loom).await {
        Ok((now, rows)) => html_response(StatusCode::OK, stuck::render_page(now, &rows)),
        Err(e) => unavailable("stuck view", &e),
    }
}

async fn stuck_api_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match stuck_rows(&loom).await {
        Ok((now, rows)) => (StatusCode::OK, json!({ "now": now, "rows": rows.iter().map(stuck::row_json).collect::<Vec<_>>() }).to_string()),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, json!({ "error": e }).to_string()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

async fn stuck_bead_route(State(loom): State<Arc<Loom>>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return html_response(StatusCode::BAD_REQUEST, "<!doctype html><p>not a bead id</p>".into());
    }
    match lc_json(&loom, &["ops-bead", &id]).await {
        Ok(d) if d.get("events").is_some() => html_response(StatusCode::OK, stuck::render_bead(stuck::num(&d["now"]).unwrap_or_else(now_s), &id, &d)),
        Ok(_) => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such bead</p>".into()),
        Err(e) if e.contains("exited 1") => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such bead</p>".into()),
        Err(e) => unavailable("bead timeline", &e),
    }
}

pub fn router(loom: Arc<Loom>) -> Router {
    Router::new()
        .route("/lifecycle", get(lifecycle_page_route))
        .route("/api/lifecycle", get(lifecycle_api_route))
        .route("/stuck", get(stuck_page_route))
        .route("/api/stuck", get(stuck_api_route))
        .route("/stuck/{id}", get(stuck_bead_route))
        .route("/api/beads", get(beads_route))
        .route("/api/ops", get(ops_route))
        .route("/", get(page_route))
        .route("/model.js", get(model_js_route))
        .route("/app.js", get(app_js_route))
        .with_state(loom)
}
