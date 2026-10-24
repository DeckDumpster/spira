mod login;
mod policy;
mod read;
mod repo_map;
mod telemetry;
mod token;

use spira_otel::exit::ExitCode;

/// `SPIRA_*` values a bash process this binary spawns must never see pre-set — the same
/// per-copy-fact / host-policy keys `cockpit-collect`'s `bootstrap_config` names
/// (wave4-decomposition.md row (b)).
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// Wave 4.8 ("retire conf re-import seams in Rust"): every `std::env::var(...)` read across
/// this crate's submodules (read.rs, repo_map.rs, token.rs) used to
/// see only this process's own already-set environment — no config document load at all
/// (wave4-decomposition.md row (b) names broker by file: SPIRA_GH_APP_*). Merges
/// `spira_config::resolve()`'s in-process answer into THIS process's own environment once,
/// at the top of `main`, before any subcommand dispatch — inserting a key only when it is
/// not already set and never one of [`NEVER_EXPORTED`]. `SPIRA_GH_APP_ID`/`_INSTALLATION_ID`/
/// `_KEY` are credentials — the registry's own default for each is empty, same as the
/// `~/.config/spira/github-app.env` fallback `token.rs` already carries, so this changes
/// nothing for a secret that only ever reaches the box's own environment or that config
/// file, and only matters for an operator who puts a non-secret override (a different
/// config file path) in the resolved config document. Best-effort: a missing registry or a containment
/// refusal leaves the environment exactly as it was.
fn merge_resolved_env() -> Result<(), String> {
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let home = std::path::PathBuf::from(std::env::var("SPIRA_HOME").unwrap_or_default());
    let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
    if home.as_os_str().is_empty() {
        eprintln!("broker: SPIRA_HOME is unset; running on the process environment alone");
        return Ok(());
    }
    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env_map)?;
    {
        for (k, v) in resolved.values {
            if NEVER_EXPORTED.contains(&k.as_str()) {
                continue;
            }
            if std::env::var_os(&k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let first = std::env::args().nth(1);
    spira_otel::exit::finish(telemetry::run(&telemetry::sink(), first.as_deref(), || dispatch().code()))
}

fn dispatch() -> ExitCode {
    if let Err(e) = merge_resolved_env() {
        eprintln!("broker: refusing to run on unresolved config: {e}");
        return ExitCode::from(1);
    }
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("");

    match sub {
        "read"    => run_read(&args[2..]),
        "token"   => run_token(),
        "identity-login-code" => run_login_code(),
        _ => {
            eprintln!("broker: usage: broker read <verb> <repo>/<id> [--artifact <name>] [--output-dir <path>]");
            eprintln!("broker:        broker token");
            eprintln!("broker:        broker identity-login-code");
            ExitCode::from(2)
        }
    }
}

fn run_read(args: &[String]) -> ExitCode {
    match read::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_login_code() -> ExitCode {
    match login::run() {
        Ok(code) => { println!("{}", code); ExitCode::SUCCESS }
        Err(e)   => { eprintln!("{}", e); ExitCode::FAILURE }
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

    // One-source-of-config (per Ryan 2026-10-05): there is no more "registry default"
    // to assert — a key's value comes only from the config file. This proves a value this
    // process's own config file DECLARES reaches the real environment via
    // merge_resolved_env(), and that a NEVER_EXPORTED key still never does.
    #[test]
    fn merge_resolved_env_reaches_a_declared_value_and_never_exports_the_forbidden_set() {

        let dir = testkit::TempDir::new("broker-merge-env");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_BD", "bd-fixture")]);
        // The real, checked-in spira/conf.d (this crate's own repo layout: `broker/`
        // sits beside `spira/`) — resolve_for_process needs a real registry to
        // validate every key it resolves.
        let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
        let _g = testkit::env(&[
            ("SPIRA_BD", None),
            ("SPIRA_MAX_AEONS", None),
            ("SPIRA_TOML", toml.to_str()),
            ("SPIRA_HOME", home.to_str()),
        ]);

        let result = merge_resolved_env();

        let got_bd = std::env::var("SPIRA_BD").ok();
        let got_max_aeons = std::env::var_os("SPIRA_MAX_AEONS");

        result.expect("merge_resolved_env resolves given a complete config file and a real registry");
        assert_eq!(got_bd, Some("bd-fixture".to_string()), "a declared config value must reach the real environment");
        assert_eq!(got_max_aeons, None, "SPIRA_MAX_AEONS must never leak into this process's own environment");
    }
}
