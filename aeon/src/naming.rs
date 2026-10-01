//! Aeon identity (lib.sh family E, wave 4.23 sp-0ffox: "plus aeon names → aeon").
//!
//! `aeon_name_take`/`aeon_named` had exactly one caller each outside their own definition:
//! this crate's own seam calls (`run.rs::take_name`, `sweep.rs::sweep`) for the former, and
//! `cockpit-collect` (across the crate boundary) for the latter — confirmed by a whole-tree
//! grep (git grep incl. Rust literals and fixtures). `aeon_name_take` is RETIRED from
//! lib.sh outright and switched in-process here (no other caller ever reached it through
//! the seam); `aeon_named` keeps a one-line lib.sh shim onto a new `aeon aeon-named <pidfile>`
//! subcommand, since cockpit-collect still needs it by name.
//!
//! AN AEON HAS A NAME. Every instance used to be `aeon-builder`, indistinguishable in the
//! pane, in `bd` history and in the commit graph. Named for the aeons of Spira. The prefix
//! stays `aeon-` so every existing count that greps for it still works.

use std::path::Path;

pub const NAMES: &[&str] = &["valefor", "ifrit", "ixion", "shiva", "bahamut", "yojimbo", "anima", "cindy", "sandy", "mindy"];

/// Names currently held by a LIVE aeon: for every `aeon-*.name` file under `run`, its
/// content counts only if the sibling `.pid` file names a live aeon (lib.sh:
/// `aeon_name_take`'s own live-name scan — a dead aeon's name is free immediately, not
/// reused by its own cursor advance either).
fn live_names(run: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(run) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("aeon-") && n.ends_with(".name"))
        })
        .filter_map(|p| {
            let pidfile = p.with_extension("pid");
            if pidfile.is_file() && strand::probe::aeon_alive(&pidfile) {
                std::fs::read_to_string(&p).ok()
            } else {
                None
            }
        })
        .collect()
}

/// `aeon_name_take <fayth>` — a name not currently held by a live aeon. `fayth` is accepted
/// (matching lib.sh's signature) but unused: names are global across the fleet, not
/// per-persona, exactly as the bash original.
///
/// A CURSOR, NOT "first free": picking the first free name meant two consecutive sessions
/// were both "valefor", hiding the fact that a bead had been dropped with work in flight —
/// a cursor makes consecutive aeons distinguishable even when nothing else is running.
pub fn aeon_name_take(run: &Path, _fayth: &str) -> String {
    let live = live_names(run);
    let cursor_path = run.join(".aeon-name-cursor");
    let last: usize = std::fs::read_to_string(&cursor_path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let total = NAMES.len();
    for tries in 0..total {
        let cursor = (last + 1 + tries) % total;
        let name = NAMES[cursor];
        if !live.iter().any(|n| n == name) {
            let _ = std::fs::write(&cursor_path, cursor.to_string());
            return name.to_string();
        }
    }
    // More concurrent aeons than names is not an error, just unusual; fall back to a
    // numbered one rather than reusing a name and making two of them indistinguishable —
    // lib.sh's `date +%s | tail -c 4`.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("aeon{}", secs % 10000)
}

/// `aeon_named <pidfile>` — the name held by that aeon, if any, else `?`.
pub fn aeon_named(pidfile: &Path) -> String {
    std::fs::read_to_string(pidfile.with_extension("name")).unwrap_or_else(|_| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn fresh(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("aeon-naming-{name}"))
    }

    #[test]
    fn a_pristine_run_dir_takes_the_name_right_after_the_cursor() {
        let run = fresh("fresh");
        // last=0 (no cursor file) -> cursor=(0+1+0)%10=1 -> NAMES[1] = "ifrit".
        assert_eq!(aeon_name_take(&run, "builder"), "ifrit");
        assert_eq!(std::fs::read_to_string(run.join(".aeon-name-cursor")).unwrap(), "1");
    }

    #[test]
    fn the_cursor_advances_and_never_repeats_the_previous_aeons_name() {
        let run = fresh("advance");
        let first = aeon_name_take(&run, "builder");
        let second = aeon_name_take(&run, "builder");
        assert_ne!(first, second, "consecutive aeons must be distinguishable even with nothing live");
        assert_eq!(first, "ifrit");
        assert_eq!(second, "ixion");
    }

    #[test]
    fn a_name_held_by_a_dead_pid_is_free_immediately() {
        let run = fresh("dead");
        std::fs::write(run.join("aeon-builder-sp-1.name"), "ifrit").unwrap();
        std::fs::write(run.join("aeon-builder-sp-1.pid"), "999999999").unwrap();
        // last=0 -> first try is "ifrit", but that name file's pid is dead, so it is NOT
        // live and "ifrit" remains available.
        assert_eq!(aeon_name_take(&run, "builder"), "ifrit");
    }

    #[test]
    fn a_name_held_by_a_live_aeon_is_skipped() {
        let run = fresh("live");
        let mut child = std::process::Command::new("sleep").arg0("aeon").arg("30").spawn().unwrap();
        std::fs::write(run.join("aeon-builder-sp-1.name"), "ifrit").unwrap();
        let pidfile = run.join("aeon-builder-sp-1.pid");
        std::fs::write(&pidfile, child.id().to_string()).unwrap();
        // Settle the fork/exec race: immediately after spawn(), /proc/<pid>/cmdline can
        // still read empty (exec not yet landed) — wait until it reports this pid alive.
        for _ in 0..200 {
            if strand::probe::aeon_alive(&pidfile) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let name = aeon_name_take(&run, "builder");
        assert_ne!(name, "ifrit", "ifrit is live — must not be handed out twice");
        assert_eq!(name, "ixion");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn named_reads_the_sibling_name_file_or_a_question_mark() {
        let run = fresh("named");
        let pf = run.join("aeon-builder-sp-1.pid");
        std::fs::write(&pf, "1").unwrap();
        std::fs::write(run.join("aeon-builder-sp-1.name"), "ixion").unwrap();
        assert_eq!(aeon_named(&pf), "ixion");
        assert_eq!(aeon_named(&run.join("aeon-builder-sp-2.pid")), "?");
    }
}
