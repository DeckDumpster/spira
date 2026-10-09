//! Loom's read endpoints: the stuck page and its bead timelines, the lifecycle view, and
//! `GET /api/ops` for the Spira ops dashboard. Every one reads the lifecycle read model or
//! the collector's snapshot files; none reads `bd`.

pub mod ops;
pub mod stuck;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use ops::OpsSnapshot;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

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
        })
    }
}

pub struct Loom {
    cfg: Config,
    /// The ops snapshot cache. A 5-second TTL: two small file reads and one systemctl call.
    ops_cell: Mutex<Option<Arc<OpsSnapshot>>>,
}

impl Loom {
    pub fn new(cfg: Config) -> Loom {
        Loom {
            cfg,
            ops_cell: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
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
}

/// Directories prepended to the child's PATH: this process may be started by a service
/// manager whose PATH has neither the lifecycle binary nor the tools behind it.
fn child_path(extra: &[String]) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    if extra.is_empty() {
        inherited
    } else {
        format!("{}:{}", extra.join(":"), inherited)
    }
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
        .env("PATH", child_path(&cfg.extra_path))
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

async fn root_route() -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/stuck")
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::empty())
        .expect("a redirect with a valid status and headers")
}

pub fn router(loom: Arc<Loom>) -> Router {
    Router::new()
        .route("/lifecycle", get(lifecycle_page_route))
        .route("/api/lifecycle", get(lifecycle_api_route))
        .route("/stuck", get(stuck_page_route))
        .route("/api/stuck", get(stuck_api_route))
        .route("/stuck/{id}", get(stuck_bead_route))
        .route("/api/ops", get(ops_route))
        .route("/", get(root_route))
        .with_state(loom)
}
