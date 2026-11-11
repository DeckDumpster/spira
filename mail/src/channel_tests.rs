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
    }
}

fn fixture(name: &str) -> (testkit::TempDir, Env) {
    let t = testkit::TempDir::new(name);
    fs::create_dir_all(t.path().join("kinds")).unwrap();
    fs::write(t.path().join("kinds/note.md"), "---\n---\n\n## Note\n\nsay it\n").unwrap();
    let e = env(t.path());
    (t, e)
}

fn args<'a>(mailbox: &'a str, from: Option<&'a str>, subject: &'a str) -> SendArgs<'a> {
    SendArgs { mailbox, from, subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" }
}

fn send(e: &Env, a: &SendArgs) -> Result<cmds::SendOutcome, String> {
    cmds::send(&FakeBd::new(vec![]), e, a, "## Note\nsomething happened\n".into())
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
#[test]
fn read_prints_the_oldest_unread_and_moves_it_from_new_to_cur() {
    let (_t, e) = fixture("mail-uc01-read");
    send(&e, &args("concierge", FROM, "first")).unwrap();
    send(&e, &args("concierge", FROM, "second")).unwrap();
    let old = names(&e, "concierge", "new")[0].clone();
    fs::OpenOptions::new().write(true).open(e.mail_root.join("concierge/new").join(&old)).unwrap().set_modified(SystemTime::now() - Duration::from_secs(9000)).unwrap();
    let printed = cmds::read_cmd(&e, "concierge", None).unwrap();
    let want = fs::read_to_string(e.mail_root.join("concierge/cur").join(&old)).unwrap();
    assert_eq!(printed, want);
    assert_eq!(names(&e, "concierge", "new").len(), 1);
    assert_eq!(names(&e, "concierge", "cur"), vec![old]);
}

// covers: UC-operator-channel-01
#[test]
fn read_with_nothing_unread_fails_naming_the_mailbox() {
    let (_t, e) = fixture("mail-uc01-read-empty");
    maildir::mail_ensure(&e.mail_root.join("concierge")).unwrap();
    let err = cmds::read_cmd(&e, "concierge", None).unwrap_err();
    assert!(err.contains("no unread mail in concierge"), "{err}");
}

// covers: UC-operator-channel-01
#[test]
fn a_read_verb_on_a_missing_mailbox_fails_and_creates_nothing() {
    let (_t, e) = fixture("mail-uc01-missing");
    assert!(cmds::count(&e, "ghost").unwrap_err().contains("no such mailbox"));
    assert!(cmds::list(&e, "ghost", false).is_err());
    assert!(!e.mail_root.join("ghost").exists());
}

// covers: UC-operator-channel-01
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
}

// covers: UC-operator-channel-01
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
#[test]
fn a_send_with_no_subject_is_refused_and_delivers_nothing() {
    let (_t, e) = fixture("mail-uc02-subject");
    let err = send(&e, &args("concierge", FROM, "")).unwrap_err();
    assert!(err.contains("missing Subject"), "{err}");
    assert!(names(&e, "concierge", "new").is_empty());
}

// covers: UC-operator-channel-02
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
#[test]
fn template_prints_the_kinds_skeleton_and_an_unknown_kind_is_refused_and_named() {
    let (_t, e) = fixture("mail-uc03-template");
    assert_eq!(cmds::template(&e.kinds_dir, "note").unwrap(), "\n## Note\n\nsay it");
    assert_eq!(cmds::template(&e.kinds_dir, "nosuchkind").unwrap_err(), "unknown kind: nosuchkind");
    assert_eq!(cmds::template(&e.kinds_dir, "").unwrap_err(), "kind required");
}

// covers: UC-operator-channel-03
#[test]
fn a_send_of_an_unknown_kind_is_refused_naming_it_and_delivers_nothing() {
    let (_t, e) = fixture("mail-uc03-kind");
    let mut a = args("concierge", FROM, "odd kind");
    a.kind = "nosuchkind";
    assert!(send(&e, &a).unwrap_err().contains("nosuchkind"));
    assert!(names(&e, "concierge", "new").is_empty());
}

// covers: UC-operator-channel-04
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
#[test]
fn a_repeat_to_the_operator_within_the_window_is_refused_naming_the_override() {
    let (t, e) = fixture("mail-uc06-repeat");
    send(&e, &args("operator", FROM, "gate red 5 times")).unwrap();
    let err = send(&e, &args("operator", FROM, "gate red 6 times")).unwrap_err();
    assert!(err.contains("repeat refused") && err.contains("SPIRA_MAIL_REPEAT_CONSIDERED"), "{err}");
    assert_eq!(names(&e, "operator", "new").len(), 1);
    assert!(fs::read_dir(t.path().join("run/mail-repeat")).unwrap().flatten().any(|f| f.file_name().to_string_lossy().ends_with(".refused")));
}

// covers: UC-operator-channel-06
#[test]
fn a_repeat_outside_the_window_and_a_different_subject_are_delivered() {
    let (_t, mut e) = fixture("mail-uc06-window");
    send(&e, &args("operator", FROM, "gate red")).unwrap();
    send(&e, &args("operator", FROM, "queue stuck")).unwrap();
    e.repeat_window_s = 0;
    send(&e, &args("operator", FROM, "gate red")).unwrap();
    assert_eq!(names(&e, "operator", "new").len(), 3);
}

// covers: UC-operator-channel-06
#[test]
fn the_repeat_override_lets_a_repeat_through_with_a_header() {
    let (_t, mut e) = fixture("mail-uc06-override");
    send(&e, &args("operator", FROM, "gate red")).unwrap();
    e.repeat_considered = Some("operator asked again".into());
    let out = send(&e, &args("operator", FROM, "gate red")).unwrap();
    assert!(fs::read_to_string(out.delivered_path).unwrap().contains("\nX-Spira-Repeat-Override: operator asked again\n"));
}

// covers: UC-operator-channel-06
#[test]
fn the_repeat_guard_does_not_apply_to_other_mailboxes() {
    let (_t, e) = fixture("mail-uc06-other");
    send(&e, &args("concierge", FROM, "gate red")).unwrap();
    send(&e, &args("concierge", FROM, "gate red")).unwrap();
    assert_eq!(names(&e, "concierge", "new").len(), 2);
}

// covers: UC-operator-channel-06
#[test]
fn a_lint_refused_operator_send_writes_no_stamp_so_the_corrected_send_is_not_a_repeat() {
    let (t, e) = fixture("mail-uc06-lint-order");
    assert!(send(&e, &args("operator", None, "gate red")).unwrap_err().contains("missing From"));
    let stamps = fs::read_dir(t.path().join("run/mail-repeat")).map(|d| d.flatten().filter(|f| !f.file_name().to_string_lossy().ends_with(".lock")).count()).unwrap_or(0);
    assert_eq!(stamps, 0);
    send(&e, &args("operator", FROM, "gate red")).unwrap();
}

// covers: UC-operator-channel-07
#[test]
fn done_sets_the_replied_flag_once_and_a_second_done_changes_nothing() {
    let (_t, e) = fixture("mail-uc07-done");
    send(&e, &args("concierge", FROM, "handled")).unwrap();
    let id = names(&e, "concierge", "new")[0].clone();
    cmds::done(&e, "concierge", &id, None).unwrap();
    assert_eq!(names(&e, "concierge", "cur"), vec![format!("{id}:2,R")]);
    assert!(names(&e, "concierge", "new").is_empty());
    cmds::done(&e, "concierge", &id, None).unwrap();
    assert_eq!(names(&e, "concierge", "cur"), vec![format!("{id}:2,R")]);
}

// covers: UC-operator-channel-07
#[test]
fn done_on_an_unknown_id_fails_and_flags_nothing() {
    let (_t, e) = fixture("mail-uc07-unknown");
    send(&e, &args("concierge", FROM, "bystander")).unwrap();
    let before = names(&e, "concierge", "new");
    assert!(cmds::done(&e, "concierge", "no-such-id", None).unwrap_err().contains("not found"));
    assert_eq!(names(&e, "concierge", "new"), before);
    assert!(names(&e, "concierge", "cur").is_empty());
}
