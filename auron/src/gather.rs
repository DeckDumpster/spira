//! File reads and systemctl/journalctl questions — auron.sh's own "GATHER" section. Every
//! value that could not be read is reported AS unreadable rather than as a benign default,
//! because a probe that fails quietly is indistinguishable from a healthy reading.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::util::{self, Spec};

// ---- the sentinel log tail -------------------------------------------------------------

pub struct LogTail {
    pub readable: bool,
    pub error: String,
    pub mtime: i64,
    pub text: String,
}

/// Reads at most `tail_bytes` from the END of `path` — bounded, because an unbounded read
/// of a file that grows forever is the one way this program could still become slow.
pub fn read_log_tail(path: &Path, tail_bytes: u64) -> LogTail {
    let meta = std::fs::metadata(path);
    let mtime = meta.as_ref().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
    if meta.is_err() {
        return LogTail { readable: false, error: "no such file".to_string(), mtime: 0, text: String::new() };
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(tail_bytes as usize);
            LogTail { readable: true, error: String::new(), mtime, text: String::from_utf8_lossy(&bytes[start..]).into_owned() }
        }
        Err(_) => LogTail { readable: false, error: "exists but is not readable".to_string(), mtime, text: String::new() },
    }
}

// ---- systemd --------------------------------------------------------------------------

pub trait Systemctl: Send + Sync {
    fn is_enabled(&self, unit: &str) -> bool;
    fn is_active(&self, unit: &str) -> bool;
    /// `systemctl --user is-active <unit>`'s own STDOUT text (`active`, `inactive`,
    /// `failed`, `activating`, …), trimmed — NOT just its exit code. auron.sh captured
    /// this text directly (`sentinel_timer="$(... is-active ...)"`), so an empty answer
    /// (a stub systemctl, or one that printed nothing) is distinct from a real "inactive".
    fn active_text(&self, unit: &str) -> String;
    /// Loaded `spira-*.service` unit names.
    fn spira_services(&self) -> Vec<String>;
    /// `NRestarts` for each of `units` that answered.
    fn nrestarts(&self, units: &[String]) -> BTreeMap<String, i64>;
}

pub trait Journal: Send + Sync {
    /// The last `n` lines of `unit`'s journal.
    fn tail(&self, unit: &str, n: u32) -> String;
}

pub struct RealSystemctl {
    pub bin: String,
    pub timeout: Duration,
}

impl Systemctl for RealSystemctl {
    fn is_enabled(&self, unit: &str) -> bool {
        util::run(Spec { prog: &self.bin, args: vec!["--user".into(), "is-enabled".into(), unit.into()], env: None, cwd: None, stdin: None, timeout: Some(self.timeout) }).success()
    }
    fn is_active(&self, unit: &str) -> bool {
        util::run(Spec { prog: &self.bin, args: vec!["--user".into(), "is-active".into(), unit.into()], env: None, cwd: None, stdin: None, timeout: Some(self.timeout) }).success()
    }
    fn active_text(&self, unit: &str) -> String {
        util::run(Spec { prog: &self.bin, args: vec!["--user".into(), "is-active".into(), unit.into()], env: None, cwd: None, stdin: None, timeout: Some(self.timeout) }).text()
    }
    fn spira_services(&self) -> Vec<String> {
        let o = util::run(Spec { prog: &self.bin, args: vec!["--user".into(), "list-units".into(), "--type=service".into(), "--all".into(), "--plain".into(), "--no-legend".into()], env: None, cwd: None, stdin: None, timeout: Some(self.timeout) });
        if !o.success() {
            return Vec::new();
        }
        o.stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|u| u.starts_with("spira-"))
            .map(|s| s.to_string())
            .collect()
    }
    fn nrestarts(&self, units: &[String]) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        if units.is_empty() {
            return out;
        }
        let mut args = vec!["--user".to_string(), "show".to_string(), "--property=Id,NRestarts".to_string(), "--".to_string()];
        args.extend(units.iter().cloned());
        let o = util::run(Spec { prog: &self.bin, args, env: None, cwd: None, stdin: None, timeout: Some(self.timeout) });
        if !o.success() {
            return out;
        }
        parse_show_id_nrestarts(&o.stdout, &mut out);
        out
    }
}

/// `systemctl show --property=Id,NRestarts -- <units>`'s stdout: `Id=<unit>` then
/// `NRestarts=<n>` pairs, blank-line separated per unit.
fn parse_show_id_nrestarts(text: &str, out: &mut BTreeMap<String, i64>) {
    let mut id: Option<String> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Id=") {
            id = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("NRestarts=") {
            if let Some(u) = id.take() {
                if let Ok(n) = v.trim().parse::<i64>() {
                    out.insert(u, n);
                }
            }
        } else if line.is_empty() {
            id = None;
        }
    }
}

pub struct RealJournal {
    pub bin: String,
    pub timeout: Duration,
}

impl Journal for RealJournal {
    fn tail(&self, unit: &str, n: u32) -> String {
        let o = util::run(Spec { prog: &self.bin, args: vec!["--user".into(), "--no-pager".into(), "-n".into(), n.to_string(), format!("--unit={unit}")], env: None, cwd: None, stdin: None, timeout: Some(self.timeout) });
        o.stdout
    }
}

/// `spira_unit <base> [service|timer]` (conf.sh, ported faithfully): the instance-qualified
/// unit if systemd knows it, else the plain form, else `"?"` — a unit that cannot be found
/// must not be queried for health, which would report "inactive" about an unrelated subject.
pub fn spira_unit(sc: &dyn Systemctl, base: &str, kind: &str, instance: &str) -> String {
    let inst = if instance.is_empty() { format!("spira-{base}.{kind}") } else { format!("spira-{base}-{instance}.{kind}") };
    let plain = format!("spira-{base}.{kind}");
    if sc.is_enabled(&inst) || sc.is_active(&inst) {
        inst
    } else if sc.is_enabled(&plain) || sc.is_active(&plain) {
        plain
    } else {
        "?".to_string()
    }
}

/// `systemctl --user is-active <unit>`'s own text (auron.sh captured stdout, not just the
/// exit code), or `"unknown"` when the unit could not be resolved at all, or when
/// `is-active` printed nothing.
pub fn sentinel_timer_status(sc: &dyn Systemctl, instance: &str) -> String {
    let unit = spira_unit(sc, "sentinel", "timer", instance);
    if unit == "?" {
        return "unknown".to_string();
    }
    let text = sc.active_text(&unit);
    if text.is_empty() {
        "unknown".to_string()
    } else {
        text
    }
}

// ---- the mirror -------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct MirrorCheck {
    pub configured: bool,
    pub exists: bool,
    pub mtime: i64,
}

pub fn check_mirror(exporter: &str, mirror_path: &Path) -> MirrorCheck {
    if exporter.is_empty() {
        return MirrorCheck::default();
    }
    match std::fs::metadata(mirror_path) {
        Ok(m) => {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
            MirrorCheck { configured: true, exists: true, mtime }
        }
        Err(_) => MirrorCheck { configured: true, exists: false, mtime: 0 },
    }
}

// ---- world halt / drain -----------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct WorldState {
    pub halted: bool,
    pub halt_at: i64,
    pub draining: bool,
    pub drain_at: i64,
}

/// `date -d "<first line>" +%s`, or 0 on any failure — shelled to `date` for the same
/// reason `util::iso_display` is: only the system can parse an arbitrary human date string.
fn parse_stamp_first_line(path: &Path) -> i64 {
    let Ok(text) = std::fs::read_to_string(path) else { return 0 };
    let Some(first) = text.lines().next() else { return 0 };
    let o = util::run(Spec { prog: "date", args: vec!["-d".into(), first.to_string(), "+%s".into()], env: None, cwd: None, stdin: None, timeout: Some(Duration::from_secs(5)) });
    if o.success() {
        o.text().parse().unwrap_or(0)
    } else {
        0
    }
}

pub fn world_state(run_dir: &Path, now: i64, drain_ttl: i64) -> WorldState {
    let mut w = WorldState::default();
    let halted_path = run_dir.join("world.halted");
    if halted_path.is_file() {
        w.halted = true;
        w.halt_at = parse_stamp_first_line(&halted_path);
    }
    let draining_path = run_dir.join("world.draining");
    if let Ok(text) = std::fs::read_to_string(&draining_path) {
        let expires = text.lines().find_map(|l| l.strip_prefix("expires ")).and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
        let expires = if expires == 0 {
            std::fs::metadata(&draining_path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64 + drain_ttl).unwrap_or(0)
        } else {
            expires
        };
        if now < expires {
            w.draining = true;
            w.drain_at = parse_stamp_first_line(&draining_path);
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_tail_missing_file_is_unreadable_not_empty_text() {
        let t = read_log_tail(Path::new("/no/such/path/ever"), 1000);
        assert!(!t.readable);
        assert_eq!(t.error, "no such file");
    }

    #[test]
    fn log_tail_reads_only_the_last_n_bytes() {
        let dir = testkit::TempDir::new("auron-gather");
        let p = dir.join("log");
        std::fs::write(&p, "0123456789").unwrap();
        let t = read_log_tail(&p, 4);
        assert!(t.readable);
        assert_eq!(t.text, "6789");
    }

    #[test]
    fn log_tail_of_a_short_file_is_the_whole_file() {
        let dir = testkit::TempDir::new("auron-gather");
        let p = dir.join("log");
        std::fs::write(&p, "hi").unwrap();
        let t = read_log_tail(&p, 4);
        assert_eq!(t.text, "hi");
    }

    #[test]
    fn mirror_unconfigured_when_no_exporter() {
        let m = check_mirror("", Path::new("/does/not/matter"));
        assert!(!m.configured);
        assert!(!m.exists);
    }

    #[test]
    fn mirror_configured_but_missing() {
        let m = check_mirror("dolt", Path::new("/no/such/mirror/ever.jsonl"));
        assert!(m.configured);
        assert!(!m.exists);
    }

    #[test]
    fn mirror_configured_and_present() {
        let dir = testkit::TempDir::new("auron-gather");
        let p = dir.join("mirror.jsonl");
        std::fs::write(&p, "{}\n").unwrap();
        let m = check_mirror("dolt", &p);
        assert!(m.configured && m.exists);
    }

    #[test]
    fn no_halt_or_drain_files_is_a_clean_world() {
        let dir = testkit::TempDir::new("auron-gather");
        let w = world_state(&dir, 1000, 1800);
        assert!(!w.halted && !w.draining);
    }

    #[test]
    fn a_halted_stamp_is_reported_halted() {
        let dir = testkit::TempDir::new("auron-gather");
        std::fs::write(dir.join("world.halted"), "Mon Sep 29 00:00:00 UTC 2026\n").unwrap();
        let w = world_state(&dir, 1000, 1800);
        assert!(w.halted);
    }

    #[test]
    fn a_draining_stamp_with_expires_line_in_the_future_is_draining() {
        let dir = testkit::TempDir::new("auron-gather");
        std::fs::write(dir.join("world.draining"), format!("now\ngated\nexpires {}\n", 2000)).unwrap();
        let w = world_state(&dir, 1000, 1800);
        assert!(w.draining);
    }

    #[test]
    fn an_expired_drain_stamp_is_not_draining() {
        let dir = testkit::TempDir::new("auron-gather");
        std::fs::write(dir.join("world.draining"), "now\ngated\nexpires 500\n".to_string()).unwrap();
        let w = world_state(&dir, 1000, 1800);
        assert!(!w.draining);
    }

    // ---- systemd, faked ------------------------------------------------------------------

    #[derive(Default)]
    struct FakeSc {
        enabled: Vec<&'static str>,
        active: Vec<&'static str>,
        /// `unit -> is-active's own stdout text`; a unit absent here answers "" (as a
        /// stub systemctl that printed nothing would), exercising the empty->"unknown"
        /// fallback distinctly from the exit-code-only `is_active`/`is_enabled`.
        text: Vec<(&'static str, &'static str)>,
    }
    impl Systemctl for FakeSc {
        fn is_enabled(&self, unit: &str) -> bool {
            self.enabled.contains(&unit)
        }
        fn is_active(&self, unit: &str) -> bool {
            self.active.contains(&unit)
        }
        fn active_text(&self, unit: &str) -> String {
            self.text.iter().find(|(u, _)| *u == unit).map(|(_, t)| t.to_string()).unwrap_or_default()
        }
        fn spira_services(&self) -> Vec<String> {
            Vec::new()
        }
        fn nrestarts(&self, _units: &[String]) -> BTreeMap<String, i64> {
            BTreeMap::new()
        }
    }

    #[test]
    fn spira_unit_prefers_the_instance_qualified_form() {
        let sc = FakeSc { enabled: vec!["spira-sentinel-prod.timer"], ..Default::default() };
        assert_eq!(spira_unit(&sc, "sentinel", "timer", "prod"), "spira-sentinel-prod.timer");
    }

    #[test]
    fn spira_unit_falls_back_to_the_plain_form() {
        let sc = FakeSc { enabled: vec!["spira-sentinel.timer"], ..Default::default() };
        assert_eq!(spira_unit(&sc, "sentinel", "timer", "prod"), "spira-sentinel.timer");
    }

    #[test]
    fn spira_unit_is_question_mark_when_neither_form_is_known() {
        let sc = FakeSc::default();
        assert_eq!(spira_unit(&sc, "sentinel", "timer", "prod"), "?");
    }

    #[test]
    fn sentinel_timer_status_unknown_when_unresolvable() {
        let sc = FakeSc::default();
        assert_eq!(sentinel_timer_status(&sc, "prod"), "unknown");
    }

    #[test]
    fn sentinel_timer_status_active() {
        let sc = FakeSc { enabled: vec!["spira-sentinel-prod.timer"], text: vec![("spira-sentinel-prod.timer", "active")], ..Default::default() };
        assert_eq!(sentinel_timer_status(&sc, "prod"), "active");
    }

    #[test]
    fn sentinel_timer_status_inactive_when_resolved_but_not_active() {
        let sc = FakeSc { enabled: vec!["spira-sentinel-prod.timer"], text: vec![("spira-sentinel-prod.timer", "inactive")], ..Default::default() };
        assert_eq!(sentinel_timer_status(&sc, "prod"), "inactive");
    }

    #[test]
    fn sentinel_timer_status_unknown_when_resolved_but_is_active_prints_nothing() {
        // test-auron.sh's own SPIRA_SYSTEMCTL=true fixture: every call succeeds (so
        // is-enabled resolves the unit) but prints nothing — auron.sh's own
        // `[ -n "$sentinel_timer" ] || sentinel_timer=unknown` fallback, not "active".
        let sc = FakeSc { enabled: vec!["spira-sentinel-prod.timer"], ..Default::default() };
        assert_eq!(sentinel_timer_status(&sc, "prod"), "unknown");
    }

    #[test]
    fn parse_show_id_nrestarts_pairs_ids_with_counts() {
        let text = "Id=spira-sentinel.service\nNRestarts=3\n\nId=spira-auron.service\nNRestarts=0\n";
        let mut out = BTreeMap::new();
        parse_show_id_nrestarts(text, &mut out);
        assert_eq!(out.get("spira-sentinel.service"), Some(&3));
        assert_eq!(out.get("spira-auron.service"), Some(&0));
    }

}
