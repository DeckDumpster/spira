//! Running `spira-lc` bounded, without resolving anything through the caller's PATH.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const LC_TIMEOUT: Duration = Duration::from_secs(15); // batch-job: spira-lc requests queue behind a loaded Dolt in the round VM

/// Print a service answer: a "cannot tell" refusal goes to stderr, so a caller capturing stdout
/// as an id or a value gets nothing, never the refusal text (law-never-derive-an-id-from-output).
pub fn print_answer(code: i32, out: &str) {
    if out.is_empty() {
        return;
    }
    if is_refusal(code, out) {
        eprintln!("{out}");
    } else {
        println!("{out}");
    }
}

pub fn is_refusal(code: i32, out: &str) -> bool {
    code != 0 && out.starts_with("cannot tell")
}

/// `SPIRA_LC_BIN`, else `spira-lc` beside this executable, else the bare name.
pub fn lc_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("SPIRA_LC_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("spira-lc")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("spira-lc"))
}

/// Run `bin args` with a deadline enforced here, not by a `timeout` program. A deadline
/// hit is exit 124. Output is combined stdout+stderr.
pub fn run_bounded(mut cmd: Command, limit: Duration) -> (i32, String) {
    let dir = std::env::temp_dir();
    let tag = format!("spira-lc-call-{}-{:?}", std::process::id(), std::thread::current().id());
    let (op, ep) = (dir.join(format!("{tag}.out")), dir.join(format!("{tag}.err")));
    let files = std::fs::File::create(&op).and_then(|o| std::fs::File::create(&ep).map(|e| (o, e)));
    let (o, e) = match files {
        Ok(f) => f,
        Err(err) => return (2, format!("running spira-lc: {err}")),
    };
    cmd.stdin(Stdio::null()).stdout(o).stderr(e);
    let result = match cmd.spawn() {
        Err(err) => (2, format!("running spira-lc: {err}")),
        Ok(mut child) => {
            let start = Instant::now();
            let code = loop {
                match child.try_wait() {
                    Ok(Some(st)) => break st.code().unwrap_or(2),
                    Ok(None) if start.elapsed() >= limit => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break 124;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => break 2,
                }
            };
            let read = |p: &PathBuf| std::fs::read_to_string(p).unwrap_or_default();
            (code, format!("{}{}", read(&op), read(&ep)))
        }
    };
    let _ = std::fs::remove_file(&op);
    let _ = std::fs::remove_file(&ep);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_child_is_killed_at_the_deadline_with_no_timeout_program() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "sleep 30"]).env_clear();
        let t = Instant::now();
        assert_eq!(run_bounded(c, Duration::from_millis(200)).0, 124);
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn runs_with_an_empty_path_and_returns_output_and_code() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo hi; exit 3"]).env_clear();
        assert_eq!(run_bounded(c, LC_TIMEOUT), (3, "hi\n".to_string()));
    }

    #[test]
    fn a_deadline_refusal_is_never_stdout() {
        assert!(is_refusal(2, "cannot tell: deadline"));
        assert!(!is_refusal(0, "sp-abc12"), "an id is data");
        assert!(!is_refusal(1, "sp-abc12"));
    }

    #[test]
    fn a_missing_binary_is_cannot_tell() {
        assert_eq!(run_bounded(Command::new("/nonexistent/spira-lc"), LC_TIMEOUT).0, 2);
    }
}
