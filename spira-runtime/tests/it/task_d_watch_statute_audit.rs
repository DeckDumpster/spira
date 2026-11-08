//! T1–T6 for `watch.statute_audit` (contract
//! `wiki/projects/spira/designs/tasks/watch-statute-audit.md`), a P4 weekly task. The behaviour
//! tests are red until the task is ported: the stub's `decide` panics, the rig quarantines the
//! stream, and `assert_healthy` turns that into the failure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_statute_audit::*;

/// 2026-11-03, a Tuesday, 00:00 UTC.
const TUESDAY: u64 = 1_793_664_000_000;

fn config() -> Config {
    Config { db: "/db".into(), home_repo: "spira".into(), persona: PERSONA.into(), max: MAX }
}

/// The release of `rule/src/audit.rs`'s tests: `bead.sh` knows `file` and `lint`.
fn release() -> BTreeMap<String, String> {
    [("bead.sh".to_string(), "#   bead.sh file \"<title>\"\n#   bead.sh lint".to_string())].into()
}

fn book(bad: usize) -> Snapshot {
    let mut memories: BTreeMap<String, String> = [("law-good".to_string(), "run bead.sh lint".to_string())].into();
    memories.extend((0..bad).map(|i| (format!("law-bad-{i}"), format!("run bead.sh drop{i} <id>"))));
    Snapshot { memories, release: release(), open_titles: vec![], persona: PERSONA.into(), repo: "spira".into() }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchStatuteAudit::boot();
    let mut r = TaskRig::new(seed, WatchStatuteAudit::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(FILE_BEAD, |_| Ok("sp-new".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn filed(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == FILE_BEAD).map(|q| q.input.clone()).collect()
}

#[test]
fn t1_the_row_is_registered_with_a_weekly_calendar_that_fires_next_on_monday() {
    let row = acc::row(NAME);
    assert_eq!((row.stream(), row.effects, row.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    assert_eq!(next_tick(TUESDAY), TUESDAY + 6 * DAY_MS, "the next fire after a Tuesday is the following Monday 00:00");
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (row.build)(&s)).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(next_tick(0)));
    assert_eq!((row.cadence_ms)(&s), next_tick(0));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_a_book_whose_tools_all_exist_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(book(0))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_a_run_files_at_most_five_and_skips_a_statute_already_open() {
    let r = rig(1, Arc::new(Mutex::new(book(7))));
    assert_eq!(filed(&r).len(), MAX, "the sixth filing is refused by the batch bound");
    let mut open = book(1);
    open.open_titles = vec!["Statute law-bad-0 names a tool or verb that does not exist: bead.sh drop0".into()];
    assert!(filed(&rig(1, Arc::new(Mutex::new(open)))).is_empty(), "a slug in an open title is not filed again");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_statute_audit.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.statute_audit.bogus") && !sim.contains(&"watch.statute_audit.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// Seven offenders, two already open: the first five of the rest in BTreeMap order, with
/// `finding_title` and `cmd_audit`'s body (`rule/src/main.rs:714-727`).
#[test]
fn t5_the_filings_are_the_ones_cmd_audit_made() {
    let mut s = book(7);
    s.open_titles = vec!["x law-bad-1 y".into(), "law-bad-4".into()];
    let f = filed(&rig(1, Arc::new(Mutex::new(s))));
    let slugs: Vec<&str> = ["law-bad-0", "law-bad-2", "law-bad-3", "law-bad-5", "law-bad-6"].to_vec();
    assert_eq!(f.len(), 5, "{f:?}");
    for (filing, slug) in f.iter().zip(slugs) {
        assert!(filing.contains(&format!("Statute {slug} names a tool or verb that does not exist: bead.sh drop")), "{filing}");
    }
    assert!(f[0].contains("The statute names the above and the activated release has no such tool or verb. Build it, or amend the statute to name what exists (rule.sh enact with --tool)."));
}

#[test]
fn t6_eight_weeks_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|seed| rig(seed, Arc::new(Mutex::new(book(7)))), 7, 56);
    acc::assert_healthy(&r, STREAM);
    assert!(filed(&r).len() <= 5 * 9, "at most five filings a week");
    assert_journal_budget(&r, 1);
}
