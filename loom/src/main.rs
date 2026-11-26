//! The Loom server.

use loom::{router, Config, Loom};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    match loom::server_runtime() {
        Ok(rt) => rt.block_on(serve()),
        Err(e) => {
            eprintln!("loom: tokio runtime: {e}");
            ExitCode::from(1)
        }
    }
}

async fn serve() -> ExitCode {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };

    // `--print-config` resolves the configuration and stops: a preflight that wants to say
    // what this box will actually do, without starting the server.
    if std::env::args().any(|a| a == "--print-config") {
        print!("{}", cfg.render_pinned());
        return ExitCode::SUCCESS;
    }

    let listener = match tokio::net::TcpListener::bind(&cfg.addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("loom: cannot listen on {}: {e}", cfg.addr);
            return ExitCode::from(2);
        }
    };
    // The bound address, not the configured one: a configured port of 0 is resolved here, and
    // a log line naming what was ASKED FOR cannot be used to reach the thing that is running.
    match listener.local_addr() {
        Ok(a) => eprintln!(
            "loom: serving http://{a}/ (Now), /rounds, /stuck, /units, /lifecycle and /api/ops — budget {} ms",
            cfg.budget.as_millis()
        ),
        Err(e) => eprintln!("loom: listening, but cannot name the address: {e}"),
    }

    let pinned0 = cfg.render_pinned();
    let tick = std::time::Duration::from_secs(std::env::var("SPIRA_LOOM_TICK").ok().and_then(|t| t.parse::<u64>().ok()).unwrap_or(5).max(1));
    let conf_file = loom::confwatch::conf_file();
    let app = loom::telemetry::metered(router(Arc::new(Loom::new(cfg))), spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok());
    let restart = async move {
        match conf_file {
            Some(f) => {
                let why = loom::confwatch::changed(f, tick, pinned0).await;
                eprintln!("loom: {why} — exiting for restart");
            }
            None => std::future::pending().await,
        }
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(restart).await {
        eprintln!("loom: stopped serving: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
