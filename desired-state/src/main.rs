//! `spira` — the desired-state document.
//!
//!   spira apply <document>              install a fragment as the host's desired state
//!   spira compose [<fragment>...]       merge fragments and materialise the composite
//!   spira schema                        the JSON Schema every kind's spec validates against
//!
//! Both `apply` and `compose` run the same composer; `apply` is `compose` for the case where
//! a fresh install has only one fragment to hand it — see the design's own account of why
//! that is not a special case, just the smallest federation. `compose` with no arguments
//! merges every `*.toml` fragment already sitting in `~/.config/spira/desired.d/`.
//!
//! The composite and its version history live under `$SPIRA_DESIRED_DIR`, defaulting to
//! `${XDG_CONFIG_HOME:-$HOME/.config}/spira/desired` — the same host config directory
//! `~/.config/spira/desired.d/` (the operator's own fragments) already lives under.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use spira_desired_state::compose::compose;
use spira_desired_state::producer::{parse_fragment, Fragment};
use spira_desired_state::resource::json_schema;
use spira_desired_state::store::{now_rfc3339, FsStore, MaterializeOutcome};

fn config_home() -> PathBuf {
    env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env::var("HOME").unwrap_or_default()).join(".config"))
}

fn desired_dir() -> PathBuf {
    env::var("SPIRA_DESIRED_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| config_home().join("spira").join("desired"))
}

/// The operator's own fragments — one producer among the federation, not a special case.
fn desired_fragments_dir() -> PathBuf {
    env::var("SPIRA_DESIRED_FRAGMENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| config_home().join("spira").join("desired.d"))
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
    let store = FsStore::new(desired_dir());
    match store.write_version(&composite, &contributors, &now_rfc3339()) {
        Ok(MaterializeOutcome::NewVersion { version }) => {
            println!("materialised version {version}");
            ExitCode::SUCCESS
        }
        Ok(MaterializeOutcome::Unchanged { version }) => {
            println!("unchanged at version {version}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_apply(document: Option<&String>) -> ExitCode {
    let Some(path) = document else {
        eprintln!("usage: spira apply <document>");
        return ExitCode::FAILURE;
    };
    run_compose(std::slice::from_ref(path))
}

/// With no paths given, composes every `*.toml` fragment in `$SPIRA_DESIRED_FRAGMENTS_DIR`
/// (default `~/.config/spira/desired.d/`) — the operator's own producer, and every other
/// producer that drops its fragment there.
fn cmd_compose(paths: &[String]) -> ExitCode {
    if !paths.is_empty() {
        return run_compose(paths);
    }
    let dir = desired_fragments_dir();
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
