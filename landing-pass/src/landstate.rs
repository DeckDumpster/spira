//! The landstate ledger (family S, decomposition row 16): `land_mark` (write) and
//! `land_state` (read), lib.sh functions this crate absorbed (sp-cnnt6, "wave 4.16").
//! `land_mark` is the landstate dir's ONE writer (wave4-decomposition.md's safety note
//! (c4)) — every other caller in the tree either calls this crate's CLI (`landing-pass
//! mark`/`landing-pass state`) or reads the ledger's files directly, never writes them.
//!
//! `_tsd_landing_event` (the best-effort landing-event dual-write) is folded into [`land_mark`]
//! itself rather than kept as a separate function: it had exactly one caller in lib.sh and
//! no life of its own. It now appends in-process through the `tsd` crate instead of
//! shelling to the `tsd-write` binary, so "the binary is unbuilt or missing" — the bash
//! comment's own reason this was always best-effort — can no longer happen; a bad run root
//! or a lock that will not clear are what is left to swallow.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn landstate_dir(run: &Path) -> PathBuf {
    run.join("landstate")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// lib.sh `land_mark <id> <state> <tip> [reason] [extra]`. Writes
/// `$SPIRA_RUN/landstate/<id>` atomically — a temp file named `<id>.<pid>`, then renamed
/// over the target, exactly lib.sh's own `$LANDSTATE/$1.$$` — as
/// `"<state> <tip|none> <epoch> <reason>[ <extra>]"`, no trailing newline. Then best-effort
/// appends a landing-event row; that append's success never changes the return value.
///
/// `mkdir -p` failing on the parent directory is treated as success with nothing written,
/// matching lib.sh's own `mkdir -p ... || return 0` — kept byte for byte because every
/// current caller fires this under `set -e` with no return-value check (lib.sh's own
/// comment: "every other call site ... does not check the return").
pub fn land_mark(run: &Path, id: &str, state: &str, tip: &str, reason: &str, extra: &str) -> bool {
    let path = landstate_dir(run).join(id);
    let Some(dir) = path.parent() else { return true };
    if fs::create_dir_all(dir).is_err() {
        return true;
    }
    let tip_for_file = if tip.is_empty() { "none" } else { tip };
    let mut content = format!("{state} {tip_for_file} {} {reason}", unix_now());
    if !extra.is_empty() {
        content.push(' ');
        content.push_str(extra);
    }
    let ok = crate::util::atomic_write(&path, &content).is_ok();
    let _ = tsd_landing_event(run, id, state, tip, reason);
    ok
}

/// lib.sh `land_state <id>` — the record's bytes with every newline removed (`tr -d '\n'`),
/// or `None` where bash `return 1`'d on an unreadable file.
pub fn land_state(run: &Path, id: &str) -> Option<String> {
    let text = fs::read_to_string(landstate_dir(run).join(id)).ok()?;
    Some(text.chars().filter(|c| *c != '\n').collect())
}

/// lib.sh `_tsd_landing_event <id> <state> <tip> [reason]`: a `landing-event` row —
/// `bead`/`state`/`tip` always, `reason` only when non-empty, matching the bash function's
/// own two-shape call (with and without a reason). Best-effort: every failure is handed
/// back to [`land_mark`], which discards it.
fn tsd_landing_event(run: &Path, id: &str, state: &str, tip: &str, reason: &str) -> Result<(), String> {
    let mut fields: Vec<(String, serde_json::Value)> = vec![
        ("bead".to_string(), serde_json::Value::String(id.to_string())),
        ("state".to_string(), serde_json::Value::String(state.to_string())),
        ("tip".to_string(), serde_json::Value::String(tip.to_string())),
    ];
    if !reason.is_empty() {
        fields.push(("reason".to_string(), serde_json::Value::String(reason.to_string())));
    }
    let now = unix_now();
    let line = tsd::build_row(&tsd::iso_utc(now), &hostname(), "landing-event", &fields)?;
    append_line(&tsd::family_path(run, "landing-event"), &line)
}

/// Appends one line under an exclusive flock, the same contract `tsd-write` holds for every
/// other producer (tsd/src/main.rs `append_line`; duplicated here rather than shared because
/// the two crates do not otherwise depend on each other and this is the one place land_mark
/// needs it).
fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = f.as_raw_fd();
    // SAFETY: flock on a file descriptor we own for the duration of the call.
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(format!("{}: flock failed", path.display()));
    }
    let r = writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()));
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    r.map_err(|e| e.to_string())
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown-host".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> testkit::TempDir {
        testkit::TempDir::new("landstate")
    }

    #[test]
    fn mark_writes_the_record_with_no_trailing_newline() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "LANDED", "deadbeef", "spira", ""));
        let text = fs::read_to_string(landstate_dir(&r).join("sp-a")).unwrap();
        assert!(!text.ends_with('\n'), "{text:?}");
        let mut it = text.split_whitespace();
        assert_eq!(it.next(), Some("LANDED"));
        assert_eq!(it.next(), Some("deadbeef"));
        assert!(it.next().unwrap().parse::<u64>().is_ok());
        assert_eq!(it.next(), Some("spira"));
    }

    #[test]
    fn mark_defaults_an_empty_tip_to_none_in_the_file() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "GATING", "", "", ""));
        let text = fs::read_to_string(landstate_dir(&r).join("sp-a")).unwrap();
        assert!(text.starts_with("GATING none "), "{text:?}");
    }

    #[test]
    fn mark_appends_extra_only_when_non_empty() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "WITHDRAWN", "none", "operator", "suites=a,b"));
        let text = fs::read_to_string(landstate_dir(&r).join("sp-a")).unwrap();
        assert!(text.ends_with(" suites=a,b"), "{text:?}");
    }

    #[test]
    fn mark_is_the_only_writer_the_temp_file_is_named_id_dot_pid() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "LANDED", "t", "", ""));
        // No stray temp file survives a clean write.
        let left: Vec<_> = fs::read_dir(landstate_dir(&r)).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn mark_dual_writes_a_landing_event_row() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "LANDED", "deadbeef", "spira", ""));
        let fam = tsd::family_path(&r, "landing-event");
        let text = fs::read_to_string(&fam).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(row["bead"], "sp-a");
        assert_eq!(row["state"], "LANDED");
        assert_eq!(row["tip"], "deadbeef");
        assert_eq!(row["reason"], "spira");
    }

    #[test]
    fn mark_omits_the_reason_field_when_empty() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "GATING", "t1", "", ""));
        let fam = tsd::family_path(&r, "landing-event");
        let text = fs::read_to_string(&fam).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert!(row.get("reason").is_none(), "{row}");
    }

    #[test]
    fn mark_succeeds_even_when_the_tsd_write_cannot_be_made() {
        let r = run();
        // A plain file where the tsd family dir must go: mkdir -p on it fails.
        fs::write(r.join("tsd"), "").unwrap();
        assert!(land_mark(&r, "sp-a", "LANDED", "t1", "", ""), "the landstate write must not depend on the tsd write");
        assert!(fs::read_to_string(landstate_dir(&r).join("sp-a")).is_ok());
        assert!(!tsd::family_path(&r, "landing-event").exists());
    }

    #[test]
    fn mark_mkdir_failure_returns_true_and_writes_nothing() {
        let r = run();
        // A plain file where the landstate dir itself would need to go: mkdir -p fails.
        fs::write(r.join("landstate"), "not a dir").unwrap();
        let got = land_mark(&r, "sp-a", "LANDED", "t1", "", "");
        assert!(got, "mkdir -p failing is bash's own `return 0`");
        assert!(fs::metadata(r.join("landstate")).unwrap().is_file(), "untouched — no write was attempted");
    }

    #[test]
    fn state_reads_back_with_newlines_stripped() {
        let r = run();
        fs::create_dir_all(landstate_dir(&r)).unwrap();
        fs::write(landstate_dir(&r).join("sp-a"), "LANDED\ndeadbeef\n1700000000 spira").unwrap();
        assert_eq!(land_state(&r, "sp-a"), Some("LANDEDdeadbeef1700000000 spira".to_string()));
    }

    #[test]
    fn state_is_none_when_unreadable() {
        let r = run();
        assert_eq!(land_state(&r, "sp-missing"), None);
    }

    #[test]
    fn mark_then_state_round_trips() {
        let r = run();
        assert!(land_mark(&r, "sp-a", "CERTIFIED", "deadbeef", "", ""));
        let s = land_state(&r, "sp-a").unwrap();
        assert!(s.starts_with("CERTIFIED deadbeef "), "{s:?}");
    }
}
