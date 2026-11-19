//! A question about a work bead files its own decision bead, linked to the work bead without
//! ever blocking it. Run through `cmds::send` against a scripted store and a scratch Maildir.

use std::fs;

use crate::bead::fake::FakeBd;
use crate::bead::BdOut;
use crate::cmds::{self, SendArgs};
use crate::env::Env;
use crate::message;

const BODY: &str = "## Question\nProceed with option A?\n\n## Default\nproceed with option A\n\n## Class basis\nneeds a policy ruling\n";

fn env(root: &std::path::Path, allow_blocking: bool) -> Env {
    Env {
        mail_root: root.join("mail"),
        kinds_dir: root.join("kinds"),
        index_file: root.join("index"),
        mute: false,
        loom_budget_ms: 1000,
        repeat_window_s: 0,
        tidy_fresh_s: 0,
        id_prefix: "sp".into(),
        ask_label: "needs-ops".into(),
        db: "db".into(),
        bd_conn_retries: 0,
        operator_actor: "op".into(),
        run_dir: root.join("run"),
        home: root.to_path_buf(),
        mail_from: None,
        lint_considered: Some("test".into()),
        repeat_considered: Some("test".into()),
        operator_considered: Some("test".into()),
        statute_guards: String::new(),
        statute_considered: None,
        allow_blocking,
        bead_id: None,
        lock_timeout_ms: 1000,
        condition: None,
    }
}

/// duplicate-ask check, create, dep add, default note, the cited bead's render.
fn store() -> FakeBd {
    FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok("sp-dec1"), BdOut::ok(""), BdOut::ok(""), BdOut::ok("[]")])
}

fn ask_about_work_bead(tag: &str, allow_blocking: bool) -> (FakeBd, String) {
    let t = testkit::TempDir::new(tag);
    let e = env(t.path(), allow_blocking);
    let a = SendArgs { mailbox: "operator", from: Some("Builder <builder@spira>"), subject: "Proceed with option A?", kind: "question", default: "proceed with option A", class: "policy", bead: "sp-work1", urgent: false, digest: false, dry_run: false, requested_by: "" };
    let bd = store();
    let out = cmds::send(&bd, &e, &a, BODY.into()).unwrap();
    let sent = fs::read_to_string(out.delivered_path).unwrap();
    (bd, sent)
}

fn dep_adds(bd: &FakeBd) -> Vec<Vec<String>> {
    bd.calls().into_iter().map(|c| c.0).filter(|c| c.starts_with(&["dep".to_string(), "add".to_string()])).collect()
}

// covers: UC-operator-channel-14
#[test]
fn a_question_about_a_work_bead_files_a_separate_decision_bead_and_names_it_in_x_spira_bead() {
    let (bd, sent) = ask_about_work_bead("mail-uc14-decision", false);
    assert!(bd.calls().iter().any(|c| c.0[0] == "create"), "{:?}", bd.calls());
    assert_eq!(message::header_ci_before_blank(&sent, "x-spira-bead"), "sp-dec1");
    assert_eq!(message::header_ci_before_blank(&sent, "x-spira-work-bead"), "sp-work1");
}

// covers: UC-operator-channel-14
#[test]
fn the_decision_bead_is_linked_to_the_work_bead_as_relates_to_never_blocking() {
    let (bd, _) = ask_about_work_bead("mail-uc14-link", false);
    let adds = dep_adds(&bd);
    assert_eq!(adds.len(), 1, "{adds:?}");
    assert_eq!(adds[0], ["dep", "add", "sp-work1", "sp-dec1", "--type", "relates-to"]);
}

// covers: UC-operator-channel-14
#[test]
fn only_the_explicit_override_makes_the_decision_block_the_work_bead() {
    let (bd, _) = ask_about_work_bead("mail-uc14-override", true);
    let adds = dep_adds(&bd);
    assert_eq!(adds, [["dep", "add", "sp-work1", "sp-dec1", "--type", "blocks"]]);
}
