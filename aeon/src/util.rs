//! Clock formatting, the log line, and one bounded process runner every port uses.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The result of one subprocess: its exit code (-1 when it could not start, 124 when it
/// timed out — `timeout(1)`'s own code, which is what bash callers saw), stdout, stderr.
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

/// Everything needed to start one process. `env` REPLACES the environment (the caller
/// builds the whole map); `stdin` is written by a thread so a large payload cannot deadlock.
pub struct Spec<'a> {
    pub prog: &'a str,
    pub args: Vec<String>,
    pub env: Option<&'a std::collections::BTreeMap<String, String>>,
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an epoch.
pub fn iso_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Howard Hinnant's days-to-civil.
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

/// `date -d @<epoch> +'%H:%M:%S %Z'` in the host's local zone.
pub fn local_hms_zone(epoch: i64) -> Option<String> {
    let t: libc::time_t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes into `tm`, which we own; strftime writes at most buf.len().
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return None;
    }
    let mut buf = [0u8; 64];
    let fmt = b"%H:%M:%S %Z\0";
    let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), fmt.as_ptr() as *const libc::c_char, &tm) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n]).into_owned())
}

// ---- the log line --------------------------------------------------------------------

/// Where `log` lines go: stdout in production (lib.sh's `log` prints to stdout), a buffer
/// in tests.
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

/// `<ts> spira: <msg>` — lib.sh's `log`.
pub fn log_line(epoch: i64, msg: &str) -> String {
    format!("{} spira: {msg}", iso_utc(epoch))
}

/// Remove bd's hint lines (`💡`, `warning`, `  Fix`, `  Or`) — aeon.sh's `grep -vE`.
pub fn strip_bd_hints(text: &str) -> String {
    text.lines()
        .filter(|l| !(l.starts_with('💡') || l.starts_with("warning") || l.starts_with("  Fix") || l.starts_with("  Or")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `json_only`: drop anything before the first line starting with `[` or `{`.
pub fn json_only(text: &str) -> String {
    let mut out = Vec::new();
    let mut on = false;
    for l in text.split_inclusive('\n') {
        if !on && (l.starts_with('[') || l.starts_with('{')) {
            on = true;
        }
        if on {
            out.push(l);
        }
    }
    out.concat()
}

/// Python's `round()` (half to even) of a float, as an integer.
pub fn round_half_even(v: f64) -> i64 {
    let r = v.round();
    if (v - v.trunc()).abs() == 0.5 {
        let t = v.trunc();
        if (t as i64) % 2 == 0 {
            t as i64
        } else {
            r as i64
        }
    } else {
        r as i64
    }
}

/// Write beside, then rename, so a reader never sees a partial file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// A process with this pid exists (`[ -d /proc/$pid ]`).
pub fn pid_alive(pid: &str) -> bool {
    !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && Path::new("/proc").join(pid).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_matches_date() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn round_half_even_is_pythons() {
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(3.5), 4);
        assert_eq!(round_half_even(0.5), 0);
        assert_eq!(round_half_even(1.4999), 1);
        assert_eq!(round_half_even(12.345), 12);
        assert_eq!(round_half_even(-2.5), -2);
    }

    #[test]
    fn json_only_strips_warnings_before_payload() {
        assert_eq!(json_only("warning: x\n[{\"id\":1}]\n"), "[{\"id\":1}]\n");
        assert_eq!(json_only("nothing here\n"), "");
    }

    #[test]
    fn strip_bd_hints_drops_hint_lines() {
        let t = "sp-a · title\n💡 tip\nwarning: w\n  Fix: x\n  Or: y\nNOTES";
        assert_eq!(strip_bd_hints(t), "sp-a · title\nNOTES");
    }

    #[test]
    fn run_times_out_with_124() {
        let o = run(Spec { prog: "sleep", args: vec!["5".into()], env: None, cwd: None, stdin: None, timeout: Some(Duration::from_millis(100)) });
        assert_eq!(o.code, 124);
    }

    #[test]
    fn run_passes_stdin_and_captures() {
        let o = run(Spec { prog: "cat", args: vec![], env: None, cwd: None, stdin: Some(b"payload".to_vec()), timeout: None });
        assert_eq!((o.code, o.stdout.as_str()), (0, "payload"));
    }
}
