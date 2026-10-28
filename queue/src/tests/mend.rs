//! `queue round mended|return|environment|flaky|unsure`: what the verbs send spira-lc. The
//! machine's own refusals (caps, the escalation check, stale tips) are tested in lifecycle
//! and spira-lc; here the facts the verb assembles are what is under test.

use super::*;

fn mend_world() -> (T, String) {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/heads/local/main", "b0");
    t.git.set("refs/remotes/origin/main", "f0");
    for (id, tip) in [("sp-a", "ta"), ("sp-b", "tb")] {
        t.lc_row(id, "CERTIFIED", tip, 100);
        t.git.ancestor("b0", tip);
        t.git.ancestor(tip, "merged-tb");
    }
    t.git
        .trees
        .borrow_mut()
        .insert("merged-ta".into(), "3".repeat(40));
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), "3".repeat(40));
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = t
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("batch="))
        .unwrap()
        .to_string();
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    t.git.file(
        "b0",
        "spira/mend-fences",
        "# fences\ngate/\nspira/gate-suites\n",
    );
    t.git.stat("b0", "ta", &[("queue/src/a.rs", 4, 0)]);
    t.git.stat("b0", "tb", &[("queue/src/b.rs", 2, 0)]);
    t.git.file("ta2", "queue/Cargo.toml", "");
    t.git.ancestor("ta", "ta2");
    (t, batch)
}

fn mend_call(t: &T) -> Vec<String> {
    t.lc.calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("mend "))
        .cloned()
        .collect()
}

fn mending_events(t: &T) -> Vec<String> {
    t.lc.calls
        .borrow()
        .iter()
        .filter_map(|c| c.strip_prefix("mending-event "))
        .map(String::from)
        .collect()
}

#[test]
fn mended_sends_the_move_the_diff_and_the_checks_the_touched_crate_requires() {
    let (t, batch) = mend_world();
    t.git.stat("ta", "ta2", &[("queue/src/a.rs", 1, 1)]);
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-x.sh",
            "--tips",
            "sp-a:ta2",
            "--checked",
            "crate:queue"
        ]),
        0,
        "{}",
        t.err()
    );
    let calls = mend_call(&t);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with(&format!("mend {batch} 1 test-x.sh sp-a:ta:ta2 ")),
        "{}",
        calls[0]
    );
    assert!(
        calls[0].contains("\"required_checks\":[\"crate:queue\"]")
            && calls[0].contains("\"checked\":[\"crate:queue\"]"),
        "{}",
        calls[0]
    );
    assert!(
        calls[0].contains("\"member_files\":[\"queue/src/a.rs\",\"queue/src/b.rs\"]"),
        "every member's diff is the reference: {}",
        calls[0]
    );
    assert!(
        calls[0].contains("fences=[\"gate/\",\"spira/gate-suites\"]"),
        "the fence list is read from the base: {}",
        calls[0]
    );
}

#[test]
fn a_fence_edit_reaches_the_machine_with_no_ask_so_the_machine_can_refuse_it() {
    let (t, batch) = mend_world();
    t.git.stat("ta", "ta2", &[("gate/src/lib.rs", 3, 1)]);
    *t.lc.mend_refused.borrow_mut() = Some(
        "refused: MendRefused { reasons: [\"gate/src/lib.rs is gate or fence code\"] }".into(),
    );
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-x.sh",
            "--tips",
            "sp-a:ta2",
            "--checked",
            "crate:queue"
        ]),
        1
    );
    assert!(
        mend_call(&t)[0].contains("\"path\":\"gate/src/lib.rs\"")
            && mend_call(&t)[0].contains("\"ask\":null"),
        "{:?}",
        mend_call(&t)
    );
    assert!(t.err().contains("gate or fence code"), "{}", t.err());
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-x.sh",
            "--tips",
            "sp-a:ta2",
            "--checked",
            "crate:queue",
            "--ask",
            "ask-9"
        ]),
        1
    );
    assert!(
        mend_call(&t)[1].contains("\"ask\":\"ask-9\""),
        "{:?}",
        mend_call(&t)
    );
}

#[test]
fn a_second_mender_builds_on_the_first_menders_tip_or_returns() {
    let (t, batch) = mend_world();
    t.lc.mend_tips
        .borrow_mut()
        .insert("sp-a".into(), "ta2".into());
    t.git.ancestor("ta2", "ta3");
    t.git.stat("ta2", "ta3", &[("queue/src/a.rs", 1, 0)]);
    t.git.file("ta3", "queue/Cargo.toml", "");
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-y.sh",
            "--tips",
            "sp-a:ta3",
            "--checked",
            "crate:queue"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        mend_call(&t)[0].contains("sp-a:ta2:ta3"),
        "the move starts from the recorded tip: {:?}",
        mend_call(&t)
    );

    t.git.ancestor("ta", "tz");
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-z.sh",
            "--tips",
            "sp-a:tz",
            "--checked",
            ""
        ]),
        1
    );
    assert_eq!(
        mend_call(&t).len(),
        1,
        "a fix that does not apply is never offered to the machine"
    );
    let events = mending_events(&t);
    assert!(
        events.last().unwrap().contains("test-z.sh")
            && events.last().unwrap().contains("fix no longer applies")
            && events.last().unwrap().contains("Return"),
        "{events:?}"
    );
}

#[test]
fn a_mend_is_refused_without_the_fence_list_at_the_base() {
    let (t, batch) = mend_world();
    t.git.files.borrow_mut().remove("b0:spira/mend-fences");
    t.git.stat("ta", "ta2", &[("queue/src/a.rs", 1, 0)]);
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-x.sh",
            "--tips",
            "sp-a:ta2",
            "--checked",
            "crate:queue"
        ]),
        1
    );
    assert!(
        mend_call(&t).is_empty() && t.err().contains("fence list"),
        "{}",
        t.err()
    );
}

#[test]
fn the_third_mend_on_a_bead_escalates_once_to_the_operator() {
    let (t, batch) = mend_world();
    t.git.stat("ta", "ta2", &[("queue/src/a.rs", 1, 0)]);
    *t.lc.mend_refused.borrow_mut() =
        Some("refused: MendCap { bead: \"sp-a\", mends: 2 }; the failure is returned".into());
    assert_eq!(
        t.run(&[
            "round",
            "mended",
            &batch,
            "test-x.sh",
            "--tips",
            "sp-a:ta2",
            "--checked",
            "crate:queue"
        ]),
        1
    );
    let mails = t
        .scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("mail-operator"))
        .count();
    assert_eq!(mails, 1, "{:?}", t.scripts.calls.borrow());
}

#[test]
fn the_other_outcomes_are_one_event_each_and_return_needs_its_diagnosis() {
    let (t, batch) = mend_world();
    assert_eq!(
        t.run(&[
            "round",
            "environment",
            &batch,
            "test-e.sh",
            "--reason",
            "fixture server never started"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "unsure", &batch, "test-u.sh"]), 0);
    assert_eq!(t.run(&["round", "flaky", &batch, "test-f.sh"]), 0);
    assert_eq!(
        t.run(&["round", "return", &batch, "test-r.sh"]),
        2,
        "a return carries its diagnosis"
    );
    assert_eq!(
        t.run(&[
            "round",
            "return",
            &batch,
            "test-r.sh",
            "--reason",
            "sp-a drops the lock"
        ]),
        0
    );
    let events = mending_events(&t).join("\n");
    for want in [
        "test-e.sh {\"Diagnosis\"",
        "test-e.sh {\"Environment\"",
        "test-u.sh {\"Unsure\"",
        "test-f.sh {\"Flaky\"",
        "test-r.sh {\"Return\":{\"at\":",
        "sp-a drops the lock",
    ] {
        assert!(events.contains(want), "{want} missing from:\n{events}");
    }
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("observe-flake test-f.sh")),
        "{:?}",
        t.scripts.calls.borrow()
    );
}

#[test]
fn await_ask_sends_the_ask_the_failure_waits_on_and_needs_one() {
    let (t, batch) = mend_world();
    assert_eq!(t.run(&["round", "await-ask", &batch, "test-a.sh"]), 2);
    assert_eq!(t.run(&["round", "await-ask", &batch, "test-a.sh", "--ask", "ask-9"]), 0, "{}", t.err());
    let events = mending_events(&t).join("\n");
    assert!(events.contains("test-a.sh {\"Await\":{\"ask\":\"ask-9\"}}"), "{events}");
}

#[test]
fn every_entry_of_the_shipped_fence_list_names_something_that_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let list = fs::read_to_string(root.join("spira/mend-fences")).unwrap();
    let entries: Vec<&str> = list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert!(entries.len() > 3);
    for e in entries {
        let hit = match e.strip_suffix('*') {
            Some(prefix) => {
                let (dir, name) = prefix.rsplit_once('/').unwrap_or((".", prefix));
                fs::read_dir(root.join(dir))
                    .unwrap()
                    .filter_map(Result::ok)
                    .any(|f| f.file_name().to_string_lossy().starts_with(name))
            }
            None => root.join(e).exists(),
        };
        assert!(hit, "{e} fences nothing: the path was renamed or removed");
    }
}
