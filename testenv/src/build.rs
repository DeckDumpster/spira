//! One build tool: `cargo build --profile <p> --workspace` in the worktree under test,
//! cargo's default `target/` (DESIGN.md §1, §6). The result is `target/<profile-dir>/`.

use crate::runtime::cancelled;
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

/// The caller's warm slot (`CARGO_TARGET_DIR` under the aeon warm directory): any other value
/// is ignored, so an inherited override can never redirect a build elsewhere.
pub fn warm_target(var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let p = PathBuf::from(var?);
    let ours = p.is_absolute()
        && p.is_dir()
        && p.components().any(|c| c.as_os_str() == spira_config::build::AEON_WARM_DIR);
    ours.then_some(p)
}

/// Make `<worktree>/target/<profile dir>` resolve to the warm slot's, so every reader of
/// [`artifacts_dir`] finds what cargo wrote there.
pub fn link_artifacts(worktree: &Path, profile: &str, warm: &Path) -> std::io::Result<()> {
    let target = worktree.join("target");
    std::fs::create_dir_all(&target)?;
    let link = target.join(profile_dir(profile));
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&link)?,
        Ok(_) => std::fs::remove_dir_all(&link)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(warm.join(profile_dir(profile)), link)
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
    /// cargo was killed because the run caught TERM/INT/HUP mid-build (sp-tcarr): rc 2, a
    /// signal, never the candidate's rc 4 — nothing about the branch was judged.
    Cancelled,
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
        // THE SHARED STORE (sp-xtdqi): resolved in-process, same as `wrapper_from_env` —
        // never the ambient `~/.cargo/config.toml` dependency this bead retires.
        let store = spira_config::build::Store::from_env();
        let stdout_to_stderr = {
            use std::os::fd::AsFd;
            std::io::stderr()
                .as_fd()
                .try_clone_to_owned()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null())
        };
        let warm = warm_target(std::env::var_os("CARGO_TARGET_DIR"));
        let mut cargo_cmd = Command::new("cargo");
        cargo_cmd
            .args(["build", "--profile", profile, "--workspace"])
            // One-shot: no incremental cache (a switch, not CARGO_INCREMENTAL, which would
            // split the compilation cache's keys).
            .args(spira_config::build::one_shot(profile))
            .envs(wrapper.env(store.as_ref()))
            .current_dir(worktree)
            // cargo's default target dir, or the session's warm slot (linked below): both are
            // stable paths, and <worktree>/target/<p> is the one place SPIRA_ARTIFACTS may point.
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
        if let Some(w) = &warm {
            cargo_cmd.env("CARGO_TARGET_DIR", w);
        }
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
            // TERM/INT/HUP mid-build (sp-tcarr): this loop otherwise only watches the
            // deadline, so a signalled run sat here until cargo finished on its own —
            // minutes, not the "stop launching, kill what is running" the signal asked for.
            if cancelled() {
                // SAFETY: signalling the process group of a child we spawned.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
                let _ = child.wait();
                return Err(BuildError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if !status.success() {
            return Err(BuildError::Failed(status.code().unwrap_or(1)));
        }
        if let Some(w) = &warm {
            if link_artifacts(worktree, profile, w).is_err() {
                return Err(BuildError::NoArtifacts(artifacts_dir(worktree, profile)));
            }
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

    /// A real `cargo build` (no fakes), real signal-flag flip mid-build, real clock: proves
    /// the poll loop in `Cargo::build` actually watches `cancelled()`, not just `--deadline`
    /// (sp-tcarr). Without that check, this either returns `Ok` after the full 10s sleep
    /// (never `Cancelled`) or simply takes ~10s instead of under 5 — either is the
    /// regression this guards: a signalled build sitting until cargo finishes on its own.
    #[test]
    fn a_caught_signal_mid_build_is_cancelled_promptly_not_after_the_full_sleep() {
        use crate::runtime::CANCEL;
        use std::sync::atomic::Ordering;

        let d = testkit::TempDir::new("build-signal-test");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname = \"slowbuild\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("build.rs"),
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(10)); }\n",
        )
        .unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() { println!(\"ok\"); }\n").unwrap();

        crate::runtime::OBSERVE_CANCEL.with(|o| o.set(true));
        CANCEL.store(false, Ordering::SeqCst);
        let flipper = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            CANCEL.store(true, Ordering::SeqCst);
        });

        let t0 = Instant::now();
        let result = Cargo.build(&d, "dev", None);
        let elapsed = t0.elapsed();
        flipper.join().unwrap();
        CANCEL.store(false, Ordering::SeqCst); // never leak into another test

        assert_eq!(result, Err(BuildError::Cancelled), "{result:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "cancellation should be prompt; took {elapsed:?}"
        );
    }

    #[test]
    fn only_a_warm_slot_is_honoured_as_the_target() {
        let d = testkit::TempDir::new("warm-target");
        let slot = d.join("aeon-warm").join("slot.1");
        std::fs::create_dir_all(&slot).unwrap();
        let other = d.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(warm_target(Some(slot.clone().into())), Some(slot.clone()));
        assert_eq!(warm_target(Some(other.into())), None);
        assert_eq!(warm_target(Some("aeon-warm/slot.9".into())), None);
        assert_eq!(warm_target(None), None);
    }

    #[test]
    fn artifacts_resolve_into_the_warm_slot_replacing_a_stale_dir() {
        let d = testkit::TempDir::new("warm-link");
        let slot = d.join("aeon-warm").join("slot.1");
        std::fs::create_dir_all(slot.join("aeon")).unwrap();
        std::fs::write(slot.join("aeon").join("tool"), "x").unwrap();
        let wt = d.join("wt");
        std::fs::create_dir_all(wt.join("target").join("aeon")).unwrap();
        std::fs::write(wt.join("target").join("aeon").join("stale"), "x").unwrap();
        link_artifacts(&wt, "aeon", &slot).unwrap();
        link_artifacts(&wt, "aeon", &slot).unwrap();
        assert!(artifacts_dir(&wt, "aeon").join("tool").is_file());
        assert!(!artifacts_dir(&wt, "aeon").join("stale").exists());
    }

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
