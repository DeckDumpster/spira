//! Where the programs `work` itself runs are found. The model's PATH is restricted to `work`
//! and the toolchain on purpose; the reviewer and the repository's check rows are trusted
//! children and resolve against the release instead.

use std::path::{Path, PathBuf};

/// `$SPIRA_RELEASE`, else the release `exe` sits in (`<release>/model-bin/work` or `<release>/bin/work`).
pub fn release_root(env_release: Option<&str>, exe: &Path) -> Option<PathBuf> {
    match env_release.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => Some(PathBuf::from(r.trim_end_matches('/'))),
        None => exe.parent().and_then(Path::parent).filter(|r| r.join("bin").is_dir() && r.join("spira").is_dir()).map(Path::to_path_buf),
    }
}

/// The directories a trusted child resolves against, in order: the release's `bin/` and
/// `spira/`, the box's own tool tail (`spira.path`), then the inherited PATH.
pub fn search_dirs(release: Option<&Path>, tail: &str, inherited: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(r) = release {
        dirs.push(r.join("bin"));
        dirs.push(r.join("spira"));
    }
    dirs.extend(tail.split(':').chain(inherited.split(':')).filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

pub fn resolve(prog: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: &Path| p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if prog.contains('/') {
        return executable(Path::new(prog)).then(|| PathBuf::from(prog));
    }
    dirs.iter().map(|d| d.join(prog)).find(|p| executable(p))
}

pub fn join_path(dirs: &[PathBuf]) -> String {
    dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(":")
}

/// The search dirs for this process. The box's tool tail (it holds `bd`) is for the reviewer
/// only: a repository's check rows never see it.
pub fn process_dirs(with_tail: bool) -> Vec<PathBuf> {
    let exe = std::env::current_exe().unwrap_or_default();
    let release = release_root(std::env::var(spira_config::RELEASE_ENV).ok().as_deref(), &exe);
    let tail = if with_tail { spira_config::process::cfg("SPIRA_PATH").unwrap_or_default() } else { String::new() };
    search_dirs(release.as_deref(), &tail, &std::env::var("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_comes_from_the_environment_before_the_executable() {
        let exe = Path::new("/r/exe-rel/model-bin/work");
        assert_eq!(release_root(Some("/r/env-rel/"), exe), Some(PathBuf::from("/r/env-rel")));
        assert_eq!(release_root(Some(""), Path::new("/usr/bin/work")), None);
    }

    #[test]
    fn the_release_bin_precedes_the_inherited_path() {
        let d = search_dirs(Some(Path::new("/r")), "/t", "/usr/bin::/bin");
        assert_eq!(join_path(&d), "/r/bin:/r/spira:/t:/usr/bin:/bin");
    }

    #[test]
    fn a_missing_program_does_not_resolve() {
        assert_eq!(resolve("no-such-program-sp-nqmjx7", &[PathBuf::from("/usr/bin")]), None);
        assert!(resolve("sh", &[PathBuf::from("/bin"), PathBuf::from("/usr/bin")]).is_some());
    }
}
