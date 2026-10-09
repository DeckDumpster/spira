//! refusal-watch pass --state FILE [--window-secs N] [--baseline N] [--repeat-secs N] [--max-files N]
//!
//! Exit 0 swept, 1 a filing failed, 3 the refusals could not be read (never a green pass).

use refusal_watch::{parse, parse_state, pass, render_state, Config};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

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

fn read_refusals(window: u64) -> Result<String, String> {
    let out = spira_config::bounded::bounded(spira_config::lifecycle_row::lc_bin())
        .args(["ops-refusals", &window.to_string()])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "spira-lc ops-refusals exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn file_incident(title: &str, body: &str) -> Result<(), String> {
    let bin = std::env::var("REFUSAL_WATCH_FILER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "incident.sh".into());
    let mut child = Command::new(bin)
        .args(["file", title, "-"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(body.as_bytes())
        .map_err(|e| e.to_string())?;
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("incident file exited {st}"))
    }
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
    let read = read_refusals(cfg.window_secs).and_then(|j| parse(&j));
    let (now, classes) = match read {
        Ok(v) => v,
        Err(e) => {
            eprintln!("refusal-watch: cannot read the refused events: {e}");
            return ExitCode::from(3);
        }
    };
    let state = parse_state(&std::fs::read_to_string(&state_path).unwrap_or_default());
    let out = pass(&classes, now, &cfg, state, &mut |t, b| file_incident(t, b));
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
