//! The Concierge Stop and StopFailure hooks are shell scripts and are the subject (the
//! binary-spawning exception): each runs under `bash` against a fake `/proc` tree
//! (`SPIRA_PROC_ROOT`), a transcript file and, for StopFailure, a stub `watchd`.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PROMPT_MS: u64 = 1_700_000_100_000;
const QUEUED_MS: u64 = 1_700_000_200_000;

fn hook_path(name: &str) -> PathBuf {
    testkit::manifest_dir().join("../spira/hooks").join(name)
}

struct Fx {
    tmp: testkit::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let tmp = testkit::TempDir::new("concierge-hooks");
        let d = tmp.path();
        for sub in ["proc/100", "proc/200", "proc/300", "proc/400", "run/watchd", "stubs"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        let w = |p: &str, c: &[u8]| std::fs::write(d.join(p), c).unwrap();
        w("proc/100/stat", b"100 (claude) S 1 ");
        w("proc/100/comm", b"claude");
        w("proc/200/stat", b"200 (bash) S 100 ");
        w("proc/300/cmdline", b"");
        w("proc/400/stat", b"400 (sleep) S 100 ");
        w("proc/400/cmdline", b"sleep\x00300\x00");
        let fx = Fx { tmp };
        fx.transcript(&[user(PROMPT_MS)]);
        fx
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }
    fn waiter(&self, argv: &[&str]) {
        let mut b = Vec::new();
        for a in argv {
            b.extend(a.as_bytes());
            b.push(0);
        }
        std::fs::write(self.p("proc/200/cmdline"), b).unwrap();
    }
    fn transcript(&self, lines: &[String]) {
        std::fs::write(self.p("transcript.jsonl"), lines.join("\n") + "\n").unwrap();
    }
    fn record(&self, exited_ms: u64, lines: u32, ancestors: &str) {
        std::fs::write(
            self.p("run/watchd/concierge-inbox.delivered"),
            format!("{{\"exited_ms\": {exited_ms}, \"lines\": {lines}, \"ancestors\": {ancestors}, \"text\": \"a thing needs you\"}}\n"),
        )
        .unwrap();
    }
    fn run(&self, hook: &str, env: &[(&str, &str)], stdin: &str) -> (i32, String) {
        let mut c = Command::new("bash");
        c.arg(hook_path(hook))
            .env_clear()
            .env("PATH", format!("{}:{}", self.p("stubs").display(), std::env::var("PATH").unwrap()))
            .env("SPIRA_RUN", self.p("run"))
            .env("SPIRA_PROC_ROOT", self.p("proc"))
            .env("SPIRA_CONCIERGE", "1")
            .env("SPIRA_CONCIERGE_SESSION_PID", "100")
            .env("CALLS", self.p("calls"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        (out.status.code().unwrap(), text)
    }
    fn stop(&self, env: &[(&str, &str)]) -> String {
        let input = format!("{{\"transcript_path\": \"{}\"}}", self.p("transcript.jsonl").display());
        let (code, out) = self.run("concierge-waiter-stop.sh", env, &input);
        assert_eq!(code, 0, "{out}");
        out
    }
    fn stop_blocks(&self) -> bool {
        let out = self.stop(&[]);
        if out.is_empty() {
            return false;
        }
        assert_eq!(out.matches("\"decision\": \"block\"").count(), 1, "{out}");
        true
    }
}

fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86400, secs % 86400);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}
fn user(ms: u64) -> String {
    format!("{{\"type\":\"user\",\"message\":{{\"content\":\"hi\"}},\"timestamp\":\"{}\"}}", iso(ms))
}
fn result(ms: u64) -> String {
    format!("{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"content\":\"x\"}}]}},\"timestamp\":\"{}\"}}", iso(ms))
}
fn queued(ms: u64) -> String {
    format!("{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"queued_command\",\"prompt\":\"<task-notification>\"}},\"timestamp\":\"{}\"}}", iso(ms))
}

const WATCHD: &[&str] = &["x/bin/watchd", "next", "concierge-inbox"];

// covers: UC-operator-channel-49
#[test_id(t0vcpv0)]
#[test]
fn a_live_waiter_under_the_session_allows_idle_and_vanishing_siblings_are_silent() {
    let fx = Fx::new();
    fx.waiter(WATCHD);
    assert_eq!(fx.stop(&[]), "");
}

// covers: UC-operator-channel-49
#[test_id(tkvpwyi)]
#[test]
fn no_waiter_and_no_record_blocks_once_naming_the_waiter_and_the_override() {
    let fx = Fx::new();
    let out = fx.stop(&[]);
    assert_eq!(out.matches("\"decision\": \"block\"").count(), 1, "{out}");
    assert!(out.contains("watchd next concierge-inbox"), "{out}");
    assert!(out.contains("SPIRA_CONCIERGE_ALLOW_IDLE"), "{out}");
}

// covers: UC-operator-channel-49
#[test_id(ts7yep8)]
#[test]
fn the_retired_inbox_next_argv_and_a_watchd_on_another_watcher_do_not_satisfy_it() {
    let fx = Fx::new();
    fx.waiter(&["bash", &fx.p("run/concierge-notes/inbox-next.sh").display().to_string()]);
    assert!(fx.stop_blocks());
    fx.waiter(&["x/bin/watchd", "next", "other-watcher"]);
    assert!(fx.stop_blocks());
}

// covers: UC-operator-channel-49
#[test_id(tbik4e8)]
#[test]
fn a_waiter_of_this_session_that_delivered_during_the_turn_allows_idle() {
    let fx = Fx::new();
    fx.record(PROMPT_MS + 5000, 1, "[100, 1]");
    assert!(!fx.stop_blocks());
}

// covers: UC-operator-channel-49
#[test_id(tzu3d1b)]
#[test]
fn a_delivery_before_the_last_prompt_or_queued_notice_blocks() {
    let fx = Fx::new();
    fx.record(PROMPT_MS - 5000, 1, "[100, 1]");
    assert!(fx.stop_blocks());
    fx.record(QUEUED_MS - 5000, 1, "[100, 1]");
    fx.transcript(&[user(PROMPT_MS), queued(QUEUED_MS)]);
    assert!(fx.stop_blocks());
}

// covers: UC-operator-channel-49
#[test_id(t395lce)]
#[test]
fn another_sessions_delivery_or_a_record_of_zero_lines_blocks() {
    let fx = Fx::new();
    fx.record(PROMPT_MS + 5000, 1, "[300, 1]");
    assert!(fx.stop_blocks());
    fx.record(PROMPT_MS + 5000, 0, "[100, 1]");
    assert!(fx.stop_blocks());
}

// covers: UC-operator-channel-49
#[test_id(tth1fyv)]
#[test]
fn a_missing_empty_or_unparseable_input_blocks() {
    let fx = Fx::new();
    fx.record(PROMPT_MS + 5000, 1, "[100, 1]");
    std::fs::remove_file(fx.p("transcript.jsonl")).unwrap();
    assert!(fx.stop_blocks(), "transcript missing");
    fx.transcript(&[]);
    assert!(fx.stop_blocks(), "no input entry");
    fx.transcript(&[user(PROMPT_MS)]);
    std::fs::write(fx.p("run/watchd/concierge-inbox.delivered"), "not json").unwrap();
    assert!(fx.stop_blocks(), "unparseable record");
}

// covers: UC-operator-channel-49
#[test_id(trps0i7)]
#[test]
fn a_later_tool_result_does_not_move_the_turn_start() {
    let fx = Fx::new();
    fx.transcript(&[user(PROMPT_MS), result(PROMPT_MS + 9000)]);
    fx.record(PROMPT_MS + 5000, 1, "[100, 1]");
    assert!(!fx.stop_blocks());
}

// covers: UC-operator-channel-49
#[test_id(txeee2n)]
#[test]
fn the_override_allows_idle_and_outside_the_concierge_the_stop_hook_is_inert() {
    let fx = Fx::new();
    assert_eq!(fx.stop(&[("SPIRA_CONCIERGE_ALLOW_IDLE", "1")]), "");
    assert_eq!(fx.stop(&[("SPIRA_CONCIERGE", "0")]), "");
}

/// Stands in for `watchd undeliver`, whose own behaviour watchd's unit tests prove: it only
/// records the call and, for a delivery whose ancestors hold the pid, copies the text over.
const WATCHD_STUB: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
[ "$1 $2 $3" = "undeliver concierge-inbox --session" ] || exit 2
grep -q "\"ancestors\": \[.*\b$4\b" "$SPIRA_RUN/watchd/concierge-inbox.delivered" || exit 0
sed 's/.*"text": "\(.*\)"}$/\1/' "$SPIRA_RUN/watchd/concierge-inbox.delivered" > "$SPIRA_RUN/watchd/concierge-inbox.redeliver"
"#;

fn failed(fx: &Fx, env: &[(&str, &str)]) -> String {
    testkit::write_exe(fx.p("stubs/watchd"), WATCHD_STUB);
    let (code, out) = fx.run("concierge-turn-failed.sh", env, r#"{"hook_event_name": "StopFailure", "error": "overloaded_error"}"#);
    assert_eq!(code, 0, "{out}");
    out
}
fn exists(p: &Path) -> bool {
    p.exists()
}

// covers: UC-operator-channel-50
#[test_id(trx5wq2)]
#[test]
fn outside_the_concierge_the_failed_turn_hook_prints_and_hands_back_nothing() {
    let fx = Fx::new();
    fx.record(1, 1, "[100,1]");
    assert_eq!(failed(&fx, &[("SPIRA_CONCIERGE", "0")]), "");
    assert!(!exists(&fx.p("run/watchd/concierge-inbox.redeliver")) && !exists(&fx.p("calls")));
}

// covers: UC-operator-channel-50
#[test_id(tfwtyot)]
#[test]
fn a_failed_turn_hands_this_sessions_delivery_back_for_the_next_waiter() {
    let fx = Fx::new();
    fx.record(1, 1, "[100,1]");
    assert_eq!(failed(&fx, &[]), "");
    assert_eq!(std::fs::read_to_string(fx.p("run/watchd/concierge-inbox.redeliver")).unwrap().trim(), "a thing needs you");
    assert_eq!(std::fs::read_to_string(fx.p("calls")).unwrap().trim(), "undeliver concierge-inbox --session 100");
}

// covers: UC-operator-channel-50
#[test_id(tli4axi)]
#[test]
fn another_sessions_delivery_is_not_handed_back() {
    let fx = Fx::new();
    fx.record(1, 1, "[300,1]");
    failed(&fx, &[]);
    assert!(!exists(&fx.p("run/watchd/concierge-inbox.redeliver")));
}

// covers: UC-operator-channel-50
#[test_id(t9ysi1r)]
#[test]
fn with_no_claude_ancestor_the_failed_turn_hook_asks_nothing_and_exits_zero() {
    let fx = Fx::new();
    fx.record(1, 1, "[100,1]");
    std::fs::remove_file(fx.p("proc/100/comm")).unwrap();
    testkit::write_exe(fx.p("stubs/watchd"), WATCHD_STUB);
    let mut c = Command::new("bash");
    c.arg(hook_path("concierge-turn-failed.sh"))
        .env_clear()
        .env("PATH", format!("{}:{}", fx.p("stubs").display(), std::env::var("PATH").unwrap()))
        .env("SPIRA_RUN", fx.p("run"))
        .env("SPIRA_PROC_ROOT", fx.p("proc"))
        .env("SPIRA_CONCIERGE", "1")
        .env("CALLS", fx.p("calls"))
        .stdin(Stdio::null());
    assert!(c.status().unwrap().success());
    assert!(!exists(&fx.p("calls")));
}

/// The SessionStart hook's output, run beside an empty `conf.sh` as the harness in force.
fn session_start(env: &[(&str, &str)]) -> String {
    let tmp = testkit::TempDir::new("concierge-session-start");
    let d = tmp.path();
    for sub in ["hooks", "bin"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    std::fs::copy(hook_path("session.sh"), d.join("hooks/session.sh")).unwrap();
    std::fs::write(d.join("conf.sh"), "").unwrap();
    std::fs::write(d.join("inbox"), "").unwrap();
    testkit::write_exe(d.join("bin/watchd"), "#!/bin/sh\n[ \"$1\" = status ] && echo NAME\nexit 0\n");
    let mut command = Command::new("bash");
    command
        .arg(d.join("hooks/session.sh"))
        .env_clear()
        .env("PATH", format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap()))
        .env("SPIRA_RUN", d.join("run"))
        .env("SPIRA_CONCIERGE_INBOX", d.join("inbox"))
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    let out = command.output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test_id(t616ksq)]
#[test]
fn the_concierges_session_start_names_the_event_stream_and_no_one_shot_waiter() {
    let out = session_start(&[("SPIRA_CONCIERGE", "1")]);
    for want in ["spira-sub follow --as concierge", "spira-sub add|drop|list"] {
        assert!(out.contains(want), "{want}: {out}");
    }
    assert!(!out.contains("watchd next"), "{out}");
    assert!(!session_start(&[]).contains("spira-sub"), "another session is told nothing of it");
}

/// One `event script` line per hook `concierge.sh`'s `session_settings` gives the session.
fn session_hooks() -> Vec<String> {
    const RUN: &str = r#"eval "$(sed -n '/^session_settings() {$/,/^}$/p' "$1")"
session_settings | python3 -c '
import json, sys
for event, groups in json.load(sys.stdin)["hooks"].items():
    for group in groups:
        for hook in group["hooks"]:
            print(event, hook["command"].removeprefix("/h/hooks/"))
'"#;
    let out = Command::new("bash")
        .args(["-c", RUN, "bash"])
        .arg(testkit::manifest_dir().join("../concierge.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_HOME", "/h")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
    lines.sort();
    lines
}

// covers: UC-operator-channel-49
#[test_id(txibqk0)]
#[test]
fn the_concierge_session_is_given_no_stop_hook_beside_its_failed_turn_and_bash_hooks() {
    let hooks = session_hooks();
    let stop: Vec<&String> = hooks.iter().filter(|h| h.starts_with("Stop ")).collect();
    assert!(stop.is_empty(), "{stop:?}");
    assert_eq!(hooks, ["PreToolUse concierge-raw-pr-fence.sh", "PreToolUse statute-check-hook.sh", "StopFailure concierge-turn-failed.sh"]);
}
