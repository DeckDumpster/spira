//! `ctx-meter line --run <dir> --metrics <endpoint>` and `ctx-meter env [--run <dir>] [<transcript>]`:
//! see DESIGN.md.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: ctx-meter line --run <dir> --metrics <endpoint> | ctx-meter env [--run <dir>] [<transcript>]";

fn now() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

#[derive(Default)]
struct Args {
    mode: String,
    run: Option<PathBuf>,
    metrics: Option<String>,
    transcript: Option<String>,
}

fn parse(args: &[String]) -> Result<Args, (u8, String)> {
    let usage = || (2, USAGE.to_string());
    let mut a = Args { mode: args.first().cloned().ok_or_else(usage)?, ..Default::default() };
    let mut it = args[1..].iter();
    while let Some(w) = it.next() {
        match w.as_str() {
            "--run" => a.run = Some(it.next().ok_or_else(usage)?.into()),
            "--metrics" => a.metrics = Some(it.next().ok_or_else(usage)?.clone()),
            t if a.mode == "env" && !t.starts_with("--") && a.transcript.is_none() => a.transcript = Some(t.to_string()),
            _ => return Err(usage()),
        }
    }
    match a.mode.as_str() {
        "line" if a.run.is_some() && a.metrics.is_some() && a.transcript.is_none() => Ok(a),
        "env" if a.metrics.is_none() => Ok(a),
        _ => Err(usage()),
    }
}

fn run(a: &Args) -> Result<(), (u8, String)> {
    if a.mode == "env" {
        let mut cfg = ctx_meter::Config::resolve().map_err(|e| (1, e))?;
        cfg.status = a.run.clone().or_else(|| std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(|d| spira_config::ctx_meter_dir(Path::new(&d)))).unwrap_or_default();
        for l in ctx_meter::env(&cfg, now(), a.transcript.as_deref().map(Path::new)) {
            println!("{l}");
        }
        return Ok(());
    }
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let cwd = std::env::current_dir().unwrap_or_default();
    println!("{}", ctx_meter::line(a.run.as_deref().unwrap_or(Path::new("")), now(), &stdin, &cwd));
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = parse(&args);
    let endpoint = match &parsed {
        Ok(a) if a.mode == "line" => a.metrics.clone().filter(|m| !m.is_empty()),
        _ => spira_config::process::metrics_endpoint(),
    };
    spira_otel::oneshot::begin(endpoint.as_deref(), "ctx-meter", "line");
    let code = match parsed.and_then(|a| {
        spira_otel::oneshot::operation(&a.mode);
        run(&a)
    }) {
        Ok(()) => 0,
        Err((c, msg)) => {
            eprintln!("ctx-meter: {msg}");
            c
        }
    };
    spira_otel::oneshot::end(code as i32);
    ExitCode::from(code)
}
