//! Production `Gh`/`Proc`: exec `timeout $GH_TIMEOUT $SPIRA_GH <args...>`, exactly the two
//! processes bash's `ghq()` launched (`timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"`).
//! The credential path is whatever `$SPIRA_GH` is — this never reads a token itself.

use crate::ports::{Gh, GhOut, Proc};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub struct RealGh;

fn gh_timeout() -> String {
    std::env::var("GH_TIMEOUT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "120".into())
}

fn spira_gh() -> String {
    env("SPIRA_GH").unwrap_or_else(|| "gh".into())
}

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`env`] (that
/// would recurse into `resolved_config()` while it is still being built).
fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `$SPIRA_HOME`, else `<release>/spira` derived from this binary's own install location
/// (`<release>/bin/forge`) — mirroring every other rewritten tool with no unit of its own
/// setting `SPIRA_HOME` directly (cockpit-collect's `home_dir`).
fn spira_home_dir() -> std::path::PathBuf {
    if let Some(h) = raw_env("SPIRA_HOME") {
        return std::path::PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| std::path::PathBuf::from("spira"))
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): this crate used to read every
/// `SPIRA_*` key straight out of its own process environment, with no snapshot and no
/// the config document load at all (wave4-decomposition.md row (b) names forge by file: SPIRA_GH,
/// SPIRA_QUEUE_ACTIONS_APP_ID). Resolved once, lazily, and cached:
/// `spira_config::resolve_for_process`. A resolution failure yields an empty
/// [`spira_config::resolve::Resolved`] — [`env`]'s own callers see exactly the behaviour
/// this crate had before this bead, never a panic.
fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = spira_home_dir();
        let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
        spira_config::resolve::resolve_for_process(&home, &repo, &env_map).unwrap_or_default()
    })
}

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so an explicit env override still wins exactly as it did before this bead.
/// Public: `main.rs`'s own `SPIRA_QUEUE_ACTIONS_APP_ID` read uses this too, rather than a
/// second copy of the same fallback chain.
pub fn env(name: &str) -> Option<String> {
    raw_env(name).or_else(|| {
        let v = resolved_config().get(name);
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn run(repo: Option<&Path>, args: &[&str], input: Option<&[u8]>, merge_stderr: bool) -> GhOut {
    let mut c = Command::new("timeout");
    c.arg(gh_timeout()).arg(spira_gh()).args(args);
    if let Some(r) = repo {
        c.current_dir(r);
    }
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    c.stdout(Stdio::piped());
    c.stderr(if merge_stderr { Stdio::piped() } else { Stdio::null() });
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(_) => return GhOut { code: -1, stdout: Vec::new() },
    };
    if let Some(bytes) = input {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(bytes);
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return GhOut { code: -1, stdout: Vec::new() },
    };
    let mut stdout = out.stdout;
    if merge_stderr {
        stdout.extend_from_slice(&out.stderr);
    }
    GhOut { code: out.status.code().unwrap_or(-1), stdout }
}

impl Gh for RealGh {
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut {
        run(repo, args, None, false)
    }
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut {
        run(repo, args, input, true)
    }
}

pub struct RealProc;

impl Proc for RealProc {
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> (i32, Vec<u8>) {
        let mut c = Command::new(program);
        c.args(args);
        c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        c.stdout(Stdio::piped());
        c.stderr(Stdio::null());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(_) => return (-1, Vec::new()),
        };
        if let Some(bytes) = input {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(bytes);
            }
        }
        match child.wait_with_output() {
            Ok(o) => (o.status.code().unwrap_or(-1), o.stdout),
            Err(_) => (-1, Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wave 4.8: `env` now falls back to `spira_config::resolve()` between the raw
    /// environment and the caller's own default. This is the ONLY test in this crate
    /// that calls `env`/`resolved_config` — the `OnceLock` inside `resolved_config`
    /// computes once per process and never resets. SPIRA_HOME points at a throwaway
    /// fixture with its own `conf.d` (never the real box's).
    #[test]
    fn env_falls_back_to_the_registry_then_the_callers_default() {
        let dir = testkit::TempDir::new("forge-real-env");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_GH"),
            "TYPE=string\nGROUP=gh\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_GH:=gh}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::env::set_var("SPIRA_HOME", &home);
        std::env::set_var("SPIRA_TOML", dir.join("no-such-config.toml"));
        std::env::remove_var("SPIRA_GH");

        assert_eq!(env("SPIRA_GH"), Some("gh".to_string()), "a registry default must reach env() without an env override");
        assert_eq!(env("SPIRA_NO_SUCH_KEY_AT_ALL_EVER"), None);

        std::env::set_var("SPIRA_GH", "/custom/gh");
        assert_eq!(env("SPIRA_GH"), Some("/custom/gh".to_string()), "an explicit env override still wins over the registry");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
