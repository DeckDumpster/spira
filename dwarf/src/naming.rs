//! Dwarf identity (lib.sh family E, wave 4.23 sp-0ffox: "plus dwarf names → dwarf").
//!
//! `dwarf_name_take`/`dwarf_named` had exactly one caller each outside their own definition:
//! this crate's own seam calls (`run.rs::take_name`, `sweep.rs::sweep`) for the former, and
//! `cockpit-collect` (across the crate boundary) for the latter — confirmed by a whole-tree
//! grep (git grep incl. Rust literals and fixtures). `dwarf_name_take` is RETIRED from
//! lib.sh outright and switched in-process here (no other caller ever reached it through
//! the seam); `dwarf_named` keeps a one-line lib.sh shim onto a new `dwarf dwarf-named <pidfile>`
//! subcommand, since cockpit-collect still needs it by name.
//!
//! A DWARF HAS A NAME. Every instance used to be `dwarf-builder`, indistinguishable in the
//! pane, in `bd` history and in the commit graph. Named for the dwarves of Spira. The prefix
//! stays `dwarf-` so every existing count that greps for it still works.

use std::path::Path;

pub const NAMES: &[&str] = &["brokkr", "eitri", "dvalin", "andvari", "alviss", "fjalar", "galar", "regin", "nordri", "austri"];

/// Names currently held by a LIVE dwarf: for every `dwarf-*.name` file under `run`, its
/// content counts only if the sibling `.pid` file names a live dwarf (lib.sh:
/// `dwarf_name_take`'s own live-name scan — a dead dwarf's name is free immediately, not
/// reused by its own cursor advance either).
fn live_names(run: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(run) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("dwarf-") && n.ends_with(".name"))
        })
        .filter_map(|p| {
            let pidfile = p.with_extension("pid");
            if pidfile.is_file() && strand::probe::dwarf_alive(&pidfile) {
                std::fs::read_to_string(&p).ok()
            } else {
                None
            }
        })
        .collect()
}

/// `dwarf_name_take <rune>` — a name not currently held by a live dwarf. `rune` is accepted
/// (matching lib.sh's signature) but unused: names are global across the fleet, not
/// per-persona, exactly as the bash original.
///
/// A CURSOR, NOT "first free": picking the first free name meant two consecutive sessions
/// were both "brokkr", hiding the fact that a bead had been dropped with work in flight —
/// a cursor makes consecutive dwarves distinguishable even when nothing else is running.
pub fn dwarf_name_take(run: &Path, _rune: &str) -> String {
    let live = live_names(run);
    let cursor_path = run.join(".dwarf-name-cursor");
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
    // More concurrent dwarves than names is not an error, just unusual; fall back to a
    // numbered one rather than reusing a name and making two of them indistinguishable —
    // lib.sh's `date +%s | tail -c 4`.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("dwarf{}", secs % 10000)
}

/// `dwarf_named <pidfile>` — the name held by that dwarf, if any, else `?`.
pub fn dwarf_named(pidfile: &Path) -> String {
    std::fs::read_to_string(pidfile.with_extension("name")).unwrap_or_else(|_| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    

    fn fresh(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("dwarf-naming-{name}"))
    }

    #[test]
    fn a_pristine_run_dir_takes_the_name_right_after_the_cursor() {
        let run = fresh("fresh");
        // last=0 (no cursor file) -> cursor=(0+1+0)%10=1 -> NAMES[1] = "eitri".
        assert_eq!(dwarf_name_take(&run, "builder"), "eitri");
        assert_eq!(std::fs::read_to_string(run.join(".dwarf-name-cursor")).unwrap(), "1");
    }

    #[test]
    fn the_cursor_advances_and_never_repeats_the_previous_dwarves_name() {
        let run = fresh("advance");
        let first = dwarf_name_take(&run, "builder");
        let second = dwarf_name_take(&run, "builder");
        assert_ne!(first, second, "consecutive dwarves must be distinguishable even with nothing live");
        assert_eq!(first, "eitri");
        assert_eq!(second, "dvalin");
    }

    #[test]
    fn a_name_held_by_a_dead_pid_is_free_immediately() {
        let run = fresh("dead");
        std::fs::write(run.join("dwarf-builder-sp-1.name"), "eitri").unwrap();
        std::fs::write(run.join("dwarf-builder-sp-1.pid"), "999999999").unwrap();
        // last=0 -> first try is "eitri", but that name file's pid is dead, so it is NOT
        // live and "eitri" remains available.
        assert_eq!(dwarf_name_take(&run, "builder"), "eitri");
    }

    #[test]
    fn a_name_held_by_a_live_dwarf_is_skipped() {
        let run = fresh("live");
        std::fs::write(run.join("dwarf-builder-sp-1.name"), "eitri").unwrap();
        let pidfile = run.join("dwarf-builder-sp-1.pid");
        std::fs::write(&pidfile, "1").unwrap();
        strand::probe::write_lease(&pidfile, i64::MAX);
        let name = dwarf_name_take(&run, "builder");
        assert_ne!(name, "eitri", "eitri is live — must not be handed out twice");
        assert_eq!(name, "dvalin");
    }

    #[test]
    fn named_reads_the_sibling_name_file_or_a_question_mark() {
        let run = fresh("named");
        let pf = run.join("dwarf-builder-sp-1.pid");
        std::fs::write(&pf, "1").unwrap();
        std::fs::write(run.join("dwarf-builder-sp-1.name"), "dvalin").unwrap();
        assert_eq!(dwarf_named(&pf), "dvalin");
        assert_eq!(dwarf_named(&run.join("dwarf-builder-sp-2.pid")), "?");
    }
}
