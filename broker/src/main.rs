mod execute;
mod policy;
mod read;
mod repo_map;
mod submit;
mod token;

use std::process::ExitCode;

/// `SPIRA_*` values a bash process this binary spawns must never see pre-set — the same
/// per-copy-fact / host-policy keys `cockpit-collect`'s `bootstrap_config` names
/// (wave4-decomposition.md row (b)).
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// Wave 4.8 ("retire conf re-import seams in Rust"): every `std::env::var(...)` read across
/// this crate's submodules (execute.rs, read.rs, submit.rs, repo_map.rs, token.rs) used to
/// see only this process's own already-set environment — no spira.toml load at all
/// (wave4-decomposition.md row (b) names broker by file: SPIRA_GH_APP_*). Merges
/// `spira_config::resolve()`'s in-process answer into THIS process's own environment once,
/// at the top of `main`, before any subcommand dispatch — inserting a key only when it is
/// not already set and never one of [`NEVER_EXPORTED`]. `SPIRA_GH_APP_ID`/`_INSTALLATION_ID`/
/// `_KEY` are credentials — the registry's own default for each is empty, same as the
/// `~/.config/spira/github-app.env` fallback `token.rs` already carries, so this changes
/// nothing for a secret that only ever reaches the box's own environment or that config
/// file, and only matters for an operator who puts a non-secret override (a different
/// config file path) in spira.toml. Best-effort: a missing registry or a containment
/// refusal leaves the environment exactly as it was.
fn merge_resolved_env() {
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let home = std::path::PathBuf::from(std::env::var("SPIRA_HOME").unwrap_or_default());
    let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
    if let Ok(resolved) = spira_config::resolve::resolve_for_process(&home, &repo, &env_map) {
        for (k, v) in resolved.values {
            if NEVER_EXPORTED.contains(&k.as_str()) {
                continue;
            }
            if std::env::var_os(&k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}

fn main() -> ExitCode {
    merge_resolved_env();
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("");

    match sub {
        "submit"  => run_submit(&args[2..]),
        "execute" => run_execute(),
        "read"    => run_read(&args[2..]),
        "token"   => run_token(),
        _ => {
            eprintln!("broker: usage: broker submit <verb> <target> --reason <text> --bead <id> [--class <class>]");
            eprintln!("broker:        broker execute");
            eprintln!("broker:        broker read <verb> <repo>/<id> [--artifact <name>] [--output-dir <path>]");
            eprintln!("broker:        broker token");
            ExitCode::from(2)
        }
    }
}

fn run_submit(args: &[String]) -> ExitCode {
    // submit <verb> <target> --reason <text> --bead <id> [--class <class>]
    // target format: <repo>/<number>
    if args.len() < 2 {
        eprintln!("broker submit: usage: broker submit <verb> <target> --reason <text> --bead <id>");
        return ExitCode::from(2);
    }

    let verb_str = &args[0];
    let target   = &args[1];

    let verb = match policy::Verb::parse(verb_str) {
        Some(v) => v,
        None => {
            eprintln!("broker submit: unknown verb '{}'; allowed: run-rerun run-cancel pr-close pr-comment issue-comment issue-close", verb_str);
            return ExitCode::from(2);
        }
    };

    let (repo, number) = match target.rfind('/') {
        Some(i) => (target[..i].to_string(), target[i+1..].to_string()),
        None => {
            eprintln!("broker submit: target must be <repo>/<number>, got '{}'", target);
            return ExitCode::from(2);
        }
    };
    if number.is_empty() || repo.is_empty() {
        eprintln!("broker submit: target must be <repo>/<number>, got '{}'", target);
        return ExitCode::from(2);
    }

    let mut reason = String::new();
    let mut bead   = String::new();
    let mut class: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => { i += 1; if i < args.len() { reason = args[i].clone(); } }
            "--bead"   => { i += 1; if i < args.len() { bead   = args[i].clone(); } }
            "--class"  => { i += 1; if i < args.len() { class  = Some(args[i].clone()); } }
            f => { eprintln!("broker submit: unknown flag '{}'", f); return ExitCode::from(2); }
        }
        i += 1;
    }

    if reason.is_empty() {
        eprintln!("broker submit: --reason is required");
        return ExitCode::from(2);
    }
    if bead.is_empty() {
        eprintln!("broker submit: --bead is required");
        return ExitCode::from(2);
    }

    match submit::run(submit::Args { verb, repo, number, reason, bead, class }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_execute() -> ExitCode {
    match execute::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_read(args: &[String]) -> ExitCode {
    match read::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_token() -> ExitCode {
    match token::mint() {
        Ok(t)  => { println!("{}", t); ExitCode::SUCCESS }
        Err(e) => { eprintln!("broker token: {}", e); ExitCode::FAILURE }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ENV VARS ARE PROCESS-GLOBAL: the one test below that resolves config takes this
    // lock for its whole body.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Wave 4.8: merge_resolved_env() must reach a registry key this crate never hardcoded
    // a default for, and must never leak a NEVER_EXPORTED key into this process's own
    // environment.
    #[test]
    fn merge_resolved_env_reaches_a_registry_default_and_never_exports_the_forbidden_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("SPIRA_HOME");
        let saved_bd = std::env::var_os("SPIRA_BD");
        let saved_max_aeons = std::env::var_os("SPIRA_MAX_AEONS");
        std::env::remove_var("SPIRA_BD");
        std::env::remove_var("SPIRA_MAX_AEONS");
        let dir = testkit::TempDir::new("broker-merge-env");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_BD"),
            "TYPE=string\nGROUP=bd\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_BD:=bd}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::env::set_var("SPIRA_HOME", &home);

        merge_resolved_env();

        let got_bd = std::env::var("SPIRA_BD").ok();
        let got_max_aeons = std::env::var_os("SPIRA_MAX_AEONS");

        match saved_home {
            Some(v) => std::env::set_var("SPIRA_HOME", v),
            None => std::env::remove_var("SPIRA_HOME"),
        }
        match saved_bd {
            Some(v) => std::env::set_var("SPIRA_BD", v),
            None => std::env::remove_var("SPIRA_BD"),
        }
        match saved_max_aeons {
            Some(v) => std::env::set_var("SPIRA_MAX_AEONS", v),
            None => std::env::remove_var("SPIRA_MAX_AEONS"),
        }

        assert_eq!(got_bd, Some("bd".to_string()), "a registry default must reach the real environment");
        assert_eq!(got_max_aeons, None, "SPIRA_MAX_AEONS must never leak into this process's own environment");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
