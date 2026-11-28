//! Operator-channel behaviours exercised through `cmds`, the entry point the CLI shell calls,
//! on a scratch Maildir: no daemon, no bead store, no wall-clock wait. Ages are planted with
//! file mtimes and windows are an hour, so nothing races a timer.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::bead::fake::FakeBd;
use crate::cmds::{self, SendArgs};
use crate::env::Env;
use crate::maildir;

fn env(root: &Path) -> Env {
    Env {
        mail_root: root.join("mail"),
        kinds_dir: root.join("kinds"),
        index_file: root.join("index"),
        mute: false,
        loom_budget_ms: 1000,
        repeat_window_s: 3600,
        tidy_fresh_s: 0,
        id_prefix: "sp".into(),
        ask_label: String::new(),
        db: String::new(),
        bd_conn_retries: 0,
        operator_actor: "op".into(),
        run_dir: root.join("run"),
        home: root.to_path_buf(),
        mail_from: None,
        lint_considered: None,
        repeat_considered: None,
        operator_considered: Some("operator mailbox under test".into()),
        statute_guards: String::new(),
        statute_considered: None,
        allow_blocking: false,
        bead_id: None,
        lock_timeout_ms: 1000,
        condition: None,
    }
}

fn fixture(name: &str) -> (testkit::TempDir, Env) {
    let t = testkit::TempDir::new(name);
    fs::create_dir_all(t.path().join("kinds")).unwrap();
    fs::write(t.path().join("kinds/note.md"), "---\n---\n\n## Note\n\nsay it\n").unwrap();
    fs::write(t.path().join("kinds/alert.md"), "---\n---\n\n## Alert\n\nsay it\n").unwrap();
    let e = env(t.path());
    (t, e)
}

fn args<'a>(mailbox: &'a str, from: Option<&'a str>, subject: &'a str) -> SendArgs<'a> {
    SendArgs { mailbox, from, subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" }
}

/// The one non-ask kind the operator's mailbox takes from this fixture's override.
fn to_operator<'a>(from: Option<&'a str>, subject: &'a str) -> SendArgs<'a> {
    SendArgs { kind: "alert", ..args("operator", from, subject) }
}

fn send(e: &Env, a: &SendArgs) -> Result<cmds::SendOutcome, String> {
    cmds::send(&FakeBd::new(vec![]), e, a, "## Note\nsomething happened\n\n## Alert\nsomething happened\n".into())
}

fn names(e: &Env, mailbox: &str, sub: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(e.mail_root.join(mailbox).join(sub)).map(|d| d.flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn age(e: &Env, mailbox: &str, secs: u64) {
    for n in names(e, mailbox, "new") {
        let f = fs::OpenOptions::new().write(true).open(e.mail_root.join(mailbox).join("new").join(n)).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }
}

const FROM: Option<&str> = Some("Gate <gate@spira>");

// covers: UC-operator-channel-01
#[test_id(tnck6cq)]
#[test]
fn a_sent_message_lands_whole_in_new_and_leaves_nothing_in_tmp() {
    let (_t, e) = fixture("mail-uc01-send");
    let out = send(&e, &args("concierge", FROM, "gate is red")).unwrap();
    assert_eq!(names(&e, "concierge", "new").len(), 1);
    assert!(names(&e, "concierge", "tmp").is_empty());
    let text = fs::read_to_string(out.delivered_path).unwrap();
    assert!(text.starts_with("From: Gate <gate@spira>\nSubject: gate is red\n"), "{text}");
    assert!(text.contains("something happened"));
}

// covers: UC-operator-channel-01
#[test_id(tvxiw5r)]
#[test]
fn read_prints_the_oldest_unread_and_moves_it_from_new_to_cur() {
    let (_t, e) = fixture("mail-uc01-read");
    send(&e, &args("concierge", FROM, "first")).unwrap();
    send(&e, &args("concierge", FROM, "second")).unwrap();
    let old = names(&e, "concierge", "new")[0].clone();
    fs::OpenOptions::new().write(true).open(e.mail_root.join("concierge/new").join(&old)).unwrap().set_modified(SystemTime::now() - Duration::from_secs(9000)).unwrap();
    let printed = cmds::read_cmd(&e, "concierge", None).unwrap();
    let want = fs::read_to_string(e.mail_root.join("concierge/cur").join(format!("{old}:2,S"))).unwrap();
    assert_eq!(printed, want);
    assert_eq!(names(&e, "concierge", "new").len(), 1);
    assert_eq!(names(&e, "concierge", "cur"), vec![format!("{old}:2,S")]);
}

// covers: UC-operator-channel-01
#[test_id(tj3imb5)]
#[test]
fn a_message_read_is_flagged_seen_so_count_and_tidy_agree_it_was_read() {
    let (_t, e) = fixture("mail-read-seen");
    maildir::mail_ensure(&e.mail_root.join("operator")).unwrap();
    let failed = SendArgs { kind: "alert", digest: true, ..args("operator", FROM, "DEPLOY FAILED #7 [spira]") };
    send(&e, &failed).unwrap();
    let id = names(&e, "operator", "new")[0].clone();
    cmds::read_cmd(&e, "operator", Some(&id)).unwrap();
    let read = e.mail_root.join("operator/cur").join(format!("{id}:2,S"));
    assert!(read.is_file());
    assert!(!maildir::is_unread(&read));
    assert_eq!(cmds::count(&e, "operator").unwrap(), 0);
    let bd = FakeBd::new(vec![crate::bead::BdOut::ok("[]"), crate::bead::BdOut::ok(r#"[{"id":"probe"}]"#)]);
    let report = crate::tidy::tidy(&bd, true, &e.mail_root, &e.run_dir, "operator", "asks", 86_400, false).unwrap();
    assert_eq!((report.archived, report.kept), (1, 0), "a read digest alert leaves inside the fresh window");
    assert!(e.mail_root.join("archive/cur").join(format!("{id}:2,S")).is_file());
}

// covers: UC-operator-channel-01
#[test_id(to99xsp)]
#[test]
fn archive_by_hand_moves_one_message_and_records_who_and_why() {
    let (_t, e) = fixture("mail-archive-hand");
    send(&e, &args("concierge", FROM, "stale thing")).unwrap();
    send(&e, &args("concierge", FROM, "other thing")).unwrap();
    let id = names(&e, "concierge", "new")[0].clone();
    cmds::archive(&e, "concierge", &id, "the condition is gone", "ryan").unwrap();
    assert_eq!(names(&e, "concierge", "new").len(), 1, "only the named message moved");
    let text = fs::read_to_string(e.mail_root.join("archive/cur").join(&id)).unwrap();
    let trailer = text.lines().last().unwrap();
    assert!(trailer.starts_with("-- archived: ") && trailer.ends_with(" by ryan: the condition is gone"), "{trailer}");
}

// covers: UC-operator-channel-01
#[test_id(tjzy7sm)]
#[test]
fn archive_by_hand_refuses_without_a_why_and_moves_nothing() {
    let (_t, e) = fixture("mail-archive-nowhy");
    send(&e, &args("concierge", FROM, "stale thing")).unwrap();
    let id = names(&e, "concierge", "new")[0].clone();
    for why in ["", "  "] {
        let err = cmds::archive(&e, "concierge", &id, why, "ryan").unwrap_err();
        assert!(err.contains("--why"), "{err}");
    }
    assert_eq!(names(&e, "concierge", "new"), vec![id]);
    assert!(!e.mail_root.join("archive").exists());
}

// covers: UC-operator-channel-01
#[test_id(t6lemm1)]
#[test]
fn a_hand_action_is_recorded_under_the_sessions_sender_else_the_operator() {
    let (_t, mut e) = fixture("mail-actor");
    assert_eq!(cmds::actor(&e), "op");
    e.mail_from = Some("Dwarf <dwarf@spira>".into());
    assert_eq!(cmds::actor(&e), "dwarf");
}

// covers: UC-operator-channel-01
#[test_id(tyww89w)]
#[test]
fn a_sent_alarm_carries_its_condition_header() {
    let (_t, mut e) = fixture("mail-condition-header");
    e.condition = Some("mail-health:concierge".into());
    let out = send(&e, &args("concierge", FROM, "concierge is not reading its mail")).unwrap();
    assert!(fs::read_to_string(out.delivered_path).unwrap().contains("\nX-Spira-Condition: mail-health:concierge\n"));
}

// covers: UC-operator-channel-01
#[test_id(t4z6q88)]
#[test]
fn read_with_nothing_unread_fails_naming_the_mailbox() {
    let (_t, e) = fixture("mail-uc01-read-empty");
    maildir::mail_ensure(&e.mail_root.join("concierge")).unwrap();
    let err = cmds::read_cmd(&e, "concierge", None).unwrap_err();
    assert!(err.contains("no unread mail in concierge"), "{err}");
}

// covers: UC-operator-channel-01
#[test_id(t517ik4)]
#[test]
fn a_read_verb_on_a_missing_mailbox_fails_and_creates_nothing() {
    let (_t, e) = fixture("mail-uc01-missing");
    assert!(cmds::count(&e, "ghost").unwrap_err().contains("no such mailbox"));
    assert!(cmds::list(&e, "ghost", false).is_err());
    assert!(!e.mail_root.join("ghost").exists());
}

// covers: UC-operator-channel-01
#[test_id(tu5kopt)]
#[test]
fn list_unread_shows_only_new_and_list_shows_new_then_cur() {
    let (_t, e) = fixture("mail-uc01-list");
    send(&e, &args("concierge", FROM, "read me")).unwrap();
    send(&e, &args("concierge", FROM, "still unread")).unwrap();
    let first = names(&e, "concierge", "new")[0].clone();
    cmds::read_cmd(&e, "concierge", Some(&first)).unwrap();
    let unread = cmds::list(&e, "concierge", true).unwrap();
    assert_eq!(unread.lines().count(), 1, "{unread}");
    assert!(unread.contains("[new]"));
    let all = cmds::list(&e, "concierge", false).unwrap();
    assert_eq!(all.lines().count(), 2, "{all}");
    assert!(all.contains("[cur]") && all.contains("[new]"));
    let id_of = |l: &str| l.split_whitespace().next().unwrap().to_string();
    let unread_id = id_of(unread.lines().next().unwrap());
    cmds::read_cmd(&e, "concierge", Some(&unread_id)).unwrap();
    let read_line = all.lines().find(|l| l.contains("[cur]")).unwrap();
    cmds::done(&e, "concierge", &id_of(read_line), None).unwrap();
}

// covers: UC-operator-channel-01
#[test_id(t9lb8mf)]
#[test]
fn count_is_the_unread_total_and_unread_age_is_empty_when_there_is_none() {
    let (_t, e) = fixture("mail-uc01-count");
    maildir::mail_ensure(&e.mail_root.join("concierge")).unwrap();
    assert_eq!((cmds::count(&e, "concierge").unwrap(), cmds::unread_age(&e, "concierge").unwrap()), (0, None));
    send(&e, &args("concierge", FROM, "one")).unwrap();
    send(&e, &args("concierge", FROM, "two")).unwrap();
    assert_eq!(cmds::count(&e, "concierge").unwrap(), 2);
}

// covers: UC-operator-channel-01
#[test_id(trvtu37)]
#[test]
fn unread_age_is_the_age_of_the_oldest_unread_message() {
    let (_t, e) = fixture("mail-uc01-age");
    send(&e, &args("concierge", FROM, "old")).unwrap();
    age(&e, "concierge", 7200);
    send(&e, &args("concierge", FROM, "fresh")).unwrap();
    let a = cmds::unread_age(&e, "concierge").unwrap().unwrap();
    assert!((7200..7260).contains(&a), "{a}");
}

// covers: UC-operator-channel-02
#[test_id(t6rvkqj)]
#[test]
fn a_send_with_no_subject_is_refused_and_delivers_nothing() {
    let (_t, e) = fixture("mail-uc02-subject");
    let err = send(&e, &args("concierge", FROM, "")).unwrap_err();
    assert!(err.contains("missing Subject"), "{err}");
    assert!(names(&e, "concierge", "new").is_empty());
}

// covers: UC-operator-channel-02
#[test_id(tqrdwl3)]
#[test]
fn an_omitted_from_falls_back_to_the_configured_sender_and_without_one_is_refused() {
    let (_t, mut e) = fixture("mail-uc02-from");
    let err = send(&e, &args("concierge", None, "no sender")).unwrap_err();
    assert!(err.contains("missing From"), "{err}");
    e.mail_from = Some("Fallback <fb@spira>".into());
    let out = send(&e, &args("concierge", None, "has sender")).unwrap();
    assert!(fs::read_to_string(out.delivered_path).unwrap().starts_with("From: Fallback <fb@spira>\n"));
}

// covers: UC-operator-channel-03
#[test_id(tm7u53a)]
#[test]
fn template_prints_the_kinds_skeleton_and_an_unknown_kind_is_refused_and_named() {
    let (_t, e) = fixture("mail-uc03-template");
    assert_eq!(cmds::template(&e.kinds_dir, "note").unwrap(), "\n## Note\n\nsay it");
    assert_eq!(cmds::template(&e.kinds_dir, "nosuchkind").unwrap_err(), "unknown kind: nosuchkind");
    assert_eq!(cmds::template(&e.kinds_dir, "").unwrap_err(), "kind required");
}

// covers: UC-operator-channel-03
#[test_id(t3akfd9)]
#[test]
fn a_send_of_an_unknown_kind_is_refused_naming_it_and_delivers_nothing() {
    let (_t, e) = fixture("mail-uc03-kind");
    let mut a = args("concierge", FROM, "odd kind");
    a.kind = "nosuchkind";
    assert!(send(&e, &a).unwrap_err().contains("nosuchkind"));
    assert!(names(&e, "concierge", "new").is_empty());
}

// covers: UC-operator-channel-04
#[test_id(tq60nhn)]
#[test]
fn the_lint_override_admits_an_offender_and_records_its_reason_in_a_header() {
    let (_t, mut e) = fixture("mail-uc04-override");
    assert!(send(&e, &args("concierge", FROM, "sp-abc12")).unwrap_err().contains("bead id"));
    e.lint_considered = Some("legacy sender".into());
    let out = send(&e, &args("concierge", FROM, "sp-abc12")).unwrap();
    let text = fs::read_to_string(out.delivered_path).unwrap();
    assert!(text.contains("\nX-Spira-Lint-Override: legacy sender\n"), "{text}");
    assert!(out.steps.contains(&"lint: overridden".to_string()));
}

// covers: UC-operator-channel-06
#[test_id(t6k1v7p)]
#[test]
fn a_repeat_to_the_operator_within_the_window_is_refused_naming_the_override() {
    let (t, e) = fixture("mail-uc06-repeat");
    send(&e, &to_operator(FROM, "gate red 5 times")).unwrap();
    let err = send(&e, &to_operator(FROM, "gate red 6 times")).unwrap_err();
    assert!(err.contains("repeat refused") && err.contains("SPIRA_MAIL_REPEAT_CONSIDERED"), "{err}");
    assert_eq!(names(&e, "operator", "new").len(), 1);
    assert!(fs::read_dir(t.path().join("run/mail-repeat")).unwrap().flatten().any(|f| f.file_name().to_string_lossy().ends_with(".refused")));
}

// covers: UC-operator-channel-06
#[test_id(to4x696)]
#[test]
fn a_repeat_outside_the_window_and_a_different_subject_are_delivered() {
    let (_t, mut e) = fixture("mail-uc06-window");
    send(&e, &to_operator(FROM, "gate red")).unwrap();
    send(&e, &to_operator(FROM, "queue stuck")).unwrap();
    e.repeat_window_s = 0;
    send(&e, &to_operator(FROM, "gate red")).unwrap();
    assert_eq!(names(&e, "operator", "new").len(), 3);
}

// covers: UC-operator-channel-06
#[test_id(tvtij60)]
#[test]
fn the_repeat_override_lets_a_repeat_through_with_a_header() {
    let (_t, mut e) = fixture("mail-uc06-override");
    send(&e, &to_operator(FROM, "gate red")).unwrap();
    e.repeat_considered = Some("operator asked again".into());
    let out = send(&e, &to_operator(FROM, "gate red")).unwrap();
    assert!(fs::read_to_string(out.delivered_path).unwrap().contains("\nX-Spira-Repeat-Override: operator asked again\n"));
}

// covers: UC-operator-channel-06
#[test_id(tvodja5)]
#[test]
fn the_repeat_guard_does_not_apply_to_other_mailboxes() {
    let (_t, e) = fixture("mail-uc06-other");
    send(&e, &args("concierge", FROM, "gate red")).unwrap();
    send(&e, &args("concierge", FROM, "gate red")).unwrap();
    assert_eq!(names(&e, "concierge", "new").len(), 2);
}

// covers: UC-operator-channel-06
#[test_id(tkdsvy0)]
#[test]
fn a_lint_refused_operator_send_writes_no_stamp_so_the_corrected_send_is_not_a_repeat() {
    let (t, e) = fixture("mail-uc06-lint-order");
    assert!(send(&e, &to_operator(None, "gate red")).unwrap_err().contains("missing From"));
    let stamps = fs::read_dir(t.path().join("run/mail-repeat")).map(|d| d.flatten().filter(|f| !f.file_name().to_string_lossy().ends_with(".lock")).count()).unwrap_or(0);
    assert_eq!(stamps, 0);
    send(&e, &to_operator(FROM, "gate red")).unwrap();
}

// covers: UC-operator-channel-07
// covers: UC-cockpit-observability-26
#[test_id(t4y4y2i)]
#[test]
fn done_sets_the_replied_and_seen_flags_once_and_a_second_done_changes_nothing() {
    let (_t, e) = fixture("mail-uc07-done");
    send(&e, &args("concierge", FROM, "handled")).unwrap();
    let id = names(&e, "concierge", "new")[0].clone();
    cmds::done(&e, "concierge", &id, None).unwrap();
    assert_eq!(names(&e, "concierge", "cur"), vec![format!("{id}:2,RS")]);
    assert!(names(&e, "concierge", "new").is_empty());
    cmds::done(&e, "concierge", &id, None).unwrap();
    assert_eq!(names(&e, "concierge", "cur"), vec![format!("{id}:2,RS")]);
}

// covers: UC-operator-channel-07
// covers: UC-cockpit-observability-26
#[test_id(tmf3f9u)]
#[test]
fn done_on_an_unknown_id_fails_and_flags_nothing() {
    let (_t, e) = fixture("mail-uc07-unknown");
    send(&e, &args("concierge", FROM, "bystander")).unwrap();
    let before = names(&e, "concierge", "new");
    assert!(cmds::done(&e, "concierge", "no-such-id", None).unwrap_err().contains("not found"));
    assert_eq!(names(&e, "concierge", "new"), before);
    assert!(names(&e, "concierge", "cur").is_empty());
}

// covers: UC-operator-channel-11
#[test_id(t3s5l0l)]
#[test]
fn a_send_to_a_dwarf_with_a_live_mailbox_lands_in_its_new() {
    let (_t, e) = fixture("mail-uc11-live");
    maildir::mail_ensure(&e.mail_root.join("dwarf-alive")).unwrap();
    send(&e, &args("dwarf:alive", FROM, "Hello")).unwrap();
    assert_eq!(names(&e, "dwarf-alive", "new").len(), 1);
}

// covers: UC-operator-channel-11
#[test_id(trvj70f)]
#[test]
fn a_send_to_a_dwarf_with_no_mailbox_is_refused_naming_dwarf_and_creates_nothing() {
    let (_t, e) = fixture("mail-uc11-dead");
    let err = send(&e, &args("dwarf:dead", FROM, "Hello")).unwrap_err();
    assert!(err.contains("dwarf:dead") && err.contains("no live mailbox"), "{err}");
    assert!(!e.mail_root.join("dwarf-dead").exists());
}
