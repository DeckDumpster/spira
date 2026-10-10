//! The Loom server.

use loom::{router, Config, Loom};
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
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
        println!("SPIRA_LOOM_ADDR={}", cfg.addr);
        println!("SPIRA_LOOM_BUDGET_MS={}", cfg.budget.as_millis());
        println!("SPIRA_LOOM_CACHE_S={}", cfg.cache.as_secs());
        println!("SPIRA_LC_BIN={}", cfg.lc);
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
            "loom: serving http://{a}/stuck, /lifecycle and /api/ops — budget {} ms",
            cfg.budget.as_millis()
        ),
        Err(e) => eprintln!("loom: listening, but cannot name the address: {e}"),
    }

    let app = router(Arc::new(Loom::new(cfg)));
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("loom: stopped serving: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
