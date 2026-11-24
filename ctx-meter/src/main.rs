//! `ctx-meter [env [transcript]]`: see DESIGN.md.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

fn now() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

fn run(args: &[String]) -> Result<(), (u8, String)> {
    let mode = args.first().map(String::as_str).unwrap_or("line");
    if mode != "line" && mode != "env" || (mode == "line" && args.len() > 1) {
        return Err((2, "usage: ctx-meter [env [<transcript>]]".into()));
    }
    spira_otel::oneshot::operation(mode);
    let cfg = ctx_meter::Config::resolve().map_err(|e| (1, e))?;
    if mode == "env" {
        for l in ctx_meter::env(&cfg, now(), args.get(1).map(Path::new)) {
            println!("{l}");
        }
        return Ok(());
    }
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let cwd = std::env::current_dir().unwrap_or_default();
    println!("{}", ctx_meter::line(&cfg, now(), &stdin, &cwd));
    Ok(())
}

fn main() -> ExitCode {
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "ctx-meter", "line");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err((c, msg)) => {
            eprintln!("ctx-meter: {msg}");
            c
        }
    };
    spira_otel::oneshot::end(code as i32);
    ExitCode::from(code)
}
