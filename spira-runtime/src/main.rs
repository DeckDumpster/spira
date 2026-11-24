//! `spira-runtime serve` — runs until stopped. Config is read once here and pinned.

use std::sync::Arc;

use runtime::host::{serve_with, ServeOpts};
use runtime::ports::WallClock;
use runtime::socket::SocketJournal;
use runtime::FileMarker;
use spira_runtime::notify::Notifier;
use spira_runtime::{sink::OtlpSink, Effects, Settings};
use std::time::Duration;
use tokio::sync::mpsc;

const USAGE: &str = "usage: spira-runtime serve|check-config|reflect <ids>…|land-escalate <age-secs>";

/// The OTLP log queue: a full queue drops and counts rather than blocking the loop.
const LOG_QUEUE: usize = 1024;
const SUBMIT_QUEUE: usize = 16;
const NO_WATCHDOG_BOOT: Duration = Duration::from_secs(20);

/// A line for the log that already carries its UTC stamp; any other `String` error is stamped on the way out.
struct Stamped(String);

impl From<String> for Stamped {
    fn from(line: String) -> Self {
        Stamped(spira_runtime::stamped(&format!("spira-runtime: {line}")))
    }
}

fn check() -> Result<(), String> {
    spira_config::process::check_config(&Settings::keys())?;
    Ok(())
}

fn land_escalate(age: Option<String>) -> Result<(), String> {
    use spira_runtime::tasks::a::sentinel_pass as sp;
    let age: u64 = age.and_then(|a| a.parse().ok()).ok_or(USAGE)?;
    let run = spira_config::process::cfg("SPIRA_RUN")?;
    let w = sp::land_escalation(age, &sp::land_log_tail(std::path::Path::new(&run)));
    let spec = runtime::event::EffectSpec::keyed(sp::ASK, "land-escalate", serde_json::to_string(&w).map_err(|e| e.to_string())?, 5_000);
    sp::ask(&spec).map(|subject| println!("{subject}"))  // loop-safe: a one-shot verb, no loop runs
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(spira_runtime::tasks::a::sentinel_pass::REFLECT_VERB) {
        let ids = spira_runtime::tasks::a::sentinel_pass::reflect_ids(std::env::args().skip(2));
        match sentinel::reflect::run(&ids) {
            Ok(log) => print!("{log}"),
            Err(e) => {
                eprintln!("spira-runtime reflect: {e}"); // loop-safe: no event loop exists on this path
                std::process::exit(1); // loop-safe: same
            }
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("land-escalate") && std::env::args().count() == 3 {
        if let Err(e) = land_escalate(std::env::args().nth(2)) {
            eprintln!("spira-runtime: {e}");  // loop-safe: a one-shot verb, no loop runs
            std::process::exit(1);  // loop-safe: a one-shot verb, no loop runs
        }
        return;
    }
    let verb = std::env::args().nth(1);
    if std::env::args().count() != 2 || !matches!(verb.as_deref(), Some("serve" | "check-config")) {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    if let Err(e) = run(verb.as_deref() == Some("check-config")) {
        eprintln!("{}", e.0);
        std::process::exit(1);
    }
}

fn run(check_only: bool) -> Result<(), Stamped> {
    check()?;
    let settings = Settings::load()?;
    if check_only {
        let mode = settings.task_modes.get(spira_runtime::tasks::c::lc_ship::NAME).copied().unwrap_or(runtime::Mode::Observe);
        spira_runtime::tasks::c::lc_ship::check_credentials(mode, &spira_config::process::cfg(spira_runtime::tasks::c::lc_ship::URL_KEY)?, |k| std::env::var(k).ok())?;
        println!("ok {}", settings.hash());
        return Ok(());
    }
    settings.marker_path()?;
    if let Some(credential) = settings.reconciler.values.get("SPIRA_LC_PASSWORD_FILE") {
        // Before any thread exists: the programs the reconciler launches read the store's credential from their environment.
        std::env::set_var("SPIRA_LC_PASSWORD_FILE", credential);
    }
    let notifier = Arc::new(spira_runtime::notify::Notifier::from_env().map_err(|e| format!("notify socket: {e}"))?);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(serve(settings, notifier))
}

async fn serve(settings: Settings, notifier: Arc<Notifier>) -> Result<(), Stamped> {
    let base_mail = mail::env::Env::load().map_err(|e| format!("mail environment: {e}"))?;
    let mail_env = spira_runtime::operator_mail(base_mail.clone());
    let concierge_mail = spira_runtime::concierge_mail(base_mail);
    let kick: Arc<dyn Fn() + Send + Sync> = {
        let n = notifier.clone();
        Arc::new(move || n.kick())
    };
    let max_sleep = notifier.max_sleep();
    let metrics = spira_otel::metrics::MetricsSink::for_component(settings.metrics_endpoint.as_deref(), "spira-runtime");
    let debug = spira_config::process::cfg("SPIRA_RUNTIME_LOG_DEBUG")? == "1";
    let cap_mb: u64 = spira_config::process::cfg_parse("SPIRA_RUNTIME_LOG_CAP_MB")?;
    let keep: usize = spira_config::process::cfg_parse("SPIRA_RUNTIME_LOG_KEEP")?;
    let logs = spira_otel::logs::LogSink::new(settings.otel_dir(), "spira-runtime").capped(cap_mb.max(1).saturating_mul(1 << 20), keep);
    let sink = Arc::new(OtlpSink::with_metrics(logs, LOG_QUEUE, metrics, debug));
    let journal = Arc::new(SocketJournal::new(settings.lc_socket.clone(), spira_otel::vlogs::Shipper::start(&settings.victorialogs_url)));
    let clock = Arc::new(WallClock);
    let board = Arc::new(runtime::engine::StatusBoard::default());
    let deadline = max_sleep.map_or(NO_WATCHDOG_BOOT, |m| m * 2);
    let startup = Arc::new(spira_runtime::Startup::new(deadline));
    let boot = {
        let (settings, clock, sink, board, startup) = (settings.clone(), clock.clone(), sink.clone(), board.clone(), startup.clone());
        move || spira_runtime::start_engine(&settings, journal, clock, Arc::new(FileMarker(settings.marker_path().expect("checked before the engine boots"))), sink, board, &startup)
    };
    let mut starting = runtime::offload::offload("startup", None, boot).map_err(|e| e.to_string())?;
    let started = tokio::time::timeout(deadline, &mut starting).await.map_err(|_| Stamped(startup.unfinished()))?.map_err(|e| e.to_string())?;
    let (engine, store) = started.map_err(Stamped)?;
    let (tx, rx) = mpsc::channel(SUBMIT_QUEUE);
    // One unit manager: the `unit.*` effects and the `unit.ran` source share it, so one reader drains its signals.
    let backend = runtime::units::from_env(clock.clone()).map_err(|e| format!("unit manager: {e}"))?;
    let _sources = spira_runtime::sources::real(&|k: &str| spira_config::process::cfg(k), backend.completions(), clock.clone(), store)?.start(tx.clone());
    let repin = Arc::new(spira_runtime::repin::Repin::new(settings.clone(), Arc::new(|keys: &[String]| spira_config::process::fresh(keys)), Arc::new(|k: &str| spira_config::process::cfg(k))));
    let control = spira_runtime::status::Control::new(board.clone(), spira_runtime::control_submit(tx), clock.clone()).with_repin(repin.clone());
    spira_runtime::spawn_control(&settings.control_socket, control)?;
    notifier.ready();
    let opts = ServeOpts { kick: Some(kick), max_sleep_ms: max_sleep.map(|d| d.as_millis() as u64), sink: Some(sink), pool: None, before_turn: Some(Arc::new(move |engine| repin.apply(engine))), turn_budget: None };
    let units = backend.units();
    serve_with(engine, clock, Arc::new(Effects { heartbeat_url: settings.heartbeat_url.clone(), world: settings.world.clone(), forge: settings.forge.clone(), mail: Some(Arc::new(mail_env)), reconciler: Some(settings.reconciler.clone()), concierge_mail: Some(Arc::new(concierge_mail)), units: Some(units), board: Some(board), ..Effects::default() }), settings.pool_caps, rx, opts).await.map_err(|e| Stamped::from(e.to_string()))
}
