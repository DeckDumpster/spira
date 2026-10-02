//! `--repo` / `SPIRA_REPO` resolution for the commands that read a checkout.

use crate::config::Env;
use spira_config::repos::Registry;
use std::path::{Path, PathBuf};

/// A bare word with no path separator names a registered repo; anything else is a path.
/// With no `--repo` the home repo's configured checkout answers, never a raw `SPIRA_REPO`
/// (a sourced shell can hold the release root there).
pub fn resolve(arg: Option<&Path>, env: &Env) -> Option<PathBuf> {
    let home = env.get("SPIRA_HOME").filter(|s| !s.is_empty()).map(PathBuf::from).or_else(|| env.get("SPIRA_RELEASE").filter(|s| !s.is_empty()).map(|r| PathBuf::from(r).join("spira")))?;
    let reg = Registry::from_env(env.clone(), &home);
    match arg {
        Some(p) => {
            let s = p.to_string_lossy();
            if !s.contains('/') {
                if let Some(root) = reg.root(&s) {
                    return Some(PathBuf::from(root));
                }
            }
            Some(p.to_path_buf())
        }
        None => reg.root("").map(PathBuf::from),
    }
}
