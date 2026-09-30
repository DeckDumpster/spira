//! A faithful, native port of lib.sh's `aeons_live_total` (DESIGN.md) — NOT a
//! reimplementation of `world.sh`'s `live_aeons`, which is a different count for a
//! different purpose (per-pid identity, to slay one) and stays in `proc.rs`. This one
//! exists because `aeons.sh status` is a DISPLAY of the exact number the summon gate
//! itself binds on — `lib.sh`'s own comment: "duplicating that logic here is how the two
//! would drift" — so it has to be the same query, not a proc scan that happens to agree
//! most of the time. Ported rather than called through a seam because it is two small,
//! self-contained branches (a systemctl count, and a test-only pidfile fallback) with no
//! dependency on a `.fayth` file's own bash evaluation.

use std::path::Path;

/// The production branch: how many `spira-aeon-*` units systemd knows about right now.
/// `unit_count` is injected so a test supplies a fixed list instead of a real systemd.
pub fn live_total_systemd(unit_count: impl Fn(&str) -> usize) -> u32 {
    unit_count("spira-aeon-*") as u32
}

/// `aeon_alive`: true when `pidfile` names a pid that is (a) still running and (b) still
/// running something that is actually an aeon — `(^|/)aeon( |$)|aeon\.sh` against the
/// space-joined cmdline, ported directly rather than pulled in as a `regex` dependency for
/// one fixed pattern.
fn matches_aeon_cmd(cmd: &str) -> bool {
    if cmd.contains("aeon.sh") {
        return true;
    }
    let bytes = cmd.as_bytes();
    for (i, _) in cmd.match_indices("aeon") {
        let before_ok = i == 0 || bytes[i - 1] == b'/';
        let after = i + "aeon".len();
        let after_ok = after == bytes.len() || bytes[after] == b' ';
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// `aeon_alive <pidfile>` — `pid_alive` and `read_cmdline` are injected so this is testable
/// without a real `/proc`.
pub fn aeon_alive(pidfile: &Path, pid_alive: impl Fn(&str) -> bool, read_cmdline: impl Fn(&str) -> Option<String>) -> bool {
    let Ok(pid) = std::fs::read_to_string(pidfile) else { return false };
    let pid = pid.trim();
    if pid.is_empty() || !pid_alive(pid) {
        return false;
    }
    let Some(cmd) = read_cmdline(pid) else { return false };
    matches_aeon_cmd(&cmd)
}

/// The test-only fallback branch: count `$SPIRA_RUN/aeon-*.pid` whose `aeon_alive` is
/// true, removing any pidfile that fails the check — matching lib.sh's own litter cleanup.
pub fn live_total_pidfiles(run_dir: &Path, pid_alive: impl Fn(&str) -> bool, read_cmdline: impl Fn(&str) -> Option<String>) -> u32 {
    let mut n = 0u32;
    let Ok(entries) = std::fs::read_dir(run_dir) else { return 0 };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.starts_with("aeon-") || !s.ends_with(".pid") {
            continue;
        }
        let p = e.path();
        if aeon_alive(&p, &pid_alive, &read_cmdline) {
            n += 1;
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_aeon_cmd_matches_bare_and_slash_and_dotsh_forms() {
        assert!(matches_aeon_cmd("aeon --bead sp-1"));
        assert!(matches_aeon_cmd("/opt/x/bin/aeon --bead sp-1"));
        assert!(matches_aeon_cmd("bash /opt/x/spira/aeon.sh --bead sp-1"));
        assert!(matches_aeon_cmd("aeon"));
        assert!(!matches_aeon_cmd("bash -c cat aeon-notes.txt"));
        assert!(!matches_aeon_cmd("myaeon --bead sp-1"));
    }

    #[test]
    fn aeon_alive_false_for_missing_or_dead_or_wrong_process() {
        let d = testkit::TempDir::new("fleet-alive");
        let pf = d.join("aeon-builder-sp1.pid");
        std::fs::write(&pf, "123\n").unwrap();
        assert!(!aeon_alive(&pf, |_| false, |_| Some("aeon --bead sp-1".into())), "dead pid");
        assert!(!aeon_alive(&pf, |_| true, |_| Some("sleep 10".into())), "alive but not an aeon");
        assert!(aeon_alive(&pf, |_| true, |_| Some("aeon --bead sp-1".into())), "alive and an aeon");
    }

    #[test]
    fn live_total_pidfiles_counts_only_alive_aeons_and_sweeps_the_rest() {
        let d = testkit::TempDir::new("fleet-total");
        std::fs::write(d.join("aeon-a.pid"), "1\n").unwrap();
        std::fs::write(d.join("aeon-b.pid"), "2\n").unwrap();
        std::fs::write(d.join("not-an-aeon-pidfile"), "3\n").unwrap();
        let n = live_total_pidfiles(&d, |pid| pid == "1", |_| Some("aeon --bead sp-1".into()));
        assert_eq!(n, 1);
        assert!(d.join("aeon-a.pid").exists());
        assert!(!d.join("aeon-b.pid").exists(), "dead pidfile is swept");
    }

    #[test]
    fn live_total_systemd_counts_whatever_the_probe_returns() {
        assert_eq!(live_total_systemd(|pat| if pat == "spira-aeon-*" { 3 } else { 0 }), 3);
    }
}
