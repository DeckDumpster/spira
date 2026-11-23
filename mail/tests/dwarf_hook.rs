//! The shell hook is the subject (the binary-spawning exception): `dwarf-mail-deliver.sh` runs
//! under `bash` against a scratch Maildir. It moves files itself, so no `mail` binary is involved.

#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::path::Path;
use std::process::Command;

fn hook() -> std::path::PathBuf {
    testkit::manifest_dir().join("../spira/hooks/dwarf-mail-deliver.sh")
}

fn run(mail: &Path, bead: &str) -> (Option<i32>, String) {
    let out = Command::new("bash")
        .arg(hook())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_MAIL", mail)
        .env("BEAD_ID", bead)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn mailbox(t: &testkit::TempDir) -> std::path::PathBuf {
    for s in ["new", "cur", "tmp"] {
        fs::create_dir_all(t.path().join("dwarf-b1").join(s)).unwrap();
    }
    t.path().join("dwarf-b1")
}

fn plant(box_: &Path, name: &str, from: &str, body: &str) {
    fs::write(box_.join("new").join(name), format!("From: {from}\nSubject: s\nDate: Mon, 1 Jan 2024 00:00:00 +0000\nMessage-ID: <{name}@spira>\n\n{body}\n")).unwrap();
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).unwrap().count()
}

// covers: UC-operator-channel-11
#[test_id(t6niiow)]
#[test]
fn a_queued_dwarf_message_is_injected_as_post_tool_use_context_and_moved_to_cur() {
    let t = testkit::TempDir::new("dwarf-hook-deliver");
    let b = mailbox(&t);
    plant(&b, "123.msg", "Operator <op@spira>", "New requirement added.");
    let (code, out) = run(t.path(), "b1");
    assert_eq!(code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("New requirement added.") && ctx.contains("Operator"), "{ctx}");
    assert_eq!((count(&b.join("new")), count(&b.join("cur"))), (0, 1));
}

// covers: UC-operator-channel-11
#[test_id(t6mnbuy)]
#[test]
fn a_delivered_dwarf_message_is_not_injected_again_and_an_empty_mailbox_is_silent() {
    let t = testkit::TempDir::new("dwarf-hook-once");
    let b = mailbox(&t);
    assert_eq!(run(t.path(), "b1"), (Some(0), String::new()));
    plant(&b, "456.msg", "Ops <ops@spira>", "Do not forget to rebase.");
    assert!(run(t.path(), "b1").1.contains("Do not forget"));
    assert_eq!(run(t.path(), "b1"), (Some(0), String::new()));
}

// covers: UC-operator-channel-11
#[test_id(tizk6f4)]
#[test]
fn the_dwarf_hook_is_silent_and_leaves_mail_queued_when_bead_id_is_empty() {
    let t = testkit::TempDir::new("dwarf-hook-nobead");
    let b = mailbox(&t);
    plant(&b, "1.msg", "Op <op@spira>", "queued");
    let nameless = t.path().join("dwarf-");
    fs::create_dir_all(nameless.join("new")).unwrap();
    fs::create_dir_all(nameless.join("cur")).unwrap();
    plant(&nameless, "2.msg", "Op <op@spira>", "queued");
    assert_eq!(run(t.path(), ""), (Some(0), String::new()));
    assert_eq!((count(&b.join("new")), count(&nameless.join("new"))), (1, 1));
}
