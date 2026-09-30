//! What `release acceptance` does to the world, behind a trait: run a program (captured or
//! shown), look a program up on `PATH`, read the clock, sleep. The real host does these for
//! real; the unit tests script a fake one with a fake clock (DESIGN.md "acceptance").

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One program invocation: `env K=V ... prog args...` in `cwd`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmd {
    pub prog: String,
    pub args: Vec<String>,
    /// Added to (and overriding) the inherited environment, like `env K=V prog`.
    pub env: Vec<(String, String)>,
}

impl Cmd {
    pub fn new(prog: impl Into<String>) -> Cmd {
        Cmd { prog: prog.into(), ..Cmd::default() }
    }
    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        self.args.push(a.into());
        self
    }
    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, a: I) -> Cmd {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Cmd {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn envs(mut self, e: &[(String, String)]) -> Cmd {
        self.env.extend(e.iter().cloned());
        self
    }
    /// `prog arg arg ...`, for fakes to match on and for messages.
    pub fn line(&self) -> String {
        std::iter::once(self.prog.as_str()).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }
    /// The value this command sets for `k`, if it sets one.
    pub fn env_of(&self, k: &str) -> Option<&str> {
        self.env.iter().rev().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str())
    }
}

/// A finished captured run: exit code (127 when it could not start, 128+n on signal n), its
/// stdout followed by its stderr (`text`, the script's `2>&1`), and its stdout alone (`out`,
/// the script's `2>/dev/null` — what a reader parses, so a warning on stderr can never
/// corrupt a JSON reply or a sha).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Out {
    pub rc: i32,
    pub text: String,
    pub out: String,
}

impl Out {
    /// A run whose stdout is `s` and whose stderr is empty.
    pub fn stdout(rc: i32, s: &str) -> Out {
        Out { rc, text: s.into(), out: s.into() }
    }
}

pub trait Host {
    /// Run `c`, capturing its output.
    fn run(&self, c: &Cmd) -> Out;
    /// Run `c` with its output streamed to ours (the script's `2>&1 | tee`); the exit code.
    fn show(&self, c: &Cmd) -> i32;
    /// `command -v prog` against this process's `PATH`.
    fn on_path(&self, prog: &str) -> bool;
    fn now(&self) -> u64;
    fn sleep(&self, secs: u64);
}

pub struct RealHost;

fn command(c: &Cmd) -> Command {
    let mut cmd = Command::new(&c.prog);
    cmd.args(&c.args);
    for (k, v) in &c.env {
        cmd.env(k, v);
    }
    cmd
}

fn code(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

impl Host for RealHost {
    fn run(&self, c: &Cmd) -> Out {
        match command(c).stdin(Stdio::null()).output() {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).into_owned();
                let text = format!("{out}{}", String::from_utf8_lossy(&o.stderr));
                Out { rc: code(o.status), text, out }
            }
            Err(e) => Out { rc: 127, text: format!("{}: {e}\n", c.prog), out: String::new() },
        }
    }

    fn show(&self, c: &Cmd) -> i32 {
        let _ = std::io::stdout().flush();
        match command(c).stdin(Stdio::null()).status() {
            Ok(s) => code(s),
            Err(e) => {
                println!("{}: {e}", c.prog);
                127
            }
        }
    }

    fn on_path(&self, prog: &str) -> bool {
        which(prog, &std::env::var("PATH").unwrap_or_default()).is_some()
    }

    fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

/// The first executable `prog` on `path` (a `:`-separated list).
pub fn which(prog: &str, path: &str) -> Option<PathBuf> {
    if prog.is_empty() || prog.contains('/') {
        return None;
    }
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(prog)).find(|p| crate::fsutil::is_executable(p))
}
