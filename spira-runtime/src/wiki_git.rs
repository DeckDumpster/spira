//! The processes the runtime launches, each under a deadline: git on the wiki checkout (not store
//! state, so spira-lc has no verb over it), tmux on a view flip, `overrides.sh apply` on the
//! overrides tick, and the bead-stage exporter on a reconciler-flow pass. Effects run on the offload
//! pool, never on the event loop.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT_DEADLINE: Duration = Duration::from_secs(4);

/// Runs git in `dir`; the exit code and stdout, or an error when git could not start or outlived
/// `GIT_DEADLINE`. Stdout is drained while git runs so a large listing cannot fill the pipe and
/// stall it into the deadline.
pub fn git(dir: &Path, args: &[&str]) -> Result<(Option<i32>, Vec<u8>), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    bounded(cmd, GIT_DEADLINE).map(|(code, out, _)| (code, out))
}

/// Runs git in `dir` and returns stdout on exit 0; otherwise the cause in git's own words: the
/// exit code with its stderr, death by signal with its stderr, or the spawn or timeout error.
pub fn git_ok(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    git_ok_with(Path::new("git"), dir, args)
}

/// [`git_ok`] through the git program `program`.
pub fn git_ok_with(program: &Path, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(program);
    cmd.arg("-C").arg(dir).args(args);
    ok_or_cause(cmd, GIT_DEADLINE)
}

fn ok_or_cause(cmd: Command, deadline: Duration) -> Result<Vec<u8>, String> {
    match bounded(cmd, deadline)? {
        (Some(0), out, _) => Ok(out),
        (Some(code), _, err) => Err(format!("exit {code}: {}", String::from_utf8_lossy(&err).trim())),
        (None, _, err) => Err(format!("killed by signal: {}", String::from_utf8_lossy(&err).trim())),
    }
}

/// Runs tmux under `deadline` and returns its stdout. `TMUX` is cleared: it names whatever server
/// the service was started from, not the cockpit's own.
pub fn tmux(program: &Path, args: &[&str], deadline: Duration) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).env_remove("TMUX");
    match bounded(cmd, deadline)? {
        (Some(0), out, _) => Ok(String::from_utf8_lossy(&out).into_owned()),
        (code, _, _) => Err(format!("tmux {} exited {code:?}", args.first().unwrap_or(&""))),
    }
}

/// `gh auth token` under `config_dir` (when set); the exit code and stdout.
pub fn gh_auth_token(gh: &str, config_dir: &str, deadline: Duration) -> Result<(Option<i32>, Vec<u8>), String> {
    let mut cmd = Command::new(gh);
    cmd.args(["auth", "token"]);
    if !config_dir.is_empty() {
        cmd.env("GH_CONFIG_DIR", config_dir);
    }
    bounded(cmd, deadline).map(|(code, out, _)| (code, out))
}

/// One `overrides.sh apply` pass under `timeout`; a failed override fails the pass, and its
/// stdout and stderr together say which. With none declared in `dir` nothing runs.
pub fn overrides_apply(dir: &Path) -> Result<String, String> {
    if !spira_config::overrides::declared(dir)? {
        return Ok(String::new());
    }
    // FORK-EDGE: overrides.sh is 223 lines of shell (over the ~100-line port bound).
    let out = spira_config::bounded::bounded("overrides.sh").arg("apply").stdin(Stdio::null()).output().map_err(|e| format!("overrides.sh: {e}"))?;
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(said.trim().to_string()) } else { Err(said.trim().to_string()) }
}

/// One `<exporter> <mode>` run under `timeout` (`lifecycle` brings the `bead-stage` family up to the
/// lifecycle log, `check` fails when it trails the log); a failed run names its exit code and what it said on stderr.
pub fn exporter_run(bin: &str, mode: &str) -> Result<(), String> {
    // FORK-EDGE: sibling binary tsd-lifecycle-export (the bead-stage exporter).
    let out = spira_config::bounded::bounded(bin).arg(mode).stdin(Stdio::null()).output().map_err(|e| format!("{bin}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{bin} {mode}: exit {}: {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
}

/// Exit code (none when killed by a signal), stdout, stderr.
type Ran = (Option<i32>, Vec<u8>, Vec<u8>);

fn bounded(mut cmd: Command, deadline: Duration) -> Result<Ran, String> {
    use std::io::Read;
    fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = vec![];
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    }
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("spawn: {e}"))?;
    let out = drain(child.stdout.take().ok_or("no stdout pipe")?);
    let err = drain(child.stderr.take().ok_or("no stderr pipe")?);
    let started = Instant::now();
    let code = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status.code(),
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} ms (deadline {deadline:?})", started.elapsed().as_millis()));
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    Ok((code, out.join().map_err(|_| "stdout reader panicked")?, err.join().map_err(|_| "stderr reader panicked")?))
}

#[cfg(test)]
mod exporter_tests {
    use super::exporter_run;

    fn exporter(dir: &std::path::Path, body: &str) -> String {
        let p = dir.join("fake-export");
        testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
        p.display().to_string()
    }

    #[test_id(tqq2s2q)]
    #[test]
    fn the_exporter_is_run_in_the_mode_asked_and_its_refusal_is_carried() {
        let d = testkit::TempDir::new("exporter-run");
        let bin = exporter(d.path(), "[ \"$1\" = check ] && { echo 'trails the log' >&2; exit 1; }; exit 0");
        assert!(exporter_run(&bin, "lifecycle").is_ok());
        let e = exporter_run(&bin, "check").unwrap_err();
        assert!(e.contains("check: exit 1") && e.contains("trails the log"), "{e}");
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::bounded;
    use std::process::Command;
    use std::time::Duration;

    fn script(dir: &std::path::Path, body: &str) -> Command {
        let p = dir.join("fake");
        testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
        Command::new(p)
    }

    #[test_id(teq48wa)]
    #[test]
    fn a_process_past_its_deadline_is_reported_with_the_elapsed_milliseconds() {
        let d = testkit::TempDir::new("bounded-timeout");
        let e = bounded(script(d.path(), "sleep 1"), Duration::from_millis(50)).unwrap_err();
        assert!(e.starts_with("timed out after ") && e.contains(" ms (deadline 50ms)"), "{e}");
        assert!(e.trim_start_matches("timed out after ").chars().next().unwrap().is_ascii_digit(), "{e}");
    }

    #[test_id(tv7i2y3)]
    #[test]
    fn a_program_that_cannot_start_is_reported_as_a_spawn_error() {
        let d = testkit::TempDir::new("bounded-spawn");
        let e = bounded(Command::new(d.path().join("spira-no-such-program")), Duration::from_secs(1)).unwrap_err();
        assert!(e.starts_with("spawn:"), "{e}");
    }

    #[test_id(ttos16a)]
    #[test]
    fn a_chatty_stderr_does_not_stall_the_process_and_is_returned() {
        let d = testkit::TempDir::new("bounded-stderr");
        let (code, _, err) = bounded(script(d.path(), "yes x | head -c 204800 >&2; exit 3"), Duration::from_secs(5)).unwrap();
        assert_eq!(code, Some(3));
        assert_eq!(err.len(), 204800);
    }

    #[test_id(tvh3zn3)]
    #[test]
    fn git_ok_carries_gits_stderr_in_its_exit_message() {
        let d = testkit::TempDir::new("git-ok-exit");
        testkit::git(d.path(), &["init", "-q"]);
        let e = super::super::git_ok(d.path(), &["log", "no-such-ref"]).unwrap_err();
        assert!(e.starts_with("exit 128: ") && e.contains("unknown revision"), "{e}");
    }

    #[test_id(t4qkwt8)]
    #[test]
    fn a_process_killed_by_a_signal_is_named_with_its_stderr_by_git_ok_s_cause_by_a_signal() {
        let d = testkit::TempDir::new("git-ok-signal");
        let e = super::super::ok_or_cause(script(d.path(), "echo cut short >&2; kill -9 $$"), Duration::from_secs(5)).unwrap_err();
        assert_eq!(e, "killed by signal: cut short");
    }
}
