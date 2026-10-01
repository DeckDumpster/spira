//! `bead event` — `spira/lib.sh`'s `spira_event`: a per-(kind, target) rate-limited line in
//! `$SPIRA_RUN/events.log`, with a suppressed run's count ridden out on the next emission
//! rather than dropped. Ported from `spira_event` (sp-ogu8x, wave 4.24, family Z — see
//! `wave4-decomposition.md` row 24). Contract and parity notes: `DESIGN.md` "event".
//!
//! This is also the CANONICAL home now: `strand/src/check.rs` carried its own, independent
//! Rust copy of this exact algorithm (written before family Z had an owning crate, to emit
//! `branch.reclaimed`). It now calls [`emit`] instead of keeping a second copy — two
//! writers of the same `events.log`/`events/<key>` shapes is exactly the drift this bead
//! exists to retire, since census/tsd/cockpit readers parse what either one writes.

use std::fs;
use std::io::Write;
use std::path::Path;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm). Lets
/// `utc_stamp`/`utc_hhmm` below format a UTC timestamp with no date crate and no `date(1)`
/// subprocess, matching `strand/src/timefmt.rs`'s copy byte for byte (same algorithm, kept
/// as a second small copy rather than a shared crate — see DESIGN.md "Decisions").
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_stamp(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// `HH:MMZ`.
fn utc_hhmm(t: i64) -> String {
    let s = t.rem_euclid(86400);
    format!("{:02}:{:02}Z", s / 3600, s % 3600 / 60)
}

/// The bash's `tr -c 'a-zA-Z0-9._@-' '_'` over `"<kind>@<target-or-plan>"`.
fn sanitize_key(kind: &str, target: &str) -> String {
    let target = if target.is_empty() { "plan" } else { target };
    format!("{kind}@{target}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') { c } else { '_' })
        .collect()
}

/// `spira_event <kind> <target|-> <title> [detail]`, ported. `now` governs the cooldown
/// decision and what is stamped into the cooldown file — the same two things
/// `${SPIRA_NOW:-$(date -u +%s)}` governed in the bash. The LOGGED timestamp column does
/// not take `now`: like the bash's own fresh `$(date -u '+%Y-%m-%dT%H:%M:%SZ')` call, it is
/// always the real wall clock (see DESIGN.md "event" — "the logged timestamp is not the
/// injected clock").
///
/// Returns `Err(())` for exactly the two cases the bash `return 1`s on: an empty `kind` or
/// `title`, or a `run_dir/events` it cannot create. Every later failure (the log append)
/// is swallowed, as the bash's own `|| true` swallows it — `Ok(())` either way.
pub fn emit(run_dir: &Path, cooldown: i64, now: i64, kind: &str, target: &str, title: &str, detail: &str) -> Result<(), ()> {
    if kind.is_empty() || title.is_empty() {
        return Err(());
    }
    let target = if target == "-" { "" } else { target };
    let dir = run_dir.join("events");
    if fs::create_dir_all(&dir).is_err() {
        return Err(());
    }

    // Prune cooldown files well past their window — real wall-clock age, not `now`: the
    // bash's `find -mmin` reads each file's real mtime against the real clock regardless
    // of `$SPIRA_NOW`, so a test that fakes `now` does not also fake which files are stale.
    let minutes = (cooldown * 2) / 60 + 1;
    let max_age = std::time::Duration::from_secs((minutes.max(1) as u64) * 60);
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let old = e
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|el| el > max_age);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    let key = sanitize_key(kind, target);
    let f = dir.join(&key);
    let (last, supp) = fs::read_to_string(&f)
        .ok()
        .map(|s| {
            let mut it = s.split_whitespace();
            (
                it.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
                it.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));

    if last > 0 && now - last < cooldown {
        let _ = fs::write(&f, format!("{last} {}\n", supp + 1));
        return Ok(());
    }

    let mut title = title.to_string();
    if supp > 0 {
        title.push_str(&format!(" (+{supp} more since {})", utc_hhmm(last)));
    }
    let _ = fs::write(&f, format!("{now} 0\n"));

    let mut line = format!(
        "{}\tkind: {}\ttarget: {}\t{}",
        utc_stamp(wall_clock_now()),
        kind,
        if target.is_empty() { "-" } else { target },
        title
    );
    if !detail.is_empty() {
        line.push('\t');
        line.push_str(detail);
    }
    line.push('\n');
    if let Ok(mut fh) = fs::OpenOptions::new().create(true).append(true).open(run_dir.join("events.log")) {
        let _ = fh.write_all(line.as_bytes());
    }
    Ok(())
}

/// The real wall clock, never `$SPIRA_NOW` — see `emit`'s doc comment. Kept as its own
/// function (rather than inlined) so a test can see exactly where the bash's "a fresh
/// `date -u` call" lands in this port.
fn wall_clock_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("bead-event-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn events_log(dir: &Path) -> String {
        fs::read_to_string(dir.join("events.log")).unwrap_or_default()
    }

    fn rows(dir: &Path) -> usize {
        events_log(dir).lines().filter(|l| !l.is_empty()).count()
    }

    #[test]
    fn a_first_outcome_is_recorded_whole() {
        let d = tmp("first");
        emit(&d, 3600, 1000, "bead.landed", "sp-x", "landed spira/sp-x on brain's main", "merged as abc1234").unwrap();
        assert_eq!(rows(&d), 1);
        let log = events_log(&d);
        assert!(log.contains("kind: bead.landed"));
        assert!(log.contains("target: sp-x"));
        assert!(log.contains("landed spira/sp-x on brain's main"));
        assert!(log.contains("merged as abc1234"));
        assert!(!log.contains("mail"));
    }

    #[test]
    fn a_plan_level_outcome_carries_a_dash_for_target() {
        let d = tmp("plan");
        emit(&d, 3600, 1000, "spira.note", "-", "the plan moved", "").unwrap();
        assert!(events_log(&d).contains("target: -"));
    }

    #[test]
    fn a_repeat_inside_the_window_is_not_a_second_row() {
        let d = tmp("repeat");
        emit(&d, 3600, 1000, "branch.reclaimed", "sp-b0c", "reclaimed sp-b0c — reclaim 1", "").unwrap();
        assert_eq!(rows(&d), 1);
        emit(&d, 3600, 1001, "branch.reclaimed", "sp-b0c", "reclaimed sp-b0c — reclaim 2", "").unwrap();
        assert_eq!(rows(&d), 1);
    }

    #[test]
    fn a_storm_is_one_row_but_distinct_beads_are_not_suppressed() {
        let d = tmp("storm");
        for n in 1..=26 {
            emit(&d, 3600, 1000 + n, "branch.reclaimed", "sp-b0c", &format!("reclaimed sp-b0c — reclaim {n}"), "").unwrap();
        }
        assert_eq!(rows(&d), 1, "a 26-reclaim storm is one row, not 26");

        let d2 = tmp("storm-distinct");
        for n in 1..=6 {
            emit(&d2, 3600, 1000, "branch.reclaimed", &format!("sp-storm{n}"), &format!("reclaimed sp-storm{n}"), "").unwrap();
        }
        assert_eq!(rows(&d2), 6, "six different beads reclaiming is six rows");
    }

    #[test]
    fn a_second_kind_on_the_same_bead_is_not_a_repeat() {
        let d = tmp("second-kind");
        emit(&d, 3600, 1000, "bead.landed", "sp-y", "landed sp-y", "").unwrap();
        emit(&d, 3600, 1000, "bead.reopened", "sp-y", "reopened sp-y", "").unwrap();
        assert_eq!(rows(&d), 2);
    }

    #[test]
    fn suppressed_rides_out_on_the_next_emission_with_a_count() {
        let d = tmp("suppressed");
        emit(&d, 1, 1000, "branch.reclaimed", "sp-b0c", "reclaimed sp-b0c — reclaim 1", "").unwrap();
        for _ in 0..3 {
            emit(&d, 999, 1000, "branch.reclaimed", "sp-b0c", "reclaimed sp-b0c — repeat", "").unwrap();
        }
        assert_eq!(rows(&d), 1, "three repeats are held");
        emit(&d, 1, 1002, "branch.reclaimed", "sp-b0c", "reclaimed sp-b0c — reclaim 5", "").unwrap();
        assert_eq!(rows(&d), 2, "the expired window emits again");
        assert!(events_log(&d).contains("+3 more since"), "carrying what it held back");
    }

    #[test]
    fn the_window_belongs_to_the_first_emission_not_the_last_suppression() {
        let d = tmp("hot");
        emit(&d, 2, 1000, "branch.reclaimed", "sp-hot", "reclaimed sp-hot — 1", "").unwrap();
        for n in 1..=4 {
            emit(&d, 2, 1000 + n, "branch.reclaimed", "sp-hot", &format!("reclaimed sp-hot — {}", n + 1), "").unwrap();
        }
        assert!(rows(&d) >= 2, "a loop faster than the window still surfaces");
    }

    #[test]
    fn an_empty_kind_or_title_is_refused_and_nothing_is_written() {
        let d = tmp("refused");
        assert_eq!(emit(&d, 3600, 1000, "", "sp-x", "no kind", ""), Err(()));
        assert_eq!(rows(&d), 0);
        assert_eq!(emit(&d, 3600, 1000, "bead.landed", "sp-x", "", ""), Err(()));
        assert_eq!(rows(&d), 0);
    }

    #[test]
    fn key_sanitization_matches_the_tr_complement_class() {
        assert_eq!(sanitize_key("bead.landed", "sp-x"), "bead.landed@sp-x");
        assert_eq!(sanitize_key("bead.landed", ""), "bead.landed@plan");
        assert_eq!(sanitize_key("a b", "x/y"), "a_b@x_y");
    }

    #[test]
    fn utc_formatting_matches_strands_copy() {
        // 2026-09-29T01:02:03Z, cross-checked against strand/src/timefmt.rs's own test.
        let t = 1790643723;
        assert_eq!(utc_stamp(t), "2026-09-29T01:02:03Z");
        assert_eq!(utc_hhmm(t), "01:02Z");
    }
}
