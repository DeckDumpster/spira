//! The container runtime seam. Everything the runner does to a container goes through
//! [`ContainerRuntime`]; [`Podman`] is the real one, and the orchestration is unit-tested
//! against a fake (fixture.rs, run.rs tests). DESIGN.md §4.2, decision D2.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Set by SIGINT/SIGTERM: stop launching, kill what is running, tear down, exit.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

extern "C" fn on_signal(_: libc::c_int) {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as usize);
        libc::signal(libc::SIGTERM, on_signal as usize);
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
        let rc = run_bounded(cmd, &path, req.timeout, req.deadline);
        let output = read_lossy(&path);
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
}
