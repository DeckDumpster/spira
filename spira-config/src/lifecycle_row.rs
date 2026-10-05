//! Every path that creates a bead creates its lifecycle row too when `lifecycle_enforce` is
//! on: a bead with no `spira_lifecycle` row can never be claimed. Off, nothing here runs
//! `spira-lc`.

use std::process::{Command, Stdio};

pub const LC_BIN_ENV: &str = "SPIRA_LC_BIN";
const LC_TIMEOUT_SECS: &str = "30";

pub fn lc_bin() -> String {
    std::env::var(LC_BIN_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "spira-lc".to_string())
}

/// The id `bd create` printed: `--silent` prints the bare id, `--json` an object (or a
/// one-element array) with `id`.
pub fn created_id(stdout: &str) -> Option<String> {
    let text = stdout.trim();
    if text.starts_with('{') || text.starts_with('[') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let obj = if v.is_array() { v.get(0)? } else { &v };
        return obj.get("id").and_then(|i| i.as_str()).map(String::from);
    }
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    (!last.contains(char::is_whitespace)).then(|| last.to_string())
}

/// `Ok(true)`: the row exists now. `Ok(false)`: enforce is off and `bin` was never run.
pub fn ensure_row_with(enforce: bool, bin: &str, id: &str) -> Result<bool, String> {
    if !enforce {
        return Ok(false);
    }
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "create-bead", id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(true)
    } else {
        Err(format!(
            "{bin} create-bead {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn ensure_row(id: &str) -> Result<bool, String> {
    ensure_row_retrying(crate::lifecycle_enforce(None), &lc_bin(), id, ROW_ATTEMPTS, 250)
}

const ROW_ATTEMPTS: u32 = 3;

/// `create-bead` is idempotent, so a transient failure is retried before it is reported.
pub fn ensure_row_retrying(enforce: bool, bin: &str, id: &str, attempts: u32, backoff_ms: u64) -> Result<bool, String> {
    let mut last = ensure_row_with(enforce, bin, id);
    for n in 1..attempts {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms * u64::from(n)));
        last = ensure_row_with(enforce, bin, id);
    }
    last
}

/// Call after a successful `bd create` whose stdout is `created_stdout`. A failure is
/// reported on stderr and returned; the bead exists, so the caller must not retry the create.
pub fn after_create(who: &str, created_stdout: &str) -> Result<bool, String> {
    if !crate::lifecycle_enforce(None) {
        return Ok(false);
    }
    let Some(id) = created_id(created_stdout) else {
        let e = format!("cannot read the created bead's id from {created_stdout:?}");
        eprintln!("{who}: LIFECYCLE: {e}; the new bead has no lifecycle row and cannot be claimed");
        return Err(e);
    };
    ensure_row(&id).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was created but has NO lifecycle row and cannot be claimed: {e}");
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(dir: &std::path::Path, exit: i32) -> String {
        let p = dir.join("lc");
        let log = dir.join("calls");
        testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> {}\nexit {exit}\n", log.display()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn created_id_reads_silent_and_json() {
        assert_eq!(created_id("sp-abc\n").as_deref(), Some("sp-abc"));
        assert_eq!(created_id("{\"id\":\"sp-j\",\"title\":\"t\"}").as_deref(), Some("sp-j"));
        assert_eq!(created_id("[{\"id\":\"sp-k\"}]").as_deref(), Some("sp-k"));
        assert_eq!(created_id("  \n"), None);
        assert_eq!(created_id("warning: something odd"), None);
    }

    #[test]
    fn on_creates_the_row_and_off_never_runs_the_binary() {
        let t = testkit::TempDir::new("lcrow");
        let bin = stub(t.path(), 0);
        assert_eq!(ensure_row_with(true, &bin, "sp-x"), Ok(true));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "create-bead sp-x\n");
        std::fs::remove_file(t.path().join("calls")).unwrap();
        assert_eq!(ensure_row_with(false, &bin, "sp-y"), Ok(false));
        assert!(!t.path().join("calls").exists(), "off made a spira-lc call");
    }

    #[test]
    fn a_failing_create_bead_is_retried_then_reported() {
        let t = testkit::TempDir::new("lcrow-retry");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_retrying(true, &bin, "sp-r", 3, 0).is_err());
        let calls = std::fs::read_to_string(t.path().join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 3);
    }

    #[test]
    fn a_failing_create_bead_is_an_error() {
        let t = testkit::TempDir::new("lcrow-fail");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_with(true, &bin, "sp-z").unwrap_err().contains("exited 3"));
        assert!(ensure_row_with(true, "/nonexistent/lc", "sp-z").is_err());
    }
}
