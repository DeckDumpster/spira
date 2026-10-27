use super::*;
use std::cell::RefCell;

#[test]
fn deliberate_causes_list() {
    assert_eq!(deliberate_causes_text(), "work-close-converted 1\neject 0\n");
    assert!(admission_exempt("work-close-converted"));
    assert!(!admission_exempt("eject"));
    assert!(!admission_exempt("gate-red"));
    assert!(!admission_exempt("rebase-conflict"));
    assert!(!admission_exempt(""));
    assert!(!admission_exempt("unrecorded"));
}

// ---------------------------------------------------------------------------------------
// a fake World: every call recorded, nothing touches a subprocess or a filesystem

#[derive(Default)]
struct Fake {
    reopen_fails: bool,
    note_fails: bool,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn log(&self, s: impl Into<String>) {
        self.calls.borrow_mut().push(s.into());
    }
    fn has(&self, s: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == s)
    }
}

impl World for Fake {
    fn write_ejected(&mut self, id: &str, suites: &str) {
        self.log(format!("write_ejected {id} {suites}"));
    }
    fn record_failing(&mut self, id: &str, suites: &str) {
        self.log(format!("record_failing {id} {suites}"));
    }
    fn record_rejection(&mut self, id: &str, r: &spira_config::escalation::Rejection) {
        self.log(format!("record_rejection {id} {}|{}|{}", r.cause, r.suites, r.reason));
    }
    fn lc_reopen(&mut self, id: &str, cause: &str) -> Result<(), String> {
        self.log(format!("lc_reopen {id} {cause}"));
        if self.reopen_fails {
            Err("machine refused".into())
        } else {
            Ok(())
        }
    }
    fn stop_holder(&mut self, id: &str, cause: &str) -> Result<(), String> {
        self.log(format!("stop_holder {id} {cause}"));
        Ok(())
    }
    fn write_reopen_event(&mut self, id: &str, cause: &str) {
        self.log(format!("write_reopen_event {id} {cause}"));
    }
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.log(format!("note {id} {text}"));
        if self.note_fails {
            Err("note refused".into())
        } else {
            Ok(())
        }
    }
}

fn opts(id: &str, cause: &str) -> Opts {
    Opts { id: id.into(), cause: cause.into(), note: String::new(), suites: String::new() }
}

#[test]
fn happy_path_every_step_in_order() {
    let mut w = Fake::default();
    let rc = run(&opts("sp-a", "gate-red"), &mut w);
    assert_eq!(rc, 0);
    assert_eq!(
        *w.calls.borrow(),
        vec![
            "stop_holder sp-a gate-red",
            "record_rejection sp-a gate-red||",
            "lc_reopen sp-a gate-red",
            "write_reopen_event sp-a gate-red",
        ]
    );
}

#[test]
fn suites_write_the_sidecar() {
    let mut w = Fake::default();
    let mut o = opts("sp-a", "batch-eject");
    o.suites = "test-x.sh".into();
    run(&o, &mut w);
    assert!(w.has("write_ejected sp-a test-x.sh"));
}

#[test]
fn no_suites_means_no_sidecar_write() {
    let mut w = Fake::default();
    run(&opts("sp-a", "gate-red"), &mut w);
    assert!(!w.calls.borrow().iter().any(|c| c.starts_with("write_ejected")));
}

#[test]
fn work_close_converted_is_exempt_leaves_the_row_alone() {
    let mut w = Fake::default();
    assert_eq!(run(&opts("sp-a", "work-close-converted"), &mut w), 0);
    assert!(!w.calls.borrow().iter().any(|c| c.starts_with("lc_reopen")), "the conversion carries the CERTIFIED row forward");
    assert!(w.has("write_reopen_event sp-a work-close-converted"));
}

#[test]
fn eject_is_deliberate_but_not_exempt() {
    let mut w = Fake::default();
    run(&opts("sp-a", "eject"), &mut w);
    assert!(w.has("lc_reopen sp-a eject"), "the row is withdrawn through the machine");
}

#[test]
fn note_only_called_when_non_empty() {
    let mut w = Fake::default();
    run(&opts("sp-a", "gate-red"), &mut w);
    assert!(!w.calls.borrow().iter().any(|c| c.starts_with("note ")));

    let mut w2 = Fake::default();
    let mut o = opts("sp-a", "gate-red");
    o.note = "a human-readable note".into();
    run(&o, &mut w2);
    assert!(w2.has("note sp-a a human-readable note"));
}

#[test]
fn lc_reopen_failure_sets_rc_1() {
    let mut w = Fake { reopen_fails: true, ..Default::default() };
    assert_eq!(run(&opts("sp-a", "gate-red"), &mut w), 1);
}

#[test]
fn note_failure_sets_rc_1_but_only_when_a_note_was_given() {
    let mut w = Fake { note_fails: true, ..Default::default() };
    assert_eq!(run(&opts("sp-a", "gate-red"), &mut w), 0, "no note given: note's own (would-be) failure never runs");

    let mut w2 = Fake { note_fails: true, ..Default::default() };
    let mut o = opts("sp-a", "gate-red");
    o.note = "x".into();
    assert_eq!(run(&o, &mut w2), 1);
}

#[test]
fn write_reopen_event_failure_can_never_set_rc_nonzero() {
    // write_reopen_event has no Result at all in this trait — the bash wrapper it shims
    // always returns 0 regardless of what its child did, so there is nothing here that
    // COULD fail the reopen. This test exists so a future refactor that adds a Result to
    // the trait method has to consciously decide to keep ignoring it, not drift into
    // checking it by accident.
    let mut w = Fake::default();
    assert_eq!(run(&opts("sp-a", "gate-red"), &mut w), 0);
    assert!(w.has("write_reopen_event sp-a gate-red"));
}

#[test]
fn every_failure_together_is_still_just_rc_1() {
    let mut w = Fake { reopen_fails: true, note_fails: true, ..Default::default() };
    let mut o = opts("sp-a", "gate-red");
    o.note = "x".into();
    assert_eq!(run(&o, &mut w), 1);
}

// ---------------------------------------------------------------------------------------
// crate::unpoison::Live's own `reopen::World` impl: the real bd / landing-pass argv shapes
// (wave 4.19, row I) — no fake, real subprocesses against fixture scripts, the same shape
// unpoison_tests.rs's own `live()` uses for its World impl.

use crate::store::Store;
use crate::unpoison::Live;
use std::time::Duration;

fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
    p.to_string_lossy().into_owned()
}

const RECORD: &str = r#"{ printf 'ARGV'; for a in "$@"; do printf ' [%s]' "$a"; done; printf '\nSTDIN '; cat; printf '\n'; } >> @LOG@"#;

fn live(tag: &str, bd_body: &str) -> (Live, testkit::TempDir) {
    live_lc(tag, bd_body, 0)
}

fn live_lc(tag: &str, bd_body: &str, lc_exit: i32) -> (Live, testkit::TempDir) {
    let dir = testkit::TempDir::new(&format!("spira-claim-reopen-{tag}"));
    std::fs::create_dir_all(dir.join("run")).unwrap();
    let bd = script(&dir, "bd", &bd_body.replace("@LOG@", &dir.join("bd.log").to_string_lossy()));
    let lc = script(&dir, "lc", &format!("{}\nexit {lc_exit}", RECORD.replace("@LOG@", &dir.join("lc.log").to_string_lossy())));
    let l = Live {
        store: Store { bd, db: Some("/fake/db".into()), lc, timeout: Duration::from_secs(10) },
        run_dir: dir.join("run"),
        asked_dir: std::path::PathBuf::new(),
        ask_label: String::new(),
        beads_actor: "harness".into(),
    };
    (l, dir)
}

#[test]
fn live_reopen_writes_sidecar_reopens_strips_label_releases_notes() {
    let (mut w, dir) = live("e2e", RECORD);
    let o = Opts {
        id: "sp-a".into(),
        cause: "batch-eject".into(),
        note: "a human-readable note".into(),
        suites: "test-x.sh".into(),
    };
    assert_eq!(run(&o, &mut w), 0);

    // The sidecar lives in its own directory, created on demand: nothing creates the retired
    // landstate ledger any more (sp-2c1n0), and nothing here may write into it again.
    let ejected = std::fs::read_to_string(dir.join("run/ejected/sp-a")).unwrap();
    assert_eq!(ejected, "test-x.sh", "no trailing newline, exactly printf '%s'");
    assert!(!dir.join("run/landstate").exists(), "reopen recreated the retired landstate ledger");

    let bd_log = std::fs::read_to_string(dir.join("bd.log")).unwrap();
    let bd_argv: Vec<&str> = bd_log.lines().filter(|l| l.starts_with("ARGV")).collect();
    // The reopen is the machine's alone: no bd reopen, no label edit, no bd assign (the bead's
    // state and its holder are the lifecycle row's), and the reopen cause is a lifecycle fact.
    // bd sees only the note.
    let lc_log = std::fs::read_to_string(dir.join("lc.log")).unwrap();
    let lc_argv: Vec<&str> = lc_log.lines().filter(|l| l.starts_with("ARGV")).collect();
    assert_eq!(lc_argv.len(), 4, "{lc_log}");
    assert!(lc_argv[1].starts_with("ARGV [fact] [sp-a] [--kind] [rejection] [--actor] [harness] [--cause] "), "{lc_log}");
    assert_eq!(lc_argv[2], "ARGV [reopen] [sp-a] [batch-eject] [harness]", "{lc_log}");
    assert!(lc_argv[3].starts_with("ARGV [fact] [sp-a] [--kind] [reopen] [--actor] [harness] [--cause] [batch-eject]"), "{lc_log}");
    let bd_argv: Vec<&str> = bd_log.lines().filter(|l| l.starts_with("ARGV")).collect();
    assert_eq!(bd_argv.len(), 1, "{bd_log}");
    for gone in ["[reopen]", "[assign]", "[label]", "[--status]", "INSERT INTO events"] {
        assert!(!bd_log.contains(gone), "bd was handed {gone}: {bd_log}");
    }
    assert_eq!(bd_argv[0], "ARGV [-C] [/fake/db] [note] [sp-a] [--stdin]");
    assert!(bd_log.contains("STDIN a human-readable note"), "{bd_log}");
}

#[test]
fn live_without_suites_writes_no_sidecar() {
    let (mut w, dir) = live("noop", RECORD);
    let o = Opts { id: "sp-b".into(), cause: "gate-red".into(), note: String::new(), suites: String::new() };
    assert_eq!(run(&o, &mut w), 0);
    assert!(!dir.join("run/ejected/sp-b").exists());
}

#[test]
fn live_a_bead_with_no_row_reopens_and_a_refused_row_fails() {
    let o = |id: &str| Opts { id: id.into(), cause: "gate-red".into(), note: String::new(), suites: String::new() };
    let (mut none, _d) = live_lc("norow", RECORD, 1);
    assert_eq!(run(&o("sp-a"), &mut none), 0, "no row: nothing to diverge");
    let (mut terminal, _d2) = live_lc("terminal", RECORD, 3);
    assert_eq!(run(&o("sp-a"), &mut terminal), 1, "a terminal row is refused");
    let (mut down, _d3) = live_lc("down", RECORD, 2);
    assert_eq!(run(&o("sp-a"), &mut down), 1, "an unreachable machine is a failure");
}

// ---------------------------------------------------------------------------------------
// the harness stops the holding dwarf before the bead leaves WORKING

fn fake_dwarf(run: &std::path::Path, bead: &str, lease_until: i64) {
    std::fs::create_dir_all(run).unwrap();
    let pf = run.join(format!("dwarf-builder-{bead}.pid"));
    std::fs::write(&pf, "1\n").unwrap();
    std::fs::write(pf.with_extension("lease"), lease_until.to_string()).unwrap();
}

#[test]
fn a_live_holder_gets_a_stop_request_and_is_waited_for() {
    let dir = testkit::TempDir::new("spira-claim-stop-holder");
    let run = dir.join("run");
    fake_dwarf(&run, "sp-a", 10_000);
    let clock = std::cell::Cell::new(100i64);
    let ticks = std::cell::Cell::new(0);
    let r = stop_holder_in(&run, "sp-a", "local-tier-red", false, 60, &|| clock.get(), &|| {
        clock.set(clock.get() + 1);
        ticks.set(ticks.get() + 1);
        // the dwarf sees the request on its next beat and tears down
        if run.join("dwarf/sp-a.stop").is_file() && ticks.get() == 3 {
            let _ = std::fs::remove_file(run.join("dwarf-builder-sp-a.lease"));
        }
    });
    assert!(r.is_ok(), "{r:?}");
    assert!(ticks.get() >= 3, "returned before the dwarf exited");
    let said = std::fs::read_to_string(run.join("dwarf/sp-a.stop")).unwrap();
    assert!(said.contains("local-tier-red"), "{said}");
}

#[test]
fn a_holder_that_will_not_exit_refuses_the_release() {
    let dir = testkit::TempDir::new("spira-claim-stop-stuck");
    let run = dir.join("run");
    fake_dwarf(&run, "sp-a", 1_000_000);
    let clock = std::cell::Cell::new(100i64);
    let r = stop_holder_in(&run, "sp-a", "gate-red", false, 5, &|| clock.get(), &|| clock.set(clock.get() + 1));
    assert!(r.unwrap_err().contains("did not exit"));
}

#[test]
fn no_live_holder_and_an_dwarves_own_call_wait_for_nothing() {
    let dir = testkit::TempDir::new("spira-claim-stop-none");
    let run = dir.join("run");
    let never = || panic!("must not wait");
    assert!(stop_holder_in(&run, "sp-a", "x", false, 5, &|| 100, &never).is_ok());
    fake_dwarf(&run, "sp-a", 10_000);
    assert!(stop_holder_in(&run, "sp-a", "x", true, 5, &|| 100, &never).is_ok());
    assert!(!run.join("dwarf/sp-a.stop").exists(), "a dwarf's own hand-back must not stop itself");
    fake_dwarf(&run, "sp-b", 50);
    assert!(stop_holder_in(&run, "sp-b", "x", false, 5, &|| 100, &never).is_ok(), "an expired lease is not a live holder");
}

#[test]
fn a_refused_stop_leaves_the_bead_working() {
    struct Stuck(Fake);
    impl World for Stuck {
        fn write_ejected(&mut self, i: &str, s: &str) { self.0.write_ejected(i, s) }
        fn record_failing(&mut self, i: &str, s: &str) { self.0.record_failing(i, s) }
        fn record_rejection(&mut self, i: &str, r: &spira_config::escalation::Rejection) { self.0.record_rejection(i, r) }
        fn lc_reopen(&mut self, i: &str, c: &str) -> Result<(), String> { self.0.lc_reopen(i, c) }
        fn stop_holder(&mut self, _: &str, _: &str) -> Result<(), String> { Err("still running".into()) }
        fn write_reopen_event(&mut self, i: &str, c: &str) { self.0.write_reopen_event(i, c) }
        fn note(&mut self, i: &str, t: &str) -> Result<(), String> { self.0.note(i, t) }
    }
    let mut w = Stuck(Fake::default());
    assert_eq!(run(&opts("sp-a", "local-tier-red"), &mut w), 1);
    assert!(w.0.calls.borrow().is_empty(), "the row moved with the dwarf still running: {:?}", w.0.calls.borrow());
}

#[test]
fn the_stop_comes_before_the_state_leaves_working() {
    let mut w = Fake::default();
    assert_eq!(run(&opts("sp-a", "local-tier-red"), &mut w), 0);
    let calls = w.calls.borrow();
    let stop = calls.iter().position(|c| c.starts_with("stop_holder")).unwrap();
    let lc = calls.iter().position(|c| c.starts_with("lc_reopen")).unwrap();
    assert!(stop < lc);
    drop(calls);
    let mut w = Fake::default();
    assert_eq!(run(&opts("sp-a", "work-close-converted"), &mut w), 0);
    assert!(!w.has("stop_holder sp-a work-close-converted"), "a conversion is the holder's own hand-off");
}

#[test]
fn only_a_red_ejection_records_failing_checks() {
    let red = Opts { id: "sp-a".into(), cause: "eject-red".into(), note: String::new(), suites: "test-x.sh,test-y.sh".into() };
    let mut w = Fake::default();
    assert_eq!(run(&red, &mut w), 0);
    assert!(w.has("record_failing sp-a test-x.sh,test-y.sh"), "{:?}", w.calls.borrow());
    let plain = Opts { cause: "eject".into(), ..red };
    let mut w = Fake::default();
    assert_eq!(run(&plain, &mut w), 0);
    assert!(!w.calls.borrow().iter().any(|c| c.starts_with("record_failing")), "a harness eject judges nothing");
}

#[test]
fn a_judged_reopen_records_a_rejection_and_a_harness_one_does_not() {
    let mut w = Fake::default();
    let red = Opts { id: "sp-a".into(), cause: "eject-red".into(), note: "five tests failed".into(), suites: "test-x.sh".into() };
    assert_eq!(run(&red, &mut w), 0);
    assert!(w.has("record_rejection sp-a eject-red|test-x.sh|five tests failed"), "{:?}", w.calls.borrow());
    for cause in ["eject", "gate-no-verdict", "gate-base-red", "queue-eject"] {
        let mut w = Fake::default();
        assert_eq!(run(&Opts { cause: cause.into(), ..red.clone() }, &mut w), 0);
        assert!(!w.calls.borrow().iter().any(|c| c.starts_with("record_rejection")), "{cause} is not a rejection");
    }
}
