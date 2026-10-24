use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

struct Fake {
    busy_for: Cell<u32>,
    slept: Cell<u32>,
    code: i32,
    out: String,
    open: RefCell<BTreeSet<String>>,
    filed: RefCell<Vec<(String, String)>>,
    appended: RefCell<Vec<(String, String)>>,
    recorded: RefCell<Vec<(String, Verdict)>>,
    released: Cell<u32>,
}

impl Fake {
    fn new(code: i32, out: &str) -> Fake {
        Fake { busy_for: Cell::new(0), slept: Cell::new(0), code, out: out.into(), open: Default::default(), filed: Default::default(), appended: Default::default(), recorded: Default::default(), released: Cell::new(0) }
    }
}

impl World for Fake {
    fn round_busy(&self) -> bool {
        self.busy_for.get() > self.slept.get()
    }
    fn sleep(&self) {
        self.slept.set(self.slept.get() + 1);
    }
    fn day(&self) -> String {
        "2026-10-25".into()
    }
    fn head(&self) -> Result<String, String> {
        Ok("0123456789abcdef".into())
    }
    fn prepare_tree(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn release_tree(&self) {
        self.released.set(self.released.get() + 1);
    }
    fn forensics(&self, _: &str) -> String {
        "/run/forensics-0123".into()
    }
    fn run(&self, _: &str) -> (i32, String) {
        (self.code, self.out.clone())
    }
    fn is_open(&self, id: &str) -> Result<bool, String> {
        Ok(self.open.borrow().contains(id))
    }
    fn file(&self, t: &str, b: &str) -> Result<String, String> {
        let id = format!("sp-b{}", self.filed.borrow().len());
        self.filed.borrow_mut().push((t.into(), b.into()));
        self.open.borrow_mut().insert(id.clone());
        Ok(id)
    }
    fn append(&self, id: &str, b: &str, _: &str) -> Result<(), String> {
        self.appended.borrow_mut().push((id.into(), b.into()));
        Ok(())
    }
    fn record(&self, c: &str, v: &Verdict) {
        self.recorded.borrow_mut().push((c.into(), v.clone()));
    }
}

const RED: &str = "phase A\n  ok    install\n  FAIL  work summoned: spira_lc has no read grant\n  FAIL  bead filed: Access denied\n\n3 passed, 2 failed\n";

fn go(w: &Fake, st: &mut State) -> Outcome {
    pass(w, st, &mut |_| Ok(()), 3)
}

#[test]
fn a_green_run_records_commit_and_verdict_and_files_nothing() {
    let w = Fake::new(0, "ok\n");
    let o = go(&w, &mut State::default());
    assert_eq!(o.verdict, Some(Verdict::Pass));
    assert!(w.filed.borrow().is_empty() && o.failed.is_empty());
    assert_eq!(w.recorded.borrow()[0], ("0123456789abcdef".to_string(), Verdict::Pass));
    assert_eq!(w.released.get(), 1);
}

#[test]
fn a_red_run_files_one_bead_per_failing_check_naming_the_lines_commit_and_forensics() {
    let w = Fake::new(1, RED);
    let o = go(&w, &mut State::default());
    assert_eq!(o.filed.len(), 2);
    let filed = w.filed.borrow();
    assert!(filed[0].1.contains("FAIL  work summoned: spira_lc has no read grant"));
    assert!(filed[0].1.contains("0123456789abcdef") && filed[0].1.contains("/run/forensics-0123"));
    assert!(filed[0].1.contains("## Build"));
    assert!(!filed[0].1.contains("bead filed"), "each bead names its own check's lines");
}

#[test]
fn a_repeat_of_the_same_failure_appends_instead_of_filing() {
    let w = Fake::new(1, RED);
    let mut st = State::default();
    go(&w, &mut st);
    let again = go(&w, &mut st);
    assert_eq!(w.filed.borrow().len(), 2, "no second filing");
    assert_eq!(again.amended.len(), 2);
    assert!(w.appended.borrow()[0].1.contains("FAIL  work summoned"));
}

#[test]
fn a_closed_bead_is_filed_afresh() {
    let w = Fake::new(1, RED);
    let mut st = State::default();
    go(&w, &mut st);
    w.open.borrow_mut().clear();
    go(&w, &mut st);
    assert_eq!(w.filed.borrow().len(), 4);
}

#[test]
fn a_reason_that_varies_between_days_is_still_the_same_failure() {
    assert_eq!(failure_key("FAIL  bead filed: Access denied"), failure_key("FAIL  bead filed: timeout"));
}

#[test]
fn filings_are_bounded_and_the_overflow_is_folded_into_the_last_bead() {
    let out: String = (0..7).map(|i| format!("  FAIL  check {i}: r\n")).collect();
    let w = Fake::new(1, &out);
    let o = go(&w, &mut State::default());
    assert_eq!(o.filed.len(), MAX_BEADS);
    let last = &w.filed.borrow()[MAX_BEADS - 1].1;
    assert!(last.contains("check 6"), "folded lines stay verbatim in a bead");
}

#[test]
fn a_run_that_could_not_start_alarms_and_files_no_bead() {
    let w = Fake::new(2, "a\nb\nworkspace build failed\n");
    let o = go(&w, &mut State::default());
    assert!(o.failed[0].contains("workspace build failed"), "{:?}", o.failed);
    assert!(w.filed.borrow().is_empty());
    assert!(matches!(w.recorded.borrow()[0].1, Verdict::NotRun(_)));
}

#[test]
fn a_timeout_exit_is_not_a_pass_or_a_red() {
    assert!(matches!(verdict(124, ""), Verdict::NotRun(_)));
}

#[test]
fn it_waits_for_a_round_rather_than_skipping_the_day() {
    let w = Fake::new(0, "");
    w.busy_for.set(2);
    let o = go(&w, &mut State::default());
    assert_eq!(w.slept.get(), 2);
    assert_eq!(o.verdict, Some(Verdict::Pass));
}

#[test]
fn a_round_that_never_ends_alarms_and_does_not_run() {
    let w = Fake::new(0, "");
    w.busy_for.set(99);
    let o = go(&w, &mut State::default());
    assert!(o.verdict.is_none() && !o.failed.is_empty() && w.recorded.borrow().is_empty());
}

#[test]
fn state_round_trips() {
    let mut s = State::default();
    s.beads.insert("work summoned".into(), "sp-1".into());
    assert_eq!(State::parse(&s.render()), s);
}
