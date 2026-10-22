//! Loom's read endpoints: the stuck page and its bead timelines, the lifecycle view, and
//! `GET /api/ops` for the Spira ops dashboard, and `/units`, the per-unit run history. Every one reads the lifecycle read model or
//! the collector's snapshot files; none reads `bd`.

pub mod aeons;
pub mod forge_rounds;
pub mod now;
pub mod ops;
pub mod shell;
pub mod rounds;
pub mod states;
pub mod stuck;
pub mod telemetry;
pub mod units;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use ops::OpsSnapshot;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The metrics store the units page reads executions from, from `SPIRA_METRICS_ENDPOINT`.
    pub metrics_endpoint: Option<String>,
    /// The `broker` the forge batch pages read CI detail through. Overridable in tests.
    pub broker: String,
    /// The materialised desired-state directory, from `SPIRA_DESIRED_DIR`: the declared unit set the units page scopes to.
    pub desired_dir: String,
    /// The `units-install` that lists the enable manifest when no Composite declares units.
    pub units_install: String,
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
            metrics_endpoint: cfg("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty()),
            broker: std::env::var("SPIRA_BROKER").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "broker".to_string()),
            desired_dir: cfg("SPIRA_DESIRED_DIR").map_err(|e| format!("loom: {e}"))?,
            units_install: std::env::var("SPIRA_UNITS_INSTALL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "units-install".to_string()),
        })
    }
}

pub struct Loom {
    cfg: Config,
    /// The ops snapshot cache. A 5-second TTL: two small file reads and one systemctl call.
    ops_cell: Mutex<Option<Arc<OpsSnapshot>>>,
    /// The units snapshot, held for `cfg.cache`; the lock is kept across the refresh so
    /// concurrent viewers share one.
    units_cell: Mutex<Option<Arc<units::Snapshot>>>,
    units_refreshing: AtomicBool,
    forge_ci: forge_rounds::Cache,
    /// `spira-lc` batch reads by query, each held for `cfg.cache`; a failed refresh serves the last good read, marked stale.
    batches_cell: Mutex<std::collections::HashMap<String, (std::time::Instant, Value)>>,
}

impl Loom {
    pub fn new(cfg: Config) -> Loom {
        Loom {
            cfg,
            ops_cell: Mutex::new(None),
            units_cell: Mutex::new(None),
            units_refreshing: AtomicBool::new(false),
            forge_ci: forge_rounds::Cache::default(),
            batches_cell: Mutex::new(Default::default()),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub async fn units_snapshot(&self) -> Arc<units::Snapshot> {
        let mut held = self.units_cell.lock().await;
        if let Some(s) = held.as_ref().filter(|s| s.taken.elapsed() < self.cfg.cache) {
            return s.clone();
        }
        let src = units::Sources {
            run: self.cfg.run.clone(),
            systemctl: self.cfg.systemctl.clone(),
            metrics_endpoint: self.cfg.metrics_endpoint.clone(),
            extra_path: self.cfg.extra_path.join(":"),
            budget: self.cfg.budget,
            desired_dir: self.cfg.desired_dir.clone(),
            units_install: self.cfg.units_install.clone(),
        };
        let snap = Arc::new(units::take(&src, now_s()).await);
        *held = Some(snap.clone());
        snap
    }

    /// The units snapshot as held, never waiting on the source: a page with a latency budget
    /// shows what it has (marked stale) and starts at most one refresh behind it.
    pub fn units_held(self: &Arc<Self>) -> Option<Arc<units::Snapshot>> {
        let held = self.units_cell.try_lock().ok().and_then(|h| h.clone());
        if held.as_ref().map_or(true, |s| s.taken.elapsed() >= self.cfg.cache) && !self.units_refreshing.swap(true, Ordering::AcqRel) {
            let me = self.clone();
            tokio::spawn(async move {
                me.units_snapshot().await;
                me.units_refreshing.store(false, Ordering::Release);
            });
        }
        held
    }

    /// `list --batches` for `args`: the cached read, the fresh one, or the last good one with the error that kept it from refreshing.
    pub async fn batches(&self, args: &[&str]) -> Result<(Vec<Value>, Option<String>), String> {
        let key = args.join(" ");
        let mut held = self.batches_cell.lock().await;
        if let Some((_, v)) = held.get(&key).filter(|(at, _)| at.elapsed() < self.cfg.cache) {
            return Ok((v.as_array().cloned().unwrap_or_default(), None));
        }
        let mut full = vec!["list", "--batches"];
        full.extend_from_slice(args);
        match lc_json(self, &full).await {
            Ok(v) => {
                held.insert(key, (std::time::Instant::now(), v.clone()));
                Ok((v.as_array().cloned().unwrap_or_default(), None))
            }
            Err(e) => match held.get(&key) {
                Some((_, v)) => Ok((v.as_array().cloned().unwrap_or_default(), Some(e))),
                None => Err(e),
            },
        }
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

/// Work in flight, read from the collector's `cockpit.env` through the same `Flight` the pane
/// uses. A missing or unparsable file is the unreadable view (`?`), never an idle one.
fn inflight(loom: &Loom) -> cockpit_ops::health::inflight::Flight {
    use cockpit_ops::health::{inflight::Flight, model::Snapshot};
    let text = std::fs::read_to_string(std::path::Path::new(&loom.config().run).join("cockpit.env")).unwrap_or_default();
    Flight::from_snapshot(&Snapshot::parse(&text), now_s(), LIFECYCLE_STALE_S)
}

/// Older than this, the page says the collector has stopped rather than showing old numbers as live.
pub(crate) const LIFECYCLE_STALE_S: i64 = 60;

async fn lifecycle_page_route(
    State(loom): State<Arc<Loom>>,
    uri: axum::http::Uri,
) -> Response {
    let fragment = uri.query().is_some_and(|q| q.split('&').any(|kv| kv == "fragment" || kv.starts_with("fragment=")));
    let (status, ctype, body) = match lifecycle_snapshot(&loom) {
        Ok((mut snap, age)) => {
            // The pass's progress is read fresh on every request, not from the pane's last
            // snapshot, so the page's test progress is live (per Ryan 2026-10-09).
            if let Some(p) = std::fs::read(std::path::Path::new(&loom.config().run).join("round-progress.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| cockpit_ops::lcview::PassProgress::from_json(&v))
            {
                snap.progress = Some(p);
                snap.now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(snap.now);
            }
            snap.loom_base = Some(String::new());
            let v = cockpit_ops::lcview::view(&snap);
            let stale = (age > LIFECYCLE_STALE_S).then_some(age);
            if fragment {
                let mut frag: Value = serde_json::from_str(&cockpit_ops::lctui::render_fragment(&v, stale)).unwrap_or_default();
                let banner = format!("{}{}", inflight(&loom).html(now_s()), frag["banner"].as_str().unwrap_or_default());
                frag["banner"] = Value::String(banner);
                (StatusCode::OK, "application/json", frag.to_string())
            } else {
                let html = cockpit_ops::lcview::render_html(&v, stale, 5).replacen("<header id=banner>", &format!("<header id=banner>{}", inflight(&loom).html(now_s())), 1);
                (StatusCode::OK, "text/html; charset=utf-8", shell::wrap_foreign(&html, "lifecycle", &now::bar(Some(&snap), now_s())))
            }
        }
        Err(e) if fragment => (StatusCode::SERVICE_UNAVAILABLE, "application/json", serde_json::json!({ "banner": format!("<div class='line bad'>lifecycle view unavailable: {e}</div>"), "tree": "" }).to_string()),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, "text/html; charset=utf-8", format!("<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=10><p>lifecycle view unavailable: {e}</p>")),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

async fn vm_round_route(State(loom): State<Arc<Loom>>, axum::extract::Path(batch): axum::extract::Path<String>) -> Response {
    const TAIL: u64 = 256 * 1024;
    let (snap, _) = match lifecycle_snapshot(&loom) {
        Ok(x) => x,
        Err(e) => return unavailable("round view", &e),
    };
    let Some(b) = snap.batches.iter().find(|b| b.id == batch && !b.log.is_empty()) else {
        return html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such round</p>".into());
    };
    let vm = snap
        .vm_runs
        .iter()
        .filter(|r| r.run == b.id || r.cmd.iter().any(|a| a.contains(&b.id)))
        .map(|r| r.vm.clone())
        .next()
        .unwrap_or_default();
    let log = match std::fs::File::open(&b.log) {
        Ok(mut f) => {
            use std::io::{Read, Seek, SeekFrom};
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL)));
            let mut buf = Vec::new();
            let _ = f.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        }
        Err(e) => format!("(log not readable: {e})"),
    };
    let members = b.members.iter().filter(|m| !b.ejected.contains(m)).map(|m| format!("<a href=/beads/{0}>{0}</a>", stuck::esc(m))).collect::<Vec<_>>().join(" ");
    let live = snap.vm_runs.iter().any(|r| !r.idle && !r.held && r.run == b.id);
    let refresh = if live { "<meta http-equiv=refresh content=5>" } else { "" };
    html_response(
        StatusCode::OK,
        format!(
            "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>{refresh}<title>{id}</title><style>body{{font:14px monospace;margin:8px;background:#111;color:#ddd}}a{{color:#8cf}}pre{{white-space:pre-wrap;word-break:break-all}}</style><h3>{repo} · {id}</h3><p>{phase} · VM {vm}</p><p>members {members}</p><p><a href=/lifecycle>back</a></p><pre>{log}</pre>",
            id = stuck::esc(&b.id),
            repo = stuck::esc(&b.repo),
            phase = stuck::esc(&b.phase),
            vm = if vm.is_empty() { "—".to_string() } else { stuck::esc(&vm) },
            log = stuck::esc(&log),
        ),
    )
}

async fn lifecycle_api_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match lifecycle_snapshot(&loom) {
        Ok((snap, age)) => {
            let v = cockpit_ops::lcview::view(&snap);
            (StatusCode::OK, serde_json::json!({ "age_s": age, "stale": age > LIFECYCLE_STALE_S, "view": v, "inflight": inflight(&loom) }).to_string())
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

fn page_bar(loom: &Loom) -> shell::Bar {
    now::bar(lifecycle_snapshot(loom).ok().map(|(s, _)| s).as_ref(), now_s())
}

async fn stuck_page_route(State(loom): State<Arc<Loom>>) -> Response {
    match stuck_rows(&loom).await {
        Ok((now, rows)) => html_response(StatusCode::OK, shell::with_bar(&stuck::render_page(now, &rows), &page_bar(&loom))),
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

fn valid_bead_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

async fn stuck_bead_redirect(axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    if !valid_bead_id(&id) {
        return html_response(StatusCode::BAD_REQUEST, "<!doctype html><p>not a bead id</p>".into());
    }
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, format!("/beads/{id}"))
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::empty())
        .expect("a redirect with a valid status and headers")
}

fn bead_rounds_html(snap: &cockpit_ops::lcview::Snapshot, id: &str) -> String {
    let mut rows: Vec<_> = snap.batches.iter().filter(|b| b.members.iter().any(|m| m == id)).collect();
    rows.sort_by_key(|b| std::cmp::Reverse(b.opened_at.max(b.last_at)));
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("<div style=\"padding:.5rem .8rem\"><div class=dim>rounds</div>");
    for b in rows {
        let name = if b.log.is_empty() { stuck::esc(&b.id) } else { format!("<a href=/rounds/{0}>{0}</a>", stuck::esc(&b.id)) };
        let outcome = if b.ejected.iter().any(|m| m == id) { "<span class=red>ejected</span>" } else { "in round" };
        out.push_str(&format!("<div>{name} · {} · {} · {outcome}</div>", stuck::esc(&b.repo), stuck::esc(&b.state)));
    }
    out.push_str("</div>");
    out
}

async fn stuck_bead_route(State(loom): State<Arc<Loom>>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    if !valid_bead_id(&id) {
        return html_response(StatusCode::BAD_REQUEST, "<!doctype html><p>not a bead id</p>".into());
    }
    let rounds = lifecycle_snapshot(&loom).map(|(s, _)| bead_rounds_html(&s, &id)).unwrap_or_default();
    match lc_json(&loom, &["ops-bead", &id]).await {
        Ok(d) if d.get("events").is_some() => html_response(StatusCode::OK, shell::with_bar(&stuck::render_bead(stuck::num(&d["now"]).unwrap_or_else(now_s), &id, &d, &rounds), &page_bar(&loom))),
        Ok(_) => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such bead</p>".into()),
        Err(e) if e.contains("exited 1") => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such bead</p>".into()),
        Err(e) => unavailable("bead timeline", &e),
    }
}

async fn units_page_route(State(loom): State<Arc<Loom>>) -> Response {
    html_response(StatusCode::OK, shell::with_bar(&units::render_page(loom.units_snapshot().await.as_ref()), &page_bar(&loom)))
}

async fn units_api_route(State(loom): State<Arc<Loom>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(units::snapshot_json(loom.units_snapshot().await.as_ref()).to_string().into())
        .expect("a response with a valid status and headers")
}

async fn states_page_route(State(loom): State<Arc<Loom>>) -> Response {
    match lifecycle_snapshot(&loom) {
        Ok((snap, _)) => {
            let view = cockpit_ops::lcview::view(&snap);
            html_response(StatusCode::OK, shell::with_bar(&states::render_page(&view), &now::bar(Some(&snap), now_s())))
        }
        Err(e) => unavailable("states view", &e),
    }
}

async fn states_api_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match lifecycle_snapshot(&loom) {
        Ok((snap, age)) => (StatusCode::OK, states::api_json(&cockpit_ops::lcview::view(&snap), age).to_string()),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, json!({ "error": e }).to_string()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("a response with a valid status and headers")
}

/// The `(bead, phase)` WORKING rows the pane reads, or why they could not be read.
async fn working_rows(loom: &Loom) -> (Vec<(String, String)>, Option<String>) {
    match lc_json(loom, &["list", "--state", "WORKING"]).await.and_then(|v| spira_config::lc_state::parse_rows(&v.to_string())) {
        Ok(rows) => (rows.into_iter().map(|r| (r.bead_id, r.phase.unwrap_or_else(|| "?".into()))).collect(), None),
        Err(e) => (Vec::new(), Some(format!("phases unknown: {e}"))),
    }
}

async fn aeons_data(loom: &Loom) -> Result<(cockpit_ops::health::model::Snapshot, Vec<(String, String)>, Option<String>), String> {
    let path = std::path::Path::new(&loom.config().run).join("cockpit.env");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (working, note) = working_rows(loom).await;
    Ok((cockpit_ops::health::model::Snapshot::parse(&text), working, note))
}

async fn aeons_page_route(State(loom): State<Arc<Loom>>) -> Response {
    match aeons_data(&loom).await {
        Ok((snap, working, note)) => {
            let bar = lifecycle_snapshot(&loom).map_or_else(|_| shell::Bar::default(), |(s, _)| now::bar(Some(&s), now_s()));
            html_response(StatusCode::OK, shell::with_bar(&aeons::render_page(&snap, &working, note.as_deref()), &bar))
        }
        Err(e) => unavailable("aeons view", &e),
    }
}

async fn aeons_api_route(State(loom): State<Arc<Loom>>) -> Response {
    let (status, body) = match aeons_data(&loom).await {
        Ok((snap, working, note)) => {
            let mut v = aeons::render_json(&snap, &working);
            v["note"] = serde_json::json!(note);
            (StatusCode::OK, v.to_string())
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

async fn now_route(State(loom): State<Arc<Loom>>) -> Response {
    match lifecycle_snapshot(&loom) {
        Ok((snap, _)) => {
            let view = cockpit_ops::lcview::view(&snap);
            let held = loom.units_held();
            let units = match held.as_deref() {
                None => now::UnitsState::Pending,
                Some(u) => {
                    let age_s = u.taken.elapsed().as_secs() as i64;
                    now::UnitsState::Held { snap: u, age_s, stale: u.taken.elapsed() >= loom.config().cache }
                }
            };
            let page = now::render_now(&snap, &view, &units);
            html_response(StatusCode::OK, shell::with_bar(&page, &now::bar(Some(&snap), now_s())))
        }
        Err(e) => unavailable("now view", &e),
    }
}

async fn round_route(State(loom): State<Arc<Loom>>, axum::extract::Path((repo, batch)): axum::extract::Path<(String, String)>) -> Response {
    if let Some(b) = forge_rounds::read_all(&forge_queue_dir(&loom)).into_iter().find(|b| b.repo == repo && b.key == batch) {
        let target = format!("{}/{}", b.repo, b.run);
        let fetch = {
            let loom = loom.clone();
            async move { broker_json(&loom, &["read", "run-jobs", &target]).await }
        };
        let ci = loom.forge_ci.ci(&b, fetch).await;
        let snap = lifecycle_snapshot(&loom).ok().map(|(snap, _)| snap);
        let page = forge_rounds::render_detail(now_s(), &b, &ci, snap.as_ref().map_or("", |s| s.forge_base.as_str()));
        let bar = snap.map(|snap| now::bar(Some(&snap), now_s())).unwrap_or_default();
        return html_response(StatusCode::OK, shell::with_bar(&page, &bar));
    }
    match lifecycle_snapshot(&loom) {
        Ok((snap, _)) => match snap.batches.iter().find(|b| b.id == batch && b.repo == repo) {
            Some(b) => html_response(StatusCode::OK, shell::with_bar(&now::render_round(&snap, b), &now::bar(Some(&snap), now_s()))),
            None => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such round</p>".into()),
        },
        Err(_) => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such round</p>".into()),
    }
}

async fn broker_json(loom: &Loom, args: &[&str]) -> Result<Value, String> {
    let cfg = loom.config();
    let bin = cfg.broker.clone();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(args)
        .env("PATH", child_path(&cfg.extra_path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(cfg.budget, cmd.output())
        .await
        .map_err(|_| format!("broker {} over its {} ms budget", args.join(" "), cfg.budget.as_millis()))?
        .map_err(|e| format!("could not run {bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("broker {} exited {}: {}", args.join(" "), out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("broker {}: {e}", args.join(" ")))
}

fn forge_queue_dir(loom: &Loom) -> std::path::PathBuf {
    std::path::Path::new(&loom.config().run).join("queue")
}

async fn rounds_page_route(State(loom): State<Arc<Loom>>) -> Response {
    let window = rounds::WINDOW_S.to_string();
    let forge = forge_rounds::render_list(now_s(), &forge_rounds::read_all(&forge_queue_dir(&loom)));
    match loom.batches(&["--window", &window]).await {
        Ok((batches, stale)) => {
            let page = rounds::render_list(now_s(), &batches, stale.as_deref()).replacen("</main>", &format!("{forge}</main>"), 1);
            html_response(StatusCode::OK, shell::with_bar(&page, &page_bar(&loom)))
        }
        Err(e) => {
            let page = shell::shell("rounds", "Rounds", 15, &format!("<header><h1>Rounds</h1></header><main><p class=dim>round records unavailable: {}</p>{forge}</main>", stuck::esc(&e)));
            html_response(StatusCode::OK, shell::with_bar(&page, &shell::Bar::default()))
        }
    }
}

fn log_param(uri: &axum::http::Uri) -> Option<String> {
    uri.query()?.split('&').find_map(|kv| kv.strip_prefix("log=")).filter(|v| safe_id(v)).map(str::to_string)
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// The round, its pass logs and the selected log (default newest); `None` is a 404.
async fn round_detail(loom: &Loom, id: &str) -> Result<Option<(Value, Option<String>, Vec<std::path::PathBuf>)>, String> {
    let (rows, stale) = loom.batches(&["--batch", id]).await?;
    let Some(batch) = rows.into_iter().next() else { return Ok(None) };
    let heads: Vec<String> = batch["passes"].as_array().into_iter().flatten().filter_map(|p| p["head"].as_str().map(str::to_string)).collect();
    let logs = rounds::pass_logs(&loom.config().run, id, &heads, batch["log"].as_str().unwrap_or(""));
    Ok(Some((batch, stale, logs)))
}

fn pick_log<'a>(logs: &'a [std::path::PathBuf], want: Option<&str>) -> Option<&'a std::path::PathBuf> {
    match want {
        Some(w) => logs.iter().find(|p| rounds::stem(p) == w),
        None => logs.last(),
    }
}

async fn round_page_route(State(loom): State<Arc<Loom>>, axum::extract::Path(id): axum::extract::Path<String>, uri: axum::http::Uri) -> Response {
    if !safe_id(&id) {
        return html_response(StatusCode::BAD_REQUEST, "<!doctype html><p>not a round id</p>".into());
    }
    match round_detail(&loom, &id).await {
        Ok(Some((batch, stale, logs))) => {
            let chosen = pick_log(&logs, log_param(&uri).as_deref());
            let detail = rounds::Detail {
                now: now_s(),
                batch: &batch,
                logs: logs.iter().map(|p| (rounds::stem(p), rounds::read_pass_log(p))).collect(),
                selected: chosen.map(|p| rounds::stem(p)),
                tail: chosen.map(|p| rounds::tail(p)).unwrap_or_default(),
            };
            html_response(StatusCode::OK, shell::with_bar(&rounds::render_detail(&detail, stale.as_deref()), &page_bar(&loom)))
        }
        Ok(None) => html_response(StatusCode::NOT_FOUND, "<!doctype html><p>no such round</p>".into()),
        Err(e) => unavailable("round", &e),
    }
}

async fn round_api_route(State(loom): State<Arc<Loom>>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let json_response = |status, body: Value| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, "no-store")
            .body(body.to_string().into())
            .expect("a response with a valid status and headers")
    };
    if !safe_id(&id) {
        return json_response(StatusCode::BAD_REQUEST, json!({ "error": "not a round id" }));
    }
    match round_detail(&loom, &id).await {
        Ok(Some((batch, stale, logs))) => {
            let detail = rounds::Detail {
                now: now_s(),
                batch: &batch,
                logs: logs.iter().map(|p| (rounds::stem(p), rounds::read_pass_log(p))).collect(),
                selected: None,
                tail: String::new(),
            };
            let mut body = rounds::detail_json(&detail);
            body["stale"] = json!(stale);
            json_response(StatusCode::OK, body)
        }
        Ok(None) => json_response(StatusCode::NOT_FOUND, json!({ "error": "no such round" })),
        Err(e) => json_response(StatusCode::SERVICE_UNAVAILABLE, json!({ "error": e })),
    }
}

async fn round_log_route(State(loom): State<Arc<Loom>>, axum::extract::Path(id): axum::extract::Path<String>, uri: axum::http::Uri) -> Response {
    let plain = |status, body: String| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(body.into())
            .expect("a response with a valid status and headers")
    };
    if !safe_id(&id) {
        return plain(StatusCode::BAD_REQUEST, "not a round id".into());
    }
    let batch = loom.batches(&["--batch", &id]).await.ok().and_then(|(rows, _)| rows.into_iter().next()).unwrap_or(Value::Null);
    let heads = batch["passes"].as_array().into_iter().flatten().filter_map(|p| p["head"].as_str().map(str::to_string)).collect::<Vec<_>>();
    let logs = rounds::pass_logs(&loom.config().run, &id, &heads, batch["log"].as_str().unwrap_or(""));
    match pick_log(&logs, log_param(&uri).as_deref()) {
        Some(p) => plain(StatusCode::OK, rounds::tail(p)),
        None => plain(StatusCode::NOT_FOUND, "no such pass log".into()),
    }
}

/// The file a `/shots/<bead>/<tip>/<file>` request names under `run/shots`, or `None` for anything
/// that is not a bead id, a full commit name and a plain `.png` name.
pub fn shot_path(run: &str, bead: &str, tip: &str, file: &str) -> Option<std::path::PathBuf> {
    let plain = |s: &str, extra: &[char]| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c));
    let stem = file.strip_suffix(".png")?;
    let ok = !run.is_empty() && plain(bead, &['-', '.']) && !bead.starts_with('.') && tip.len() == 40 && tip.bytes().all(|b| b.is_ascii_hexdigit()) && plain(stem, &['-', '_']);
    ok.then(|| std::path::Path::new(run).join("shots").join(bead).join(tip).join(file))
}

async fn shot_route(State(loom): State<Arc<Loom>>, axum::extract::Path((bead, tip, file)): axum::extract::Path<(String, String, String)>) -> Response {
    let status = |code: StatusCode| Response::builder().status(code).body(axum::body::Body::empty()).expect("a response with a valid status");
    let Some(path) = shot_path(&loom.config().run, &bead, &tip, &file) else { return status(StatusCode::BAD_REQUEST) };
    match tokio::task::spawn_blocking(move || std::fs::read(path)).await.unwrap_or_else(|_| Err(std::io::ErrorKind::Other.into())) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "no-store")
            .body(bytes.into())
            .expect("a response with a valid status and headers"),
        Err(_) => status(StatusCode::NOT_FOUND),
    }
}

pub fn router(loom: Arc<Loom>) -> Router {
    Router::new()
        .route("/shots/{bead}/{tip}/{file}", get(shot_route))
        .route("/lifecycle", get(lifecycle_page_route))
        .route("/rounds/{batch}", get(vm_round_route))
        .route("/api/lifecycle", get(lifecycle_api_route))
        .route("/stuck", get(stuck_page_route))
        .route("/api/stuck", get(stuck_api_route))
        .route("/stuck/{id}", get(stuck_bead_redirect))
        .route("/beads/{id}", get(stuck_bead_route))
        .route("/units", get(units_page_route))
        .route("/api/units", get(units_api_route))
        .route("/states", get(states_page_route))
        .route("/api/states", get(states_api_route))
        .route("/rounds", get(rounds_page_route))
        .route("/rounds/spira/{id}", get(round_page_route))
        .route("/rounds/spira/{id}/log", get(round_log_route))
        .route("/api/rounds/spira/{id}", get(round_api_route))
        .route("/api/ops", get(ops_route))
        .route("/aeons", get(aeons_page_route))
        .route("/api/aeons", get(aeons_api_route))
        .route("/", get(now_route))
        .route("/rounds/{repo}/{batch}", get(round_route))
        .with_state(loom)
}

#[cfg(test)]
mod shot_path_tests {
    use super::shot_path;

    const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn only_a_bead_a_full_tip_and_a_plain_png_name_resolve() {
        let ok = shot_path("/r", "sp-ab.1", TIP, "index-390.png").expect("a valid shot");
        assert_eq!(ok, std::path::Path::new("/r/shots/sp-ab.1").join(TIP).join("index-390.png"));
        for (b, t, f) in [("..", TIP, "a.png"), ("sp-a", "abc", "a.png"), ("sp-a", TIP, "../a.png"), ("sp-a", TIP, "a.txt"), ("sp-a/b", TIP, "a.png"), ("sp-a", TIP, ".png")] {
            assert!(shot_path("/r", b, t, f).is_none(), "{b} {t} {f}");
        }
        assert!(shot_path("", "sp-a", TIP, "a.png").is_none());
    }
}

#[cfg(test)]
mod bead_page_tests {
    use super::*;
    use cockpit_ops::lcview::{BatchRow, Snapshot};

    #[test]
    fn a_bead_ejected_from_one_round_and_landed_in_another_lists_both() {
        let row = |id: &str, ejected: &[&str], log: &str, at: i64| BatchRow {
            id: id.into(), members: vec!["sp-x".into(), "sp-y".into()], ejected: ejected.iter().map(|s| s.to_string()).collect(),
            log: log.into(), opened_at: at, repo: "r".into(), state: "x".into(), ..Default::default()
        };
        let snap = Snapshot { batches: vec![row("b-1", &["sp-x"], "/l/1", 1), row("b-2", &[], "", 2), BatchRow { id: "b-3".into(), members: vec!["sp-z".into()], ..Default::default() }], ..Default::default() };
        let html = bead_rounds_html(&snap, "sp-x");
        assert!(html.contains("href=/rounds/b-1") && html.contains("ejected"), "{html}");
        assert!(html.contains("b-2") && !html.contains("href=/rounds/b-2"), "{html}");
        assert!(!html.contains("b-3"), "positive control: a round without the bead is absent: {html}");
        assert!(bead_rounds_html(&snap, "sp-none").is_empty());
    }
}
