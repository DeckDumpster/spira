//! `release verify <sha>` (DESIGN.md "verify"). Reports every failure, not just the first.

use std::process::Command;
use crate::config::Config;
use crate::deps_check::{self, SpiraEnv};
use crate::spira_env::{self, Nix};
use crate::fsutil;
use crate::manifest::Manifest;
use std::path::{Path, PathBuf};

pub struct VerifyOpts<'a> {
    pub pre_activate: bool,
    pub system_dirs: Vec<PathBuf>,
    pub nix: &'a dyn Nix,
}

/// The release directory for `sha`, if `sha` is a full sha and it exists.
pub fn release_dir(cfg: &Config, sha: &str) -> Result<PathBuf, String> {
    if !crate::is_sha(sha) {
        return Err(format!("{sha:?} is not a full commit sha (40 hex digits); releases are named by exactly that"));
    }
    let d = cfg.releases.join(sha);
    if !d.is_dir() {
        return Err(format!("no release {sha} in {}", cfg.releases.display()));
    }
    Ok(d)
}

/// The file half of verify: MANIFEST names this sha and matches the tree exactly.
pub fn check_files(rel: &Path, sha: &str) -> Vec<String> {
    let m = match Manifest::load(rel) {
        Ok(m) => m,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    if m.commit != sha {
        problems.push(format!("MANIFEST says commit {}, but the release is named {sha}", m.commit));
    }
    match m.check(rel) {
        Ok(p) => problems.extend(p),
        Err(e) => problems.push(e),
    }
    problems
}

pub fn verify(cfg: &Config, sha: &str, o: &VerifyOpts) -> Result<Vec<String>, String> {
    let rel = release_dir(cfg, sha)?;
    let mut problems = check_files(&rel, sha);
    match fsutil::writable_paths(&rel) {
        Ok(w) if !w.is_empty() => {
            let shown: Vec<&str> = w.iter().take(5).map(String::as_str).collect();
            problems.push(format!("{} path(s) are writable, e.g. {}", w.len(), shown.join(", ")));
        }
        Ok(_) => {}
        Err(e) => problems.push(e),
    }
    problems.extend(crate::build::clashes(&rel, &o.system_dirs).into_iter().map(|c| format!("name clash: {c}")));
    problems.extend(spira_config::release_env::model_bin_problems(&rel));
    match crate::units::check_binaries(&rel) {
        Ok(p) => problems.extend(p),
        Err(e) => problems.push(e),
    }
    problems.extend(crate::symbols::problems(&rel));
    if o.pre_activate {
        match pre_activate_env(cfg, &rel) {
            Ok(envs) => problems.extend(deps_problems(&rel, &envs[2].1, o.nix)),
            Err(e) => problems.push(format!("deps: cannot build the release's own PATH: {e}")),
        }
        let pa = rel.join("spira/pre-activate.sh");
        if !fsutil::is_executable(&pa) {
            problems.push(format!("{} is missing; refusing an unverifiable release", pa.display()));
        } else {
            match pre_activate_env(cfg, &rel) {
                Ok(envs) => {
                    // batch-job: this runs whatever its caller names, as long as that takes
                    let mut cmd = Command::new(&pa);
                    cmd.arg(&rel);
                    for (k, v) in &envs {
                        cmd.env(k, v);
                    }
                    let scratch = match staged_config(cfg, &rel) {
                        Ok(Some((dir, spec))) => {
                            cmd.env("SPIRA_TOML", spec);
                            Some(dir)
                        }
                        Ok(None) => None,
                        Err(e) => {
                            problems.push(format!("cannot apply the release's config delta for pre-activate: {e}"));
                            return Ok(problems);
                        }
                    };
                    let res = cmd.output();
                    if let Some(d) = scratch {
                        let _ = std::fs::remove_dir_all(d);
                    }
                    match res {
                        Ok(out) if out.status.success() => {}
                        Ok(out) => {
                            let err = String::from_utf8_lossy(&out.stderr);
                            let tail: Vec<&str> = err.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
                            problems.push(format!("pre-activate failed ({}): {}", out.status, tail.join(" | ")));
                        }
                        Err(e) => problems.push(format!("cannot run {}: {e}", pa.display())),
                    }
                }
                Err(e) => problems.push(format!("cannot build the release's own PATH for pre-activate: {e}")),
            }
        }
    }
    Ok(problems)
}

/// The dependency check, against the env `rel`'s own flake builds. The build is linked in a
/// scratch dir: the installed env link is activation's to move.
fn deps_problems(rel: &Path, path: &str, nix: &dyn Nix) -> Vec<String> {
    if !spira_env::has_flake(rel) {
        return deps_check::problems(rel, path, SpiraEnv::Installed);
    }
    let scratch = match crate::config_delta::scratch_dir() {
        Ok(d) => d,
        Err(e) => return cannot_build(rel, path, &e),
    };
    let out = scratch.join("spira-env");
    let problems = match nix.build(&spira_env::target(rel), &out) {
        Ok(()) => deps_check::problems(rel, path, SpiraEnv::Built(&out.join("bin"))),
        Err(e) => cannot_build(rel, path, &e),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    problems
}

fn cannot_build(rel: &Path, path: &str, why: &str) -> Vec<String> {
    let mut problems = vec![format!("deps: cannot build the release's spira-env: {why}")];
    problems.extend(deps_check::problems(rel, path, SpiraEnv::Unbuilt));
    problems
}

/// The config layers with the release's own delta applied, in a scratch dir, and the
/// `SPIRA_TOML` naming them: pre-activate judges the release against the config it will run
/// with, never the one in force. `None` when the release declares no delta.
fn staged_config(cfg: &Config, rel: &Path) -> Result<Option<(PathBuf, String)>, String> {
    let Some(delta) = crate::config_delta::load(rel)? else { return Ok(None) };
    let txn = crate::config_delta::prepare(cfg, rel, &delta, true)?;
    let dir = crate::config_delta::scratch_dir()?;
    match txn.stage(&dir) {
        Ok(spec) => Ok(Some((dir, spec))),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// The `SPIRA_RELEASE`, `SPIRA_HOME` and `PATH` overrides `verify`'s pre-activate child runs
/// with, built from `rel` — the release **under verification** — never inherited from the
/// caller. A bare command inside `pre-activate.sh` would otherwise resolve against the
/// caller's `PATH`, and the registry check against the caller's `SPIRA_HOME`: `locate_home`
/// prefers `SPIRA_HOME` over `SPIRA_RELEASE`, so a service's inherited home would judge the
/// new binary against the active release's `conf.d`. Other inherited variables are harmless.
pub(crate) fn pre_activate_env(cfg: &Config, rel: &Path) -> Result<[(String, String); 3], String> {
    let rel_str = rel.display().to_string();
    let tail = cfg.path_tail()?;
    let env_bin = spira_config::spira_env_bin(&cfg.home()?.display().to_string());
    let path = spira_config::release_path_with_env_and_tail(&rel_str, &env_bin, &tail)?;
    Ok([(spira_config::RELEASE_ENV.to_string(), rel_str), ("SPIRA_HOME".to_string(), rel.join("spira").display().to_string()), ("PATH".to_string(), path)])
}
