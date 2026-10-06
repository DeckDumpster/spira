//! The Loom server. One route today; the static page joins it as a second.
//!
//! IT REFUSES TO START WITHOUT A DATABASE. `bd` given a path it cannot use falls back to
//! discovering one from its working directory, so a server that guessed would not fail — it
//! would come up healthy and serve a different harness's graph, which is the failure nobody
//! looks for because everything appears to work.

use loom::{router, Config, Loom};
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    let cfg = Config::from_env();

    // `--print-config` resolves the configuration and stops: a preflight that wants to say
    // what this box will actually do, without starting the server.
    if std::env::args().any(|a| a == "--print-config") {
        println!("SPIRA_LOOM_ADDR={}", cfg.addr);
        println!("SPIRA_LOOM_BUDGET_MS={}", cfg.budget.as_millis());
        println!("SPIRA_LOOM_CACHE_S={}", cfg.cache.as_secs());
        println!("SPIRA_DB={}", cfg.db);
        return ExitCode::SUCCESS;
    }

    if cfg.db.trim().is_empty() {
        eprintln!(
            "loom: SPIRA_DB is empty — refusing to guess a beads database.\n\
             loom: start this through the harness so its configuration is in force."
        );
        return ExitCode::from(2);
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
            "loom: serving http://{a}/ and /api/beads and /api/ops — budget {} ms, {} s cache",
            cfg.budget.as_millis(),
            cfg.cache.as_secs()
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
