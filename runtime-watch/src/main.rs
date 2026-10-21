//! runtime-watch pass --state FILE [--persona NAME] [--max-files N]
//!
//! Exit 0 swept, 1 a filing failed (retried next pass), 3 an upstream source or a pin could not
//! be read — never treated as in support.

use runtime_watch::{pass, real::Real};
use spira_config::repos::Registry;
use spira_otel::metrics::{MetricsSink, Point};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: runtime-watch pass --state FILE [--persona NAME] [--max-files N]";
pub const COMPONENT: &str = "runtime-watch";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) != Some("pass") {
        eprintln!("runtime-watch: unknown command\n{USAGE}");
        return ExitCode::from(2);
    }
    let (mut state, mut persona, mut max_files) = (None, "builder".to_string(), 10usize);
    let mut it = argv[1..].iter();
    while let Some(a) = it.next() {
        let Some(v) = it.next() else {
            eprintln!("runtime-watch: {a} needs a value\n{USAGE}");
            return ExitCode::from(2);
        };
        match a.as_str() {
            "--state" => state = Some(PathBuf::from(v)),
            "--persona" => persona = v.clone(),
            "--max-files" => match v.parse() {
                Ok(n) => max_files = n,
                Err(_) => {
                    eprintln!("runtime-watch: --max-files {v:?} is not a number");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("runtime-watch: unknown argument {a}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(state) = state else {
        eprintln!("runtime-watch: --state is required\n{USAGE}");
        return ExitCode::from(2);
    };
    let home = match spira_config::resolve::locate_home_for_process() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("runtime-watch: cannot locate the harness home: {e}");
            return ExitCode::from(2);
        }
    };
    let registry = Registry::from_env(std::env::vars().collect(), &home);
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::oneshot::begin(endpoint.as_deref(), COMPONENT, "pass");
    let o = pass(&Real::new(registry, state, persona), max_files);
    let sink = MetricsSink::for_component(endpoint.as_deref(), COMPONENT);
    sink.push(&[Point::Gauge { name: "spira_runtime_watch_cannot_check", value: o.cannot_check.len() as f64 }], &[]);
    sink.push(&[Point::Count { name: "spira_runtime_bumps_filed", value: o.filed.len() as u64 }], &[]);
    println!(
        "runtime-watch: {} filed, {} already open, {} supported, {} failed, {} cannot check",
        o.filed.len(),
        o.open.len(),
        o.current.len(),
        o.failed.len(),
        o.cannot_check.len()
    );
    for f in &o.failed {
        eprintln!("runtime-watch: failed — {f}");
    }
    for c in &o.cannot_check {
        println!("runtime-watch: cannot check — {c}");
    }
    let code = if !o.cannot_check.is_empty() { 3 } else { i32::from(!o.failed.is_empty()) };
    spira_otel::oneshot::end(code);
    ExitCode::from(code as u8)
}
