//! `release prune` (DESIGN.md "prune").

use crate::activate;
use crate::config::Config;
use crate::fsutil;
use crate::manifest::Manifest;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: Vec<String>,
    pub kept: Vec<String>,
    /// What could not be removed, and why.
    pub failed: Vec<String>,
}

fn pid_alive(pid: &str) -> bool {
    Path::new("/proc").join(pid).exists()
}

/// Keep the newest `cfg.keep` releases by MANIFEST `built` time, plus whatever `current`
/// names, the rollback target and a standing hotfix; remove the rest, and any stage or
/// throw-away target directory whose builder is gone.
pub fn prune(cfg: &Config) -> Result<Pruned, String> {
    let mut protected = BTreeSet::new();
    if let Some(c) = activate::current(cfg) {
        protected.insert(c);
    }
    if let Ok(state) = cfg.state_dir() {
        let h = activate::read_history(&state)?;
        if h.len() >= 2 {
            protected.insert(h[h.len() - 2].sha.clone());
        }
        if let Some(hf) = activate::read_hotfix(&state)? {
            protected.insert(hf.sha);
        }
    }
    let rd = match fs::read_dir(&cfg.releases) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pruned::default()),
        Err(e) => return Err(format!("cannot read {}: {e}", cfg.releases.display())),
    };
    let mut releases = Vec::new();
    let mut out = Pruned::default();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if crate::is_sha(&name) && path.is_dir() && !e.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            let built = Manifest::load(&path).ok().and_then(|m| m.built).unwrap_or_default();
            releases.push((built, name));
        } else if let Some(rest) = name.strip_prefix(".stage-").or_else(|| name.strip_prefix(".target-")) {
            let pid = rest.rsplit('-').next().unwrap_or("");
            if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) && !pid_alive(pid) {
                match fsutil::remove_tree(&path) {
                    Ok(()) => out.removed.push(name),
                    Err(e) => out.failed.push(e),
                }
            }
        }
    }
    // Newest first; an unreadable `built` sorts oldest.
    releases.sort_by(|a, b| b.cmp(a));
    for (i, (_, name)) in releases.into_iter().enumerate() {
        if i < cfg.keep || protected.contains(&name) {
            out.kept.push(name);
            continue;
        }
        match fsutil::remove_tree(&cfg.releases.join(&name)) {
            Ok(()) => out.removed.push(name),
            Err(e) => out.failed.push(e),
        }
    }
    Ok(out)
}
