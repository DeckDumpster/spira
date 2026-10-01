//! systemd access and the pure decisions world.sh makes over it — split apart exactly the
//! way world.sh's own `WORLD_LIB=1` split existed for, so a test drives the decision
//! functions directly against fixture state without spawning a process or touching a real
//! systemd (docs/test-plan/instance-lifecycle.md §5).

use std::env;
use std::process::Command;

/// `$SPIRA_SYSTEMCTL`, defaulting to `systemctl` — the same seam name world.sh and
/// sentinel.sh both carry, so one stub covers every caller in a test.
pub fn systemctl_bin() -> String {
    env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string())
}

/// Run `$SPIRA_SYSTEMCTL --user <args>`, returning trimmed stdout (empty on any failure —
/// world.sh's own calls are all `2>/dev/null` and tolerate a missing systemctl).
pub fn run(args: &[&str]) -> String {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    Command::new(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run `$SPIRA_SYSTEMCTL --user <args>`, returning only whether it exited zero — for an
/// action (`stop`/`start`) where bash's own `&&` gates the "stopped"/"started" line on the
/// command's exit status, not on any output.
pub fn run_ok(args: &[&str]) -> bool {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    Command::new(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Same as [`run`] but returning every stdout line, for a `list-units`/`list-unit-files` call.
pub fn run_lines(args: &[&str]) -> Vec<String> {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    Command::new(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// `is-active` / `is-enabled`-style column 1 of a `list-units --no-legend` line.
pub fn first_field(line: &str) -> Option<&str> {
    line.split_whitespace().next()
}

/// What `start` (and `status`'s degraded check) does for one timer, given its enabled
/// state and any recorded suspension reason — `_start_action`, verbatim decision, zero I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAction {
    Start,
    SkipSuspended(String),
    SkipDisabled,
}

pub fn start_action(is_enabled_disabled: bool, suspended_reason: Option<&str>) -> StartAction {
    if let Some(r) = suspended_reason {
        return StartAction::SkipSuspended(r.to_string());
    }
    if is_enabled_disabled {
        return StartAction::SkipDisabled;
    }
    StartAction::Start
}

/// A timer subject, stripped of `.timer` and, if present, its instance suffix — the
/// normalisation `_start_action` and `_is_essential_timer` both apply before comparing
/// against `TIMER_PRIORITY` or a ctrl suspension key.
pub fn subject_of(timer: &str, instance_suffix_bare: &str) -> String {
    let base = timer.strip_suffix(".timer").unwrap_or(timer);
    if instance_suffix_bare.is_empty() {
        base.to_string()
    } else {
        base.strip_suffix(&format!("-{instance_suffix_bare}")).unwrap_or(base).to_string()
    }
}

/// `_is_essential_timer`: true when `timer`'s subject is one of `TIMER_PRIORITY`.
pub fn is_essential_timer(timer: &str, instance: &str, priority: &[&str]) -> bool {
    let subj = subject_of(timer, instance);
    priority.contains(&subj.as_str())
}

/// The fixed order `world.sh` stops cleanly in: summons first so nothing new is born, then
/// the legs that act on what already exists.
pub const TIMER_PRIORITY: &[&str] = &[
    "spira-sentinel",
    "spira-summon",
    "spira-ops",
    "spira-watchtower",
    "spira-archivist",
    "spira-archive",
    "spira-skew",
];

/// Timers treated as CI watchers — left running on a plain `stop` (only `--hard` stops them).
pub const CI_WATCHER_BASES: &[&str] = &["spira-gate-check"];

pub fn is_ci_watcher(timer: &str) -> bool {
    CI_WATCHER_BASES
        .iter()
        .any(|b| timer == format!("{b}.timer") || timer.starts_with(&format!("{b}-")))
}

/// `_status_timer_row`: one formatted status line for a timer, given its state and the
/// paired service's last `Result` (empty/"success" means "don't annotate").
pub fn status_timer_row(timer: &str, state: &str, svc_result: &str) -> String {
    if !svc_result.is_empty() && svc_result != "success" {
        format!("  {timer:<26} {state} (svc: {svc_result})\n")
    } else {
        format!("  {timer:<26} {state}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_action_prefers_suspension_over_disabled() {
        assert_eq!(start_action(true, Some("graph hygiene")), StartAction::SkipSuspended("graph hygiene".into()));
    }

    #[test]
    fn start_action_skips_disabled_with_no_suspension() {
        assert_eq!(start_action(true, None), StartAction::SkipDisabled);
    }

    #[test]
    fn start_action_starts_enabled_unsuspended() {
        assert_eq!(start_action(false, None), StartAction::Start);
    }

    #[test]
    fn subject_of_strips_timer_and_instance() {
        assert_eq!(subject_of("spira-groom-prod.timer", "prod"), "spira-groom");
        assert_eq!(subject_of("spira-groom.timer", "prod"), "spira-groom");
        assert_eq!(subject_of("spira-groom.timer", ""), "spira-groom");
    }

    #[test]
    fn is_essential_timer_matches_priority_list_after_instance_strip() {
        assert!(is_essential_timer("spira-sentinel-prod.timer", "prod", TIMER_PRIORITY));
        assert!(!is_essential_timer("spira-groom-prod.timer", "prod", TIMER_PRIORITY));
    }

    #[test]
    fn ci_watcher_matches_base_and_instance_qualified_form() {
        assert!(is_ci_watcher("spira-gate-check.timer"));
        assert!(is_ci_watcher("spira-gate-check-prod.timer"));
        assert!(!is_ci_watcher("spira-groom.timer"));
    }

    #[test]
    fn status_timer_row_annotates_only_non_success() {
        assert_eq!(status_timer_row("spira-groom.timer", "active", ""), "  spira-groom.timer          active\n");
        assert_eq!(status_timer_row("spira-groom.timer", "active", "success"), "  spira-groom.timer          active\n");
        assert_eq!(
            status_timer_row("spira-groom.timer", "inactive", "exit-code"),
            "  spira-groom.timer          inactive (svc: exit-code)\n"
        );
    }
}
