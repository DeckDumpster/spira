//! `spira-runtime serve` — runs until stopped. Config is read once here and pinned.

use std::sync::Arc;

use runtime::host::{serve_with, ServeOpts};
use runtime::ports::WallClock;
use runtime::socket::SocketJournal;
use runtime::FileMarker;
use spira_runtime::{open_engine, sink::OtlpSink, Effects, Settings};
use tokio::sync::mpsc;

const USAGE: &str = "usage: spira-runtime serve|check-config";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let verb = std::env::args().nth(1);
    if std::env::args().count() != 2 || !matches!(verb.as_deref(), Some("serve" | "check-config")) {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    if verb.as_deref() == Some("check-config") {
        match Settings::load() {
            Ok(s) => println!("ok {}", s.hash()),
            Err(e) => {
                eprintln!("spira-runtime: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(e) = serve().await {
        eprintln!("spira-runtime: {e}");
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), String> {
    let settings = Settings::load()?;
    let notifier = Arc::new(spira_runtime::notify::Notifier::from_env().map_err(|e| format!("notify socket: {e}"))?);
    let kick: Arc<dyn Fn() + Send + Sync> = {
        let n = notifier.clone();
        Arc::new(move || n.kick())
    };
    let sink = Arc::new(OtlpSink(spira_otel::logs::LogSink::new(settings.otel_dir(), "spira-runtime")));
    let journal = Arc::new(SocketJournal::new(settings.lc_socket.clone()));
    let clock = Arc::new(WallClock);
    let mut engine = open_engine(&settings, journal, clock.clone(), Arc::new(FileMarker(settings.marker_path())), sink, kick.clone(), vec![]).map_err(|e| e.to_string())?;
    let (id, stream, kind) = runtime::heartbeat::Heartbeat::boot();
    runtime::Executor::submit(&mut engine, id, stream, kind).map_err(|e| e.to_string())?;
    let (id, stream, kind) = spira_runtime::watchers::round_duty::RoundDuty::boot();
    runtime::Executor::submit(&mut engine, id, stream, kind).map_err(|e| e.to_string())?;
    let (_keep_open, rx) = mpsc::channel(1);
    notifier.ready();
    let opts = ServeOpts { kick: Some(kick), max_sleep_ms: notifier.max_sleep().map(|d| d.as_millis() as u64) };
    serve_with(engine, clock, Arc::new(Effects { heartbeat_url: settings.heartbeat_url.clone(), world: settings.world.clone() }), 4, rx, opts).await.map_err(|e| e.to_string())
}
