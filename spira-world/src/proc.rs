//! /proc scanning — world.sh's `argv_has`, `live_aeons`, `live_workers`, and the
//! `work_services` exclusion, all NEVER `pgrep -f` (law-pgrep-nominates: a substring match
//! against the whole command line nominates any shell whose `-c` text merely mentions the
//! path, including this process's own). Every predicate here matches an EXACT argv element,
//! read from `/proc/<pid>/cmdline`, which is NUL-separated and therefore immune to the
//! quoting tricks a substring match falls for.

use std::path::Path;

/// `argv_has` — true when one of `wants` is an exact element of `cmdline` (a raw
/// `/proc/<pid>/cmdline` buffer: NUL-separated, NUL-terminated). Pure and fast: no I/O, so
/// every caller below can be tested without a real `/proc`.
pub fn argv_has(cmdline: &[u8], wants: &[&str]) -> bool {
    cmdline
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .any(|tok| wants.iter().any(|w| tok == w.as_bytes()))
}

fn read_cmdline(pid_dir: &Path) -> Option<Vec<u8>> {
    std::fs::read(pid_dir.join("cmdline")).ok()
}

/// One aeon match: its pid and the systemd unit `systemctl status <pid>` resolves it to
/// (empty if none — a fixture aeon or one hand-run outside a unit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAeon {
    pub pid: String,
    pub unit: String,
}

/// Scan `/proc/[0-9]*` for a process whose argv contains one of `aeon_paths` — both homes
/// (`$SPIRA_HOME/aeon.sh` / `$SPIRA_PROD/aeon.sh`), the release bin-relative path, and
/// whatever `command -v aeon` resolves to today, so this is blind to neither home in a
/// split checkout nor to the Rust binary replacing the old script. `unit_of` resolves a pid
/// to its systemd unit the same way world.sh does (`systemctl --user status <pid>`, first
/// line, second field) — injected so tests need no real systemd.
pub fn live_aeons(proc_root: &Path, aeon_paths: &[&str], unit_of: impl Fn(&str) -> String) -> Vec<LiveAeon> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else { return out };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().filter(|s| s.chars().all(|c| c.is_ascii_digit())) else { continue };
        let Some(cmdline) = read_cmdline(&e.path()) else { continue };
        if argv_has(&cmdline, aeon_paths) {
            out.push(LiveAeon { pid: pid.to_string(), unit: unit_of(pid) });
        }
    }
    out.sort_by(|a, b| a.pid.cmp(&b.pid));
    out
}

/// Scan `/proc/[0-9]*` for a process whose argv contains one of `worker_paths` (gate.sh /
/// landing-pass, both homes) — the processes that survive a `spira-landing.service` stop
/// if it was killed before they finished.
pub fn live_workers(proc_root: &Path, worker_paths: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else { return out };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().filter(|s| s.chars().all(|c| c.is_ascii_digit())) else { continue };
        let Some(cmdline) = read_cmdline(&e.path()) else { continue };
        if argv_has(&cmdline, worker_paths) {
            out.push(pid.to_string());
        }
    }
    out.sort();
    out
}

/// Is `unit` one of the work services `world.sh stop` excludes deliberately: the operator's
/// own dashboard (`spira-cockpit[-<instance>]`), the query engine it depends on
/// (`spira-loom[-<instance>]`), and the watcher units handled separately by `--hard`
/// (`spira-watch@*`). Pure string matching, ported 1:1 from `work_services`'s `grep -Ev`.
pub fn is_excluded_work_service(unit: &str, instance_suffix: &str) -> bool {
    if unit.starts_with("spira-watch@") {
        return true;
    }
    matches!(
        unit,
        "spira-cockpit.service" | "spira-loom.service"
    ) || unit == format!("spira-cockpit{instance_suffix}.service")
        || unit == format!("spira-loom{instance_suffix}.service")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmdline(parts: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn argv_has_matches_exact_element_only() {
        let c = cmdline(&["bash", "-c", "echo /opt/x/spira/aeon.sh"]);
        // The path only appears inside a -c STRING, never as its own argv element.
        assert!(!argv_has(&c, &["/opt/x/spira/aeon.sh"]));
        let c2 = cmdline(&["/opt/x/spira/aeon.sh", "--bead", "sp-1"]);
        assert!(argv_has(&c2, &["/opt/x/spira/aeon.sh"]));
    }

    #[test]
    fn argv_has_is_immune_to_operator_discussing_the_path() {
        // The scar world.sh's own comment names: a shell whose -c text merely mentions the
        // path must not be nominated.
        let c = cmdline(&["bash", "-c", "cat $SPIRA_PROD/aeon.sh"]);
        assert!(!argv_has(&c, &["/prod/aeon.sh"]));
    }

    #[test]
    fn live_aeons_finds_a_matching_proc_and_resolves_its_unit() {
        let d = testkit::TempDir::new("proc-live-aeons");
        let pid_dir = d.join("4242");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(pid_dir.join("cmdline"), cmdline(&["/opt/x/spira/aeon.sh", "--bead", "sp-1"])).unwrap();
        // A non-numeric entry (e.g. "self") must be skipped without panicking.
        std::fs::create_dir_all(d.join("self")).unwrap();
        let found = live_aeons(&d, &["/opt/x/spira/aeon.sh"], |pid| format!("unit-for-{pid}"));
        assert_eq!(found, vec![LiveAeon { pid: "4242".into(), unit: "unit-for-4242".into() }]);
    }

    #[test]
    fn live_aeons_ignores_non_matching_processes() {
        let d = testkit::TempDir::new("proc-live-aeons");
        let pid_dir = d.join("77");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(pid_dir.join("cmdline"), cmdline(&["sleep", "10"])).unwrap();
        let found = live_aeons(&d, &["/opt/x/spira/aeon.sh"], |_| String::new());
        assert!(found.is_empty());
    }

    #[test]
    fn live_workers_finds_gate_and_landing_pass() {
        let d = testkit::TempDir::new("proc-live-workers");
        let pid_dir = d.join("55");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(pid_dir.join("cmdline"), cmdline(&["/opt/x/spira/gate.sh", "concierge/sp-1", "spira"])).unwrap();
        let found = live_workers(&d, &["/opt/x/spira/gate.sh"]);
        assert_eq!(found, vec!["55".to_string()]);
    }

    #[test]
    fn excluded_work_services_cover_cockpit_loom_and_watchers() {
        assert!(is_excluded_work_service("spira-cockpit.service", ""));
        assert!(is_excluded_work_service("spira-cockpit-prod.service", "-prod"));
        assert!(is_excluded_work_service("spira-loom-prod.service", "-prod"));
        assert!(is_excluded_work_service("spira-watch@pr-notify.service", ""));
        assert!(!is_excluded_work_service("spira-landing.service", "-prod"));
        assert!(!is_excluded_work_service("spira-cockpit-prod.service", ""), "wrong instance suffix must not match");
    }
}
