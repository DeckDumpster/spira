//! The container runtime seam. Everything the runner does to a container goes through
//! [`ContainerRuntime`]; [`Podman`] is the real one, and the orchestration is unit-tested
//! against a fake (fixture.rs, run.rs tests). DESIGN.md §4.2, decision D2.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Set by SIGINT/SIGTERM/SIGHUP: stop launching, kill what is running, tear down, exit.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

extern "C" fn on_signal(_: libc::c_int) {
    CANCEL.store(true, Ordering::SeqCst);
}

/// TERM/INT/HUP (SIGKILL cannot be caught — `testenv wait` exists for that case, sp-tcarr).
pub fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(s, on_signal as usize);
        }
    }
}

/// rc for a run killed by the per-suite timeout (coreutils `timeout`'s own).
pub const RC_TIMEOUT: i32 = 124;
/// rc for a run killed because the batch was cancelled.
pub const RC_CANCELLED: i32 = 130;
/// rc for a run killed because the batch's `--deadline` passed (DESIGN.md D7). Outside
/// 0..=255, so no process can exit with it.
pub const RC_DEADLINE: i32 = -124;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub container: String,
    pub user: Option<String>,
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    pub timeout: Option<Duration>,
    /// The batch's hard deadline (`--deadline`): past it the exec is killed like a timeout,
    /// but reports [`RC_DEADLINE`]. None = no deadline.
    pub deadline: Option<Instant>,
    /// Where the combined stdout+stderr goes; the outcome carries it too.
    pub output: Option<PathBuf>,
}

impl ExecRequest {
    pub fn new(container: &str, argv: &[&str]) -> Self {
        ExecRequest {
            container: container.into(),
            user: None,
            env: Vec::new(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            timeout: None,
            deadline: None,
            output: None,
        }
    }
    pub fn user(mut self, u: &str) -> Self {
        self.user = Some(u.into());
        self
    }
    pub fn env(mut self, env: &[(String, String)]) -> Self {
        self.env.extend(env.iter().cloned());
        self
    }
    pub fn env_value(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecOutcome {
    pub rc: i32,
    pub output: String,
}

impl ExecOutcome {
    pub fn ok(&self) -> bool {
        self.rc == 0
    }

    /// The last `n` lines of the captured output, so a fault carries its evidence.
    pub fn tail(&self, n: usize) -> String {
        let lines: Vec<&str> = self.output.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

pub trait ContainerRuntime: Send + Sync {
    /// `podman exec [--user U] [-e K=V ...] <container> argv...`, bounded by `timeout`.
    fn exec(&self, req: &ExecRequest) -> ExecOutcome;
    /// `podman container inspect --format <format> <name>`; None on failure or empty output.
    fn inspect(&self, name: &str, format: &str) -> Option<String>;
    fn exists(&self, name: &str) -> bool;
    /// Every container (running or not) whose name starts with `prefix`.
    fn names_with_prefix(&self, prefix: &str) -> Vec<String>;
    /// stop, rm, and remove both cargo volumes — the orphan sweep's teardown.
    fn purge(&self, name: &str);
    /// `<this executable> container <args>` (tag, up, probe, down; DESIGN.md §12, D17): stdout captured,
    /// stderr passed through. Past `deadline` it is killed (its whole process group) and
    /// reports [`RC_DEADLINE`] (DESIGN.md D9).
    fn testenv(&self, args: &[String], deadline: Option<Instant>) -> ExecOutcome;
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_output() -> PathBuf {
    std::env::temp_dir().join(format!(
        "testenv-exec-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::SeqCst)
    ))
}

/// Run `cmd` with stdout+stderr into `out` (created), bounded by `timeout` and `deadline`;
/// SIGTERM then, 10 s later, SIGKILL on expiry (rc 124), on the deadline (rc
/// [`RC_DEADLINE`]; the timeout wins a tie) or on cancellation (rc 130).
pub fn run_bounded(
    mut cmd: Command,
    out: &Path,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
) -> i32 {
    let file = match File::create(out) {
        Ok(f) => f,
        Err(_) => return 127,
    };
    let Ok(err) = file.try_clone() else {
        return 127;
    };
    cmd.stdin(Stdio::null()).stdout(file).stderr(err);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return 127,
    };
    let start = Instant::now();
    let mut killed: Option<(i32, Instant)> = None;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                if let Some((rc, _)) = killed {
                    return rc;
                }
                return st.code().unwrap_or_else(|| 128 + st_signal(&st));
            }
            Ok(None) => {}
            Err(_) => return 127,
        }
        match killed {
            None => {
                let expired = timeout.is_some_and(|t| start.elapsed() >= t);
                let cut = deadline.is_some_and(|d| Instant::now() >= d);
                if expired || cut || cancelled() {
                    // SAFETY: signalling our own child by pid.
                    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                    let rc = if expired {
                        RC_TIMEOUT
                    } else if cut {
                        RC_DEADLINE
                    } else {
                        RC_CANCELLED
                    };
                    killed = Some((rc, Instant::now()));
                }
            }
            Some((_, at)) if at.elapsed() >= Duration::from_secs(10) => {
                let _ = child.kill();
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn st_signal(st: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.signal().unwrap_or(0)
}

fn read_lossy(p: &Path) -> String {
    let mut buf = Vec::new();
    if let Ok(mut f) = File::open(p) {
        let _ = f.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// How much longer than podman's own CLI we wait for conmon's exit file before giving up
/// and calling the status genuinely lost (sp-3azqi): conmon has already written it by the
/// time podman's `exec` gives up under load, just not fast enough for podman's own,
/// shorter-fused wait — see [`recover_lost_exit`].
const EXEC_WAIT_RECOVERY_GRACE: Duration = Duration::from_secs(20);

/// podman's own `exec` gave up waiting for conmon to write the process's exit-status file
/// and exited 255 reporting so — a race under load, not a dead container: the file is
/// still being written and reads fine moments later (DESIGN.md: the fix removes the
/// cause, not just the symptom, by not trusting podman's own collection as the only
/// source). The error names the exact file podman was waiting on; this pulls it out of the
/// captured output so [`recover_lost_exit`] can keep watching it after podman's CLI quit.
fn exec_wait_timeout_file(output: &str) -> Option<PathBuf> {
    let marker = "Error: timed out waiting for file ";
    let line = output.lines().rev().find(|l| l.starts_with(marker))?;
    let path = line[marker.len()..].trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Poll `path` — conmon's own exit-status file, a plain decimal exit code with no
/// newline, the same file `exec_wait_timeout_file` named — for up to `grace`, returning the
/// real exit code the moment it appears and parses. `None` means it never showed up (or
/// never parsed) inside the grace period: the status really is lost, not merely slow.
fn recover_lost_exit(path: &Path, grace: Duration) -> Option<i32> {
    if grace.is_zero() {
        return None;
    }
    let deadline = Instant::now() + grace;
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(code) = text.trim().parse::<i32>() {
                return Some(code);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The real runtime: podman on PATH; the container driver is this executable's own
/// `container` subcommand, run as a child against `harness` (DESIGN.md §12, D17).
pub struct Podman {
    pub exe: PathBuf,
    pub harness: PathBuf,
}

impl Podman {
    fn podman_out(&self, args: &[&str]) -> Option<String> {
        let o = Command::new("podman")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
    fn podman_quiet(&self, args: &[&str]) -> bool {
        Command::new("podman")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

pub fn podman_exec_args(req: &ExecRequest) -> Vec<String> {
    let mut a = vec!["exec".to_string()];
    if let Some(u) = &req.user {
        a.push("--user".into());
        a.push(u.clone());
    }
    for (k, v) in &req.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    a.push(req.container.clone());
    a.extend(req.argv.iter().cloned());
    a
}

impl ContainerRuntime for Podman {
    fn exec(&self, req: &ExecRequest) -> ExecOutcome {
        let mut cmd = Command::new("podman");
        cmd.args(podman_exec_args(req));
        let (path, temp) = match &req.output {
            Some(p) => (p.clone(), false),
            None => (temp_output(), true),
        };
        let mut rc = run_bounded(cmd, &path, req.timeout, req.deadline);
        let output = read_lossy(&path);
        // sp-3azqi: podman's own exec lost the exit status — conmon's exit-file wait gave
        // up under load, rc 255, the file named in its own error. Give it a little more
        // patience than podman's CLI did (bounded by whatever's left of the batch's
        // deadline, never past it) before calling the status genuinely lost: this removes
        // the common case rather than merely relabeling it a fault downstream.
        if rc == 255 {
            if let Some(exit_file) = exec_wait_timeout_file(&output) {
                let grace = match req.deadline {
                    Some(d) => d.saturating_duration_since(Instant::now()).min(EXEC_WAIT_RECOVERY_GRACE),
                    None => EXEC_WAIT_RECOVERY_GRACE,
                };
                if let Some(real_rc) = recover_lost_exit(&exit_file, grace) {
                    rc = real_rc;
                }
            }
        }
        if temp {
            let _ = fs::remove_file(&path);
        }
        ExecOutcome { rc, output }
    }

    fn inspect(&self, name: &str, format: &str) -> Option<String> {
        let s = self.podman_out(&["container", "inspect", "--format", format, name])?;
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn exists(&self, name: &str) -> bool {
        self.podman_quiet(&["container", "exists", name])
    }

    fn names_with_prefix(&self, prefix: &str) -> Vec<String> {
        let filter = format!("name=^{prefix}");
        self.podman_out(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"])
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| l.starts_with(prefix))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn purge(&self, name: &str) {
        self.podman_quiet(&["stop", name]);
        self.podman_quiet(&["rm", name]);
        self.podman_quiet(&["volume", "rm", &format!("{name}-cargo-reg")]);
        self.podman_quiet(&["volume", "rm", &format!("{name}-cargo-git")]);
    }

    fn testenv(&self, args: &[String], deadline: Option<Instant>) -> ExecOutcome {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("container")
            .args(args)
            .env("SPIRA_TESTENV_HARNESS", &self.harness)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Never let an inherited artifacts dir reach the driver (DESIGN.md §5).
        cmd.env_remove("SPIRA_ARTIFACTS")
            .env_remove("SPIRA_ARTIFACTS_ROOT");
        capture_bounded(cmd, deadline)
    }
}

/// Run `cmd` (stdout piped by the caller) to completion or `deadline`, whichever is first;
/// at the deadline its process group is killed and the rc is [`RC_DEADLINE`].
pub fn capture_bounded(mut cmd: Command, deadline: Option<Instant>) -> ExecOutcome {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                rc: 127,
                output: e.to_string(),
            }
        }
    };
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let rc = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or_else(|| 128 + st_signal(&st)),
            Ok(None) => {}
            Err(_) => break 127,
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            // SAFETY: signalling the process group of a child we spawned.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            break RC_DEADLINE;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let buf = reader.and_then(|h| h.join().ok()).unwrap_or_default();
    ExecOutcome {
        rc,
        output: String::from_utf8_lossy(&buf).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `install_signal_handlers` must cover HUP, not just INT/TERM (sp-tcarr: testenv was
    /// missing it — gate's own real.rs already caught all three). `raise()` delivers to the
    /// calling thread synchronously, so the flag is visible the instant it returns.
    #[test]
    fn install_signal_handlers_catches_hup() {
        CANCEL.store(false, Ordering::SeqCst);
        install_signal_handlers();
        unsafe { libc::raise(libc::SIGHUP) };
        let caught = cancelled();
        CANCEL.store(false, Ordering::SeqCst); // never leak into another test
        assert!(caught, "SIGHUP should set the cancellation flag");
    }

    #[test]
    fn tail_keeps_only_the_last_lines_of_the_output() {
        let o = ExecOutcome { rc: 5, output: "a\nb\nc\nd".into() };
        assert_eq!(o.tail(2), "c\nd");
        assert_eq!(o.tail(10), "a\nb\nc\nd");
        assert_eq!(ExecOutcome { rc: 1, output: String::new() }.tail(3), "");
    }

    #[test]
    fn exec_args_put_user_and_env_before_the_container() {
        let req = ExecRequest::new("c1", &["bash", "/workspace/spira/test-a.sh"])
            .user("spirauser")
            .env(&[("A".into(), "1".into()), ("B".into(), "".into())]);
        assert_eq!(
            podman_exec_args(&req),
            vec![
                "exec",
                "--user",
                "spirauser",
                "-e",
                "A=1",
                "-e",
                "B=",
                "c1",
                "bash",
                "/workspace/spira/test-a.sh"
            ]
        );
        assert_eq!(req.env_value("B"), Some(""));
    }

    #[test]
    fn run_bounded_captures_both_streams_and_the_rc() {
        let out = temp_output();
        let mut c = Command::new("sh");
        c.args(["-c", "echo out; echo err >&2; exit 3"]);
        assert_eq!(run_bounded(c, &out, None, None), 3);
        let text = read_lossy(&out);
        assert!(text.contains("out") && text.contains("err"));
        let _ = fs::remove_file(out);
    }

    #[test]
    fn run_bounded_times_out_with_124() {
        let out = temp_output();
        let mut c = Command::new("sleep");
        c.arg("5");
        let t0 = Instant::now();
        assert_eq!(
            run_bounded(c, &out, Some(Duration::from_millis(200)), None),
            RC_TIMEOUT
        );
        assert!(t0.elapsed() < Duration::from_secs(4));
        let _ = fs::remove_file(out);
    }

    #[test]
    fn run_bounded_kills_at_the_deadline_with_its_own_rc() {
        let out = temp_output();
        let mut c = Command::new("sh");
        c.args(["-c", "echo started; sleep 5"]);
        let t0 = Instant::now();
        let d = Some(Instant::now() + Duration::from_millis(200));
        assert_eq!(
            run_bounded(c, &out, Some(Duration::from_secs(60)), d),
            RC_DEADLINE
        );
        assert!(t0.elapsed() < Duration::from_secs(4));
        assert!(
            read_lossy(&out).contains("started"),
            "partial output is kept"
        );
        let _ = fs::remove_file(out);
    }

    #[test]
    fn capture_bounded_returns_stdout_and_kills_the_group_at_the_deadline() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo hi; exit 4"]).stdout(Stdio::piped());
        let o = capture_bounded(c, None);
        assert_eq!((o.rc, o.output.as_str()), (4, "hi\n"));
        let mut c = Command::new("sh");
        // a grandchild holding stdout open: only a group kill ends the read
        c.args(["-c", "echo started; sleep 30 & sleep 30"]).stdout(Stdio::piped());
        let t0 = Instant::now();
        let o = capture_bounded(c, Some(Instant::now() + Duration::from_millis(300)));
        assert_eq!(o.rc, RC_DEADLINE);
        assert!(o.output.contains("started"));
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_per_suite_timeout_wins_a_tie_with_the_deadline() {
        let out = temp_output();
        let mut c = Command::new("sleep");
        c.arg("5");
        let past = Some(Instant::now());
        assert_eq!(run_bounded(c, &out, Some(Duration::ZERO), past), RC_TIMEOUT);
        let _ = fs::remove_file(out);
    }

    // ---- sp-3azqi: recovering a podman exec-wait-timeout's lost exit status ----------

    #[test]
    fn exec_wait_timeout_file_names_the_exact_path_podman_gave_up_on() {
        let out = "TAP version 14\nok 1 - a\nError: timed out waiting for file /var/lib/containers/storage/overlay-containers/deadbeef/userdata/abc/exit/deadbeef\n";
        assert_eq!(
            exec_wait_timeout_file(out),
            Some(PathBuf::from(
                "/var/lib/containers/storage/overlay-containers/deadbeef/userdata/abc/exit/deadbeef"
            ))
        );
        assert_eq!(exec_wait_timeout_file("ok 1 - a\nFAIL something\n"), None);
    }

    #[test]
    fn recover_lost_exit_reads_the_file_once_it_appears_within_grace() {
        let dir = testkit::TempDir::new("testenv-runtime-recover");
        let exit_file = dir.join("exit-code");
        let f2 = exit_file.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            fs::write(&f2, "0").unwrap();
        });
        let t0 = Instant::now();
        assert_eq!(
            recover_lost_exit(&exit_file, Duration::from_secs(5)),
            Some(0)
        );
        assert!(t0.elapsed() < Duration::from_secs(2), "recovers as soon as the file lands");
        writer.join().unwrap();
    }

    #[test]
    fn recover_lost_exit_gives_up_when_the_file_never_appears() {
        let dir = testkit::TempDir::new("testenv-runtime-recover-none");
        let never = dir.join("never-written");
        let t0 = Instant::now();
        assert_eq!(recover_lost_exit(&never, Duration::from_millis(200)), None);
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn recover_lost_exit_is_a_no_op_at_zero_grace() {
        let dir = testkit::TempDir::new("testenv-runtime-recover-zero");
        let exit_file = dir.join("exit-code");
        fs::write(&exit_file, "0").unwrap();
        // already written, but a zero grace (deadline already passed) never even looks
        assert_eq!(recover_lost_exit(&exit_file, Duration::ZERO), None);
    }

    #[test]
    fn recover_lost_exit_ignores_unparseable_content_until_it_becomes_a_number() {
        let dir = testkit::TempDir::new("testenv-runtime-recover-partial");
        let exit_file = dir.join("exit-code");
        fs::write(&exit_file, "").unwrap(); // conmon creates it before writing the code
        let f2 = exit_file.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            fs::write(&f2, "3\n").unwrap();
        });
        assert_eq!(
            recover_lost_exit(&exit_file, Duration::from_secs(5)),
            Some(3)
        );
        writer.join().unwrap();
    }
}
