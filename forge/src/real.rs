//! Production `Gh`/`Proc`: exec `timeout $GH_TIMEOUT $SPIRA_GH <args...>`, exactly the two
//! processes bash's `ghq()` launched (`timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"`).
//! The credential path is whatever `$SPIRA_GH` is — this never reads a token itself.

use crate::ports::{Gh, GhOut, Proc};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub struct RealGh;

/// `GH_TIMEOUT` is not a registered `spira/conf.d` key — the per-invocation timeout
/// override every `timeout` call here takes, read straight off the environment.
fn gh_timeout() -> String {
    std::env::var("GH_TIMEOUT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "120".into())
}

/// `SPIRA_GH`, a registered config key (spira/conf.d) — the one source of config, through
/// `spira_config::process::cfg`, never a competing environment override (per Ryan
/// 2026-10-05). Its own declared default is the empty string, and empty IS the configured
/// meaning "use the system gh" (spira/conf.d/SPIRA_GH's own DOC) — not a Rust-side
/// fallback this crate invents, so substituting `"gh"` for an empty *declared* value stays.
fn spira_gh() -> Result<String, String> {
    let v = spira_config::process::cfg("SPIRA_GH")?;
    Ok(if v.is_empty() { "gh".to_string() } else { v })
}

fn run(repo: Option<&Path>, args: &[&str], input: Option<&[u8]>, merge_stderr: bool) -> GhOut {
    let gh = match spira_gh() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("forge: {e}");
            return GhOut { code: -1, stdout: Vec::new() };
        }
    };
    let mut c = Command::new("timeout");
    c.arg(gh_timeout()).arg(gh).args(args);
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
        c.envs(spira_config::release_env::child_path_env_for_process());
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

    /// `spira_gh` resolves `SPIRA_GH` through the one door, `spira_config::process::cfg` —
    /// this is the ONLY test in this crate that drives it: `cfg`'s resolution is a
    /// process-global `OnceLock`, computed once and never reset, so a single test function
    /// cannot even vary it across two calls within itself, let alone across tests.
    /// `SPIRA_HOME`/`SPIRA_TOML` point at a throwaway fixture, never the real box's.
    #[test]
    fn spira_gh_substitutes_gh_when_the_declared_value_is_empty() {
        let dir = testkit::TempDir::new("forge-real-env");
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_GH", "")]);
        std::env::set_var("SPIRA_HOME", &real_home);
        std::env::set_var("SPIRA_TOML", &toml);

        assert_eq!(spira_gh(), Ok("gh".to_string()), "an empty declared SPIRA_GH means the system gh");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
