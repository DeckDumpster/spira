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

/// Whether `systemctl --user` can reach the user manager at all. `is-system-running` exits
/// nonzero for a merely degraded manager, so only systemctl's own connect failure counts;
/// without this check an unreachable bus reads as every unit being absent.
pub fn bus_unreachable() -> Option<String> {
    let o = Command::new(systemctl_bin()).args(["--user", "is-system-running"]).output().ok()?;
    let err = String::from_utf8_lossy(&o.stderr);
    err.contains("Failed to connect").then(|| format!("cannot reach the systemd user bus: {}", err.trim()))
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

/// Whether `name` is a unit systemd has LOADED AT ALL — enabled or not, active or not.
/// `list-unit-files <name> --no-legend` prints one line for a unit file systemd knows
/// about (nothing for one it does not); `cat <name>` additionally catches a form
/// `list-unit-files` itself does not enumerate on some systemd builds (a generator- or
/// drop-in-only unit). Neither probe starts, stops or enables anything.
pub fn unit_exists(name: &str) -> bool {
    !run_lines(&["list-unit-files", name, "--no-legend"]).is_empty() || run_ok(&["cat", name])
}

/// `_choose_timer`'s own decision, pure: given whether each form is known to systemd at
/// all, which name world.sh acts on — the instance-qualified form first, the plain form
/// second, a NAMED REFUSAL when neither is known (never a guess). sp-ivfu3-2: the timer
/// world.sh picked used to be decided by ENABLED/ACTIVE STATE
/// (`is-enabled`/`is-active`), which cannot tell "known to systemd, just off" apart from
/// "does not exist at all" — with the whole world stopped, every essential timer is
/// disabled AND inactive, so both probes failed and `enumerate_timers` picked the plain,
/// NONEXISTENT name (`spira-sentinel.timer` when only `spira-sentinel-prod.timer` was
/// ever installed), which `world start` would have "started" with systemd silently
/// no-opping on a name it has never heard of.
pub fn choose_timer(base: &str, sfx: &str, qualified_exists: bool, plain_exists: bool) -> Result<String, String> {
    let qualified = format!("{base}{sfx}.timer");
    if qualified_exists {
        return Ok(qualified);
    }
    let plain = format!("{base}.timer");
    if plain == qualified {
        return Err(format!("{plain}: no such systemd unit"));
    }
    if plain_exists {
        return Ok(plain);
    }
    Err(format!("neither {qualified} nor {plain}: no such systemd unit"))
}

/// [`choose_timer`], querying systemd itself for the two existence facts it needs.
pub fn resolve_timer(base: &str, sfx: &str) -> Result<String, String> {
    let qualified = format!("{base}{sfx}.timer");
    let plain = format!("{base}.timer");
    let qualified_exists = unit_exists(&qualified);
    let plain_exists = if plain == qualified { qualified_exists } else { unit_exists(&plain) };
    choose_timer(base, sfx, qualified_exists, plain_exists)
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

/// The three planes a unit can belong to, declared by the unit itself as `Plane=` in an
/// `[X-Spira]` section (systemd ignores `X-` sections), never listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Plane {
    Work,
    Observability,
    Maintenance,
}

pub const PLANES: [Plane; 3] = [Plane::Work, Plane::Observability, Plane::Maintenance];

impl Plane {
    pub fn name(self) -> &'static str {
        match self {
            Plane::Work => "work",
            Plane::Observability => "observability",
            Plane::Maintenance => "maintenance",
        }
    }

    pub fn parse(s: &str) -> Option<Plane> {
        PLANES.into_iter().find(|p| p.name() == s.trim())
    }
}

/// The `Plane=` a unit file's text declares in its `[X-Spira]` section. A unit that
/// declares none — or a value no plane has — is `None`; callers treat that as `Work`, so a
/// forgotten declaration is stopped by a halt rather than left running unseen.
pub fn plane_from_unit_text(text: &str) -> Option<Plane> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[X-Spira]";
        } else if in_section {
            if let Some(v) = line.strip_prefix("Plane=") {
                return Plane::parse(v);
            }
        }
    }
    None
}

pub fn plane_of(unit: &str) -> Plane {
    plane_from_unit_text(&run(&["cat", unit])).unwrap_or(Plane::Work)
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
    fn plane_is_read_from_the_x_spira_section_only() {
        let unit = "[Unit]\nPlane=maintenance\n[X-Spira]\nPlane=observability\n[Timer]\nPlane=work\n";
        assert_eq!(plane_from_unit_text(unit), Some(Plane::Observability));
    }

    #[test]
    fn plane_is_none_when_undeclared_or_unknown() {
        assert_eq!(plane_from_unit_text("[Unit]\nDescription=x\n"), None);
        assert_eq!(plane_from_unit_text("[X-Spira]\nPlane=bogus\n"), None);
        assert_eq!(plane_from_unit_text(""), None);
    }

    /// sp-ivfu3-2 (a): the world stopped — the qualified unit is KNOWN to systemd but
    /// disabled and inactive — must still choose the qualified name. Before this bead,
    /// `enumerate_timers` asked `is-enabled`/`is-active` instead of existence, and both
    /// say no for a disabled, inactive unit, so it picked the plain, nonexistent name —
    /// this is the exact state (qualified exists, both probes would say "off") the old
    /// code could not tell apart from "does not exist".
    #[test]
    fn choose_timer_prefers_the_qualified_name_when_it_exists_even_disabled_and_inactive() {
        assert_eq!(choose_timer("spira-sentinel", "-prod", true, false), Ok("spira-sentinel-prod.timer".to_string()));
    }

    #[test]
    fn choose_timer_falls_back_to_the_plain_name_when_only_it_exists() {
        assert_eq!(choose_timer("spira-sentinel", "-prod", false, true), Ok("spira-sentinel.timer".to_string()));
    }

    /// sp-ivfu3-2 (b): neither form is known to systemd at all — a named refusal, never a
    /// guess at either name.
    #[test]
    fn choose_timer_refuses_named_when_neither_exists() {
        let err = choose_timer("spira-sentinel", "-prod", false, false).unwrap_err();
        assert!(err.contains("spira-sentinel-prod.timer") && err.contains("spira-sentinel.timer"), "{err}");
    }

    /// An empty suffix makes the qualified and plain forms identical — refusing must not
    /// repeat the same name twice as if two different units were tried.
    #[test]
    fn choose_timer_refuses_once_when_the_suffix_is_empty() {
        assert_eq!(choose_timer("spira-sentinel", "", false, false), Err("spira-sentinel.timer: no such systemd unit".to_string()));
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
