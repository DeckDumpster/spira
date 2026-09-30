//! Clock formatting, the log line, and one bounded process runner — the same small,
//! per-crate utility every Rust port in this workspace carries (aeon/src/util.rs,
//! sentinel/src/host.rs, …): duplicated on purpose rather than shared, so no crate here
//! depends on another's internals for something this small.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The result of one subprocess: its exit code (-1 when it could not start, 124 when it
/// timed out), stdout, stderr.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn ok(stdout: impl Into<String>) -> Out {
        Out { code: 0, stdout: stdout.into(), stderr: String::new() }
    }
    pub fn fail(code: i32, stderr: impl Into<String>) -> Out {
        Out { code, stdout: String::new(), stderr: stderr.into() }
    }
    pub fn success(&self) -> bool {
        self.code == 0
    }
    /// stdout with one trailing newline run removed — what `$(...)` would hand a caller.
    pub fn text(&self) -> String {
        self.stdout.trim_end_matches('\n').to_string()
    }
    pub fn first_err_line(&self) -> String {
        self.stderr.lines().next().unwrap_or("").to_string()
    }
}

/// Everything needed to start one process. `env` REPLACES the environment; `stdin` is
/// written by a thread so a large payload cannot deadlock.
pub struct Spec<'a> {
    pub prog: &'a str,
    pub args: Vec<String>,
    pub env: Option<&'a BTreeMap<String, String>>,
    pub cwd: Option<&'a Path>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
}

pub fn run(spec: Spec) -> Out {
    let mut cmd = Command::new(spec.prog);
    cmd.args(&spec.args);
    if let Some(env) = spec.env {
        cmd.env_clear();
        cmd.envs(env.iter());
    }
    if let Some(d) = spec.cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Out::fail(127, format!("{}: cannot start: {e}", spec.prog)),
    };
    if let (Some(data), Some(mut sin)) = (spec.stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = sin.write_all(&data);
        });
    }
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break exit_code(s),
            Ok(None) => {
                if spec.timeout.is_some_and(|t| start.elapsed() >= t) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break 124;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Out::fail(-1, format!("wait: {e}")),
        }
    };
    Out {
        code,
        stdout: String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned(),
    }
}

/// A shell's reading of an exit status: the code, or 128+signal.
pub fn exit_code(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

// ---- time ----------------------------------------------------------------------------

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an epoch — bash's `log()`.
pub fn iso_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Howard Hinnant's days-to-civil (same algorithm as aeon/src/util.rs).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `TZ=<tz> date -d @<epoch> +'%Y-%m-%d %H:%M %Z'`, or `@<epoch>` on failure — auron.sh's
/// `iso()`. Shelled to `date` rather than reimplemented: `SPIRA_TZ` names an IANA zone, and
/// only the system's own tzdata can resolve one faithfully.
pub fn iso_display(epoch: i64, tz: &str) -> String {
    let mut env = BTreeMap::new();
    env.insert("TZ".to_string(), tz.to_string());
    let o = run(Spec { prog: "date", args: vec!["-d".into(), format!("@{epoch}"), "+%Y-%m-%d %H:%M %Z".into()], env: Some(&env), cwd: None, stdin: None, timeout: Some(Duration::from_secs(5)) });
    if o.success() && !o.stdout.trim().is_empty() {
        o.text()
    } else {
        format!("@{epoch}")
    }
}

// ---- the log line, and sinks for it ---------------------------------------------------

/// Where `log` lines go: stdout in production, a buffer in tests.
pub trait Sink: Send + Sync {
    fn out(&self, line: &str);
    fn err(&self, line: &str);
}

pub struct StdSink;

impl Sink for StdSink {
    fn out(&self, line: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
    fn err(&self, line: &str) {
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, "{line}");
        let _ = e.flush();
    }
}

#[derive(Default)]
pub struct MemSink {
    pub lines: std::sync::Mutex<Vec<String>>,
}

impl Sink for MemSink {
    fn out(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_string());
    }
    fn err(&self, line: &str) {
        self.lines.lock().unwrap().push(format!("ERR {line}"));
    }
}

impl MemSink {
    pub fn all(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

/// `<ts> spira: <msg>` — lib.sh's `log`, byte for byte.
pub fn log_line(epoch: i64, msg: &str) -> String {
    format!("{} spira: {msg}", iso_utc(epoch))
}

/// `json_only`: drop anything before the first line starting with `[` or `{` — bd's own
/// warnings can precede its JSON payload on stdout.
pub fn json_only(text: &str) -> String {
    let mut out = Vec::new();
    let mut on = false;
    for l in text.split_inclusive('\n') {
        let t = l.trim_start();
        if !on && (t.starts_with('[') || t.starts_with('{')) {
            on = true;
        }
        if on {
            out.push(l);
        }
    }
    out.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_only_drops_a_warning_banner() {
        assert_eq!(json_only("warning: noise\n{\"a\":1}\n"), "{\"a\":1}\n");
    }

    #[test]
    fn json_only_passes_clean_json_through() {
        assert_eq!(json_only("[1,2]\n"), "[1,2]\n");
    }

    #[test]
    fn json_only_empty_when_nothing_matches() {
        assert_eq!(json_only("just text\nmore text\n"), "");
    }

    #[test]
    fn iso_utc_epoch_zero() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso_utc_known_instant() {
        // 2026-09-30T00:00:00Z
        assert_eq!(iso_utc(1790726400), "2026-09-30T00:00:00Z");
    }

    #[test]
    fn log_line_shape() {
        assert_eq!(log_line(0, "hello"), "1970-01-01T00:00:00Z spira: hello");
    }
}
