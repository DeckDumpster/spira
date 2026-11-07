//! The whole pipeline against a real temporary git repository, a fake container runtime and a
//! fake builder: the contract of DESIGN.md §2 and §4, end to end, without podman or cargo.

use super::*;

// ---- the skip contract (DESIGN.md §3.7): fail closed, not open --------------------------

#[test]
fn the_verdict_line_records_slot_wait_and_suite_time_separately() {
    let timed = Finish { timing: Some(Box::new((1212, 62))), ..Finish::green(1) };
    assert_eq!(timed.verdict_line(), "VERDICT GREEN ran=1 slot_wait=1212s suite=62s");
    let fault = Finish { timing: Some(Box::new((7, 600))), ..Finish::fault(2, "over-cap", 0) };
    assert_eq!(fault.verdict_line(), "VERDICT FAULT rc=2 ran=0 reason=over-cap slot_wait=7s suite=600s");
}

#[test]
fn verdict_lines() {
    assert_eq!(Finish::green(3).verdict_line(), "VERDICT GREEN ran=3");
    assert_eq!(
        Finish::nothing().verdict_line(),
        "VERDICT GREEN ran=0 selected=0"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 2,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
            session: None,
            first_red: None,
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 1,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
            session: None,
            first_red: Some("test-x.sh: case 3 b at spira/test-x.sh:9".into()),
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=1 first=test-x.sh: case 3 b at spira/test-x.sh:9"
    );
    assert_eq!(
        Finish {
            deferred: Some((5, 300)),
            ..Finish::green(7)
        }
        .verdict_line(),
        "VERDICT GREEN ran=7 deferred=5 (deadline 300s)"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 2,
            red: 1,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: Some((3, 60)),
            skipped: 0,
            session: None,
            first_red: None,
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=2 red=1 deferred=3 (deadline 60s)"
    );
    assert_eq!(
        Finish {
            skipped: 4,
            ..Finish::green(9)
        }
        .verdict_line(),
        "VERDICT GREEN ran=9 skipped=4"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 2,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 3,
            session: None,
            first_red: None,
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2 skipped=3"
    );
    assert_eq!(
        Finish::fault(3, "install", 0).verdict_line(),
        "VERDICT FAULT rc=3 ran=0 reason=install"
    );
    assert_eq!(Finish::green(1).verdict_line_filtered(Some("a,b")), "VERDICT GREEN ran=1 tests=a,b");
    assert_eq!(
        Finish { first_red: Some("unit-tests.sh: case 1 x".into()), ..Finish { rc: 1, red: 1, ..Finish::green(1) } }
            .verdict_line_filtered(Some("x")),
        "VERDICT RED ran=1 red=1 tests=x first=unit-tests.sh: case 1 x"
    );
}

// ---- --artifacts: prebuilt executables, no cargo (DESIGN.md D8) ------------------------

// ---- the whole trial under the budget (DESIGN.md §11, D9) -------------------------------

// ---- a caught signal mid-build ends the run, not just the wait (sp-tcarr) ---------------

// ---- warm slots (DESIGN.md §11.2, D10) -------------------------------------------------

// ---------------------------------------------------------------- admission (sp-f4ig1)

#[test]
fn a_green_run_records_a_full_suite_pass_only_when_it_selected_every_suite() {
    use spira_config::local_pass::{check, Kind, Verdict};
    let t = testkit::TempDir::new("full-pass");
    let (run, dir) = (t.path().join("run"), t.path().join("spira"));
    fs::create_dir_all(&dir).unwrap();
    for n in ["test-a.sh", "test-b.sh", "not-a-suite.sh"] {
        fs::write(dir.join(n), "").unwrap();
    }
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    record_full_suite_pass(&run, "aaaa", &names(&["test-a.sh"]), &dir, &|_| {});
    assert!(check(&run, Kind::FullSuite, "aaaa", None, "u", "0").is_err());
    record_full_suite_pass(&run, "aaaa", &names(&["test-b.sh", "test-a.sh"]), &dir, &|_| {});
    assert_eq!(check(&run, Kind::FullSuite, "aaaa", None, "u", "0"), Ok(Verdict::Passed));
}
