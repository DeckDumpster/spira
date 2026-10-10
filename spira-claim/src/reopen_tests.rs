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
    fn lc_reopen(&mut self, id: &str, cause: &str) -> Result<(), String> {
        self.log(format!("lc_reopen {id} {cause}"));
        if self.reopen_fails {
            Err("machine refused".into())
        } else {
            Ok(())
        }
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
    assert_eq!(lc_argv.len(), 2, "{lc_log}");
    assert_eq!(lc_argv[0], "ARGV [reopen] [sp-a] [batch-eject] [harness]", "{lc_log}");
    assert!(lc_argv[1].starts_with("ARGV [fact] [sp-a] [--kind] [reopen] [--actor] [harness] [--cause] [batch-eject]"), "{lc_log}");
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
