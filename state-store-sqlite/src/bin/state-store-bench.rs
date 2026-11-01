//! state-store-bench — CAS-write latency quantiles on the SQLite backend at a fixed rate, under
//! optional synthetic IO load; or `conformance --path`, the conformance suite against a live
//! file under a scratch prefix it removes afterwards.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use spira_otel::metrics::{Point, Run};
use state_store_sqlite::bench::{self, Command};
use state_store_sqlite::{live, Options, SqliteBackend};

const COMPONENT: &str = "state-store-bench";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let cmd = match bench::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            Run::begin(endpoint.as_deref(), COMPONENT, "usage").end("error");
            eprintln!("state-store-bench: {e}");
            return ExitCode::from(2);
        }
    };
    match cmd {
        Command::Cas(a) => {
            let metered = Run::begin(endpoint.as_deref(), COMPONENT, "cas");
            match bench::run_cas(&a) {
                Ok(q) => {
                    let ms = |v: f64| v / 1_000.0;
                    metered.end_with(
                        "ok",
                        &[
                            Point::Gauge {
                                name: "spira_state_store_bench_cas_p50_seconds",
                                value: ms(q.p50_ms),
                            },
                            Point::Gauge {
                                name: "spira_state_store_bench_cas_p99_seconds",
                                value: ms(q.p99_ms),
                            },
                            Point::Gauge {
                                name: "spira_state_store_bench_cas_p999_seconds",
                                value: ms(q.p999_ms),
                            },
                            Point::Gauge {
                                name: "spira_state_store_bench_cas_max_seconds",
                                value: ms(q.max_ms),
                            },
                            Point::Count {
                                name: "spira_state_store_bench_cas_samples",
                                value: q.samples as u64,
                            },
                        ],
                    );
                    if a.json {
                        println!("{}", bench::report(&q, &a));
                    } else {
                        println!(
                            "samples={} p50={:.3}ms p99={:.3}ms p99.9={:.3}ms max={:.3}ms",
                            q.samples, q.p50_ms, q.p99_ms, q.p999_ms, q.max_ms
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    metered.end("error");
                    eprintln!("state-store-bench: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Conformance { path } => {
            let metered = Run::begin(endpoint.as_deref(), COMPONENT, "conformance");
            let code = conformance(&path);
            metered.end(if code == ExitCode::SUCCESS {
                "ok"
            } else {
                "error"
            });
            code
        }
    }
}

fn conformance(path: &std::path::Path) -> ExitCode {
    if !path.exists() {
        eprintln!(
            "state-store-bench: {} does not exist; conformance runs against an existing store",
            path.display()
        );
        return ExitCode::from(2);
    }
    let backend = match SqliteBackend::open(path, Options::default()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("state-store-bench: {e}");
            return ExitCode::from(1);
        }
    };
    let run_id = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        std::process::id()
    );
    match live::run(&backend, &run_id) {
        Ok(r) => {
            for (name, why) in &r.failed {
                println!("FAIL {name}: {why}");
            }
            println!(
                "conformance: {} passed, {} failed, {} skipped ({}); removed {} scratch key(s) under {}{run_id}/",
                r.passed.len(),
                r.failed.len(),
                live::LIVE_EXCLUDED.len(),
                live::LIVE_EXCLUDED.join(", "),
                r.removed,
                live::SCRATCH_PREFIX,
            );
            if r.failed.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("state-store-bench: {e}");
            ExitCode::from(1)
        }
    }
}
