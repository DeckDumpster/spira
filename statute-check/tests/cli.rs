use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_statute-check");

fn shelf() -> String {
    format!("{}/../spira/statutes", env!("CARGO_MANIFEST_DIR"))
}
fn vocab() -> String {
    format!("{}/../spira/statute-vocab", env!("CARGO_MANIFEST_DIR"))
}

fn check(args: &[&str]) -> (i32, String) {
    let o = Command::new(BIN).env_clear().args(["--dir", &shelf(), "--vocab", &vocab()]).args(args).output().unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn hook(state: &std::path::Path, env: &[(&str, &str)], command: &str) -> String {
    let payload = serde_json::json!({"tool_name": "Bash", "session_id": "s1", "tool_input": {"command": command}}).to_string();
    let log = state.join("overrides.log");
    let mut c = Command::new(BIN);
    c.env_clear().args(["hook", "--dir", &shelf(), "--vocab", &vocab(), "--state", state.to_str().unwrap(), "--log", log.to_str().unwrap()]);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut ch = c.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    ch.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    String::from_utf8_lossy(&ch.wait_with_output().unwrap().stdout).into_owned()
}

#[test]
fn an_unattributed_suite_red_in_two_rounds_is_governed_by_the_flipping_test_statute() {
    let (code, out) = check(&["suite red unattributed in two rounds with different members"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("law-a-test-that-flips-is-deleted\n"), "{out}");
    assert!(out.contains("A test that fails then passes on an unchanged tree is deleted"), "full text follows the slug");
}

#[test]
fn text_no_statute_governs_exits_one_and_prints_nothing() {
    let (code, out) = check(&["zzqx wibble"]);
    assert_eq!((code, out.as_str()), (1, ""));
}

#[test]
fn an_empty_shelf_is_refused_not_passed() {
    let d = testkit::TempDir::new("statute-check-empty");
    let o = Command::new(BIN).env_clear().args(["--dir", d.to_str().unwrap(), "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn the_first_queue_rework_is_refused_with_the_law_and_the_retry_passes_with_it_attached() {
    let d = testkit::TempDir::new("statute-check-hook");
    let env: [(&str, &str); 0] = [];
    let cmd = "queue rework sp-aaa111";
    let first: serde_json::Value = serde_json::from_str(&hook(&d, &env, cmd)).unwrap();
    assert_eq!(first["decision"], "block");
    let reason = first["reason"].as_str().unwrap();
    for want in ["law-a-test-that-flips-is-deleted", "law-landing-rounds-run-the-full-suite-on-a-vm"] {
        assert!(reason.contains(want), "{want} missing: {reason}");
    }
    assert!(!reason.contains("law-never-derive-an-id-from-output"), "an unrelated statute is not shown");
    assert!(reason.contains("STATUTE_CHECK_CONSIDERED"));
    let retry: serde_json::Value = serde_json::from_str(&hook(&d, &env, cmd)).unwrap();
    assert!(retry.get("decision").is_none(), "{retry}");
    assert!(retry["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("law-"));
}

#[test]
fn another_verb_class_is_refused_on_its_own_first_call() {
    let d = testkit::TempDir::new("statute-check-classes");
    let env: [(&str, &str); 0] = [];
    hook(&d, &env, "queue rework sp-1");
    assert!(hook(&d, &env, "queue round eject r1").contains("\"block\""));
}

#[test]
fn the_override_passes_and_is_logged_with_its_reason() {
    let d = testkit::TempDir::new("statute-check-override");
    let out = hook(&d, &[], "STATUTE_CHECK_CONSIDERED=\"law-a-test-that-flips-is-deleted read; red on both runs\" queue rework sp-1");
    assert_eq!(out, "");
    let log = std::fs::read_to_string(d.join("overrides.log")).unwrap();
    assert!(log.contains("red on both runs") && log.contains("queue rework"), "{log}");
}

#[test]
fn a_command_that_is_not_a_decision_is_untouched() {
    let d = testkit::TempDir::new("statute-check-nondecision");
    assert_eq!(hook(&d, &[], "queue status"), "");
}

#[test]
fn touching_a_rounds_pace_pause_file_is_refused_with_law_on_first_call() {
    let d = testkit::TempDir::new("statute-check-pause");
    let first = hook(&d, &[], "touch /run/x/rounds-pace/repo/pause");
    assert!(first.contains("\"block\"") && first.contains("law-"), "{first}");
    assert_eq!(hook(&d, &[], "cat /run/x/rounds-pace/repo/pause"), "");
}
