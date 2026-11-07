//! systemd access and the pure decisions world.sh makes over it — split apart exactly the
//! way world.sh's own `WORLD_LIB=1` split existed for, so a test drives the decision
//! functions directly against fixture state without spawning a process or touching a real
//! systemd (docs/test-plan/instance-lifecycle.md §5).

use std::env;

/// `$SPIRA_SYSTEMCTL`, defaulting to `systemctl` — the same seam name world.sh and
/// sentinel.sh both carry, so one stub covers every caller in a test.
// FORK-EDGE (every runner below): needs Units list-by-glob, unit-file existence, cat, is-enabled,
// show Result/MainPID and starting an installed unit, plus a config-selected test double — seven
// shell suites stub SPIRA_SYSTEMCTL, and a D-Bus backend would act on the live user manager.
pub fn systemctl_bin() -> String {
    env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string())
}

/// Run `$SPIRA_SYSTEMCTL --user <args>`, returning trimmed stdout (empty on any failure —
/// world.sh's own calls are all `2>/dev/null` and tolerate a missing systemctl).
pub fn run(args: &[&str]) -> String {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    // FORK-EDGE: systemctl, see systemctl_bin.
    spira_config::bounded::bounded(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Whether `systemctl --user` can reach the user manager at all. `is-system-running` exits
/// nonzero for a merely degraded manager, so only systemctl's own connect failure counts;
/// without this check an unreachable bus reads as every unit being absent.
pub fn bus_unreachable() -> Option<String> {
    // FORK-EDGE: systemctl, see systemctl_bin.
    let o = spira_config::bounded::bounded(systemctl_bin()).args(["--user", "is-system-running"]).output().ok()?;
    let err = String::from_utf8_lossy(&o.stderr);
    err.contains("Failed to connect").then(|| format!("cannot reach the systemd user bus: {}", err.trim()))
}

/// Run `$SPIRA_SYSTEMCTL --user <args>`, returning only whether it exited zero — for an
/// action (`stop`/`start`) where bash's own `&&` gates the "stopped"/"started" line on the
/// command's exit status, not on any output.
pub fn run_ok(args: &[&str]) -> bool {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    // FORK-EDGE: systemctl, see systemctl_bin.
    spira_config::bounded::bounded(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Same as [`run`] but returning every stdout line, for a `list-units`/`list-unit-files` call.
pub fn run_lines(args: &[&str]) -> Vec<String> {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    // FORK-EDGE: systemctl, see systemctl_bin.
    spira_config::bounded::bounded(systemctl_bin())
        .args(&a)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The `list-units` patterns that together name every dwarf unit: the `spira-dwarf-*` units
/// started by name and the units the scheduler's `sentinel-call` starts under the naming rule
/// (`runtime::units::dwarf_globs`).
pub fn dwarf_globs() -> Vec<String> {
    runtime::units::dwarf_globs().to_vec()
}

/// Parse `list-units` lines into the live dwarf units, by either name. Header-less,
/// column-split: first field is the unit, the third is ACTIVE; liveness is
/// `runtime::units::is_live_dwarf`, the rule the fleet ceiling counts by.
pub fn parse_dwarf_units(lines: &[String]) -> Vec<String> {
    lines.iter().filter_map(|l| runtime::units::live_dwarf_in_list_line(l)).map(str::to_string).collect()
}

/// dwarves.sh's "live now": the live dwarves among `list-units` lines over [`dwarf_globs`].
pub fn count_live_dwarves(lines: &[String]) -> usize {
    parse_dwarf_units(lines).len()
}

/// Live dwarves according to systemd — the authority; a /proc argv scan is blind to any
/// dwarf whose command line is not the script path.
pub fn live_dwarf_units() -> Vec<String> {
    let globs = dwarf_globs();
    let mut args = vec!["list-units"];
    args.extend(globs.iter().map(String::as_str));
    args.extend(["--no-legend", "--plain", "--no-pager"]);
    parse_dwarf_units(&run_lines(&args))
}

/// `is-active` / `is-enabled`-style column 1 of a `list-units --no-legend` line.
pub fn first_field(line: &str) -> Option<&str> {
    line.split_whitespace().next()
}

/// A timer subject, stripped of `.timer` and, if present, its instance suffix — the
/// normalisation applied before comparing against a ctrl suspension key.
pub fn subject_of(timer: &str, instance_suffix_bare: &str) -> String {
    let base = timer.strip_suffix(".timer").unwrap_or(timer);
    if instance_suffix_bare.is_empty() {
        base.to_string()
    } else {
        base.strip_suffix(&format!("-{instance_suffix_bare}")).unwrap_or(base).to_string()
    }
}

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

/// Whether a unit file's text declares `Type=oneshot`: a unit run to completion by a timer,
/// not a daemon kept up.
pub fn is_oneshot(text: &str) -> bool {
    text.lines().any(|l| l.trim() == "Type=oneshot")
}

pub fn plane_of(unit: &str) -> Plane {
    plane_from_unit_text(&run(&["cat", unit])).unwrap_or(Plane::Work)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dwarf_units_keeps_only_active_dwarf_services() {
        let l: Vec<String> = [
            "spira-dwarf-sp-1.service loaded active running dwarf sp-1",
            "● spira-dwarf-prod-sp-2.service loaded failed failed dwarf sp-2",
            "spira-dwarf-sp-3.service loaded activating start dwarf sp-3",
            "spira-loom.service loaded active running loom",
            "inactive",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(parse_dwarf_units(&l), vec!["spira-dwarf-sp-1.service", "spira-dwarf-sp-3.service"]);
        let own = vec!["spira-run-sentinel_2dcall-builder_2d1_2d0.service loaded active running spira task".to_string()];
        assert_eq!(parse_dwarf_units(&own).len(), 1, "a dwarf the scheduler started is a dwarf");
    }

    #[test]
    fn subject_of_strips_timer_and_instance() {
        assert_eq!(subject_of("spira-groom-prod.timer", "prod"), "spira-groom");
        assert_eq!(subject_of("spira-groom.timer", "prod"), "spira-groom");
        assert_eq!(subject_of("spira-groom.timer", ""), "spira-groom");
    }

    #[test]
    fn plane_is_read_from_the_x_spira_section_only() {
        let unit = "[Unit]\nPlane=maintenance\n[X-Spira]\nPlane=observability\n[Timer]\nPlane=work\n";
        assert_eq!(plane_from_unit_text(unit), Some(Plane::Observability));
    }

    #[test]
    fn is_oneshot_reads_the_type_line() {
        assert!(is_oneshot("[Service]\nType=oneshot\nExecStart=x\n"));
        assert!(!is_oneshot("[Service]\nType=simple\n"));
        assert!(!is_oneshot("[Service]\n# Type=oneshot\n"));
    }

    #[test]
    fn plane_is_none_when_undeclared_or_unknown() {
        assert_eq!(plane_from_unit_text("[Unit]\nDescription=x\n"), None);
        assert_eq!(plane_from_unit_text("[X-Spira]\nPlane=bogus\n"), None);
        assert_eq!(plane_from_unit_text(""), None);
    }
}
