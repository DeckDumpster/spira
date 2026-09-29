//! One build tool: `cargo build --profile <p> --workspace` in the worktree under test,
//! cargo's default `target/` (DESIGN.md §1, §6). The result is `target/<profile-dir>/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The directory cargo writes a profile's artifacts to: `dev`/`test` → `debug`,
/// `bench` → `release`, any custom profile → its own name.
pub fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" | "test" => "debug",
        "bench" => "release",
        other => other,
    }
}

pub fn artifacts_dir(worktree: &Path, profile: &str) -> PathBuf {
    worktree.join("target").join(profile_dir(profile))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// cargo is not on PATH: the runner's environment (rc 3).
    NoCargo,
    /// the candidate's workspace does not build (rc 4).
    Failed(i32),
    /// cargo succeeded but wrote somewhere else (a target-dir override in config): rc 3.
    NoArtifacts(PathBuf),
}

pub trait Builder: Sync {
    fn build(&self, worktree: &Path, profile: &str) -> Result<Duration, BuildError>;
}

pub struct Cargo;

impl Builder for Cargo {
    fn build(&self, worktree: &Path, profile: &str) -> Result<Duration, BuildError> {
        let t0 = Instant::now();
        let stdout_to_stderr = {
            use std::os::fd::AsFd;
            std::io::stderr()
                .as_fd()
                .try_clone_to_owned()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null())
        };
        let status = Command::new("cargo")
            .args(["build", "--profile", profile, "--workspace"])
            .current_dir(worktree)
            // cargo's DEFAULT target dir, always: the stable path is what makes it incremental,
            // and <worktree>/target/<p> is the one place SPIRA_ARTIFACTS may point.
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("SPIRA_ARTIFACTS")
            .stdin(Stdio::null())
            .stdout(stdout_to_stderr)
            .status();
        let status = match status {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(BuildError::NoCargo),
            Err(_) => return Err(BuildError::NoCargo),
        };
        if !status.success() {
            return Err(BuildError::Failed(status.code().unwrap_or(1)));
        }
        let dir = artifacts_dir(worktree, profile);
        if !dir.is_dir() {
            return Err(BuildError::NoArtifacts(dir));
        }
        Ok(t0.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_directories() {
        assert_eq!(profile_dir("aeon"), "aeon");
        assert_eq!(profile_dir("release"), "release");
        assert_eq!(profile_dir("dev"), "debug");
        assert_eq!(profile_dir("test"), "debug");
        assert_eq!(
            artifacts_dir(Path::new("/wt"), "aeon"),
            PathBuf::from("/wt/target/aeon")
        );
    }
}
