//! `forge <verb> <repo-dir> [args...]` — see DESIGN.md §3.1. Argv shape matches forge.sh's
//! own `case` dispatch exactly, so every caller (`SPIRA_FORGE`-named, bare, on PATH) needs
//! no change beyond conf.sh's default (`forge.sh` → `forge`). The verbs themselves live in the
//! library ([`forge::client`]), which in-process callers use directly.

use forge::real::{RealGh, RealProc};
use spira_otel::exit::ExitCode;
use std::io::Read;
use std::path::Path;

fn main() -> std::process::ExitCode {
    let first = std::env::args().nth(1);
    spira_otel::exit::finish(forge::telemetry::run(
        &forge::telemetry::sink(),
        first.as_deref(),
        || run_main().code(),
    ))
}

fn run_main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        eprintln!("forge: usage: forge <verb> <repo-dir> [args...]");
        return ExitCode::from(1);
    }
    let cmd = argv.remove(0);
    let repo = if !argv.is_empty() {
        argv.remove(0)
    } else {
        String::new()
    };
    let input = (cmd == "pr-create").then(stdin_bytes);
    let out = forge::client::dispatch(&RealGh, &RealProc, &cmd, Path::new(&repo), &argv, input.as_deref());
    for l in forge::diag::take() {
        eprintln!("{l}");
    }
    for l in &out.lines {
        println!("{l}");
    }
    ExitCode::from(out.code.clamp(0, 255) as u8)
}

fn stdin_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);
    buf
}
