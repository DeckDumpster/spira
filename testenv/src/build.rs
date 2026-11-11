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

/// The calling session's owned build directory (`CARGO_TARGET_DIR` under `dwarf_build_base`,
/// sp-ke0uk8): any other value is ignored, so an inherited override can never redirect a build
/// elsewhere.
pub fn warm_target(var: Option<std::ffi::OsString>, base: Option<&Path>) -> Option<PathBuf> {
    spira_config::build_dir::owned(var, base)
}

/// Make `<worktree>/target/<profile dir>` resolve to the warm slot's, so every reader of
/// [`artifacts_dir`] finds what cargo wrote there.
pub fn link_artifacts(worktree: &Path, profile: &str, warm: &Path) -> std::io::Result<()> {
    let target = worktree.join("target");
    std::fs::create_dir_all(&target)?;
    let link = target.join(profile_dir(profile));
    let dest = warm.join(profile_dir(profile));
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&dest), std::fs::canonicalize(&link)) {
        if a == b {
            return Ok(());
        }
    }
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&link)?,
        Ok(_) => std::fs::remove_dir_all(&link)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(warm.join(profile_dir(profile)), link)
}

/// Remove a symlink at `<worktree>/target/<profile dir>` whose target is gone: cargo cannot
/// create a directory where a dangling link stands.
pub fn clear_dangling_artifacts(worktree: &Path, profile: &str) -> std::io::Result<()> {
    let link = artifacts_dir(worktree, profile);
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() && !link.exists() => std::fs::remove_file(&link),
        _ => Ok(()),
    }
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
    /// The build ran out of space on the filesystem holding `path` (its target or temp dir):
    /// rc 3, the box's disk, never the candidate's rc 4.
    NoSpace(PathBuf),
    /// The build was SIGKILLed with a cgroup OOM event: rc 3, the box's memory.
    Oom,
}

const STDERR_TAIL: usize = 64 * 1024;

pub fn is_enospc(stderr_tail: &str) -> bool {
    stderr_tail.contains("No space left on device") || stderr_tail.contains("os error 28")
}

/// `oom_kill` from this process's cgroup `memory.events`.
pub fn cgroup_oom_kills() -> Option<u64> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = cg.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let ev = std::fs::read_to_string(format!(
        "/sys/fs/cgroup/{}/memory.events",
        rel.trim_start_matches('/')
    ))
    .ok()?;
    ev.lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .and_then(|v| v.trim().parse().ok())
}

pub fn is_oom(signal: Option<i32>, oom_before: Option<u64>, oom_after: Option<u64>) -> bool {
    signal == Some(libc::SIGKILL)
        && matches!((oom_before, oom_after), (Some(b), Some(a)) if a > b)
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
        let warm = warm_target(
            std::env::var_os("CARGO_TARGET_DIR"),
            spira_config::process::cfg(spira_config::build_dir::BASE_ENV)
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .as_deref(),
        );
        build_in(worktree, profile, deadline, warm)
    }
}

/// The workspace build, without the cache wrapper's environment. The warm slot goes by
/// `--target-dir`: sccache keys on every `CARGO_*` variable, so a `CARGO_TARGET_DIR` that differs
/// per slot gives each slot a cache of its own.
fn cargo_build_command(worktree: &Path, profile: &str, warm: Option<&Path>) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--profile", profile, "--workspace", "--locked"])
        // One-shot: no incremental cache (a switch, not CARGO_INCREMENTAL, which would
        // split the compilation cache's keys).
        .args(spira_config::build::one_shot(profile))
        .current_dir(worktree)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_INCREMENTAL")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("SPIRA_ARTIFACTS")
        // Its own process group, so a kill at the cutoff takes rustc with it.
        .process_group(0);
    if let Some(w) = warm {
        cmd.arg("--target-dir").arg(w);
    }
    cmd
}

fn build_in(
    worktree: &Path,
    profile: &str,
    deadline: Option<Instant>,
    warm: Option<PathBuf>,
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
    let _ = clear_dangling_artifacts(worktree, profile);
    if let Some(w) = &warm {
        let _ = std::fs::create_dir_all(w.join(profile_dir(profile)));
    }
    let mut cargo_cmd = cargo_build_command(worktree, profile, warm.as_deref());
    cargo_cmd
        .envs(wrapper.env(store.as_ref()))
        .stdin(Stdio::null())
        .stdout(stdout_to_stderr);
    // rustc's RUSTC_WRAPPER=sccache auto-starts sccache's own server on first use, a
    // long-lived daemon that outlives this build; it must never inherit a caller's lock
    // fd this process did not open itself (sp-ohwg7).
    crate::util::close_inherited_fds(&mut cargo_cmd);
    cargo_cmd.stderr(Stdio::piped());
    let oom_before = cgroup_oom_kills();
    let child = cargo_cmd.spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return Err(BuildError::NoCargo),
    };
    let tail = child.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut tail = String::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = e.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = std::io::stderr().write_all(&buf[..n]);
                tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                if tail.len() > STDERR_TAIL {
                    let mut cut = tail.len() - STDERR_TAIL;
                    while !tail.is_char_boundary(cut) {
                        cut += 1;
                    }
                    tail.drain(..cut);
                }
            }
            tail
        })
    });
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
    let tail = tail.and_then(|t| t.join().ok()).unwrap_or_default();
    if !status.success() {
        use std::os::unix::process::ExitStatusExt;
        if is_enospc(&tail) {
            let at = warm.clone().unwrap_or_else(|| worktree.join("target"));
            return Err(BuildError::NoSpace(at));
        }
        if is_oom(status.signal(), oom_before, cgroup_oom_kills())
            || (tail.contains("signal: 9, SIGKILL") && is_oom(Some(libc::SIGKILL), oom_before, cgroup_oom_kills()))
        {
            return Err(BuildError::Oom);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `cargo build` (no fakes), real signal-flag flip mid-build, real clock: proves
    /// the poll loop in `Cargo::build` actually watches `cancelled()`, not just `--deadline`
    /// (sp-tcarr). Without that check, this either returns `Ok` after the full 10s sleep
    /// (never `Cancelled`) or simply takes ~10s instead of under 5 — either is the
    /// regression this guards: a signalled build sitting until cargo finishes on its own.

    #[test]
    fn enospc_is_read_from_cargos_stderr_and_a_plain_error_is_not() {
        assert!(is_enospc("failed to write /x/.fingerprint/a: No space left on device (os error 28)"));
        assert!(is_enospc("io error (os error 28)"));
        assert!(!is_enospc("error[E0308]: mismatched types"));
    }

    #[test]
    fn oom_needs_both_a_sigkill_and_a_cgroup_oom_event() {
        assert!(is_oom(Some(9), Some(1), Some(2)));
        assert!(!is_oom(Some(9), Some(2), Some(2)));
        assert!(!is_oom(Some(15), Some(1), Some(2)));
        assert!(!is_oom(Some(9), None, Some(2)));
    }

    #[test]
    fn a_warm_slot_is_passed_as_target_dir_never_as_an_environment_variable() {
        let args = |c: &Command| c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let cmd = cargo_build_command(Path::new("/w"), "dev", Some(Path::new("/warm/slot.3")));
        assert!(args(&cmd).windows(2).any(|w| w == ["--target-dir", "/warm/slot.3"]), "{:?}", args(&cmd));
        assert!(cmd.get_envs().all(|(k, v)| k != "CARGO_TARGET_DIR" || v.is_none()), "an env CARGO_TARGET_DIR splits the cache per slot");
        let cold = cargo_build_command(Path::new("/w"), "dev", None);
        assert!(!args(&cold).iter().any(|a| a == "--target-dir"));
    }

    #[test]
    fn only_a_warm_slot_is_honoured_as_the_target() {
        let d = testkit::TempDir::new("warm-target");
        let base = d.join("warm-base");
        let slot = base.join("slot.1");
        std::fs::create_dir_all(&slot).unwrap();
        let other = d.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(warm_target(Some(slot.clone().into()), Some(&base)), Some(slot.clone()));
        assert_eq!(warm_target(Some(other.into()), Some(&base)), None);
        assert_eq!(warm_target(Some("warm-base/slot.9".into()), Some(&base)), None);
        assert_eq!(warm_target(Some(slot.clone().into()), None), None);
        assert_eq!(warm_target(None, Some(&base)), None);
    }

    #[test]
    fn artifacts_resolve_into_the_warm_slot_replacing_a_stale_dir() {
        let d = testkit::TempDir::new("warm-link");
        let base = d.join("warm-base");
        let slot = base.join("slot.1");
        std::fs::create_dir_all(slot.join("dwarf")).unwrap();
        std::fs::write(slot.join("dwarf").join("tool"), "x").unwrap();
        let wt = d.join("wt");
        std::fs::create_dir_all(wt.join("target").join("dwarf")).unwrap();
        std::fs::write(wt.join("target").join("dwarf").join("stale"), "x").unwrap();
        link_artifacts(&wt, "dwarf", &slot).unwrap();
        link_artifacts(&wt, "dwarf", &slot).unwrap();
        assert!(artifacts_dir(&wt, "dwarf").join("tool").is_file());
        assert!(!artifacts_dir(&wt, "dwarf").join("stale").exists());
    }

    #[test]
    fn artifacts_stay_when_the_warm_slot_is_spilled_onto_the_worktrees_own_target() {
        let d = testkit::TempDir::new("warm-spilled");
        let base = d.join("warm-base");
        let wt = d.join("wt");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(wt.join("target").join("dwarf")).unwrap();
        std::fs::write(wt.join("target").join("dwarf").join("tool"), "x").unwrap();
        let slot = base.join("slot.1");
        std::os::unix::fs::symlink(wt.join("target"), &slot).unwrap();
        assert_eq!(warm_target(Some(slot.clone().into()), Some(&base)), Some(slot.clone()));
        link_artifacts(&wt, "dwarf", &slot).unwrap();
        assert!(artifacts_dir(&wt, "dwarf").join("tool").is_file());
    }

    #[test]
    fn a_dangling_artifacts_link_is_cleared_so_the_build_can_recreate_it() {
        let d = testkit::TempDir::new("dangling-link");
        let wt = d.join("wt");
        std::fs::create_dir_all(wt.join("target")).unwrap();
        let gone = d.join("slot.1").join("dwarf");
        std::os::unix::fs::symlink(&gone, wt.join("target").join("dwarf")).unwrap();
        assert!(std::fs::create_dir_all(wt.join("target").join("dwarf")).is_err());
        clear_dangling_artifacts(&wt, "dwarf").unwrap();
        std::fs::create_dir_all(wt.join("target").join("dwarf")).unwrap();
        std::fs::write(wt.join("target/dwarf/keep"), "x").unwrap();
        clear_dangling_artifacts(&wt, "dwarf").unwrap();
        assert!(wt.join("target/dwarf/keep").is_file());
    }

    #[test]
    fn profile_directories() {
        assert_eq!(profile_dir("dwarf"), "dwarf");
        assert_eq!(profile_dir("release"), "release");
        assert_eq!(profile_dir("dev"), "debug");
        assert_eq!(profile_dir("test"), "debug");
        assert_eq!(
            artifacts_dir(Path::new("/wt"), "dwarf"),
            PathBuf::from("/wt/target/dwarf")
        );
    }
}
