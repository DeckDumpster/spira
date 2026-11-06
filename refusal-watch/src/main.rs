//! refusal-watch pass --state FILE [--window-secs N] [--baseline N] [--repeat-secs N] [--max-files N]
//!
//! Exit 0 swept, 1 a filing failed, 3 the refusals could not be read (never a green pass).

use refusal_watch::{parse, parse_state, pass, read_refusals, render_state, Config};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: refusal-watch pass --state FILE [--window-secs N] [--baseline N] [--repeat-secs N] [--max-files N]";

fn args(argv: &[String]) -> Result<(Config, PathBuf), String> {
    let mut cfg = Config {
        window_secs: 86400,
        baseline: 5,
        repeat_secs: 86400,
        max_files: 10,
    };
    let mut state = None;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        let n = || {
            v.parse::<u64>()
                .map_err(|_| format!("{a} {v:?} is not a number"))
        };
        match a.as_str() {
            "--state" => state = Some(PathBuf::from(v)),
            "--window-secs" => cfg.window_secs = n()?,
            "--baseline" => cfg.baseline = n()?,
            "--repeat-secs" => cfg.repeat_secs = n()?,
            "--max-files" => cfg.max_files = n()? as usize,
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    if cfg.window_secs == 0 {
        return Err("--window-secs must be positive".into());
    }
    Ok((cfg, state.ok_or("--state is required")?))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) != Some("pass") {
        eprintln!("refusal-watch: unknown command\n{USAGE}");
        return ExitCode::from(2);
    }
    let (cfg, state_path) = match args(&argv[1..]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("refusal-watch: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let read = spira_config::lc_call::LcClient::configured().and_then(|c| read_refusals(&c, cfg.window_secs)).and_then(|j| parse(&j));
    let (now, classes) = match read {
        Ok(v) => v,
        Err(e) => {
            eprintln!("refusal-watch: cannot read the refused events: {e}");
            return ExitCode::from(3);
        }
    };
    let state = parse_state(&std::fs::read_to_string(&state_path).unwrap_or_default());
    // The incident is filed in-process, exactly as `incident file <title> -` files it.
    let out = pass(&classes, now, &cfg, state, &mut |t, b| refusal_watch::file(t, b));
    if let Err(e) = std::fs::write(&state_path, render_state(&out.state)) {
        eprintln!("refusal-watch: cannot record {}: {e}", state_path.display());
        return ExitCode::from(3);
    }
    println!(
        "refusal-watch: {} classes, {} filed, {} failed, {} deferred",
        classes.len(),
        out.filed.len(),
        out.failed.len(),
        out.deferred
    );
    for t in &out.filed {
        println!("refusal-watch: filed {t}");
    }
    for f in &out.failed {
        eprintln!("refusal-watch: filing failed — {f}");
    }
    if out.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
