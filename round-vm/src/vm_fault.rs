//! A round whose VM script ended with no verdict is named from what the script left behind:
//! its phase marker ([`PHASE_FILE`], written by its EXIT trap on every exit) and its own
//! stdout and stderr ([`SCRIPT_LOG_FILE`]). The named fault goes to [`ROUND_FAULT_FILE`] in
//! the results, where certify reads it.

use std::fs;
use std::path::Path;

/// `phase=<name> rc=<n>`: the first phase that failed, else the one the script exited in.
pub const PHASE_FILE: &str = "round-phase";
/// Everything the VM script wrote, stdout and stderr, in order.
pub const SCRIPT_LOG_FILE: &str = "round-script.log";
/// The launcher's own capture of the script's stdout and stderr, fetched on every exit.
pub const LAUNCHER_OUT_FILE: &str = "vm-batch.out";
pub const LAUNCHER_ERR_FILE: &str = "vm-batch.err";
/// The named fault: a `ROUND-FAULT:` line, an optional `settings-key:` line, then the log's tail.
pub const ROUND_FAULT_FILE: &str = "round-fault.out";
/// How much of the script's log a fault carries.
pub const FAULT_TAIL: usize = 40;

const PHASE_FAILED: &str = "round-vm: PHASE-FAILED ";
const VERDICT_FAULT: &str = "VERDICT FAULT";
const SETTINGS_REFUSED: &str = "settings-refused";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundFault {
    pub cause: String,
    /// The config key a `settings-refused` fault names.
    pub settings_key: Option<String>,
    pub tail: String,
}

impl RoundFault {
    pub fn render(&self) -> String {
        let key = self.settings_key.as_deref().map(|k| format!("settings-key: {k}\n")).unwrap_or_default();
        format!("ROUND-FAULT: {}\n{key}--- the last {FAULT_TAIL} lines of the VM's script log ---\n{}\n", self.cause, self.tail)
    }
}

/// The fault whose evidence `results` holds; evidence that did not come back is named as such.
pub fn name(results: &Path) -> RoundFault {
    let marker = fs::read_to_string(results.join(PHASE_FILE)).ok();
    let at = match marker.as_deref().and_then(parse_marker) {
        Some((phase, rc)) => format!("phase={phase} rc={rc}"),
        None => format!("with no phase marker (the VM script never reached its EXIT trap, or {PHASE_FILE} did not come back)"),
    };
    // The trap's log holds stdout and stderr in order; without it the launcher's stderr is the
    // evidence there is, and the cause says which one it read.
    let (log, from) = match fs::read_to_string(results.join(SCRIPT_LOG_FILE)) {
        Ok(l) => (l, ""),
        Err(_) => match fs::read_to_string(results.join(LAUNCHER_ERR_FILE)) {
            Ok(l) => (l, " (read from the launcher's stderr: the script left no log)"),
            Err(_) => return RoundFault { cause: format!("FAULT {at}: neither the VM script's log nor the launcher's stderr came back"), settings_key: None, tail: String::new() },
        },
    };
    let lines: Vec<&str> = log.lines().collect();
    let end = lines.iter().rposition(|l| l.contains(VERDICT_FAULT) || l.starts_with(PHASE_FAILED)).map_or(lines.len(), |i| i + 1);
    let window = &lines[..end];
    let verdict = window.iter().rposition(|l| l.contains(VERDICT_FAULT));
    let reason = verdict.and_then(|i| window[i].split_whitespace().find_map(|w| w.strip_prefix("reason=")));
    let detail = window
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.contains(VERDICT_FAULT) && !l.starts_with(PHASE_FAILED.trim_end()))
        .unwrap_or("the script wrote nothing");
    let settings_key = (reason == Some(SETTINGS_REFUSED)).then(|| refused_key(&window[..verdict.unwrap_or(0)])).flatten();
    let cause = match reason {
        Some(r) => format!("FAULT {r} {at}{from}: {detail}"),
        None => format!("FAULT {at}{from}: {detail}"),
    };
    RoundFault { cause, settings_key, tail: window[end.saturating_sub(FAULT_TAIL)..].join("\n") }
}

fn parse_marker(text: &str) -> Option<(String, String)> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let field = |k: &str| line.split_whitespace().find_map(|w| w.strip_prefix(k)).map(str::to_string);
    Some((field("phase=")?, field("rc=")?))
}

/// testenv names a refused setting first on its `batch: ` line: `batch: KEY is not declared…`.
fn refused_key(before: &[&str]) -> Option<String> {
    let line = before.iter().rev().find_map(|l| l.trim().strip_prefix("batch: "))?;
    let word = line.split_whitespace().next()?;
    let key = word.split('=').next()?.trim_end_matches([':', ',', ';']);
    let ident = key.starts_with(|c: char| c.is_ascii_uppercase()) && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    ident.then(|| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn results(marker: Option<&str>, log: Option<&str>) -> TempDir {
        let d = TempDir::new();
        if let Some(m) = marker {
            fs::write(d.path().join(PHASE_FILE), m).unwrap();
        }
        if let Some(l) = log {
            fs::write(d.path().join(SCRIPT_LOG_FILE), l).unwrap();
        }
        d
    }

    #[test]
    fn a_testenv_settings_refusal_names_its_reason_its_key_and_ends_the_tail_at_the_refusal() {
        let mut log: Vec<String> = (1..=30).map(|i| format!("round-vm: setup line {i}")).collect();
        log.push("batch: SPIRA_TESTENV_SCRATCH_ROOT is not declared — every key is required; refusing".into());
        log.push("VERDICT FAULT rc=2 ran=0 reason=settings-refused".into());
        log.push("round-vm: PHASE-FAILED phase=suites rc=2".into());
        log.extend((1..=50).map(|i| format!("round-vm: UNIT-TEST later-noise-{i} ok")));
        let d = results(Some("phase=suites rc=2\n"), Some(&log.join("\n")));
        let f = name(d.path());
        assert_eq!(
            f.cause,
            "FAULT settings-refused phase=suites rc=2: batch: SPIRA_TESTENV_SCRATCH_ROOT is not declared — every key is required; refusing"
        );
        assert_eq!(f.settings_key.as_deref(), Some("SPIRA_TESTENV_SCRATCH_ROOT"));
        assert!(f.tail.ends_with("round-vm: PHASE-FAILED phase=suites rc=2"), "{}", f.tail);
        assert!(!f.tail.contains("later-noise"), "{}", f.tail);
        assert_eq!(f.tail.lines().count(), FAULT_TAIL);
        assert!(f.render().contains("settings-key: SPIRA_TESTENV_SCRATCH_ROOT\n"));
    }

    #[test]
    fn a_script_that_left_no_marker_says_so_and_still_carries_its_log() {
        let d = results(None, Some("round-vm: built the round in 9s\nkilled\n"));
        let f = name(d.path());
        assert!(f.cause.starts_with("FAULT with no phase marker"), "{}", f.cause);
        assert!(f.cause.ends_with(": killed"), "{}", f.cause);
        assert_eq!(f.settings_key, None);
    }

    #[test]
    fn without_the_scripts_log_the_launchers_stderr_is_read_and_said_to_be() {
        let d = results(Some("phase=release rc=2\n"), None);
        fs::write(d.path().join(LAUNCHER_ERR_FILE), "release: config-delta.toml: TOML parse error at line 3\nround-vm: cannot apply the head's config delta\n").unwrap();
        assert_eq!(
            name(d.path()).cause,
            "FAULT phase=release rc=2 (read from the launcher's stderr: the script left no log): round-vm: cannot apply the head's config delta"
        );
    }

    #[test]
    fn missing_evidence_is_itself_named() {
        let d = results(Some("phase=checkout rc=128\n"), None);
        assert_eq!(name(d.path()).cause, "FAULT phase=checkout rc=128: neither the VM script's log nor the launcher's stderr came back");
        assert!(name(results(None, None).path()).cause.starts_with("FAULT with no phase marker"));
    }

    #[test]
    fn a_refusal_whose_batch_line_names_no_key_names_none() {
        let d = results(Some("phase=suites rc=2\n"), Some("batch: cannot read the config\nVERDICT FAULT rc=2 ran=0 reason=settings-refused\n"));
        let f = name(d.path());
        assert_eq!(f.settings_key, None);
        assert_eq!(f.cause, "FAULT settings-refused phase=suites rc=2: batch: cannot read the config");
    }
}
