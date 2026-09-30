//! `release verify <sha>` (DESIGN.md "verify"). Reports every failure, not just the first.

use crate::config::Config;
use crate::fsutil;
use crate::manifest::Manifest;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct VerifyOpts {
    pub pre_activate: bool,
    pub system_dirs: Vec<PathBuf>,
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
    match crate::units::check_binaries(&rel) {
        Ok(p) => problems.extend(p),
        Err(e) => problems.push(e),
    }
    if o.pre_activate {
        let pa = rel.join("spira/pre-activate.sh");
        if !fsutil::is_executable(&pa) {
            problems.push(format!("{} is missing; refusing an unverifiable release", pa.display()));
        } else {
            match Command::new(&pa).arg(&rel).output() {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    let tail: Vec<&str> = err.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
                    problems.push(format!("pre-activate failed ({}): {}", out.status, tail.join(" | ")));
                }
                Err(e) => problems.push(format!("cannot run {}: {e}", pa.display())),
            }
        }
    }
    Ok(problems)
}
