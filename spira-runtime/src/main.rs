//! `spira-runtime serve` — runs until stopped. Config is read once here and pinned.

use std::sync::Arc;

use runtime::host::{serve_with, ServeOpts};
use runtime::ports::WallClock;
use runtime::socket::SocketJournal;
use runtime::FileMarker;
use spira_runtime::notify::Notifier;
use spira_runtime::{open_engine, sink::OtlpSink, Effects, Settings};
use std::time::Duration;
use tokio::sync::mpsc;

const USAGE: &str = "usage: spira-runtime serve|check-config";

/// The OTLP log queue: a full queue drops and counts rather than blocking the loop.
const LOG_QUEUE: usize = 1024;
const NO_WATCHDOG_BOOT: Duration = Duration::from_secs(20);

fn check() -> Result<(), String> {
    spira_config::process::check_config(Settings::KEYS)?;
    Ok(())
}

fn main() {
    let verb = std::env::args().nth(1);
    if std::env::args().count() != 2 || !matches!(verb.as_deref(), Some("serve" | "check-config")) {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let settings = match check().and_then(|()| Settings::load()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spira-runtime: {e}");
            std::process::exit(1);
        }
    };
    if verb.as_deref() == Some("check-config") {
        println!("ok {}", settings.hash());
        return;
    }
    let notifier = match spira_runtime::notify::Notifier::from_env() {
        Ok(n) => Arc::new(n),
        Err(e) => {
            eprintln!("spira-runtime: notify socket: {e}");
            std::process::exit(1);
        }
    };
    let result = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("tokio runtime: {e}")).and_then(|rt| rt.block_on(serve(settings, notifier)));
    if let Err(e) = result {
        eprintln!("spira-runtime: {e}");
        std::process::exit(1);
    }
}

async fn serve(settings: Settings, notifier: Arc<Notifier>) -> Result<(), String> {
    let kick: Arc<dyn Fn() + Send + Sync> = {
        let n = notifier.clone();
        Arc::new(move || n.kick())
    };
    let max_sleep = notifier.max_sleep();
    let metrics = spira_otel::metrics::MetricsSink::for_component(settings.metrics_endpoint.as_deref(), "spira-runtime");
    let sink = Arc::new(OtlpSink::with_metrics(spira_otel::logs::LogSink::new(settings.otel_dir(), "spira-runtime"), LOG_QUEUE, metrics));
    let journal = Arc::new(SocketJournal::new(settings.lc_socket.clone()));
    let clock = Arc::new(WallClock);
    let boot = {
        let (settings, clock, sink) = (settings.clone(), clock.clone(), sink.clone());
        move || -> Result<runtime::Engine, String> {
            let mut engine = open_engine(&settings, journal, clock, Arc::new(FileMarker(settings.marker_path())), sink, vec![]).map_err(|e| e.to_string())?;
            for (id, stream, kind) in [runtime::heartbeat::Heartbeat::boot(), spira_runtime::watchers::round_duty::RoundDuty::boot(), spira_runtime::watchers::inbox_keeper::InboxKeeper::boot()] {
                runtime::Executor::submit(&mut engine, id, stream, kind).map_err(|e| e.to_string())?;
            }
            Ok(engine)
        }
    };
    let deadline = max_sleep.map_or(NO_WATCHDOG_BOOT, |m| m * 2);
    let mut starting = runtime::offload::offload("startup", None, boot).map_err(|e| e.to_string())?;
    let engine = tokio::time::timeout(deadline, &mut starting).await.map_err(|_| format!("startup did not finish within {deadline:?}"))?.map_err(|e| e.to_string())??;
    let (_keep_open, rx) = mpsc::channel(1);
    notifier.ready();
    let opts = ServeOpts { kick: Some(kick), max_sleep_ms: max_sleep.map(|d| d.as_millis() as u64), sink: Some(sink), pool: None };
    serve_with(engine, clock, Arc::new(Effects { heartbeat_url: settings.heartbeat_url.clone(), world: settings.world.clone() }), 4, rx, opts).await.map_err(|e| e.to_string())
}
