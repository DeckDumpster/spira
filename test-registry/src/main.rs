use runtime::ports::SeededRng;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const COMPONENT: &str = "test-registry";
const OPERATIONS: &[&str] = &[
    "disable",
    "quarantine",
    "activate",
    "show",
    "list",
    "history",
    "deny-list",
    "clear-landed",
    "repoint",
    "backup",
    "mint",
    "stamp",
    "flips",
    "rates",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let operation = spira_otel::metrics::run::operation_label(
        spira_otel::metrics::run::verb(&args),
        OPERATIONS,
    );
    let endpoint = || spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::metrics::run::metered_exit(endpoint, COMPONENT, operation, || {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cfg = test_registry::cli::with_run_default(spira_config::process::cfg, || {
            spira_config::resolve::run_dir_for_process().map(|d| d.display().to_string())
        });
        let seed = spira_config::process::cfg("SPIRA_SEED")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            });
        let out = test_registry::cli::run(&args, now, &cfg, &|p| test_registry::open(p), &mut SeededRng::new(seed));
        if !out.stdout.is_empty() {
            println!("{}", out.stdout.trim_end_matches('\n'));
        }
        if !out.stderr.is_empty() {
            eprintln!("test-registry: {}", out.stderr);
        }
        ExitCode::from(out.code as u8)
    })
}
