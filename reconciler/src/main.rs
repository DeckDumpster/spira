// reconciler --pass: one structural pass, run from a shell. The pass itself is `reconciler::run_pass`;
// spira-runtime hosts the same pass as a handler, and this binary stays as its parity oracle.
// reconciler goodput-pass: one pass of the goodput loop (see goodput_pass.rs).

mod goodput_pass;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-reconciler");
    let result = match args.get(1).map(String::as_str) {
        Some("--pass") => spira_otel::metrics::instrument_result(&sink, "reconciler", "pass", reconciler::run_pass),
        Some(goodput_pass::VERB) => spira_otel::metrics::instrument_result(&sink, "reconciler", goodput_pass::VERB, || goodput_pass::main(&sink)),
        _ => {
            eprintln!("usage: reconciler --pass | reconciler {}", goodput_pass::VERB);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reconciler: {}", e);
            ExitCode::FAILURE
        }
    }
}
