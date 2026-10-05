//! `spira_config_writeback` (conf.sh; wave 4.7, sp-ksrss): the ONE path any code that would
//! REGENERATE a config file (`spira.toml`, a converted `spira.conf`, ...) resolves its write
//! target through, so a worktree never clobbers the operator's real file however it got
//! `HOME`.
//!
//! scar: three cutover branches, each sourcing their own `conf.sh` with the operator's real
//! `HOME`, regenerated the operator's real `spira.toml` from worktree-local state — three
//! times in one day, each time blinding queue-watch until the file was restored by hand. A
//! worktree has no business writing outside itself, however it got `HOME`.
//!
//! Pure function, no env reads here (crate convention: `chamber::persona_model_from_doc`,
//! `resolve::resolve`'s `ResolveInput`) — the CLI (`spira-config writeback`, `main.rs`) is
//! the one place this reads `SPIRA_CONFIG_WRITE`/`SPIRA_HOME`/`SPIRA_PROD`/`SPIRA_REPO`/
//! `HOME`/`XDG_CONFIG_HOME` from the process environment and builds a [`WritebackInput`].

use std::path::{Path, PathBuf};

/// The facts `writeback` decides from. Every field is a plain value, never read from the
/// environment here, so this is testable without the crate's `ENV_LOCK`.
pub struct WritebackInput<'a> {
    /// `SPIRA_CONFIG_WRITE=1` — the explicit escape hatch: the candidate is returned
    /// unchanged, no redirect, no writability check.
    pub config_write: bool,
    /// `SPIRA_HOME` — this process's own tree.
    pub home: &'a Path,
    /// `SPIRA_PROD`, if set — the tree systemd executes. Empty/absent fails closed into
    /// the redirect, same as bash's unset-SPIRA_PROD case: an unresolved production
    /// directory is its own error, reported where it is diagnosed, not folded into this
    /// answer.
    pub prod: Option<&'a Path>,
    /// `SPIRA_REPO` — the tree being developed; the first redirect tier.
    pub repo: &'a Path,
    /// `${XDG_CONFIG_HOME:-$HOME/.config}/spira` — the second redirect tier.
    pub xdg_spira_dir: PathBuf,
}

/// Resolves `candidate` (not necessarily a real `cd`-able directory yet) to its canonical
/// form when possible, else returns it unchanged — same fallback bash's
/// `"$(cd "$p" 2>/dev/null && pwd -P)" || _p="$p"` gives: an uncanonicalizable path (does
/// not exist, a dangling symlink) still compares by its given spelling rather than making
/// the whole redirect decision fail.
fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `[ -w "$path" ]` — real access-permission check (uid/gid-aware, unlike
/// `fs::metadata().permissions().readonly()`, which only inspects mode bits), so this agrees
/// with the bash `spira_config_writeback` it replaces byte for byte, including under a
/// testenv container's foreign-UID bind mount (sp-jv49c).
fn is_writable(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// A fresh, empty, writable file under the system temp dir (honors `$TMPDIR`, which
/// `std::env::temp_dir()` already reads on Unix) — the same job `mktemp
/// "${TMPDIR:-/tmp}/spira-toml.XXXXXX"` did, done natively so this crate declares no
/// dependency on it (same reasoning as `release::stage::mktemp_dir`'s own comment: `mktemp`
/// is not one of `deps-lint`'s system-utility exceptions). The auto-convert a caller's run
/// depends on must not fail for want of a place to land, so this creates the file (not just
/// names it) before returning, same as `mktemp` itself does.
fn mktemp_scratch_file() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let base = std::env::temp_dir();
    for _ in 0..64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = base.join(format!("spira-toml-{}-{nanos}-{n}", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // mktemp itself has no further fallback either; a base temp dir this hostile
            // (unwritable /tmp) is a box-level problem no caller here can repair.
            Err(_) => return p,
        }
    }
    base.join(format!("spira-toml-{}", std::process::id()))
}

/// Resolves the write target for a config file this process is about to REGENERATE.
/// `candidate` is returned unchanged only when this checkout IS the installed release
/// (`input.home` resolves to the same directory as `input.prod`) or `SPIRA_CONFIG_WRITE=1`
/// was set explicitly; otherwise the write is redirected to `input.repo`, then further to
/// `input.xdg_spira_dir`, then to a private scratch file, at the first of those that is
/// actually writable.
///
/// `input.repo` IS CHECKED, NOT ASSUMED, WRITABLE (sp-jv49c): a testenv container
/// bind-mounts it read-write for its host owner but read-only (or foreign-UID-owned) for
/// the user `conf.sh` runs as, so a redirect that lands there unconditionally hands
/// `spira-config convert` a target it cannot write and the whole auto-convert fails —
/// silently reverting every `SPIRA_*` key to its computed default instead of what
/// `spira.conf` actually says. The final fallback, a scratch file, is chosen precisely
/// because it always succeeds: the auto-convert this run's values depend on must not fail
/// for want of a place to land.
pub fn writeback(candidate: &Path, input: &WritebackInput) -> PathBuf {
    if input.config_write {
        return candidate.to_path_buf();
    }
    let home_p = canonicalize_or_self(input.home);
    if let Some(prod) = input.prod {
        if !prod.as_os_str().is_empty() {
            let prod_p = canonicalize_or_self(prod);
            if prod_p == home_p {
                return candidate.to_path_buf();
            }
        }
    }
    let basename = candidate.file_name().unwrap_or_default();
    if is_writable(input.repo) {
        return input.repo.join(basename);
    }
    if std::fs::create_dir_all(&input.xdg_spira_dir).is_ok() && is_writable(&input.xdg_spira_dir)
    {
        return input.xdg_spira_dir.join(basename);
    }
    mktemp_scratch_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn input<'a>(home: &'a Path, prod: Option<&'a Path>, repo: &'a Path, xdg: &'a Path) -> WritebackInput<'a> {
        WritebackInput { config_write: false, home, prod, repo, xdg_spira_dir: xdg.to_path_buf() }
    }

    #[test]
    fn config_write_escape_hatch_leaves_the_candidate_alone() {
        let d = testkit::TempDir::new("writeback-config-write");
        let candidate = d.join("spira.toml");
        let mut i = input(&d, None, &d, &d);
        i.config_write = true;
        assert_eq!(writeback(&candidate, &i), candidate);
    }

    #[test]
    fn single_checkout_home_equals_prod_leaves_the_candidate_alone() {
        let d = testkit::TempDir::new("writeback-single-checkout");
        let candidate = d.join("spira.toml");
        let i = input(&d, Some(&d), &d, &d);
        assert_eq!(writeback(&candidate, &i), candidate);
    }

    #[test]
    fn an_unprivileged_worktree_redirects_under_a_writable_repo() {
        let home = testkit::TempDir::new("writeback-wt-home");
        let repo = testkit::TempDir::new("writeback-wt-repo");
        let xdg = testkit::TempDir::new("writeback-wt-xdg");
        let candidate = PathBuf::from("/wherever/spira.toml");
        let i = input(&home, None, &repo, &xdg);
        assert_eq!(writeback(&candidate, &i), repo.join("spira.toml"));
    }

    #[test]
    fn empty_prod_still_redirects_rather_than_matching_home() {
        let home = testkit::TempDir::new("writeback-empty-prod-home");
        let repo = testkit::TempDir::new("writeback-empty-prod-repo");
        let xdg = testkit::TempDir::new("writeback-empty-prod-xdg");
        let candidate = PathBuf::from("/wherever/spira.toml");
        let empty = PathBuf::new();
        let i = input(&home, Some(&empty), &repo, &xdg);
        assert_eq!(writeback(&candidate, &i), repo.join("spira.toml"));
    }

    #[test]
    fn unwritable_repo_falls_further_to_xdg() {
        // A mode-0555 directory does not stop root writing it, so as root this case cannot
        // tell the fallback from the first choice: nothing to judge.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let home = testkit::TempDir::new("writeback-unwritable-repo-home");
        let repo = testkit::TempDir::new("writeback-unwritable-repo-repo");
        let xdg_parent = testkit::TempDir::new("writeback-unwritable-repo-xdgp");
        let xdg = xdg_parent.join("spira");
        let candidate = PathBuf::from("/wherever/spira.toml");
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o555)).unwrap();
        let i = input(&home, None, &repo, &xdg);
        let got = writeback(&candidate, &i);
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got, xdg.join("spira.toml"));
    }

    #[test]
    fn both_repo_and_xdg_unwritable_falls_to_a_writable_scratch_file() {
        // A mode-0555 directory does not stop root writing it, so as root this case cannot
        // tell the fallback from the first choice: nothing to judge.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let home = testkit::TempDir::new("writeback-both-unwritable-home");
        let repo = testkit::TempDir::new("writeback-both-unwritable-repo");
        let xdg_parent = testkit::TempDir::new("writeback-both-unwritable-xdgp");
        let candidate = PathBuf::from("/wherever/spira.toml");
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(&xdg_parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let xdg = xdg_parent.join("spira");
        let i = input(&home, None, &repo, &xdg);
        let got = writeback(&candidate, &i);
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&xdg_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(got.exists(), "the double-unwritable fallback is itself a writable, existing path: {got:?}");
    }

    #[test]
    fn redirect_keeps_the_candidates_basename() {
        let home = testkit::TempDir::new("writeback-basename-home");
        let repo = testkit::TempDir::new("writeback-basename-repo");
        let xdg = testkit::TempDir::new("writeback-basename-xdg");
        let candidate = PathBuf::from("/some/deep/path/spira.toml");
        let i = input(&home, None, &repo, &xdg);
        assert_eq!(writeback(&candidate, &i), repo.join("spira.toml"));
    }
}
