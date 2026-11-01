//! `spira` — the desired-state document.
//!
//!   spira apply <document>              install a fragment as the host's desired state
//!   spira compose [<fragment>...]       merge fragments and materialise the composite
//!   spira schema                        the JSON Schema every kind's spec validates against
//!
//! Both `apply` and `compose` run the same composer; `apply` is `compose` for the case where
//! a fresh install has only one fragment to hand it — see the design's own account of why
//! that is not a special case, just the smallest federation. `compose` with no arguments
//! merges every `*.toml` fragment already sitting in `SPIRA_DESIRED_FRAGMENTS_DIR`.
//!
//! The composite and its version history live under `SPIRA_DESIRED_DIR`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use spira_desired_state::compose::{compose, publish_specs};
use spira_desired_state::producer::{parse_fragment, Fragment};
use spira_desired_state::resource::json_schema;
use spira_desired_state::store::{now_rfc3339, FsStore, MaterializeOutcome};

fn desired_dir() -> Result<PathBuf, String> {
    spira_desired_state::store::default_dir()
}

/// The operator's own fragments — one producer among the federation, not a special case.
fn desired_fragments_dir() -> Result<PathBuf, String> {
    spira_config::process::cfg("SPIRA_DESIRED_FRAGMENTS_DIR").map(PathBuf::from)
}

/// `*.toml` files directly inside `dir`, sorted so a compose over the same directory always
/// merges its fragments in the same order.
fn fragment_files_in(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|ext| ext == "toml").unwrap_or(false))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    paths
}

fn read_fragment(path: &str) -> Result<Fragment, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    parse_fragment(&text).map_err(|e| format!("{path}: {e}"))
}

fn run_compose(paths: &[String]) -> ExitCode {
    let mut fragments = Vec::with_capacity(paths.len());
    for path in paths {
        match read_fragment(path) {
            Ok(f) => fragments.push(f),
            Err(e) => {
                eprintln!("spira: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let composite = match compose(&fragments) {
        Ok(c) => c,
        Err(errors) => {
            for e in &errors {
                eprintln!("spira: refused: {e}");
            }
            return ExitCode::FAILURE;
        }
    };
    let contributors: Vec<_> = fragments.iter().map(|f| f.producer.clone()).collect();
    let dir = match desired_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira: {e}");
            return ExitCode::FAILURE;
        }
    };
    let store = FsStore::new(dir);
    let materialised = match store.write_version(&composite, &contributors, &now_rfc3339()) {
        Ok(MaterializeOutcome::NewVersion { version }) => format!("materialised version {version}"),
        Ok(MaterializeOutcome::Unchanged { version }) => format!("unchanged at version {version}"),
        Err(e) => {
            eprintln!("spira: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{materialised}");
    match publish_specs(&composite, &mut apply_spec) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(n) => {
            println!("published {n} section spec(s)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira: publishing section specs: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One spec through `spira-lc section apply-spec`, its body on stdin, bounded by the lifecycle
/// call deadline.
fn apply_spec(body: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let deadline = spira_config::lc_call::LC_TIMEOUT;
    let mut cmd = Command::new(spira_config::lc_call::lc_bin());
    cmd.args(["section", "apply-spec", "--actor", "compose"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("running spira-lc: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.to_string().as_bytes())
            .map_err(|e| format!("spira-lc stdin: {e}"))?;
    }
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(st) => break st,
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("spira-lc section apply-spec passed its {}s deadline", deadline.as_secs()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "spira-lc section apply-spec exited {}: {}{}",
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn cmd_apply(document: Option<&String>) -> ExitCode {
    let Some(path) = document else {
        eprintln!("usage: spira apply <document>");
        return ExitCode::FAILURE;
    };
    run_compose(std::slice::from_ref(path))
}

/// With no paths given, composes every `*.toml` fragment in `SPIRA_DESIRED_FRAGMENTS_DIR` —
/// the operator's own producer, and every other producer that drops its fragment there.
fn cmd_compose(paths: &[String]) -> ExitCode {
    if !paths.is_empty() {
        return run_compose(paths);
    }
    let dir = match desired_fragments_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira: {e}");
            return ExitCode::FAILURE;
        }
    };
    let found = fragment_files_in(&dir);
    if found.is_empty() {
        eprintln!(
            "usage: spira compose <fragment>...\n       (no fragments found under {})",
            dir.display()
        );
        return ExitCode::FAILURE;
    }
    run_compose(&found)
}

fn cmd_schema() -> ExitCode {
    match serde_json::to_string_pretty(&json_schema()) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    spira_desired_state::telemetry::metered_exit(&args, || run_main(&args))
}

fn run_main(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("apply") => cmd_apply(args.get(1)),
        Some("compose") => cmd_compose(&args[1..]),
        Some("schema") => cmd_schema(),
        _ => {
            eprintln!(
                "usage: spira <apply|compose|schema> ...\n\
                 \n\
                 \x20 apply <document>          install a fragment as the host's desired state\n\
                 \x20 compose <fragment>...     merge N fragments and materialise the composite\n\
                 \x20 schema                    the JSON Schema every kind's spec validates against"
            );
            ExitCode::FAILURE
        }
    }
}
