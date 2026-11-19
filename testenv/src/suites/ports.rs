//! Every effect `testenv suites` has outside its own state directory, as a trait
//! (DESIGN-suites.md §5): host-check.sh, the clock and the
//! two output streams. `real.rs` implements them against the host; the unit tests implement them as
//! fakes.

use std::path::{Path, PathBuf};

use spira_config::process::{cfg, cfg_parse};

/// Resolved settings (DESIGN-suites.md §2.5): registered keys (`spira/conf.d`) through
/// `spira_config::process::cfg`/`cfg_parse` — the one source of config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The harness root (suites.sh's `$HERE/..`).
    pub root: PathBuf,
    /// `<root>/spira` (suites.sh's `$HERE`): the suites, gate-suites, the helper scripts.
    pub suite_dir: PathBuf,
    pub state: PathBuf,
    pub stale: u64,
    pub priority: u8,
    pub gate_list: PathBuf,
}

/// SPIRA_SUITES_PRIORITY's declared range (0..=4) — pure, so a unit test can drive it
/// without touching spira-config's per-process resolution cache.
fn validate_priority(priority: u8) -> Result<u8, String> {
    if priority > 4 {
        return Err(format!(
            "SPIRA_SUITES_PRIORITY={priority} is refused — priority must be 0..=4"
        ));
    }
    Ok(priority)
}

impl Settings {
    pub fn load(root: &Path) -> Result<Settings, String> {
        let suite_dir = root.join("spira");
        let gate_list = PathBuf::from(cfg("SPIRA_GATE_SUITES")?);
        let priority = validate_priority(cfg_parse("SPIRA_SUITES_PRIORITY")?)?;
        Ok(Settings {
            state: PathBuf::from(cfg("SPIRA_SUITES_STATE")?),
            stale: cfg_parse("SPIRA_SUITES_STALE")?,
            priority,
            gate_list,
            root: root.to_path_buf(),
            suite_dir,
        })
    }

    pub fn suite_path(&self, suite: &str) -> PathBuf {
        self.suite_dir.join(suite)
    }
    pub fn state_file(&self, suite: &str, ext: &str) -> PathBuf {
        self.state.join(format!("{suite}.{ext}"))
    }
}

pub trait Clock {
    fn now(&self) -> u64;
}

pub trait HostCheck {
    /// `host-check.sh <flag>`'s stdout (trailing newlines stripped), None when it cannot be
    /// read: not executable, failed, silent, or past its wall.
    fn count(&self, flag: &str) -> Option<String>;
}

pub trait Emit {
    fn out(&self, line: &str);
    fn err(&self, line: &str);
}

/// Everything a command may touch.
pub struct World<'a> {
    pub s: &'a Settings,
    pub clock: &'a dyn Clock,
    pub host: &'a dyn HostCheck,
    pub io: &'a dyn Emit,
}

impl World<'_> {
    pub fn out(&self, s: impl AsRef<str>) {
        self.io.out(s.as_ref());
    }
    pub fn err(&self, s: impl AsRef<str>) {
        self.io.err(s.as_ref());
    }
    /// lib.sh `log`'s shape, on stderr (DESIGN-suites.md §6 D1).
    pub fn log(&self, msg: &str) {
        self.io.err(&format!(
            "{} spira: {msg}",
            crate::util::iso_utc(self.clock.now())
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered key here goes through `cfg`/`cfg_parse`, which resolve once per
    /// process from spira-config's own cache — not something a unit test can drive per-case.
    /// What stays testable here is the pure validation below.
    #[test]
    fn priority_above_four_is_refused_by_name() {
        assert_eq!(validate_priority(0), Ok(0));
        assert_eq!(validate_priority(4), Ok(4));
        assert!(validate_priority(5).is_err());
    }

    fn shipped_priority(key: &str) -> u8 {
        let path = spira_config::resolve::locate_home_for_process().unwrap().join("conf.d").join(key);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let marker = format!("${{{key}:=");
        let line = text.lines().find(|l| !l.trim_start().starts_with('#') && l.contains(&marker)).unwrap_or_else(|| panic!("{key} has no shipped default"));
        let rest = &line[line.find(&marker).unwrap() + marker.len()..];
        rest[..rest.find('}').unwrap()].parse().unwrap_or_else(|_| panic!("{key} default is not a number: {line}"))
    }

    // covers: UC-ops-detection-remediation-36
    #[test]
    fn suite_reds_and_incidents_default_behind_a_reported_bug() {
        let reported = shipped_priority("SPIRA_GH_INTAKE_PRIORITY");
        for key in ["SPIRA_SUITES_PRIORITY", "SPIRA_INCIDENT_PRIORITY"] {
            let own = shipped_priority(key);
            assert!(own > reported, "{key} defaults to P{own}, reported bugs to P{reported}: the harness's own findings draw the dwarf first");
        }
    }
}
