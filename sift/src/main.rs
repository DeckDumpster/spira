//! `sift status [--json] [--run DIR] [<bead>]` — the screen's recorded state per bead, read from
//! its events and from nothing else.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{json, Value};
use sift::machine::Sifted;
use sift::FileStore;

const USAGE: &str = "usage: sift status [--json] [--run DIR] [<bead>]   (DIR defaults to $SPIRA_RUN)";

fn read(store: &FileStore, only: Option<&str>) -> Vec<Sifted> {
    match only {
        Some(id) => store.load(id).into_iter().collect(),
        None => store.all(),
    }
}

fn line(s: &Sifted) -> String {
    let v = s.status_json();
    format!("{} {} tip={}", s.id, v["state"].as_str().unwrap_or(""), v["tip"].as_str().unwrap_or(""))
}

fn run() -> Result<String, (u8, String)> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("status") {
        return Err((2, USAGE.into()));
    }
    args.remove(0);
    let (mut json_out, mut run_dir, mut bead) = (false, std::env::var("SPIRA_RUN").ok().map(PathBuf::from), None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json_out = true,
            "--run" => run_dir = Some(PathBuf::from(it.next().ok_or((2, USAGE.to_string()))?)),
            s if s.starts_with('-') || bead.is_some() => return Err((2, USAGE.into())),
            _ => bead = Some(a),
        }
    }
    let run_dir = run_dir.ok_or((2, format!("no run directory: pass --run or set SPIRA_RUN\n{USAGE}")))?;
    let found = read(&FileStore::new(run_dir.join("sift")), bead.as_deref());
    if let (Some(id), true) = (&bead, found.is_empty()) {
        return Err((1, format!("sift: {id} has no recorded events")));
    }
    if json_out {
        let beads: Vec<Value> = found.iter().map(Sifted::status_json).collect();
        return Ok(json!({"beads": beads}).to_string());
    }
    Ok(found.iter().map(line).collect::<Vec<_>>().join("\n"))
}

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err((code, msg)) => {
            eprintln!("{msg}");
            ExitCode::from(code)
        }
    }
}
