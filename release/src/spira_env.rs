//! The release's pinned tool environment: `flake.nix`'s `spira-env`, linked at
//! `~/.local/lib/spira-env`, through which every unit's PATH resolves its tools.

use crate::fsutil;
use std::path::{Path, PathBuf};
use std::process::Command;

pub trait Nix {
    /// Builds `target` and links the result at `out`; `out` is untouched when the build fails.
    fn build(&self, target: &str, out: &Path) -> Result<(), String>;
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String>;
}

/// [`Nix`] over a command runner, which is given `nix` or `nix-store` and its arguments.
pub struct Argv<'a>(pub &'a dyn Fn(&str, &[&str]) -> Result<(), String>);

impl Nix for Argv<'_> {
    fn build(&self, target: &str, out: &Path) -> Result<(), String> {
        (self.0)("nix", &["--extra-experimental-features", "nix-command flakes", "build", target, "-o", &out.display().to_string()])
    }
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String> {
        (self.0)("nix-store", &["--add-root", &root.display().to_string(), "--indirect", "--realise", &store_path.display().to_string()])
    }
}

/// nix from `$SPIRA_NIX_PROFILE_BIN`: a unit's PATH has none.
pub struct RealNix;

impl Nix for RealNix {
    fn build(&self, target: &str, out: &Path) -> Result<(), String> {
        Argv(&run_from_profile).build(target, out)
    }
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String> {
        Argv(&run_from_profile).add_root(root, store_path)
    }
}

fn run_from_profile(prog: &str, args: &[&str]) -> Result<(), String> {
    let dir = spira_config::process::cfg("SPIRA_NIX_PROFILE_BIN")?;
    if dir.is_empty() {
        return Err(format!("SPIRA_NIX_PROFILE_BIN is not set; cannot find {prog}"));
    }
    let bin = Path::new(&dir).join(prog);
    // batch-job: nix build of the release's tool env
    let out = Command::new(&bin).args(args).output().map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{} failed ({}):\n{}", bin.display(), out.status, String::from_utf8_lossy(&out.stderr).trim_end()))
}

/// What a build did to the env link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// The store path the link held before; `None` when there was no env.
    pub before: Option<PathBuf>,
    pub after: PathBuf,
}

pub fn has_flake(release_root: &Path) -> bool {
    release_root.join("flake.nix").is_file() && release_root.join("flake.lock").is_file()
}

/// The flake reference for the release's `spira-env`. The root is resolved through its symlinks:
/// nix copies a symlinked `path:` root into the store as a symlink, and a relative target (the
/// `current` link to `spira-<name>`) then resolves against `/nix/store`, which pure evaluation refuses.
pub fn target(release_root: &Path) -> String {
    let resolved = std::fs::canonicalize(release_root).unwrap_or_else(|_| release_root.to_path_buf());
    format!("path:{}#spira-env", resolved.display())
}

/// Builds `release_root`'s env into `home`'s env link and roots the env it replaced. On `Err`
/// the link is as it was. An unchanged lock rebuilds to the same store path.
pub fn build(nix: &dyn Nix, release_root: &Path, home: &Path) -> Result<Built, String> {
    let out = home.join(spira_config::SPIRA_ENV_REL);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let before = std::fs::canonicalize(&out).ok();
    nix.build(&target(release_root), &out)?;
    let after = std::fs::canonicalize(&out).map_err(|e| format!("the build left no env at {}: {e}", out.display()))?;
    if let Some(old) = before.as_deref().filter(|old| *old != after) {
        let prev = home.join(spira_config::SPIRA_ENV_PREV_REL);
        if let Err(e) = nix.add_root(&prev, old) {
            let left = restore(home, old).err().map(|r| format!("; the env link still names {}: {r}", after.display())).unwrap_or_default();
            return Err(format!("cannot root the previous spira-env {}: {e}{left}", old.display()));
        }
    }
    Ok(Built { before, after })
}

/// Atomically points the env link back at `before`.
pub fn restore(home: &Path, before: &Path) -> Result<(), String> {
    fsutil::atomic_symlink(&home.join(spira_config::SPIRA_ENV_REL), &before.display().to_string())
}
