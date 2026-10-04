use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut gate = None;
    let mut land = None;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let slot = match a.as_str() {
            "--gate-log" => &mut gate,
            "--land-times" => &mut land,
            "--out" => &mut out,
            _ => return usage(),
        };
        *slot = args.next().map(PathBuf::from);
    }
    let (Some(gate), Some(land), Some(out)) = (gate, land, out) else { return usage() };
    match sim::fit::fit_files(&gate, &land).and_then(|d| sim::fit::render(&d)) {
        Ok(text) => match std::fs::write(&out, text) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&format!("{}: {e}", out.display())),
        },
        Err(e) => fail(&e),
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("sim-fit: {msg}");
    ExitCode::from(1)
}

fn usage() -> ExitCode {
    eprintln!("usage: sim-fit --gate-log <gate.log> --land-times <land-times.tsv> --out <durations.toml>");
    ExitCode::from(2)
}
