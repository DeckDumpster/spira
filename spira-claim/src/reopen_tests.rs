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
    bd_reopen_fails: bool,
    release_fails: bool,
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
    fn bd_reopen(&mut self, id: &str) -> Result<(), String> {
        self.log(format!("bd_reopen {id}"));
        if self.bd_reopen_fails {
            Err("bd refused".into())
        } else {
            Ok(())
        }
    }
    fn remove_submitted_label(&mut self, id: &str, label: &str) {
        self.log(format!("remove_submitted_label {id} {label}"));
    }
    fn release_claim(&mut self, id: &str) -> Result<(), String> {
        self.log(format!("release_claim {id}"));
        if self.release_fails {
            Err("assign refused".into())
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
    Opts { id: id.into(), cause: cause.into(), note: String::new(), suites: String::new(), submitted_label: "spira-submitted".into() }
}

#[test]
fn happy_path_every_step_in_order() {
    let mut w = Fake::default();
    let rc = run(&opts("sp-a", "gate-red"), &mut w);
    assert_eq!(rc, 0);
    assert_eq!(
        *w.calls.borrow(),
        vec![
            "bd_reopen sp-a",
            "remove_submitted_label sp-a spira-submitted",
            "release_claim sp-a",
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
fn work_close_converted_is_exempt_keeps_label() {
    let mut w = Fake::default();
    run(&opts("sp-a", "work-close-converted"), &mut w);
    assert!(!w.calls.borrow().iter().any(|c| c.starts_with("remove_submitted_label")), "the label survives the conversion");
}

#[test]
fn eject_is_deliberate_but_not_exempt() {
    let mut w = Fake::default();
    run(&opts("sp-a", "eject"), &mut w);
    assert!(w.has("remove_submitted_label sp-a spira-submitted"), "the label is stripped");
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
fn bd_reopen_failure_sets_rc_1() {
    let mut w = Fake { bd_reopen_fails: true, ..Default::default() };
    assert_eq!(run(&opts("sp-a", "gate-red"), &mut w), 1);
}

#[test]
fn release_claim_failure_sets_rc_1() {
    let mut w = Fake { release_fails: true, ..Default::default() };
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
    let mut w = Fake { bd_reopen_fails: true, release_fails: true, note_fails: true, ..Default::default() };
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
    let dir = testkit::TempDir::new(&format!("spira-claim-reopen-{tag}"));
    std::fs::create_dir_all(dir.join("run")).unwrap();
    let bd = script(&dir, "bd", &bd_body.replace("@LOG@", &dir.join("bd.log").to_string_lossy()));
    let l = Live {
        store: Store { bd, db: Some("/fake/db".into()), lc: "true".into(), timeout: Duration::from_secs(10) },
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
        submitted_label: "spira-submitted".into(),
    };
    assert_eq!(run(&o, &mut w), 0);

    // The sidecar lives in its own directory, created on demand: nothing creates the retired
    // landstate ledger any more (sp-2c1n0), and nothing here may write into it again.
    let ejected = std::fs::read_to_string(dir.join("run/ejected/sp-a")).unwrap();
    assert_eq!(ejected, "test-x.sh", "no trailing newline, exactly printf '%s'");
    assert!(!dir.join("run/landstate").exists(), "reopen recreated the retired landstate ledger");

    let bd_log = std::fs::read_to_string(dir.join("bd.log")).unwrap();
    let bd_argv: Vec<&str> = bd_log.lines().filter(|l| l.starts_with("ARGV")).collect();
    // Exact order: reopen, strip the submitted label, release the claim, write the reopen
    // event (its own `sql` call — counters.rs's own tests already cover that INSERT byte
    // for byte, so only its presence and position are pinned here), then the note last.
    // sp-mve9i: no bd status write — the bead's state is the machine's.
    assert_eq!(bd_argv.len(), 5, "{bd_log}");
    assert!(!bd_log.contains("[--status]"), "{bd_log}");
    assert_eq!(bd_argv[0], "ARGV [-C] [/fake/db] [reopen] [sp-a]");
    assert_eq!(bd_argv[1], "ARGV [-C] [/fake/db] [label] [remove] [sp-a] [spira-submitted]");
    assert_eq!(bd_argv[2], "ARGV [-C] [/fake/db] [assign] [sp-a] []");
    assert!(bd_argv[3].starts_with("ARGV [-C] [/fake/db] [sql] [INSERT INTO events"), "{}", bd_argv[3]);
    assert!(bd_argv[3].contains("'sp-a', 'reopen', 'harness', 'batch-eject'"), "{}", bd_argv[3]);
    assert_eq!(bd_argv[4], "ARGV [-C] [/fake/db] [note] [sp-a] [--stdin]");
    assert!(bd_log.contains("STDIN a human-readable note"), "{bd_log}");
}

#[test]
fn live_without_suites_writes_no_sidecar() {
    let (mut w, dir) = live("noop", RECORD);
    let o = Opts { id: "sp-b".into(), cause: "gate-red".into(), note: String::new(), suites: String::new(), submitted_label: "spira-submitted".into() };
    assert_eq!(run(&o, &mut w), 0);
    assert!(!dir.join("run/ejected/sp-b").exists());
}
