//! The Concierge's hooks are shell scripts and are the subject (the binary-spawning
//! exception): SessionStart and the settings `concierge.sh` builds run under `bash`.

#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn hook_path(name: &str) -> PathBuf {
    testkit::manifest_dir().join("../spira/hooks").join(name)
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
    for want in ["spira-sub follow --as concierge", "spira-sub add|drop|list", "concierge/inbox", "watchd drain concierge-inbox --all"] {
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
            print(event, hook["command"].removeprefix("harness/hooks/"))
'"#;
    let out = Command::new("bash")
        .args(["-c", RUN, "bash"])
        .arg(testkit::manifest_dir().join("../concierge.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_HOME", "harness")
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
fn the_concierge_session_is_given_only_its_two_bash_hooks() {
    assert_eq!(session_hooks(), ["PreToolUse concierge-raw-pr-fence.sh", "PreToolUse statute-check-hook.sh"]);
}
