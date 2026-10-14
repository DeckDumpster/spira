use spira_submission::{health, serve, Config, SendmailSink};
use std::sync::Arc;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let cfg = match Config::from_spira() {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("spira-submission: SPIRA_SUBMISSION_ADDR is empty — not configured, not listening");
            return;
        }
        Err(e) => {
            eprintln!("spira-submission: {e}");
            std::process::exit(2);
        }
    };
    match cmd.as_str() {
        "health" => match health(cfg.socket()) {
            Ok(b) => println!("ok: {b}"),
            Err(e) => {
                eprintln!("spira-submission: {e}");
                std::process::exit(1);
            }
        },
        "" | "serve" => {
            if let Err(e) = spira_submission::server_config(&cfg.cert, &cfg.key) {
                eprintln!("spira-submission: {e}");
                std::process::exit(2);
            }
            let listener = match std::net::TcpListener::bind(cfg.socket()) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("spira-submission: bind {}: {e}", cfg.socket());
                    std::process::exit(2);
                }
            };
            eprintln!("spira-submission: listening on {} (implicit TLS)", cfg.socket());
            let sink = Arc::new(SendmailSink { argv: cfg.sendmail.clone() });
            serve(listener, cfg, sink);
        }
        other => {
            eprintln!("spira-submission: unknown subcommand {other:?} (serve | health)");
            std::process::exit(2);
        }
    }
}
