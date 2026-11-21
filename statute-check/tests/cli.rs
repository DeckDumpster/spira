#[macro_use]
extern crate spira_test_id;
use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> std::path::PathBuf {
    testkit::bin_exe("statute-check")
}

fn shelf() -> String {
    spira_config::resolve::locate_home_for_process().unwrap().join("statutes").display().to_string()
}
fn vocab() -> String {
    spira_config::resolve::locate_home_for_process().unwrap().join("statute-vocab").display().to_string()
}

fn check(args: &[&str]) -> (i32, String) {
    let o = Command::new(bin()).env_clear().args(["--dir", &shelf(), "--vocab", &vocab()]).args(args).output().unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn hook(state: &std::path::Path, env: &[(&str, &str)], command: &str) -> String {
    let payload = serde_json::json!({"tool_name": "Bash", "session_id": "s1", "tool_input": {"command": command}}).to_string();
    let log = state.join("overrides.log");
    let mut c = Command::new(bin());
    let doors = spira_config::resolve::locate_home_for_process().unwrap().join("doors.toml").display().to_string();
    let doors_log = state.join("doors.log");
    c.env_clear().args(["hook", "--dir", &shelf(), "--vocab", &vocab(), "--state", state.to_str().unwrap(), "--log", log.to_str().unwrap()]);
    c.args(["--doors", &doors, "--doors-log", doors_log.to_str().unwrap()]);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut ch = c.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    ch.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    String::from_utf8_lossy(&ch.wait_with_output().unwrap().stdout).into_owned()
}

#[test_id(tmoxq08)]
#[test]
fn an_unattributed_suite_red_in_two_rounds_is_governed_by_the_flipping_test_statute() {
    let (code, out) = check(&["suite red unattributed in two rounds with different members"]);
    assert_eq!(code, 0);
    assert!(out.contains("law-a-flipping-test-is-disabled-by-a-registry-row\n"), "{out}");
    assert!(out.contains("A test that fails then passes on an unchanged tree is disabled"), "full text follows the slug");
}

#[test_id(tejf6m0)]
#[test]
fn text_no_statute_governs_exits_one_and_prints_nothing() {
    let (code, out) = check(&["zzqx wibble"]);
    assert_eq!((code, out.as_str()), (1, ""));
}

#[test_id(triy07f)]
#[test]
fn an_empty_shelf_is_refused_not_passed() {
    let d = testkit::TempDir::new("statute-check-empty");
    let o = Command::new(bin()).env_clear().args(["--dir", d.to_str().unwrap(), "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test_id(t7x2o4y)]
#[test]
fn the_first_queue_rework_is_refused_with_the_law_and_the_retry_passes_with_it_attached() {
    let d = testkit::TempDir::new("statute-check-hook");
    let env: [(&str, &str); 0] = [];
    let cmd = "queue rework sp-aaa111";
    let first: serde_json::Value = serde_json::from_str(&hook(&d, &env, cmd)).unwrap();
    assert_eq!(first["decision"], "block");
    let reason = first["reason"].as_str().unwrap();
    for want in ["law-a-flipping-test-is-disabled-by-a-registry-row", "law-a-provisional-member-leaves-on-its-gate-red"] {
        assert!(reason.contains(want), "{want} missing: {reason}");
    }
    assert!(!reason.contains("law-never-derive-an-id-from-output"), "an unrelated statute is not shown");
    assert!(reason.contains("STATUTE_CHECK_CONSIDERED"));
    let retry: serde_json::Value = serde_json::from_str(&hook(&d, &env, cmd)).unwrap();
    assert!(retry.get("decision").is_none(), "{retry}");
    assert!(retry["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("law-"));
}

#[test_id(te5259l)]
#[test]
fn another_verb_class_is_refused_on_its_own_first_call() {
    let d = testkit::TempDir::new("statute-check-classes");
    let env: [(&str, &str); 0] = [];
    hook(&d, &env, "queue rework sp-1");
    assert!(hook(&d, &env, "queue round eject r1").contains("\"block\""));
}

#[test_id(tudu2qw)]
#[test]
fn the_override_passes_and_is_logged_with_its_reason() {
    let d = testkit::TempDir::new("statute-check-override");
    let out = hook(&d, &[], "STATUTE_CHECK_CONSIDERED=\"law-a-flipping-test-is-disabled-by-a-registry-row read; red on both runs\" queue rework sp-1");
    assert_eq!(out, "");
    let log = std::fs::read_to_string(d.join("overrides.log")).unwrap();
    assert!(log.contains("red on both runs") && log.contains("queue rework"), "{log}");
}

#[test_id(to0zh1l)]
#[test]
fn a_command_that_is_not_a_decision_is_untouched() {
    let d = testkit::TempDir::new("statute-check-nondecision");
    assert_eq!(hook(&d, &[], "queue status"), "");
}

#[test_id(t1oubao)]
#[test]
fn touching_a_rounds_pace_pause_file_is_refused_with_law_on_first_call() {
    let d = testkit::TempDir::new("statute-check-pause");
    let first = hook(&d, &[], "touch /run/x/rounds-pace/repo/pause");
    assert!(first.contains("\"block\"") && first.contains("law-"), "{first}");
    assert_eq!(hook(&d, &[], "cat /run/x/rounds-pace/repo/pause"), "");
}

#[test_id(tg1mbqq)]
#[test]
fn a_hand_move_of_the_landing_ref_is_stopped_once_and_shown_the_tool_and_logged_without_arguments() {
    let d = testkit::TempDir::new("statute-check-door");
    let env = [("SPIRA_RUNE", "concierge")];
    let cmd = "git update-ref refs/heads/local/main 0123456789abcdef";
    let first: serde_json::Value = serde_json::from_str(&hook(&d, &env, cmd)).unwrap();
    assert_eq!(first["decision"], "block");
    let reason = first["reason"].as_str().unwrap();
    assert!(reason.contains("This touches landing-ref. Its tool is queue hand-land: queue.sh hand-land <id>"), "{reason}");
    assert!(!hook(&d, &env, cmd).contains("\"block\""));
    let log = std::fs::read_to_string(d.join("doors.log")).unwrap();
    let events: Vec<Vec<&str>> = log.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(events.len(), 2, "{log}");
    assert_eq!((events[0][1], events[0][2], events[0][3], events[0][4]), ("concierge", "landing-ref", "git", "blocked"));
    assert!(!log.contains("0123456789abcdef"), "{log}");
}
