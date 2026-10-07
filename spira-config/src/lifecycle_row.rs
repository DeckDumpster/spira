//! Every path that creates a bead creates its lifecycle row too: a bead with no
//! `spira_lifecycle` row can never be claimed.

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

/// `Ok(())`: the row exists now.
pub fn ensure_row_with(bin: &str, id: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "create-bead", id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} create-bead {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn ensure_row(id: &str) -> Result<(), String> {
    ensure_row_retrying(&lc_bin(), id, ROW_ATTEMPTS, 250)
}

const ROW_ATTEMPTS: u32 = 3;

/// `create-bead` is idempotent, so a transient failure is retried before it is reported.
pub fn ensure_row_retrying(bin: &str, id: &str, attempts: u32, backoff_ms: u64) -> Result<(), String> {
    let mut last = ensure_row_with(bin, id);
    for n in 1..attempts {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms * u64::from(n)));
        last = ensure_row_with(bin, id);
    }
    last
}

/// Call after a successful `bd create` whose stdout is `created_stdout`. A failure is
/// reported on stderr and returned; the bead exists, so the caller must not retry the create.
pub fn after_create(who: &str, created_stdout: &str) -> Result<(), String> {
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

/// The bead a `bd close <id> ...` argv closes: the first operand, when it is one.
pub fn closed_id(args: &[String]) -> Option<&str> {
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("close"), Some(id)) if !id.starts_with('-') => Some(id),
        _ => None,
    }
}

/// Move the bead's lifecycle row to the terminal state its bd close earned
/// (`spira-lc reconcile-closed`), so a closed bead never stays READY and claimable.
pub fn terminalize_closed_with(bin: &str, id: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "reconcile-closed", "--apply", id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} reconcile-closed {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Call after a successful `bd close`. A failure is reported and returned; the bead is
/// closed, so the caller must not retry the close.
pub fn after_close(who: &str, args: &[String]) -> Result<(), String> {
    let Some(id) = closed_id(args) else { return Ok(()) };
    terminalize_closed_with(&lc_bin(), id).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was closed but its lifecycle row is not terminal and may read as claimable: {e}");
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
    fn ensure_row_creates_the_row() {
        let t = testkit::TempDir::new("lcrow");
        let bin = stub(t.path(), 0);
        assert_eq!(ensure_row_with(&bin, "sp-x"), Ok(()));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "create-bead sp-x\n");
    }

    #[test]
    fn a_failing_create_bead_is_retried_then_reported() {
        let t = testkit::TempDir::new("lcrow-retry");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_retrying(&bin, "sp-r", 3, 0).is_err());
        let calls = std::fs::read_to_string(t.path().join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 3);
    }

    #[test]
    fn a_failing_create_bead_is_an_error() {
        let t = testkit::TempDir::new("lcrow-fail");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_with(&bin, "sp-z").unwrap_err().contains("exited 3"));
        assert!(ensure_row_with("/nonexistent/lc", "sp-z").is_err());
    }

    #[test]
    fn a_close_runs_reconcile_closed_for_exactly_the_bead_it_closed() {
        let t = testkit::TempDir::new("lcrow-close");
        let bin = stub(t.path(), 0);
        assert_eq!(terminalize_closed_with(&bin, "sp-c"), Ok(()));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "reconcile-closed --apply sp-c\n");
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(closed_id(&a(&["close", "sp-c", "--reason-file", "-"])), Some("sp-c"));
        assert_eq!(closed_id(&a(&["close", "--force", "sp-c"])), None);
        assert_eq!(closed_id(&a(&["update", "sp-c"])), None);
        assert!(terminalize_closed_with(&stub(t.path(), 3), "sp-c").unwrap_err().contains("exited 3"));
    }
}
