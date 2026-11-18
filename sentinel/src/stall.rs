//! CHECK 2e: READY work and no dwarf called for a window, with nothing in force to explain it,
//! is one ask to the Concierge (law-total-blockage-announces-itself). The ask carries a
//! `MOOT-WHEN:` predicate, so the moot-sweep withdraws it once calling resumes; recovery is
//! never announced.

use dwarf::capacity::PauseState;

use crate::model::LcRow;
use crate::pass::Sentinel;

pub const STALL_SECS: i64 = 30 * 60;
/// A pause that explains a stall only until the stall outlasts a full usage window.
pub const PAUSE_WINDOW_SECS: i64 = 5 * 3600;
pub const SUBJECT: &str = "No dwarf has been called while READY work waits";

pub struct Inputs<'a> {
    pub now: i64,
    pub ready: usize,
    pub last_born: Option<&'a Born>,
    pub pause: &'a PauseState,
    pub halted: bool,
    pub draining: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Born {
    pub at: i64,
    pub line: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Stall {
    pub minutes: i64,
    pub ready: usize,
    pub last_born: String,
    pub pause: String,
}

pub fn last_born(ledger: &str) -> Option<Born> {
    ledger.lines().rev().find_map(|line| {
        let mut w = line.split_whitespace();
        let at = w.next().and_then(spira_desired_state::section::parse_rfc3339)?;
        (w.next() == Some("born")).then(|| Born { at: at as i64, line: line.trim().to_string() })
    })
}

pub fn describe_pause(p: &PauseState, now: i64) -> String {
    match p {
        PauseState::Open => "no pause file".into(),
        PauseState::Unknown => "pause file present but unreadable".into(),
        PauseState::Paused { until, why } if *until > now => {
            format!("pause in force until {} ({why})", crate::host::utc(*until))
        }
        PauseState::Paused { until, why } => {
            format!("pause file present, EXPIRED: until {} was {}s ago ({why})", crate::host::utc(*until), now - until)
        }
    }
}

pub fn assess(i: &Inputs) -> Option<Stall> {
    let born = i.last_born?;
    let stalled = i.now - born.at;
    if i.ready == 0 || i.halted || i.draining || stalled <= STALL_SECS {
        return None;
    }
    let in_force = matches!(i.pause, PauseState::Paused { until, .. } if *until > i.now);
    if in_force && stalled <= PAUSE_WINDOW_SECS {
        return None;
    }
    Some(Stall { minutes: stalled / 60, ready: i.ready, last_born: born.line.clone(), pause: describe_pause(i.pause, i.now) })
}

/// Exits 0 once a `born` line later than `since` is in the ledger; any failure to read it exits
/// non-zero, so a broken probe never reads as recovery.
pub fn moot_predicate(ledger: &std::path::Path, since: i64) -> String {
    let path = ledger.to_string_lossy().replace('\'', r"'\''");
    format!("awk -v t={} '$2 == \"born\" && $1 > t {{ f = 1 }} END {{ exit !f }}' '{path}'", crate::host::utc(since))
}

pub fn body(s: &Stall, predicate: &str) -> String {
    format!(
        "## Question\n{SUBJECT}\n\n## Default\nfind why `call` is not placing a dwarf (world, claims lane, pause file, fleet ceiling) and fix that\n\n\
         stalled: {} minutes\nready: {}\nlast born: {}\npause: {}\n\nMOOT-WHEN: {predicate}\n",
        s.minutes, s.ready, s.last_born, s.pause
    )
}

impl<'a> Sentinel<'a> {
    pub fn check2e(&self, rows: &[LcRow]) {
        let now = self.h.now();
        let ledger_path = self.cfg.run.join("dwarf-ledger.log");
        let born = last_born(&std::fs::read_to_string(&ledger_path).unwrap_or_default());
        let pause = dwarf::capacity::pause_state(&self.cfg.capacity_pause, now);
        let ready = rows.iter().filter(|r| r.state == "READY" && r.holds.is_empty()).count();
        let inputs = Inputs {
            now,
            ready,
            last_born: born.as_ref(),
            pause: &pause,
            halted: self.cfg.run.join("world.halted").is_file(),
            draining: self.cfg.run.join("world.draining").is_file(),
        };
        let Some(stall) = assess(&inputs) else { return };
        self.log(&format!("CHECK2e STALLED: {} minutes, {} READY, {}", stall.minutes, stall.ready, stall.pause));
        if self.ask_already_open(SUBJECT) {
            return;
        }
        let body = body(&stall, &moot_predicate(&ledger_path, now));
        let default = "find why `call` is not placing a dwarf and fix that";
        if self.mail("Sentinel <sentinel@spira>", SUBJECT, default, &body, "", true) {
            self.act("escalated: READY work and no dwarf called");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn born(ago: i64) -> Born {
        Born { at: NOW - ago, line: format!("{} born builder 7", crate::host::utc(NOW - ago)) }
    }

    fn run(ready: usize, ago: i64, pause: PauseState, halted: bool) -> Option<Stall> {
        let b = born(ago);
        assess(&Inputs { now: NOW, ready, last_born: Some(&b), pause: &pause, halted, draining: false })
    }

    fn expired() -> PauseState {
        PauseState::Paused { until: NOW - 3 * 3600, why: "usage".into() }
    }

    #[test]
    fn ready_work_and_a_quiet_ledger_past_the_window_with_an_expired_pause_is_a_stall_that_names_it() {
        let s = run(15, STALL_SECS + 60, expired(), false).expect("a stall");
        assert_eq!((s.ready, s.minutes), (15, 31));
        assert!(s.pause.contains("EXPIRED"), "{}", s.pause);
        assert!(s.last_born.contains("born builder 7"));
    }

    #[test]
    fn a_pause_in_force_a_halted_world_a_drain_or_no_ready_work_explain_the_quiet() {
        let live = PauseState::Paused { until: NOW + 600, why: "usage".into() };
        assert!(run(15, STALL_SECS + 60, live, false).is_none());
        assert!(run(15, STALL_SECS + 60, expired(), true).is_none());
        let b = born(STALL_SECS + 60);
        let drain = Inputs { now: NOW, ready: 15, last_born: Some(&b), pause: &expired(), halted: false, draining: true };
        assert!(assess(&drain).is_none());
        assert!(run(0, STALL_SECS + 60, expired(), false).is_none());
        assert!(run(15, STALL_SECS, expired(), false).is_none(), "exactly the window is not past it");
    }

    #[test]
    fn a_pause_that_outlasts_a_full_window_no_longer_explains_the_quiet() {
        let live = || PauseState::Paused { until: NOW + 600, why: "usage".into() };
        assert!(run(3, PAUSE_WINDOW_SECS, live(), false).is_none());
        assert!(run(3, PAUSE_WINDOW_SECS + 60, live(), false).is_some());
    }

    #[test]
    fn an_unreadable_pause_file_is_never_an_explanation() {
        assert!(run(3, STALL_SECS + 60, PauseState::Unknown, false).is_some());
    }

    #[test]
    fn an_empty_ledger_is_no_evidence() {
        assert_eq!(last_born("garbage\n"), None);
        assert!(assess(&Inputs { now: NOW, ready: 3, last_born: None, pause: &PauseState::Open, halted: false, draining: false }).is_none());
    }

    #[test]
    fn the_last_born_line_is_the_latest_one() {
        let l = "2026-11-18T10:00:00Z born builder 1\n2026-11-18T10:20:00Z born builder 2\n2026-11-18T10:21:00Z done builder sp-a rc=0\n";
        assert_eq!(last_born(l).unwrap().line, "2026-11-18T10:20:00Z born builder 2");
    }

    #[test]
    fn the_predicate_exits_zero_only_once_a_dwarf_is_born_after_the_ask() {
        let d = testkit::TempDir::new("stall-moot");
        let ledger = d.join("it's-ledger.log");
        let since = NOW;
        // FORK-EDGE: the predicate is a shell command by contract, run here as the moot-sweep runs it
        let sh = |p: &str| std::process::Command::new("bash").arg("-c").arg(p).status().unwrap().code();
        let pred = moot_predicate(&ledger, since);
        assert_eq!(sh(&pred), Some(2), "no ledger: the probe fails, which is not recovery");
        std::fs::write(&ledger, format!("{} born builder 1\n", crate::host::utc(since - 60))).unwrap();
        assert_eq!(sh(&pred), Some(1), "a born before the ask is not recovery");
        std::fs::write(&ledger, format!("{} born builder 1\n{} born builder 2\n", crate::host::utc(since - 60), crate::host::utc(since + 60))).unwrap();
        assert_eq!(sh(&pred), Some(0));
    }
}
