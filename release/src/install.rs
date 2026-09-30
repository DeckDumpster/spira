//! `release install-tarball` (DESIGN.md "install-tarball"): unpack a release tarball built
//! by `spira/build-tarball.sh` beside existing releases, swap `current` atomically, and
//! restart. Replaces `spira/activate.sh` for the public (GitHub tag/tarball) pipeline —
//! `release build`/`verify`/`activate` remain the local, sha-named path (DESIGN.md "build");
//! this is the second, timestamp-named one the public pipeline has always used, and the two
//! never share a release directory.
//!
//! Kept deliberately close to `spira/activate.sh`'s own behaviour rather than upgraded to
//! `activate::switch`'s stricter one (DESIGN.md "install-tarball: differences from
//! activate"): daemon-reload and a failed restart are warnings, not a rollback, because
//! `deploy.sh` already wraps the whole call in its own rollback at a coarser grain, and every
//! active `spira-*.service` unit is restarted rather than only the ones whose `ExecStart`
//! changed, because a tarball install has no prior release's rendered units to diff against.

use crate::config::Config;
use crate::fsutil;
use crate::systemctl::Systemctl;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Extracts a `.tar.gz`/`.tgz` release tarball. A trait so tests never run real `tar`.
pub trait Unpack {
    /// Extract `tarball` into the existing, empty directory `into`.
    fn extract(&self, tarball: &Path, into: &Path) -> Result<(), String>;
}

pub struct RealUnpack;

impl Unpack for RealUnpack {
    fn extract(&self, tarball: &Path, into: &Path) -> Result<(), String> {
        let st = Command::new("tar").arg("-xzf").arg(tarball).arg("-C").arg(into).status().map_err(|e| format!("cannot run tar: {e}"))?;
        if !st.success() {
            return Err(format!("tar -xzf {} failed ({st})", tarball.display()));
        }
        Ok(())
    }
}

pub struct InstallOpts {
    pub dry_run: bool,
    pub settle: Duration,
}

impl Default for InstallOpts {
    fn default() -> Self {
        InstallOpts { dry_run: false, settle: Duration::from_secs(3) }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Installed {
    /// The release directory name (`spira-<timestamp>`, from the tarball's own basename).
    pub name: String,
    /// False when `spira-releases/<name>` already existed and nothing was unpacked
    /// (releases are immutable; re-installing the same tarball is a no-op unpack).
    pub fresh: bool,
    pub restarted: Vec<String>,
    pub restart_failed: Vec<String>,
    pub pruned: Vec<String>,
    pub prune_failed: Vec<String>,
}

/// The release name a tarball's own filename promises: `spira-<timestamp>`, extension
/// stripped. `activate.sh`'s exact rule — the tarball and the directory it unpacks to name
/// each other, so a caller passing the wrong file is refused before anything is written.
pub fn release_name_from_tarball(tarball: &Path) -> Result<String, String> {
    let base = tarball.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("not a valid filename: {}", tarball.display()))?;
    let name = base.strip_suffix(".tar.gz").or_else(|| base.strip_suffix(".tgz")).ok_or_else(|| format!("expected a .tar.gz tarball, got: {base}"))?;
    if !name.starts_with("spira-") || name.len() <= "spira-".len() {
        return Err(format!("tarball must be named spira-<timestamp>.tar.gz, got: {base}"));
    }
    Ok(name.to_string())
}

/// Removes a directory when dropped, unless disarmed — the same "stage never survives a
/// failure" guard `build::build` uses.
struct Cleanup(Option<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fsutil::remove_tree(&p);
        }
    }
}

/// `release install-tarball <tarball>`.
pub fn install(cfg: &Config, sc: &dyn Systemctl, un: &dyn Unpack, tarball: &Path, o: &InstallOpts) -> Result<Installed, String> {
    if !tarball.is_file() {
        return Err(format!("tarball not found: {}", tarball.display()));
    }
    let name = release_name_from_tarball(tarball)?;
    let dir = cfg.releases.join(&name);

    if o.dry_run {
        eprintln!("release: DRY RUN: would unpack {} -> {}", tarball.display(), dir.display());
        eprintln!("release: DRY RUN: would swap current -> {name}");
        eprintln!("release: DRY RUN: would daemon-reload and restart active spira-*.service units");
        return Ok(Installed { name, ..Default::default() });
    }

    let fresh = fs::symlink_metadata(&dir).is_err();
    if !fresh {
        eprintln!("release: {name} already present — skipping unpack");
    } else {
        fs::create_dir_all(&cfg.releases).map_err(|e| format!("cannot create {}: {e}", cfg.releases.display()))?;
        let pid = std::process::id();
        let unpack_tmp = cfg.releases.join(format!(".unpack-{pid}"));
        let _ = fsutil::remove_tree(&unpack_tmp);
        fs::create_dir(&unpack_tmp).map_err(|e| format!("cannot create {}: {e}", unpack_tmp.display()))?;
        let mut guard = Cleanup(Some(unpack_tmp.clone()));

        eprintln!("release: unpacking {}", tarball.file_name().unwrap_or_default().to_string_lossy());
        un.extract(tarball, &unpack_tmp)?;

        let produced = unpack_tmp.join(&name);
        if !produced.is_dir() {
            return Err(format!("{} did not unpack to the expected directory {name}/ (looked in {})", tarball.display(), unpack_tmp.display()));
        }
        // Move BEFORE marking read-only: renaming a directory across a different parent
        // (`.unpack-<pid>/` -> `cfg.releases/`) rewrites its own `..` entry, which needs
        // write permission on the directory being moved — a read-only `produced` would
        // make every install fail right here. `spira/activate.sh` chmods after its own
        // `mv` for the same reason.
        fs::rename(&produced, &dir).map_err(|e| format!("cannot move {} to {}: {e}", produced.display(), dir.display()))?;
        guard.0 = None;
        let _ = fsutil::remove_tree(&unpack_tmp);
        fsutil::set_readonly(&dir).map_err(|e| format!("could not make {} read-only: {e}", dir.display()))?;
    }

    // Atomically swap the current symlink via rename(2): a temp symlink beside `current`,
    // then renamed over it (same directory, same filesystem).
    let cur_link = cfg.releases.join(crate::activate::CURRENT);
    fsutil::atomic_symlink(&cur_link, &name)?;
    eprintln!("release: current -> {name}");

    if let Err(e) = sc.daemon_reload() {
        eprintln!("release: WARN: daemon-reload failed — continuing: {e}");
    }

    let mut restarted = Vec::new();
    let mut restart_failed = Vec::new();
    match sc.list_active("spira-*.service") {
        Ok(units) if units.is_empty() => eprintln!("release: no active spira-*.service units to restart"),
        Ok(units) => {
            eprintln!("release: restarting: {}", units.join(" "));
            for u in units {
                match sc.restart(&u) {
                    Ok(()) => restarted.push(u),
                    Err(e) => restart_failed.push(format!("{u}: {e}")),
                }
            }
            if !restart_failed.is_empty() {
                eprintln!("release: WARN: some units failed to restart: {}", restart_failed.join("; "));
            }
            if !o.settle.is_zero() {
                std::thread::sleep(o.settle);
            }
        }
        Err(e) => eprintln!("release: WARN: could not list active spira-*.service units — continuing: {e}"),
    }

    let (pruned, prune_failed) = prune(cfg, &name);
    if !prune_failed.is_empty() {
        eprintln!("release: WARN: prune encountered an error — release count may exceed {}: {}", cfg.keep, prune_failed.join("; "));
    }

    eprintln!("release: done — {name} is now current");
    Ok(Installed { name, fresh, restarted, restart_failed, pruned, prune_failed })
}

/// Keep the newest `cfg.keep` timestamp-named (`spira-*`) release directories, newest name
/// first (the name IS the timestamp, so lexical order is chronological order — `lib.sh`'s
/// `_prune_candidates`), never removing whatever `current` now names. Best-effort: a failure
/// to remove a candidate is reported, never fails the install.
fn prune(cfg: &Config, current_name: &str) -> (Vec<String>, Vec<String>) {
    let mut names: Vec<String> = match fs::read_dir(&cfg.releases) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("spira-") && e.path().is_dir() && !e.file_type().map(|t| t.is_symlink()).unwrap_or(true))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => return (Vec::new(), Vec::new()),
    };
    names.sort();
    names.reverse();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for (i, name) in names.into_iter().enumerate() {
        if i < cfg.keep || name == current_name {
            continue;
        }
        eprintln!("release: prune: removing {name}");
        match fsutil::remove_tree(&cfg.releases.join(&name)) {
            Ok(()) => removed.push(name),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    (removed, failed)
}
