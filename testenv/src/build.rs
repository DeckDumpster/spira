//! One build tool: `cargo build --profile <p> --workspace` in the worktree under test,
//! cargo's default `target/` (DESIGN.md §1, §6). The result is `target/<profile-dir>/`.

use std::os::unix::process::CommandExt;
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
    /// cargo was killed at the trial's setup cutoff (DESIGN.md D9): rc 2, never the
    /// candidate's rc 4 — nothing about the branch was judged.
    Deadline,
    /// The box's compilation cache (sccache) is required and absent (sp-z61hj): rc 3, the
    /// runner's environment — never a cold build of every dependency.
    NoCache(String),
}

pub trait Builder: Sync {
    /// Build; past `deadline` cargo is killed and the result is [`BuildError::Deadline`].
    fn build(
        &self,
        worktree: &Path,
        profile: &str,
        deadline: Option<Instant>,
    ) -> Result<Duration, BuildError>;
}

pub struct Cargo;

impl Builder for Cargo {
    fn build(
        &self,
        worktree: &Path,
        profile: &str,
        deadline: Option<Instant>,
    ) -> Result<Duration, BuildError> {
        let t0 = Instant::now();
        // THE BUILD CACHE (sp-z61hj; spira-config/DESIGN-build-cache.md): every dependency
        // is a cache read after its first build on this box; absent sccache refuses.
        let wrapper = spira_config::build::wrapper_from_env().map_err(BuildError::NoCache)?;
        if wrapper == spira_config::build::Wrapper::Off {
            eprintln!("testenv: {}", wrapper.describe());
        }
        let stdout_to_stderr = {
            use std::os::fd::AsFd;
            std::io::stderr()
                .as_fd()
                .try_clone_to_owned()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null())
        };
        let mut cargo_cmd = Command::new("cargo");
        cargo_cmd
            .args(["build", "--profile", profile, "--workspace"])
            // One-shot: no incremental cache (a switch, not CARGO_INCREMENTAL, which would
            // split the compilation cache's keys).
            .args(spira_config::build::one_shot(profile))
            .envs(wrapper.env())
            .current_dir(worktree)
            // cargo's DEFAULT target dir, always: the stable path is what makes it incremental,
            // and <worktree>/target/<p> is the one place SPIRA_ARTIFACTS may point.
            .env_remove("CARGO_TARGET_DIR")
            // A caller's CARGO_INCREMENTAL would split the compilation cache (sccache hashes
            // every CARGO_* variable); the one-shot switch above says the same thing.
            .env_remove("CARGO_INCREMENTAL")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("SPIRA_ARTIFACTS")
            .stdin(Stdio::null())
            .stdout(stdout_to_stderr)
            // Its own process group, so a kill at the cutoff takes rustc with it.
            .process_group(0);
        // rustc's RUSTC_WRAPPER=sccache auto-starts sccache's own server on first use, a
        // long-lived daemon that outlives this build; it must never inherit a caller's lock
        // fd this process did not open itself (sp-ohwg7).
        crate::util::close_inherited_fds(&mut cargo_cmd);
        let child = cargo_cmd.spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => return Err(BuildError::NoCargo),
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {}
                Err(_) => return Err(BuildError::NoCargo),
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                // SAFETY: signalling the process group of a child we spawned.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err(BuildError::Deadline);
            }
            std::thread::sleep(Duration::from_millis(100));
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
