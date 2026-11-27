//! The Concierge's hooks are shell scripts and are the subject (the binary-spawning
//! exception): StopFailure runs under `bash` against a fake `/proc` tree (`SPIRA_PROC_ROOT`)
//! and a stub `watchd`; SessionStart and the settings `concierge.sh` builds run likewise.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        for sub in ["proc/100", "proc/200", "run/watchd", "stubs"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        let w = |p: &str, c: &[u8]| std::fs::write(d.join(p), c).unwrap();
        w("proc/100/stat", b"100 (claude) S 1 ");
        w("proc/100/comm", b"claude");
        w("proc/200/stat", b"200 (bash) S 100 ");
        Fx { tmp }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
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
