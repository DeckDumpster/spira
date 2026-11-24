//! `queue round` (DESIGN.md §2.2): one world of fakes, a real scratch directory for the
//! round record, the markers round-duty reads and the certificate land-local checks.

use super::*;

const T_ROUND: &str = "3333333333333333333333333333333333333333";

pub(super) fn round_world() -> T {
    let t = T::new(LandMode::QueueLocal);
    t.env
        .resolved
        .borrow_mut()
        .insert("SPIRA_PLAN_LABEL".into(), "plan-fixture".into());
    t.git.set("refs/heads/local/main", "b0");
    t.git.set("refs/remotes/origin/main", "f0");
    for (id, tip) in [("sp-a", "ta"), ("sp-b", "tb"), ("sp-c", "tc")] {
        t.lc_row(id, "CERTIFIED", tip, 100);
        t.git.ancestor(tip, "merged-tc");
        t.git.ancestor(tip, "merged-tb");
    }
    for head in ["merged-ta", "merged-tb", "merged-tc"] {
        t.git.ancestor("b0", head);
        t.git.trees.borrow_mut().insert(head.into(), T_ROUND.into());
    }
    t
}

fn kv_of(t: &T, name: &str) -> BTreeMap<String, String> {
    fs::read_to_string(t.qfile(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn round_phase(t: &T) -> String {
    let rec = kv_of(t, "round");
    assert!(
        !rec.contains_key("phase") && !rec.contains_key("phase_at"),
        "the round file carries no phase: {rec:?}"
    );
    t.lc.batch_state(&rec["batch_id"]).unwrap().0
}

pub(super) fn marker(t: &T, batch: &str, ext: &str) -> PathBuf {
    t.s().run.join("rounds").join(format!("{batch}.{ext}"))
}

pub(super) fn batch_of(t: &T) -> String {
    t.out()
        .lines()
        .find_map(|l| l.strip_prefix("batch="))
        .unwrap()
        .to_string()
}

fn certify_lock(t: &T, batch: &str) -> PathBuf {
    t.s()
        .queue_dir
        .join("spira")
        .join(format!("certify-{batch}.lock"))
}

/// Every `.result` line opens `<UTC ISO-8601> <cause>`, the cause from the closed set.
fn result_lines(t: &T, batch: &str) -> Vec<(String, String, String)> {
    let text = fs::read_to_string(marker(t, batch, "result")).unwrap();
    text.lines()
        .map(|l| {
            let mut f = l.splitn(3, ' ');
            let (at, cause, detail) = (
                f.next().unwrap_or(""),
                f.next().unwrap_or(""),
                f.next().unwrap_or(""),
            );
            assert!(
                at.len() == 20 && at.ends_with('Z') && at.as_bytes()[10] == b'T',
                "no UTC stamp: {l:?}"
            );
            assert!(
                crate::ops::round::END_KINDS
                    .iter()
                    .any(|k| if k.ends_with(':') {
                        cause.starts_with(k) && cause.len() > k.len()
                    } else {
                        cause == *k
                    }),
                "cause {cause:?} is not in the set: {l:?}"
            );
            (at.to_string(), cause.to_string(), detail.to_string())
        })
        .collect()
}

fn certified_tree_exists(t: &T) -> bool {
    gate::cert::path(&t.s().run.join("verdicts"), "spira", T_ROUND).is_some_and(|p| p.exists())
}

#[test_id(t1m7ggd)]
#[test]
fn open_certify_land_is_one_round_the_whole_way() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok 3 1 fp p e 0".into()),
        ("test-b.sh".into(), "skip".into()),
    ];

    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(batch, "spira-20260929T010203Z");
    assert!(
        t.out().contains("head=merged-tb") && t.out().contains("members=sp-a:ta sp-b:tb"),
        "{}",
        t.out()
    );
    assert!(
        t.lc.has(&format!("cut {batch} sp-a:ta,sp-b:tb")),
        "spira-lc cut records the batch and moves the members"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            round_phase(&t).as_str(),
            rec["head"].as_str(),
            rec["base"].as_str()
        ),
        ("OPEN", "merged-tb", "b0")
    );
    assert!(
        marker(&t, &batch, "running").exists(),
        "round-duty sees a round in flight"
    );

    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(
        status.contains("round=open")
            && status.contains(&format!("batch_id={batch}"))
            && status.contains("phase=opened"),
        "{status}"
    );
    assert!(status.contains("wall_secs=0"), "{status}");

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("wall=900") && c.contains(&format!("round={batch}/spira"))),
        "the corpus runs under the configured cap and reports each red to its round: {:?}",
        t.scripts.calls.borrow()
    );
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 3 {{\"PassGreen\":{{\"n\":1,\"suites_s\":0,\"build_s\":0,\"runner\":{{\"place\":\"unrecorded\",\"why\":\"the pass left no runner.place\"}}}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(
        t.out().lines().any(|l| l.contains(" is GREEN at ") && l.contains("runner=unrecorded (")),
        "a results dir without runner.place is named unrecorded, never left out: {}",
        t.out()
    );
    assert!(
        certified_tree_exists(&t),
        "the round GREEN certificate is on the head's tree"
    );
    assert_eq!(round_phase(&t), "GREEN");

    assert!(certify_lock(&t, &batch).exists(), "certify left its lock");
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(
        !certify_lock(&t, &batch).exists(),
        "a landed round leaves no certify lock"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
    assert!(
        t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"")
            && t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\"")
    );
    assert!(
        t.lc.has(&format!("land {batch} 4 merged-tb")),
        "spira-lc lands the batch"
    );
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
    assert_eq!(
        result_lines(&t, &batch),
        [(
            "1970-01-01T00:16:40Z".to_string(),
            "landed".to_string(),
            "merged-tb members=sp-a:ta,sp-b:tb".to_string()
        )]
    );
    let log = t.landing_log();
    assert!(
        log.lines().any(|l| l.starts_with("QUEUE ROUND-LAND ")
            && l.ends_with(&format!(
                " repo=spira batch={batch} by=batcher head=merged-tb members=sp-a:ta,sp-b:tb"
            ))),
        "the landing log names the round's members (run-state-trail): {log}"
    );
}

#[test_id(tz5woec)]
#[test]
fn a_land_that_waited_on_the_lock_while_another_lander_landed_the_round_answers_already_landed() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());

    let held = std::rc::Rc::new(std::cell::RefCell::new(Some(t.hold_lock())));
    let (h, log, b) = (held.clone(), t.s().run.join("landing.log"), batch.clone());
    let git: *const FGit = &t.git;
    *t.clock.on_sleep.borrow_mut() = Some(Box::new(move || {
        let line = format!("QUEUE ROUND-LAND 1 repo=spira batch={b} by=other head=merged-tb members=sp-a:ta,sp-b:tb\n");
        let mut all = fs::read_to_string(&log).unwrap_or_default();
        all.push_str(&line);
        fs::write(&log, all).unwrap();
        // SAFETY: the world outlives the verb that sleeps.
        let git = unsafe { &*git };
        git.set("refs/heads/local/main", "merged-tb");
        git.ancestor("merged-tb", "local/main");
        h.borrow_mut().take();
    }));
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(t.out().contains(&format!("round {batch} already landed at merged-tb")), "{}", t.out());
    let landed = t.landing_log().lines().filter(|l| l.starts_with("QUEUE ROUND-LAND ") && l.contains(&format!(" batch={batch} "))).count();
    assert_eq!(landed, 1, "{}", t.landing_log());
    assert!(!t.lc.has(&format!("land {batch}")), "{:?}", t.lc.calls.borrow());
}

#[test_id(tftrst9)]
#[test]
fn a_land_that_waited_on_the_lock_finds_the_landing_in_a_rotated_log_file() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());

    let held = std::rc::Rc::new(std::cell::RefCell::new(Some(t.hold_lock())));
    let (h, log, b) = (held.clone(), t.s().run.join("landing.log.1"), batch.clone());
    let git: *const FGit = &t.git;
    *t.clock.on_sleep.borrow_mut() = Some(Box::new(move || {
        let line = format!("QUEUE ROUND-LAND 1 repo=spira batch={b} by=other head=merged-tb members=sp-a:ta,sp-b:tb\n");
        let mut all = fs::read_to_string(&log).unwrap_or_default();
        all.push_str(&line);
        fs::write(&log, all).unwrap();
        // SAFETY: the world outlives the verb that sleeps.
        let git = unsafe { &*git };
        git.set("refs/heads/local/main", "merged-tb");
        git.ancestor("merged-tb", "local/main");
        h.borrow_mut().take();
    }));
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(
        t.out()
            .contains(&format!("round {batch} already landed at merged-tb")),
        "{}",
        t.out()
    );
    assert!(
        !t.lc.has(&format!("land {batch}")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(t2u2ai5)]
#[test]
fn land_with_the_batch_record_refused_prints_the_reason_and_exits_nonzero() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    t.lc.land_refused.set(Some("stale version 4"));
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    let e = t.err();
    assert!(
        e.contains(&batch) && e.contains("stale version 4") && e.contains("ALARM"),
        "{e}"
    );
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-tb"),
        "the release is live regardless"
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("mail-operator") && c.contains(&batch)),
        "the operator is mailed, not only the log"
    );
    assert!(
        marker(&t, &batch, "unrecorded").exists(),
        "a marker is left for the later pass to reconcile"
    );
}

fn seed_suite(t: &T, name: &str) {
    let dir = PathBuf::from(&kv_of(t, "round")["worktree"]).join("spira");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), "").unwrap();
}

#[test_id(t6n0dl5)]
#[test]
fn a_red_round_eject_naming_no_suite_is_refused_and_one_naming_a_suite_records_it() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red"]),
        2
    );
    assert!(t.err().contains("names no failing check"), "{}", t.err());
    assert_eq!(kv_of(&t, "round"), before);
    assert!(!t.lib.has("bead_reopen") && !t.lc.has("eject-member"));
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
}

#[test_id(tni4suf)]
#[test]
fn a_repo_whose_testenv_knows_only_gate_refuses_ci_and_forces_gate_for_the_whole_testenv_run() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(
        wt.join(".spira/testenv.toml"),
        "[tier.gate]\ncommand = \"true\"\n",
    )
    .unwrap();
    let eject = |suites: &str| {
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--no-rebuild",
            "--suites",
            suites,
        ])
    };
    assert_eq!(eject("ci"), 1);
    assert!(t.err().contains("known suites: gate"), "{}", t.err());
    assert_eq!(eject("testenv"), 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-b eject-red gate"), "{:?}", t.err());
}

#[test_id(twbsqdr)]
#[test]
fn a_ci_entry_red_forces_the_declared_default_tier_not_the_result_name() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(
        wt.join(".spira/testenv.toml"),
        "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\"]\n[tier.gate]\ncommand = \"true\"\n[tier.build]\ncommand = \"true\"\n",
    )
    .unwrap();
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "testenv", "--no-rebuild"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-b eject-red gate"), "{:?}", t.err());
    let calls = t.lib.calls.borrow().clone();
    let comment = calls.iter().find(|l| l.starts_with("comment sp-b")).cloned().unwrap_or_default();
    assert!(calls.iter().filter(|c| c.starts_with("bead_reopen")).all(|c| !c.contains("testenv")), "{calls:?}");
    assert!(comment.trim_end().ends_with("whatever the configured suites: gate"), "{comment}");
    assert!(!comment.contains("testenv"), "{comment}");
}

#[test_id(to5se42)]
#[test]
fn a_ci_entry_red_on_a_tree_with_no_own_testenv_is_refused_by_name() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "testenv", "--no-rebuild"]),
        1
    );
    assert!(t.err().contains("testenv does not know") && t.err().contains("testenv"), "{}", t.err());
    assert_eq!(kv_of(&t, "round"), before);
    assert!(!t.lib.has("bead_reopen") && !t.lc.has("eject-member"));
}

#[test_id(tsv4ymf)]
#[test]
fn an_eject_red_on_lint_is_accepted_and_forces_the_lint_step_on_recertification() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "lint red", "--red", "--suites", "lint", "--no-rebuild"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lib.has("bead_reopen"),
        "the ejected bead is returned: {:?}",
        t.err()
    );
}

#[test_id(t7u8pkj)]
#[test]
fn an_eject_for_the_config_delta_check_is_taken_as_a_round_step() {
    // The attribution ejects a member whose merge breaks the config delta with
    // `--suites config-delta`; refused, the round kept the member its worktree no longer held.
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round", "eject", &batch, "sp-b", "--reason", "the merged config delta does not apply", "--red", "--suites", "config-delta"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    assert!(crate::ops::round::is_round_step("config-delta"));
    assert!(spira_config::round_step::proven_by_round("config-delta"));
}

#[test_id(tmajot0)]
#[test]
fn an_eject_red_on_the_release_verify_step_is_accepted_as_a_round_step() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "the release build refused its task list", "--red", "--suites", "release-verify"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    assert!(spira_config::round_step::proven_by_round("release-verify"));
}

#[test_id(t9191gq)]
#[test]
fn every_red_name_the_queue_attributes_is_a_listed_round_step() {
    use crate::ops::round::{BUILD_RED, IMAGE_RED, LINT_RED, PREFLIGHT_RED, UNIT_RED};
    for n in [BUILD_RED, LINT_RED, UNIT_RED, IMAGE_RED, PREFLIGHT_RED] {
        assert!(spira_config::round_step::is_step(n), "{n}");
    }
}

#[test_id(t71ykqw)]
#[test]
fn an_eject_red_on_any_listed_round_step_is_accepted() {
    for name in spira_config::round_step::names() {
        let t = round_world();
        assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
        let batch = batch_of(&t);
        assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "a round step went red", "--red", "--suites", name]), 0, "{name}: {}", t.err());
        assert!(t.lc.has("eject-member"), "{name}: {:?}", t.lc.calls.borrow());
    }
}

#[test_id(tsggjaw)]
#[test]
fn an_eject_forcing_a_suite_testenv_does_not_know_is_refused_naming_the_known_ones_and_changes_nothing(
) {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&[
            "round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "round-vm"
        ]),
        1
    );
    let e = t.err();
    let steps = e.split("round steps: ").nth(1).unwrap_or("");
    let at: Vec<_> = ["workspace-build", "spira-lint", "unit-tests", "config-delta"].iter().map(|n| steps.find(n)).collect();
    assert!(at.iter().all(Option::is_some) && at.windows(2).all(|p| p[0] < p[1]), "{e}");
    assert!(
        e.contains("round-vm") && e.contains("does not know") && e.contains("test-b.sh"),
        "{e}"
    );
    assert_eq!(kv_of(&t, "round"), before);
    assert!(
        !t.lib.has("bead_reopen") && !t.lc.has("eject-member"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "positive control: {}",
        t.err()
    );
}

#[test_id(tauqy2q)]
#[test]
fn clearing_forced_suites_removes_the_proof_and_the_sidecar_and_is_gated_on_who_asks() {
    let t = round_world();
    let run = t.s().run.clone();
    spira_config::proof::record_failing(&run, "sp-a", &["round-vm".to_string()]).unwrap();
    fs::create_dir_all(run.join("ejected")).unwrap();
    fs::write(run.join("ejected/sp-a"), "round-vm").unwrap();
    assert_eq!(t.run(&["forced-suites", "sp-a"]), 0);
    assert!(t.out().contains("round-vm"), "{}", t.out());
    assert_eq!(
        t.run(&[
            "forced-suites",
            "sp-a",
            "--clear",
            "--by=dwarf",
            "--reason=x"
        ]),
        1
    );
    assert_eq!(
        spira_config::proof::failing(&run, "sp-a"),
        vec!["round-vm".to_string()],
        "a refused clear changes nothing"
    );
    assert_eq!(
        t.run(&[
            "forced-suites",
            "sp-a",
            "--clear",
            "--by=concierge",
            "--reason=round-vm is not a suite"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        spira_config::proof::failing(&run, "sp-a").is_empty() && !run.join("ejected/sp-a").exists()
    );
}

fn certified_round() -> (T, String) {
    let mut t = round_world();
    t.lib.s.home_repo = "spira".into();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join("target/release")).unwrap();
    std::os::unix::fs::symlink(
        std::env::current_exe().unwrap(),
        wt.join("target/release/release"),
    )
    .unwrap();
    release_in_force(&t);
    (t, batch)
}

#[test_id(tpu0yoh)]
#[test]
fn land_records_and_closes_mending_before_activating_a_release_that_retires_keys() {
    let (t, batch) = certified_round();
    t.lc.retired_keys.set(true);
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(
        t.lc.has(&format!("land {batch} 4 merged-tb")) && t.lc.has("mending-close-ended"),
        "{:?}",
        t.lc.calls.borrow()
    );
    let e = t.err();
    assert!(!e.contains("ALARM") && !e.contains("not declared"), "{e}");
}

#[test_id(tlaha9b)]
#[test]
fn land_with_the_record_refused_exits_nonzero_naming_the_batch() {
    let (t, batch) = certified_round();
    t.lc.land_refused.set(Some("store unreachable"));
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    let e = t.err();
    assert!(e.contains(&batch) && e.contains("ALARM"), "{e}");
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-tb"),
        "the release is live regardless"
    );
}

#[test_id(t1qux3j)]
#[test]
fn land_whose_record_another_landing_wrote_exits_zero() {
    let (t, batch) = certified_round();
    t.lc.landed_first.set(true);
    t.git.ancestor("merged-tb", "local/main");
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    let e = t.err();
    assert!(!e.contains("ALARM") && !e.contains("NO batch landing record"), "{e}");
    assert!(t.out().contains("was recorded LANDED by another landing"), "{}", t.out());
    assert!(
        !t.scripts.calls.borrow().iter().any(|c| c.contains("mail-operator")),
        "{:?}",
        t.scripts.calls.borrow()
    );
    assert!(!marker(&t, &batch, "unrecorded").exists());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test_id(tm8fmwn)]
#[test]
fn land_whose_landed_row_has_its_head_off_the_landing_ref_still_alarms() {
    let (t, batch) = certified_round();
    t.lc.landed_first.set(true);
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    assert!(t.err().contains("ALARM"), "{}", t.err());
    assert!(marker(&t, &batch, "unrecorded").exists());
}

#[test_id(tjt300y)]
#[test]
fn land_retries_a_record_that_got_no_reply() {
    let (t, batch) = certified_round();
    t.lc.land_no_reply_once.set(true);
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(!t.err().contains("ALARM"), "{}", t.err());
    assert!(!marker(&t, &batch, "unrecorded").exists());
}

#[test_id(tojpkwb)]
#[test]
fn eject_then_land_rebuilds_the_head_without_the_member() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        t.out().contains("RED") && t.out().contains("red=test-b.sh"),
        "{}",
        t.out()
    );
    assert_eq!(round_phase(&t), "ATTRIBUTING");
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(
        t.run(&["round", "land", &batch]),
        1,
        "a red round does not land"
    );
    assert!(t.err().contains("is attributing, not green"), "{}", t.err());

    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red on test-b.sh",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-b eject-red test-b.sh"));
    assert!(
        t.lc.has("event bead sp-b IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}")
            || t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\"")
    );
    assert!(t.lc.has(&format!(
        "eject-member {batch} sp-b ATTRIBUTING 5 red on test-b.sh"
    )));
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            rec["members"].as_str(),
            rec["head"].as_str(),
            round_phase(&t).as_str()
        ),
        ("sp-a:ta sp-c:tc", "merged-tc", "OPEN")
    );
    assert_eq!(rec["ejected"].trim(), "sp-b");
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("merge "))
            .count()
            >= 5,
        "the survivors are merged again onto the base"
    );

    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tc"));
    assert!(
        t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"")
            && t.lc.has("event bead sp-c CERTIFIED 3 \"Deliver\"")
    );
    assert!(t.lc.has(&format!("land {batch} 9 merged-tc")));
}

#[test_id(tsnuznb)]
#[test]
fn a_refused_lifecycle_eject_changes_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let before = kv_of(&t, "round");
    t.lc.eject_refused.set(true);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "late"]),
        1
    );
    assert!(t.err().contains("nothing changed"), "{}", t.err());
    assert_eq!(
        kv_of(&t, "round"),
        before,
        "the git head and member list stay as they were"
    );
    assert!(
        !t.lib.has("bead_reopen"),
        "the bead is not reopened for an eject the machine refused"
    );
}

#[test_id(tuaqro1)]
#[test]
fn an_eject_whose_lifecycle_write_is_locked_exits_nonzero_naming_the_lock() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.event_refused.set(Some("database is locked"));
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]),
        1
    );
    let e = t.err();
    assert!(
        e.contains("database is locked")
            && e.contains("sp-b")
            && e.contains("did not return to REWORK"),
        "{e}"
    );
}

#[test_id(tz7ojpo)]
#[test]
fn a_successful_eject_reports_the_round_and_the_remaining_count() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains(&format!(
            "ejected sp-b from round {batch}; 2 member(s) remain"
        )),
        "{}",
        t.out()
    );
}

#[test_id(tr9sz9r)]
#[test]
fn ejecting_the_last_member_closes_the_round() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "harness"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-a eject "));
    assert!(t
        .lc
        .has(&format!("abandon-batch {batch} OPEN 1 round emptied")));
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
}

#[test_id(tkzwgii)]
#[test]
fn a_harness_fault_eject_naming_suites_is_not_charged() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "round VM lacked the config delta",
            "--suites",
            "test-b.sh",
            "--harness-fault"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lib.has("bead_reopen sp-b eject test-b.sh") && !t.lib.has("bead_reopen sp-b eject-red")
    );
}

#[test_id(t4hhrmy)]
#[test]
fn abandon_returns_the_members_and_closes_the_round() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(batch_of(&t), "hand-1");
    drop(crate::lock::try_certify_lock(
        &t.s().queue_dir,
        "spira",
        "hand-1",
    ));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1"]),
        2,
        "a reason is required"
    );
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("abandon-batch hand-1 OPEN 1 base moved"));
    assert!(!t.qfile("round").exists() && !marker(&t, "hand-1", "running").exists());
    assert!(
        !certify_lock(&t, "hand-1").exists(),
        "an abandoned round leaves no certify lock"
    );
    assert_eq!(result_lines(&t, "hand-1")[0].1, "abandoned:hand");
    assert_eq!(result_lines(&t, "hand-1")[0].2, "base moved");
    assert!(!t.lc.has("land "), "an abandoned round lands nothing");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test_id(tdzc779)]
#[test]
fn abandoning_a_round_stops_its_running_legs_and_records_them_in_the_reason() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    {
        let mut legs = t.scripts.round_legs.borrow_mut();
        legs.push((
            "spira".into(),
            "hand-1".into(),
            "spira-run-batcher.local-leg-attribute2.service".into(),
        ));
        legs.push((
            "spira".into(),
            "hand-2".into(),
            "spira-run-batcher.local-other-leg.service".into(),
        ));
    }
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    let left: Vec<String> = t
        .scripts
        .round_legs
        .borrow()
        .iter()
        .map(|l| l.2.clone())
        .collect();
    assert_eq!(
        left,
        ["spira-run-batcher.local-other-leg.service"],
        "only the abandoned round's leg stops"
    );
    assert!(t.lc.has("abandon-batch hand-1 OPEN 1 base moved — stopped spira-run-batcher.local-leg-attribute2.service"), "{:?}", t.lc.calls.borrow());
}

#[test_id(t5uu9rj)]
#[test]
fn an_abandon_ends_the_build_still_in_the_worktree_before_it_removes_the_tree() {
    let t = round_world();
    t.git.materialise.set(true);
    open_hand_round(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    let building = wt.join("target/release");
    fs::create_dir_all(&building).unwrap();
    t.scripts.tree_holders.borrow_mut().push(building.clone());
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert_eq!(
        *t.scripts.stopped_holders.borrow(),
        [(building, true)],
        "the build was ended while its tree still stood"
    );
    assert!(!wt.exists(), "nothing is left at the worktree path");
}

#[test_id(t5myg7k)]
#[test]
fn an_abandon_that_cannot_end_the_build_keeps_the_tree() {
    let t = round_world();
    t.git.materialise.set(true);
    open_hand_round(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    t.scripts.tree_holders_stubborn.set(true);
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(wt.exists(), "a tree something still builds in is kept");
    assert!(t.err().contains("still running"), "{}", t.err());
}

fn open_hand_round(t: &T) {
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-1"]),
        0,
        "{}",
        t.err()
    );
}

#[test_id(t16jhbh)]
#[test]
fn an_abandoned_round_is_recorded_ended_for_the_process_driving_it() {
    let t = round_world();
    open_hand_round(&t);
    let run = t.s().run.clone();
    assert!(!spira_config::round_ended::is_ended(&run, "hand-1"));
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(spira_config::round_ended::is_ended(&run, "hand-1"));
    assert!(!spira_config::round_ended::is_ended(&run, "hand-2"));
}

#[test_id(tnqsvad)]
#[test]
fn a_refused_abandon_leaves_the_round_driven() {
    let t = round_world();
    open_hand_round(&t);
    t.scripts.round_legs_stubborn.set(true);
    assert_ne!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0);
    assert!(!spira_config::round_ended::is_ended(&t.s().run, "hand-1"));
}

#[test_id(t1wewl7)]
#[test]
fn an_abandon_names_the_driver_still_holding_the_rounds_lock() {
    let t = round_world();
    open_hand_round(&t);
    let _driver = crate::lock::try_certify_lock(&t.s().queue_dir, "spira", "hand-1");
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(t.err().contains("a driver of round hand-1 could not be stopped"), "{}", t.err());
    assert!(t.err().contains(&format!("pid={}", std::process::id())), "{}", t.err());
}

#[test_id(taa76qu)]
#[test]
fn a_leg_that_outlives_its_stop_keeps_the_round_open() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    t.scripts.round_legs_stubborn.set(true);
    assert_ne!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0
    );
    assert!(
        !t.lc.has("abandon-batch hand-1"),
        "a round whose leg still runs is not abandoned"
    );
    assert!(t.qfile("round").exists());
}

/// Opens a round of `members` and certifies it red on `test-b.sh`.
fn red_round(t: &T, members: &str) -> String {
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    assert_eq!(
        t.run(&["round", "open", "--members", members]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(t);
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(kv_of(t, "round")["red"], "test-b.sh");
    batch
}

#[test_id(t9nqgsp)]
#[test]
fn abandoning_a_red_round_of_two_members_with_no_eject_is_refused_naming_eject() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb");
    let before = kv_of(&t, "round");
    assert_eq!(t.run(&["round", "abandon", &batch, "--reason", "red"]), 1);
    assert!(
        t.err().contains(&format!("queue round eject {batch}"))
            && t.err().contains("QUEUE_ABANDON_CONSIDERED"),
        "{}",
        t.err()
    );
    assert!(
        !t.lc.has("abandon-batch"),
        "a refused abandon changes nothing"
    );
    assert_eq!(kv_of(&t, "round"), before);
}

#[test_id(taq0bad)]
#[test]
fn a_red_round_abandons_once_its_culprit_is_ejected() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb,sp-c:tc");
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "abandon", &batch, "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!("abandon-batch {batch}")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(tsltev1)]
#[test]
fn a_red_round_of_one_member_abandons_as_before() {
    let t = round_world();
    let batch = red_round(&t, "sp-b:tb");
    assert_eq!(
        t.run(&["round", "abandon", &batch, "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(result_lines(&t, &batch)[0].1, "abandoned:hand");
}

#[test_id(tdtp4cc)]
#[test]
fn an_abandon_of_a_red_round_under_the_override_records_why_on_the_rounds_events() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb");
    t.env.vars.borrow_mut().insert(
        "QUEUE_ABANDON_CONSIDERED".into(),
        "both members clash on one file".into(),
    );
    assert_eq!(
        t.run(&["round", "abandon", &batch, "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    let recorded = "QUEUE_ABANDON_CONSIDERED: both members clash on one file — red";
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with(&format!("abandon-batch {batch} ")) && c.ends_with(recorded)),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(result_lines(&t, &batch)[0].2, recorded);
}

#[test_id(t2thj0z)]
#[test]
fn ending_a_round_retires_the_mender_beads_filed_for_it() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-2"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-2", "mender-1-gate-x"), "sp-m9\n").unwrap();
    assert_eq!(
        t.run(&["round", "eject", "hand-2", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m9"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-2", "mender-1-gate-x.retired").exists());
    fs::write(marker(&t, "hand-2", "mender-2-gate-y"), "sp-m10\n").unwrap();
    assert_eq!(
        t.run(&["round", "abandon", "hand-2", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m10"), "{:?}", t.lib.calls.borrow());
}

#[test_id(t7ud1mh)]
#[test]
fn a_mender_bead_holding_an_ask_is_not_retired_until_the_ask_is_lifted() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-4"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-4", "mender-1-gate-x"), "sp-m13\n").unwrap();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .push(crate::model::LcBeadRow {
            bead_id: "sp-m13".into(),
            state: "READY".into(),
            holds: vec!["ask".into()],
            ..Default::default()
        });
    assert_eq!(
        t.run(&["round", "eject", "hand-4", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        !t.lib.has("bead_close sp-m13"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!marker(&t, "hand-4", "mender-1-gate-x.retired").exists());
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .for_each(|r| r.holds.clear());
    assert_eq!(
        t.run(&["round", "abandon", "hand-4", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m13"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-4", "mender-1-gate-x.retired").exists());
}

#[test_id(t0c4oym)]
#[test]
fn a_working_mender_that_also_holds_an_ask_is_kept_when_the_round_is_abandoned() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-6"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-6", "mender-1-gate-x"), "sp-m20\n").unwrap();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .push(crate::model::LcBeadRow {
            bead_id: "sp-m20".into(),
            state: "WORKING".into(),
            holds: vec!["ask".into()],
            ..Default::default()
        });
    assert_eq!(
        t.run(&["round", "abandon", "hand-6", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        !t.lib.has("bead_close sp-m20"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!marker(&t, "hand-6", "mender-1-gate-x.retired").exists());
}

#[test_id(tkwy2la)]
#[test]
fn a_working_mender_with_no_commit_is_closed_when_the_round_is_abandoned() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-7"]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-7", "mender-1-gate-x"), "sp-m21\n").unwrap();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .push(crate::model::LcBeadRow {
            bead_id: "sp-m21".into(),
            state: "WORKING".into(),
            ..Default::default()
        });
    assert_eq!(
        t.run(&["round", "abandon", "hand-7", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m21"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-7", "mender-1-gate-x.retired").exists());
}

#[test_id(t9f4ese)]
#[test]
fn a_working_mender_holding_a_tip_is_kept_when_the_round_is_abandoned() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-8"]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-8", "mender-1-gate-x"), "sp-m22\n").unwrap();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .push(crate::model::LcBeadRow {
            bead_id: "sp-m22".into(),
            state: "WORKING".into(),
            tip: Some("abc123".into()),
            ..Default::default()
        });
    assert_eq!(
        t.run(&["round", "abandon", "hand-8", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.lib.has("bead_close sp-m22"), "{:?}", t.lib.calls.borrow());
    assert!(!marker(&t, "hand-8", "mender-1-gate-x.retired").exists());
}

#[test_id(tsvxbew)]
#[test]
fn abandoning_a_round_keeps_a_mender_that_holds_a_submitted_fix_for_the_next_cut() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-5"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-5", "mender-1-gate-x"), "sp-m14\n").unwrap();
    fs::write(marker(&t, "hand-5", "mender-1-gate-y"), "sp-m15\n").unwrap();
    for (n, id) in [("a", "sp-m16"), ("b", "sp-m17"), ("c", "sp-m18")] {
        fs::write(
            marker(&t, "hand-5", &format!("mender-1-gate-{n}")),
            format!("{id}\n"),
        )
        .unwrap();
    }
    for (id, state, tip) in [
        ("sp-m14", "SUBMITTED", Some("9b97f8d")),
        ("sp-m15", "READY", None),
        ("sp-m16", "CERTIFIED", Some("aa")),
        ("sp-m17", "WORKING", None),
        ("sp-m18", "READY", Some("bb")),
    ] {
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push(crate::model::LcBeadRow {
                bead_id: id.into(),
                state: state.into(),
                tip: tip.map(String::from),
                ..Default::default()
            });
    }
    assert_eq!(
        t.run(&["round", "abandon", "hand-5", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    for kept in ["sp-m14", "sp-m16", "sp-m18"] {
        assert!(
            !t.lib.has(&format!("bead_close {kept}")),
            "{:?}",
            t.lib.calls.borrow()
        );
    }
    let rows = t.lc.rows.borrow();
    let state = |id: &str| {
        rows.as_ref()
            .unwrap()
            .iter()
            .find(|r| r.bead_id == id)
            .unwrap()
            .state
            .clone()
    };
    assert_eq!(state("sp-m14"), "SUBMITTED");
    assert_eq!(state("sp-m16"), "CERTIFIED");
    assert!(!marker(&t, "hand-5", "mender-1-gate-x.retired").exists());
    assert!(t.lib.has("bead_close sp-m17"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-5", "mender-1-gate-b.retired").exists());
    assert!(t.lib.has("bead_close sp-m15"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-5", "mender-1-gate-y.retired").exists());
}

#[test_id(t4y0wio)]
#[test]
fn a_later_round_event_keeps_a_submitted_mender_of_a_batch_already_closed() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .push(crate::model::LcBeadRow {
            bead_id: "sp-m19".into(),
            state: "SUBMITTED".into(),
            tip: Some("cc".into()),
            ..Default::default()
        });
    t.lc.batch_states
        .borrow_mut()
        .insert("old-2".into(), ("ABANDONED".into(), "2".into()));
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-6"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "old-2", "mender-1-gate-x"), "sp-m19\n").unwrap();
    assert_eq!(
        t.run(&["round", "abandon", "hand-6", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        !t.lib.has("bead_close sp-m19"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!marker(&t, "old-2", "mender-1-gate-x.retired").exists());
}

#[test_id(trcp07o)]
#[test]
fn a_later_round_event_retires_the_mender_beads_of_batches_already_closed() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-3"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "old-1", "mender-1-gate-x"), "sp-m11\n").unwrap();
    fs::write(marker(&t, "live-1", "mender-1-gate-y"), "sp-m12\n").unwrap();
    t.lc.batch_states
        .borrow_mut()
        .insert("old-1".into(), ("ABANDONED".into(), "2".into()));
    t.lib.close_fail.set(true);
    assert_eq!(
        t.run(&["round", "eject", "hand-3", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("sp-m11") && t.err().contains("refused"),
        "{}",
        t.err()
    );
    assert!(!marker(&t, "old-1", "mender-1-gate-x.retired").exists());
    t.lib.close_fail.set(false);
    assert_eq!(
        t.run(&["round", "abandon", "hand-3", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(marker(&t, "old-1", "mender-1-gate-x.retired").exists());
    assert!(!marker(&t, "live-1", "mender-1-gate-y.retired").exists());
}

#[test_id(tocm85g)]
#[test]
fn abandon_of_a_batch_the_lifecycle_already_abandoned_drops_the_record() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta", "--name", "hand-1"]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("hand-1".into(), ("ABANDONED".into(), "9".into()));
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    assert!(t.out().contains("round=none"), "{}", t.out());
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "retry"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round").exists() && !marker(&t, "hand-1", "running").exists());
    assert_eq!(result_lines(&t, "hand-1")[0].1, "abandoned:hand");
}

#[test_id(t914gjk)]
#[test]
fn abandon_carries_the_callers_cause_word_into_the_result_line() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta", "--name", "hand-2"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "abandon",
            "hand-2",
            "--reason",
            "it was blocked",
            "--cause",
            "Blocked Before Land"
        ]),
        0,
        "{}",
        t.err()
    );
    let lines = result_lines(&t, "hand-2");
    assert_eq!(
        (lines[0].1.as_str(), lines[0].2.as_str()),
        ("abandoned:blocked-before-land", "it was blocked")
    );
    assert_eq!(
        t.run(&["round", "land", "hand-2", "--cause", "x"]),
        2,
        "only abandon takes a cause"
    );
}

#[test_id(taxcg4c)]
#[test]
fn sweep_locks_removes_only_the_locks_of_batches_spira_lc_reports_terminal() {
    let t = round_world();
    for (b, state) in [
        ("r-landed", Some("LANDED")),
        ("r-settled", Some("SETTLED")),
        ("r-gone", Some("ABANDONED")),
        ("r-open", Some("CI_RUNNING")),
        ("r-staged", Some("DISCARDED")),
    ] {
        drop(crate::lock::try_certify_lock(&t.s().queue_dir, "spira", b));
        if let Some(s) = state {
            t.lc.batch_states
                .borrow_mut()
                .insert(b.into(), (s.into(), "3".into()));
        }
    }
    assert_eq!(
        t.run(&["round", "sweep-locks"]),
        2,
        "a repo or --all is required"
    );
    assert_eq!(
        t.run(&["round", "sweep-locks", "--repo", "spira", "--all"]),
        2
    );
    assert_eq!(
        t.run(&["round", "sweep-locks", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        crate::lock::certify_lock_batches(&t.s().queue_dir, "spira"),
        ["r-open"]
    );
    assert!(
        t.out()
            .contains("spira: removed 4 certify lock(s) of ended rounds, kept 1"),
        "{}",
        t.out()
    );
    drop(crate::lock::try_certify_lock(
        &t.s().queue_dir,
        "spira",
        "r-landed",
    ));
    assert_eq!(t.run(&["round", "sweep-locks", "--all"]), 0, "{}", t.err());
    assert_eq!(
        crate::lock::certify_lock_batches(&t.s().queue_dir, "spira"),
        ["r-open"]
    );
}

#[test_id(tqcrf6p)]
#[test]
fn abandon_requeues_a_member_the_batch_abandon_left_in_delivery() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has("event bead sp-a IN_DELIVERY 7 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "a member not in delivery is left alone"
    );
}

#[test_id(tjty8b1)]
#[test]
fn abandon_rereads_and_retries_a_requeue_refused_for_a_stale_version() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut() {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-b".into(), ("IN_DELIVERY".into(), "7".into()));
    *t.lc.event_races.borrow_mut() = Some(("IN_DELIVERY".into(), "8".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c == "event bead sp-a IN_DELIVERY 7 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "event bead sp-a IN_DELIVERY 8 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{calls:?}"
    );
}

#[test_id(tid6edn)]
#[test]
fn abandon_accepts_a_member_another_writer_moved_out_of_delivery() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut() {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    *t.lc.event_races.borrow_mut() = Some(("REWORK".into(), "8".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("event bead sp-a"))
            .count(),
        1
    );
    assert!(t.out().contains("sp-a: REWORK"), "{}", t.out());
}

#[test_id(tcaunro)]
#[test]
fn open_skips_what_is_not_certified_or_does_not_merge() {
    let t = round_world();
    t.lc_row("sp-d", "REWORK", "td", 100);
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tb".into(), vec!["list.txt".into()]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-d,sp-zz"]),
        1,
        "{}",
        t.err()
    );
    let o = t.err();
    assert!(
        o.contains("sp-d: lifecycle state=REWORK")
            && o.contains("sp-zz: no lifecycle row")
            && o.contains("2 of 3 named members"),
        "{o}"
    );
    assert!(!t.lc.has("cut") && !t.qfile("round").exists());

    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tb".into(), vec!["list.txt".into()]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 1);
    assert!(
        t.err().contains("sp-b: conflicts with the round")
            && !t.lc.has("cut")
            && !t.qfile("round").exists(),
        "{}",
        t.err()
    );

    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("ta".into());
    t.git.rebase_fail.borrow_mut().insert("ta".into(), vec![]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(
        t.err().contains("1 of 1 named members") && !t.lc.has("cut") && !t.qfile("round").exists()
    );
}

#[test_id(tis9lja)]
#[test]
fn open_rebases_a_member_that_conflicts_with_the_round_onto_its_head() {
    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tc".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("members=sp-a:ta sp-b:tb sp-c:rebased-tc") && !o.contains("skip"),
        "{o}"
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c == "rebase-onto tc merged-tb"),
        "{:?}",
        t.git.calls.borrow()
    );
    assert!(t.lc.has("cut"), "{:?}", t.lc.calls.borrow());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("cut ") && c.ends_with("sp-c:tc") && !c.contains("rebased")),
        "the cut names the certified tip, not the rebased sha: {:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.lc.has("event bead sp-c"),
        "a rebased member is not returned to rework"
    );
}

fn block(t: &T, id: &str, by: &[&str]) {
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == id)
    {
        r.blocked_by = by.iter().map(|b| b.to_string()).collect();
    }
}

#[test_id(tkma5fh)]
#[test]
fn open_refuses_a_member_given_at_a_tip_other_than_its_recorded_tip_and_creates_no_round() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:rebuilt"]),
        1,
        "{}",
        t.err()
    );
    let e = t.err();
    assert!(
        e.contains("sp-b: rebuilt is not its row's tip tb") && e.contains("1 of 2 named members"),
        "{e}"
    );
    assert!(!t.lc.has("cut") && !t.qfile("round").exists());
}

#[test_id(tu7bg4p)]
#[test]
fn open_refuses_every_member_it_cannot_admit_in_one_message() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:x1,sp-b:x2,sp-c:tc"]),
        1,
        "{}",
        t.err()
    );
    let e = t.err();
    assert!(
        e.contains("sp-a: x1 is not") && e.contains("sp-b: x2 is not"),
        "{e}"
    );
}

#[test_id(t0kikyz)]
#[test]
fn open_refuses_a_space_separated_members_value() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta sp-b:tb"]),
        2,
        "{}",
        t.err()
    );
    assert!(t.err().contains("whitespace"), "{}", t.err());
    assert!(!t.lc.has("cut") && !t.qfile("round").exists());
}

#[test_id(t59vkun)]
#[test]
fn open_refuses_a_member_token_that_runs_two_pairs_together() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta:sp-b:tb"]),
        2,
        "{}",
        t.err()
    );
    assert!(t.err().contains("holds a second colon"), "{}", t.err());
    assert!(!t.qfile("round").exists());
}

#[test_id(txd5qi9)]
#[test]
fn open_skips_a_member_whose_blocker_has_not_landed_and_names_it() {
    let t = round_world();
    block(&t, "sp-b", &["sp-eeg"]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    let o = t.err();
    assert!(
        o.contains("sp-b: depends on sp-eeg (") && !t.lc.has("cut") && !t.qfile("round").exists(),
        "{o}"
    );
}

#[test_id(t3us08o)]
#[test]
fn open_admits_a_member_with_its_blocker_in_the_same_round_blocker_first() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-b,sp-a"]), 0, "{}", t.err());
    let o = t.out();
    let members = o.lines().find_map(|l| l.strip_prefix("members=")).unwrap_or_else(|| panic!("{o}"));
    assert!(members.find("sp-a:").unwrap() < members.find("sp-b:").unwrap(), "blocker merges first: {members}");
}

#[test_id(txvhuj9)]
#[test]
fn open_admits_a_member_whose_blocker_has_landed() {
    let t = round_world();
    block(&t, "sp-b", &[]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
}

#[test_id(t7aihe8)]
#[test]
fn open_returns_a_base_conflict_to_rework_and_keeps_a_round_conflict_queued() {
    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.git.conflict_with_base.borrow_mut().insert("tb".into(), vec!["a.rs".into(), "b.rs".into()]);
    t.git.merge_fail.borrow_mut().insert("tc".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tc".into(), vec!["c.txt".into(), "d.txt".into()]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        1,
        "{}",
        t.err()
    );
    let o = t.err();
    assert!(!t.qfile("round").exists() && !t.lc.has("cut"), "{o}");
    assert!(o.contains("sp-b: conflicts with base — returned to rework — paths: a.rs b.rs\n") && o.contains("sp-c: conflicts with the round, and with a rebase onto its head — paths: c.txt d.txt\n"), "{o}");
    assert!(
        t.lc.has(
            "event bead sp-b CERTIFIED 3 {\"GateRed\":{\"tip\":\"tb\",\"reason\":\"no-rebase\"}}"
        ),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(!t.lc.has("event bead sp-c"), "{:?}", t.lc.calls.borrow());
    assert_eq!(t.lc.bead_state("sp-b").unwrap().0, "REWORK");
    assert!(
        t.lib.calls.borrow().iter().any(|c| c.starts_with("comment sp-b ") && c.contains("a.rs b.rs")),
        "{:?}",
        t.lib.calls.borrow()
    );
}

#[test_id(tk84dhi)]
#[test]
fn open_admits_a_stale_member_the_merge_takes_without_a_skip() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.err().contains("conflicts with base"), "{}", t.err());
    assert!(t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
}

#[test_id(thxf9tg)]
#[test]
fn open_fails_and_does_not_claim_a_return_the_lifecycle_refused_or_ignored() {
    for ignored in [false, true] {
        let t = round_world();
        t.git.merge_fail.borrow_mut().insert("tb".into());
        t.git.conflict_with_base.borrow_mut().insert("tb".into(), vec![]);
        if ignored {
            t.lc.event_ignored.set(true);
        } else {
            t.lc.event_refused.set(Some("refused: IllegalTransition"));
        }
        assert_eq!(
            t.run(&["round", "open", "--members", "sp-a,sp-b"]),
            1,
            "ignored={ignored}: {}",
            t.err()
        );
        assert!(
            !t.err().contains("— returned to rework")
                && t.err().contains("sp-b: conflicts with base ("),
            "{}",
            t.err()
        );
        assert!(
            !t.lc.has("cut") && !t.qfile("round").exists(),
            "{}",
            t.err()
        );
    }
}

#[test_id(t35s4u8)]
#[test]
fn open_whose_record_cannot_be_written_abandons_the_batch_it_cut() {
    let t = round_world();
    fs::create_dir_all(t.qfile("round")).unwrap();
    fs::create_dir_all(t.qfile("round.r-x")).unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b", "--name", "r-x"]),
        1,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("cut ")),
        "POSITIVE CONTROL: the cut ran first: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("abandon-batch ") && c.contains("OPEN")),
        "the cut batch is released: {calls:?}"
    );
}

#[test_id(twx6v9q)]
#[test]
fn open_whose_cut_answers_a_deadline_after_committing_is_a_round_not_a_failure() {
    let t = round_world();
    t.lc.deadline_after_write.set(Some("cut"));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("batch="), "{}", t.out());
    assert!(!t.lc.has("requeue-orphans"));
}

#[test_id(t1tfoyl)]
#[test]
fn open_whose_cut_is_refused_sweeps_orphans() {
    let t = round_world();
    t.lc.cut_refused.set(Some("refused"));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    assert!(t.lc.has("requeue-orphans"), "{:?}", t.lc.calls.borrow());
}

#[test_id(t3us8yo)]
#[test]
fn the_orphan_sweep_is_told_which_rounds_have_a_live_leg() {
    let t = round_world();
    t.scripts.round_legs.borrow_mut().push((
        "spira".into(),
        "hand-1".into(),
        "spira-run-batcher.local-leg-attribute2.service".into(),
    ));
    t.lc.cut_refused.set(Some("refused"));
    t.run(&["round", "open", "--members", "sp-a,sp-b"]);
    assert!(
        t.lc.has("requeue-orphans saw hand-1 running"),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(tqgkzvm)]
#[test]
fn open_admits_a_submitted_member_its_full_suite_certifies() {
    // law-a-round-is-feature-first-then-catch-all: a round takes SUBMITTED beads directly.
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-s"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("sp-s:ts") && !o.contains("sp-s: lifecycle state"),
        "{o}"
    );
    assert!(t.lc.has("cut"), "the round is recorded as a batch");
}

#[test_id(t6mb365)]
#[test]
fn open_admits_a_certified_and_an_unheld_submitted_member_recording_the_second_provisional() {
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-s"]), 0, "{}", t.err());
    let rec = kv_of(&t, "round");
    assert_eq!(rec["provisional"], "sp-s", "{rec:?}");
    let _ = t.out();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    assert!(t.out().lines().any(|l| l == "provisional=sp-s"), "{}", t.out());
}

#[test_id(trp10i4)]
#[test]
fn open_cuts_a_round_for_a_lone_submitted_member() {
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(t.run(&["round", "open", "--members", "sp-s"]), 0, "{}", t.err());
    assert!(t.lc.has("cut"), "{:?}", t.lc.calls.borrow());
    assert_eq!(kv_of(&t, "round")["provisional"], "sp-s");
}

#[test_id(tw0jsx3)]
#[test]
fn stage_admits_a_submitted_member_recording_it_provisional() {
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-ts");
    t.git.trees.borrow_mut().insert("merged-ts".into(), T_ROUND.into());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-s:ts"]), 0, "{}", t.err());
    assert_eq!(kv_of(&t, "round-staged")["provisional"], "sp-s");
}

#[test_id(tvnwam0)]
#[test]
fn open_and_stage_refuse_a_held_submitted_member_naming_the_hold_but_let_a_person_past_a_manual_one() {
    for kind in ["manual", "wait", "ask", "poison"] {
        let t = round_world();
        t.lc_row("sp-s", "SUBMITTED", "ts", 100);
        set_row(&t, "sp-s", |r| r.holds = vec![kind.to_string()]);
        assert_eq!(t.run(&["round", "open", "--members", "sp-s:ts", "--by", "batcher"]), 1, "{kind}");
        assert!(t.err().contains(&format!("sp-s: SUBMITTED and held {kind}")), "{kind}: {}", t.err());
        assert!(!t.lc.has("cut"));
        assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
        assert_eq!(t.run(&["round", "stage", "--members", "sp-s:ts", "--by", "batcher"]), 1, "{kind}");
        assert!(t.out().contains(&format!("sp-s: SUBMITTED and held {kind}")), "{kind}: {}", t.out());
    }
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    set_row(&t, "sp-s", |r| r.holds = vec!["manual".to_string()]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-s:ts", "--by", "concierge"]), 0, "{}", t.err());
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    set_row(&t, "sp-s", |r| r.holds = vec!["wait".to_string()]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-s:ts", "--by", "concierge"]), 1, "a wait hold binds a person too");
}

#[test_id(t8ro6l5)]
#[test]
fn stage_refuses_a_submitted_member_at_a_stale_tip() {
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-s:other"]), 1);
    assert!(t.out().contains("sp-s: other is not its row's tip ts"), "{}", t.out());
}

#[test_id(txptlyt)]
#[test]
fn open_still_refuses_a_submitted_member_at_its_ejected_red_tip() {
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    set_row(&t, "sp-s", |r| r.ejected_red_tip = Some("ts".into()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-s:ts"]), 1);
    assert!(t.err().contains("sp-s: ts is a recorded ejected-red tip"), "{}", t.err());
    assert!(!t.lc.has("cut"));
}

#[test_id(tza5uk5)]
#[test]
fn stage_merges_a_blocker_named_after_its_dependant_ahead_of_it() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-c:tc"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb,sp-a:ta"]), 0, "{}\n{}", t.out(), t.err());
    let members = kv_of(&t, "round-staged")["members"].clone();
    assert!(members.find("sp-a:").unwrap() < members.find("sp-b:").unwrap(), "blocker first: {members}");
}

#[test_id(tjgpjlp)]
#[test]
fn round_eject_says_the_row_decides_when_the_reopen_did_not_complete() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    t.lib.reopen_fails.borrow_mut().insert("sp-a".into());
    assert_eq!(t.run(&["round", "eject", &batch, "sp-a", "--reason", "red"]), 0, "{}", t.err());
    assert!(t.err().contains("bead_reopen did not complete for sp-a"), "{}", t.err());
}

#[test_id(t0cw7v8)]
#[test]
fn eject_of_a_blocker_takes_its_same_round_dependant_with_it_naming_the_blocker() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-a", "--reason", "red on lint"]), 0, "{}", t.err());
    assert!(t.out().contains("ejected sp-b from round") && t.out().contains("blocked on sp-a"), "{}", t.out());
    assert!(
        t.lc.calls.borrow().iter().any(|c| c.starts_with(&format!("eject-member {batch} sp-b ")) && c.ends_with("blocked on sp-a")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(kv_of(&t, "round")["members"], "sp-c:tc");
}

pub(super) fn file_gate_verdict(t: &T, id: &str, tip: &str, rc: i32) {
    use landing_pass::gateq::{Done, GateQueue, Job};
    let job = Job::new("spira", &format!("spira/{id}"), id, tip, false);
    let run = landing_pass::model::GateRun::parse(
        rc,
        "gate: VERDICT=FAIL reason=branch-red suite=test-x.sh".into(),
    );
    GateQueue::new(&t.s().run)
        .complete(
            0,
            &Done {
                job,
                run,
                started_ms: 1,
                finished_ms: 2,
                gate: String::new(),
            },
        )
        .unwrap();
}

#[test_id(typ2e9g)]
#[test]
fn open_skips_a_member_at_a_tip_that_already_went_red_in_a_batch() {
    let t = round_world();
    t.lc_row("sp-r", "CERTIFIED", "tsp-r", 100);
    std::fs::create_dir_all(t.qfile("red-tips").parent().unwrap()).unwrap();
    std::fs::write(t.qfile("red-tips"), "sp-r tsp-r\n").unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-r"]),
        1,
        "{}",
        t.err()
    );
    let o = t.err();
    assert!(o.contains("sp-r: tsp-r already went red in a batch"), "{o}");
    assert!(!t.qfile("round").exists());
}

#[test_id(tag4cqb)]
#[test]
fn a_second_round_is_refused_until_the_first_is_closed() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "open", "--members", "sp-b"]), 1);
    assert!(
        t.err().contains("a round is already open") && t.err().contains("phase opened"),
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "land", "someone-else"]), 1);
    assert!(
        t.err()
            .contains("is spira-20260929T010203Z, not someone-else"),
        "{}",
        t.err()
    );
}

#[test_id(t6km45i)]
#[test]
fn certify_never_reads_a_harness_fault_as_green_or_red() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(
        t.run(&["round", "certify", &batch]),
        4,
        "no results at all is a fault"
    );
    assert!(
        t.err().contains("no verdicts") && round_phase(&t) == "ATTRIBUTING",
        "{}",
        t.err()
    );

    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "killed".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("hung past its guard (guard=hang-guard elapsed="),
        "{}",
        t.err()
    );
    assert!(
        !certified_tree_exists(&t)
            && !t
                .lc
                .has("event batch spira-20260929T010203Z CI_RUNNING 4 {\"PassGreen\"")
    );
}

#[test_id(tjih5of)]
#[test]
fn a_round_vm_exit_names_the_failure_the_vm_logged() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "  ok      tmux\n  FAIL    terraform — not found on PATH and not waived\n23 ok, 1 FAIL\nround-vm: IMAGE-BUILD-FAILED: img could not be built on this VM\n";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 3,
        out: String::new(),
        err: err.into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("round-vm exited 3: round-vm: IMAGE-BUILD-FAILED: img could not be built"),
        "{}",
        t.err()
    );
}

#[test_id(tfsx9dq)]
#[test]
fn a_fixture_fault_is_the_round_cause_and_suite_fail_lines_are_not() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 3,
        out: String::new(),
        err: "  FAIL    loom does not answer at http://127.0.0.1:7300/stuck\n".into(),
    };
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "fixture-fault.out".into(),
        "FIXTURE-FAULT: install rc=7: install failed — harness fault\ninstall rc=7 output tail:\nunits-install: no such unit\n".into(),
    )];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let err = t.err();
    assert!(
        err.contains("round-vm exited 3: install rc=7: install failed — harness fault [units-install: no such unit]"),
        "{err}"
    );
    assert!(!err.contains("round-vm exited 3: FAIL"), "{err}");
}

#[test_id(tmkro4x)]
#[test]
fn a_pass_whose_tests_overran_the_cap_is_a_fault_even_with_every_verdict_in() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "round-vm: UNIT-TESTS: PASS (3 test binaries)";
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "round-fault-kind".to_string(),
        "over-cap guard=suites-cap elapsed=901s cap=900s — the suites ran past the cap\n".to_string(),
    )];
    let over = RunOut {
        rc: 124,
        out: String::new(),
        err: err.into(),
    };
    *t.scripts.round_vm_queue.borrow_mut() = vec![over.clone(), over];
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("INCOMPLETE, cut off by guard=suites-cap elapsed=901s"),
        "{}",
        t.err()
    );
    assert!(!t
        .lc
        .has("event batch spira-20260929T010203Z CI_RUNNING 4 {\"PassGreen\""));
}

const OVER_CAP_EVENTS: &str = "1 phase suites\n2 verdict FAULT over-cap\n";
const KILLED_UNIT: &str = "UNIT-TESTS-CAUSE: exit status 101 (signal: 9, SIGKILL: kill)\n";

fn over_cap_world(t: &T, once: bool) -> String {
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let capped = vec![
        ("run.events".to_string(), OVER_CAP_EVENTS.to_string()),
        ("unit-tests.out".to_string(), KILLED_UNIT.to_string()),
        (
            "test-sim-claim-rank.sh.result".to_string(),
            "unreached 1 0 -\n".to_string(),
        ),
        (
            "test-sim-dwarf-brief.sh.result".to_string(),
            "unreached 1 0 -\n".to_string(),
        ),
    ];
    *(if once {
        t.scripts.round_vm_files_once.borrow_mut()
    } else {
        t.scripts.round_vm_files.borrow_mut()
    }) = capped;
    *t.scripts.round_vm_meta.borrow_mut() = "suite_cap_s=600\n".into();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    batch_of(t)
}

fn no_attribution(t: &T) {
    assert!(!t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    assert!(
        !t.lc.calls.borrow().iter().any(|c| c.contains("PassRed")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("mender") || c.contains("path-attribute")),
        "{:?}",
        t.scripts.calls.borrow()
    );
}

#[test_id(tzrypu9)]
#[test]
fn an_over_cap_pass_with_a_killed_unit_test_ejects_nobody_and_recertifies_once() {
    for first_rc in [2, 1] {
        let t = round_world();
        let batch = over_cap_world(&t, true);
        *t.scripts.round_vm_queue.borrow_mut() = vec![
            RunOut {
                rc: first_rc,
                out: String::new(),
                err: "libtest (signal: 9, SIGKILL: kill)\n".into(),
            },
            RunOut {
                rc: 0,
                out: String::new(),
                err: String::new(),
            },
        ];
        let rc = t.run(&["round", "certify", &batch]);
        assert_eq!(rc, 0, "rc {first_rc}: {}", t.err());
        let vm = t
            .scripts
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("round-vm"))
            .count();
        assert_eq!(vm, 2);
        let calls = t.lc.calls.borrow().join("\n");
        let inc = calls.find("PassIncomplete").expect("incomplete");
        assert!(
            calls[inc..].contains("over-cap: FAULT over-cap unreached=2"),
            "{calls}"
        );
        assert!(
            inc < calls.rfind("PassStarted").unwrap()
                && calls.rfind("PassStarted").unwrap() < calls.find("PassGreen").unwrap(),
            "{calls}"
        );
        assert!(t.err().contains("FAULT over-cap round="), "{}", t.err());
        assert!(t.err().contains("unreached="), "{}", t.err());
        no_attribution(&t);
    }
}

#[test_id(ta6mbil)]
#[test]
fn a_second_over_cap_on_the_same_head_is_a_capacity_fault_and_ejects_nobody() {
    let t = round_world();
    let batch = over_cap_world(&t, false);
    let over = RunOut {
        rc: 2,
        out: String::new(),
        err: "libtest (signal: 9, SIGKILL: kill)\n".into(),
    };
    *t.scripts.round_vm_queue.borrow_mut() = vec![over.clone(), over.clone()];
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    let vms = |t: &T| {
        t.scripts
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("round-vm"))
            .count()
    };
    assert_eq!(vms(&t), 2);
    assert!(
        t.err().contains(&format!(
            "CAPACITY FAULT over-cap twice round={batch} head="
        )),
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("fault=capacity unreached=2,2"),
        "{}",
        t.out()
    );
    let paged = |t: &T| {
        t.lib
            .calls
            .borrow()
            .iter()
            .filter(|c| {
                c.starts_with("notify") && c.contains(&format!("Round {batch} over-cap twice"))
            })
            .count()
    };
    assert_eq!(paged(&t), 1);
    assert!(!t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("mail-operator")));
    no_attribution(&t);

    *t.scripts.round_vm_queue.borrow_mut() = vec![over];
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert_eq!(vms(&t), 3, "one more pass, no re-run");
    assert!(
        t.err().contains("CAPACITY FAULT over-cap twice"),
        "{}",
        t.err()
    );
    assert_eq!(paged(&t), 1, "paged once per (batch, head)");
    no_attribution(&t);
}

#[test_id(tnnwjs7)]
#[test]
fn a_pass_the_unit_tier_ended_before_any_suite_is_judged_red_on_unit_tests_not_faulted() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: "round-vm: UNIT-TESTS RED before the suites: no suite was run\n".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![];
    *t.scripts.round_vm_files.borrow_mut() = vec![
        (
            "unit.json".to_string(),
            "{\"stage\":\"unit-tests\",\"verdict\":\"red\",\"ran_before_suites\":true,\"cause\":\"exit status 101\",\"secs\":42,\"last_binary\":\"alpha\",\"failed\":[\"a::broken\",\"b::also\"]}\n".to_string(),
        ),
        ("unit-tests.out".to_string(), "UNIT-TESTS-CAUSE: exit status 101 after 42s\n".to_string()),
    ];
    assert_eq!(
        t.run(&["round", "certify", &batch]),
        1,
        "{}{}",
        t.out(),
        t.err()
    );
    assert!(
        t.out().contains("unit-tests-failed=a::broken,b::also"),
        "{}",
        t.out()
    );
    assert!(
        t.out().contains(
            "unit-tests-cause=unit-tests: exit status 101 (before the suites, after 42s)"
        ),
        "{}",
        t.out()
    );
    assert!(
        !t.err().contains("left no verdicts") && !t.err().contains("not judged"),
        "{}",
        t.err()
    );
    let events = events_of(&t, &batch);
    assert!(events.iter().any(|e| e.starts_with("{\"PassRed\"") && e.contains("\"red_suites\":[\"unit-tests\"]")), "{events:?}");
    assert!(
        !events.iter().any(|e| e.contains("PassIncomplete")),
        "{events:?}"
    );
}

#[test_id(t58rqv9)]
#[test]
fn certify_faults_on_an_unreadable_or_missing_suite_verdict() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    let ok = |s: &str| (s.to_string(), "ok".to_string());
    for (results, want) in [
        (
            vec![ok("test-a.sh"), ("test-b.sh".into(), "".into())],
            "test-b.sh (verdict \"\")",
        ),
        (
            vec![ok("test-a.sh"), ("test-b.sh".into(), "garbage".into())],
            "test-b.sh (verdict \"garbage\")",
        ),
        (vec![ok("test-a.sh")], "test-b.sh (no result)"),
    ] {
        *t.scripts.round_vm_results.borrow_mut() = results;
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
        assert!(
            t.err().contains(want) && t.err().contains("not judged"),
            "{}",
            t.err()
        );
        assert!(!certified_tree_exists(&t) && round_phase(&t) == "ATTRIBUTING");
        forget_pass_ledger(&t);
    }
    *t.scripts.round_vm_results.borrow_mut() =
        vec![ok("test-a.sh"), ("test-b.sh".into(), "skip".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(certified_tree_exists(&t));
}

#[test_id(t7x5wb8)]
#[test]
fn certify_records_a_lint_or_fence_red_that_ended_the_pass_before_any_suite_ran() {
    for red in ["spira-lint", "fence-boundary"] {
        a_pass_ended_early_by(red);
    }
}

#[test_id(tll1qwv)]
#[test]
fn certify_names_the_phase_and_first_failing_line_of_a_lint_red() {
    let t = round_world();
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "spira-lint.out".into(),
        "\nmember=sp-b r: g.rs:2: y\nlater line\n".into(),
    )];
    a_pass_ended_early_by_in(&t, "spira-lint");
    assert!(
        t.out()
            .lines()
            .any(|l| l == "red-reason=spira-lint: member=sp-b r: g.rs:2: y"),
        "{}",
        t.out()
    );
}

fn a_pass_ended_early_by(red: &str) {
    a_pass_ended_early_by_in(&round_world(), red);
}

fn a_pass_ended_early_by_in(t: &T, red: &str) {
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(t);
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![(red.into(), "red".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains(&format!("red={red}")), "{}", t.out());
    assert!(!t.err().contains("not judged"), "{}", t.err());
    assert_eq!(round_phase(t), "ATTRIBUTING");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "lint", "--red", "--suites", red]),
        0,
        "{}",
        t.err()
    );
}

fn lint_red_world() -> (T, String, std::path::PathBuf) {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("spira-lint".into(), "red".into())];
    *t.scripts.round_vm_files.borrow_mut() =
        vec![("spira-lint.out".into(), "member=sp-b r: g.rs:2: y\n".into())];
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    (t, batch, results)
}

#[test_id(thxhhmn)]
#[test]
fn certify_writes_the_deny_list_before_round_vm_takes_a_vm_and_hands_it_over() {
    let (t, batch, results) = lint_red_world();
    *t.scripts.deny_list.borrow_mut() = Some(Ok("# test-registry deny-list at=7 rows=1\nrow\n".into()));
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(
        fs::read_to_string(results.join("deny-list")).unwrap(),
        "# test-registry deny-list at=7 rows=1\nrow\n"
    );
    let calls = t.scripts.calls.borrow();
    let deny = calls.iter().position(|c| c.starts_with("deny-list --at ")).expect("deny-list call");
    let vm = calls.iter().position(|c| c.starts_with("round-vm ")).expect("round-vm call");
    assert!(deny < vm, "{calls:?}");
    assert!(
        calls[vm].ends_with(&format!(" deny-list={}", results.join("deny-list").display())),
        "{calls:?}"
    );
}

#[test_id(t6pnikq)]
#[test]
fn certify_with_place_host_pins_its_round_vm_to_the_host_and_without_it_does_not() {
    let (t, batch, _) = lint_red_world();
    assert_eq!(t.run(&["round", "certify", &batch, "--place", "host"]), 1, "{}", t.err());
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let calls = t.scripts.calls.borrow();
    let vm: Vec<&String> = calls.iter().filter(|c| c.starts_with("round-vm ")).collect();
    assert!(vm[0].contains(" place=host") && !vm[1].contains(" place=host"), "{vm:?}");
}

#[test_id(t1k4db3)]
#[test]
fn certify_refuses_a_place_other_than_host() {
    let (t, batch, _) = lint_red_world();
    assert_eq!(t.run(&["round", "certify", &batch, "--place", "ec2"]), 2);
    assert!(t.err().contains("--place takes `host`"), "{}", t.err());
}

#[test_id(t39tz14)]
#[test]
fn a_second_certify_pass_keeps_the_round_deny_list_with_its_flip_rows() {
    let (t, batch, results) = lint_red_world();
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let kept = "# test-registry deny-list at=7 rows=1\nrow\nflip-row\n";
    fs::write(results.join("deny-list"), kept).unwrap();
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(fs::read_to_string(results.join("deny-list")).unwrap(), kept);
    let calls = t.scripts.calls.borrow();
    assert_eq!(calls.iter().filter(|c| c.starts_with("deny-list --at ")).count(), 1, "{calls:?}");
    assert!(
        calls.iter().filter(|c| c.starts_with("round-vm ")).all(|c| c.ends_with(&format!(" deny-list={}", results.join("deny-list").display()))),
        "{calls:?}"
    );
}

#[test_id(tv2mcsd)]
#[test]
fn certify_takes_no_vm_when_the_deny_list_cannot_be_resolved() {
    let (t, batch, results) = lint_red_world();
    *t.scripts.deny_list.borrow_mut() = Some(Err("test-registry deny-list --at 7 exited 3: registry busy".into()));
    assert_ne!(t.run(&["round", "certify", &batch]), 0);
    assert!(t.err().contains("the deny list could not be resolved: test-registry deny-list --at 7 exited 3: registry busy"), "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("round-vm ")));
    assert!(!results.join("deny-list").exists());
    assert!(events_of(&t, &batch).iter().any(|e| e.contains("PassIncomplete")), "{:?}", events_of(&t, &batch));
}

fn judged_red_world() -> (T, String) {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&[
            "round",
            "pass-verdict",
            &batch,
            "--verdict",
            "red",
            "--red-suites",
            "spira-lint"
        ]),
        0,
        "{}",
        t.err()
    );
    t.lc.late_boundaries.set(true);
    (t, batch)
}

#[test_id(tuf9o1g)]
#[test]
fn a_suites_boundary_after_the_pass_is_judged_is_ignored_and_exits_ok() {
    let (t, batch) = judged_red_world();
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out()
            .contains("queue.sh round suites-started: ignored: pass 1 already has its verdict"),
        "{}",
        t.out()
    );
    assert!(
        !t.out().contains("suites-started: recorded for"),
        "{}",
        t.out()
    );
    assert_eq!(round_phase(&t), "ATTRIBUTING");
}

#[test_id(t0o7rdl)]
#[test]
fn an_incomplete_verdict_after_the_pass_is_judged_is_ignored_and_exits_ok() {
    let (t, batch) = judged_red_world();
    assert_eq!(
        t.run(&["round", "pass-verdict", &batch, "--verdict", "incomplete", "--reason", "no outcome"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("ignored: pass 1 already has its verdict"),
        "{}",
        t.out()
    );
    assert_eq!(round_phase(&t), "ATTRIBUTING");
}

#[test_id(tkfff57)]
#[test]
fn an_image_red_ejects_the_member_that_moved_the_tag_and_nobody_when_none_did() {
    let (t, _) = image_red_round(&["tb"]);
    assert!(t.lib.has("bead_reopen sp-b eject-red"), "{:?}", t.lib.calls.borrow());
    assert!(!t.lib.has("bead_reopen sp-a"), "{:?}", t.lib.calls.borrow());
    let calls = t.lib.calls.borrow().clone();
    assert!(calls.iter().any(|c| c.trim_end() == "bead_reopen sp-b eject-red"), "{calls:?}");
    assert!(
        calls.iter().filter(|c| c.starts_with("comment sp-b")).all(|c| !c.contains("will force")),
        "{calls:?}"
    );
    let (t, _) = image_red_round(&[]);
    assert!(!t.lib.has("bead_reopen"), "{:?}", t.lib.calls.borrow());
}

#[test_id(t99dlt2)]
#[test]
fn a_failed_suite_after_the_pass_is_judged_is_ignored_files_no_mender_and_exits_ok() {
    let (t, batch) = judged_red_world();
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "spira-lint"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out()
            .contains("queue.sh round failed: ignored: pass 1 already has its verdict"),
        "{}",
        t.out()
    );
    assert!(
        !t.lib
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("create_bug")),
        "no MENDING row was opened, so no mender bead: {:?}",
        t.lib.calls.borrow()
    );
}

#[test_id(t33z7a7)]
#[test]
fn a_boundary_the_machine_refuses_for_another_reason_still_fails() {
    let (t, batch) = judged_red_world();
    t.lc.batch_event_refused.set(Some("SuitesStarted"));
    assert_ne!(t.run(&["round", "suites-started", &batch]), 0);
    assert!(t.err().contains("IllegalTransition"), "{}", t.err());
}

#[test_id(tp683fx)]
#[test]
fn pass_start_from_attributing_starts_the_next_pass_without_a_rebuild() {
    let (t, batch) = judged_red_world();
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let events = events_of(&t, &batch);
    assert_eq!(
        events.last().map(String::as_str),
        Some("{\"PassStarted\":{\"n\":2,\"head\":\"merged-ta\"}}"),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("{\"PassRebuilt\"")),
        "{events:?}"
    );
}

#[test_id(t57ff2n)]
#[test]
fn certify_ends_red_on_a_lint_red_the_round_vm_recorded_and_re_drives_pass_two() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("spira-lint".into(), "red".into())];
    *t.scripts.round_vm_records.borrow_mut() = Some((
        t.lc.batch_view.clone(),
        ("ATTRIBUTING".into(), 9, 1, String::new()),
    ));
    t.lc.late_boundaries.set(true);
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(
        t.out().contains("red=spira-lint"),
        "{}\n{}",
        t.out(),
        t.err()
    );
    assert!(
        !t.err().contains("changed while it was being certified"),
        "{}",
        t.err()
    );
    assert_eq!(round_phase(&t), "ATTRIBUTING");

    *t.scripts.round_vm_records.borrow_mut() = None;
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let events = events_of(&t, &batch);
    assert!(
        events
            .iter()
            .any(|e| e == "{\"PassStarted\":{\"n\":2,\"head\":\"merged-ta\"}}"),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("{\"PassRebuilt\"")),
        "{events:?}"
    );
    assert!(
        events
            .last()
            .unwrap()
            .starts_with("{\"PassGreen\":{\"n\":2"),
        "{events:?}"
    );
}

#[test_id(tnl6ii1)]
#[test]
fn the_blocking_only_reader_passed_what_unjudgeable_now_refuses() {
    let found = vec![
        ("test-a.sh".to_string(), String::new()),
        ("test-b.sh".to_string(), "garbage".to_string()),
    ];
    let old_reds: Vec<_> = found
        .iter()
        .filter(|(_, s)| super::super::super::ops::round::BLOCKING.contains(&s.as_str()))
        .collect();
    assert!(
        old_reds.is_empty(),
        "control: the old reader found nothing wrong"
    );
    assert!(super::super::super::ops::round::unjudgeable(&testkit::TempDir::new("round-unjudgeable").join("nonexistent"), &found).is_some());
}

#[test_id(trwjhwt)]
#[test]
fn an_attestation_of_a_corpus_that_ran_nothing_is_refused() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    for extra in [&[][..], &["--ran", "0"][..], &["--ran", "x"][..]] {
        let mut args = vec!["round", "certify", &batch, "--attest", "merged-ta"];
        args.extend_from_slice(extra);
        assert_eq!(t.run(&args), 1, "{}", t.err());
        assert!(
            t.err().contains("a vacuous pass is not a certificate"),
            "{}",
            t.err()
        );
    }
    assert!(!certified_tree_exists(&t));
}

#[test_id(tec8fl4)]
#[test]
fn a_green_round_names_how_many_suites_it_ran() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "7"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("ran=7 wall="), "{}", t.out());
}

#[test_id(telx2bv)]
#[test]
fn attest_names_the_worktrees_own_head_and_nothing_else() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.git.set("refs/heads/elsewhere", "merged-ta");
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "elsewhere",
            "--ran",
            "3"
        ]),
        1
    );
    assert!(
        t.err().contains("is not the round worktree's head"),
        "{}",
        t.err()
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .all(|c| !c.starts_with("round-vm")),
        "an attestation runs no corpus"
    );

    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-tb",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .all(|c| !c.starts_with("round-vm")));
}

#[test_id(tyxwwfn)]
#[test]
fn round_verbs_belong_to_queue_local() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(t.err().contains("not in queue.local mode"), "{}", t.err());
    let t = round_world();
    assert_eq!(t.run(&["round", "status"]), 0);
    assert!(t.out().contains("round=none"), "{}", t.out());
}

#[test_id(tt0m14m)]
#[test]
fn a_caller_holding_the_lock_runs_the_verbs_under_it() {
    let t = round_world();
    let _held = t.hold_lock();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        crate::ops::LOCK_BUSY,
        "a busy lock has its own exit, so a caller retries on the code, never on the words"
    );
    assert!(
        t.err().contains("another queue operation holds the lock"),
        "{}",
        t.err()
    );
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
}

/// One verb's run and the queue-lock holds it took, then those a hand driver took while it ran.
struct VerbHolds {
    verb: &'static str,
    rc: i32,
    holds: Vec<crate::lock::Hold>,
}

/// Drives one round through open, certify (red), eject, certify (green) and land, each verb run by
/// itself as round-recert runs them, the caller holding no lock between them. While each
/// certify's round VM runs, a hand `round abandon` tries the queue lock once: it gets it at once,
/// or it is told the lock is busy. Answers each verb's holds and each hand try's answer.
fn drive_beside_a_hand_driver(t: &T) -> (Vec<VerbHolds>, Vec<bool>) {
    let tries = std::rc::Rc::new(RefCell::new(Vec::new()));
    let (dir, seen) = (t.s().queue_dir.clone(), tries.clone());
    *t.scripts.during_vm.borrow_mut() = Some(Box::new(move || {
        let got = crate::lock::try_lock_as(&dir, "spira", "round abandon");
        seen.borrow_mut()
            .push(matches!(got, crate::lock::Acquire::Held(_)));
    }));
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    crate::lock::take_holds();
    let mut verbs = Vec::new();
    let mut run = |verb: &'static str, args: &[&str]| {
        let rc = t.run(args);
        verbs.push(VerbHolds {
            verb,
            rc,
            holds: crate::lock::take_holds(),
        });
    };
    run(
        "round open",
        &["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"],
    );
    let batch = batch_of(t);
    run("round certify", &["round", "certify", &batch]);
    seed_suite(t, "test-b.sh");
    run(
        "round eject",
        &[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red on test-b.sh",
            "--suites",
            "test-b.sh",
        ],
    );
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    run("round certify", &["round", "certify", &batch]);
    run("round land", &["round", "land", &batch]);
    let answers = tries.borrow().clone();
    (verbs, answers)
}

/// Each verb's own holds are its own label's, each one given back before the verb returned, and
/// none outstanding when a hand driver took the lock.
fn held_only_inside_their_verb(verbs: &[VerbHolds]) -> Result<(), String> {
    use crate::lock::Hold;
    for v in verbs {
        let mut open = 0usize;
        for h in &v.holds {
            match h {
                Hold::Taken { verb, .. } | Hold::Released { verb, .. }
                    if verb != "round abandon" && verb != v.verb =>
                {
                    return Err(format!("{} took the lock as {verb}", v.verb))
                }
                Hold::Taken { verb, .. } if verb == "round abandon" && open > 0 => {
                    return Err(format!(
                        "a hand abandon took the lock while {} held it",
                        v.verb
                    ))
                }
                Hold::Taken { verb, .. } if verb == v.verb => open += 1,
                Hold::Released { verb, .. } if verb == v.verb => open -= 1,
                _ => {}
            }
        }
        if open != 0 {
            return Err(format!("{} returned still holding the lock", v.verb));
        }
        if !v
            .holds
            .iter()
            .any(|h| matches!(h, Hold::Taken { verb, .. } if verb == v.verb))
        {
            return Err(format!("{} took no lock of its own", v.verb));
        }
    }
    Ok(())
}

/// Every hand abandon got the lock the moment it asked, without waiting for a pass to end.
fn hand_abandon_never_waits(tries: &[bool]) -> Result<(), String> {
    if let Some(i) = tries.iter().position(|got| !got) {
        return Err(format!(
            "hand abandon {} found the lock busy while a round VM ran",
            i + 1
        ));
    }
    match tries.len() {
        2 => Ok(()),
        n => Err(format!(
            "a hand abandon tried {n} times, not once per certify"
        )),
    }
}

#[test_id(tbsykky)]
#[test]
fn each_round_verb_holds_the_queue_lock_only_for_its_own_transition_so_a_hand_abandon_gets_in_at_once(
) {
    let t = round_world();
    let (verbs, tries) = drive_beside_a_hand_driver(&t);
    let rcs: Vec<(&str, i32)> = verbs.iter().map(|v| (v.verb, v.rc)).collect();
    assert_eq!(
        rcs,
        [
            ("round open", 0),
            ("round certify", 1),
            ("round eject", 0),
            ("round certify", 0),
            ("round land", 0)
        ],
        "{}",
        t.err()
    );
    held_only_inside_their_verb(&verbs).unwrap();
    hand_abandon_never_waits(&tries).unwrap();
    let certify: Vec<usize> = verbs
        .iter()
        .filter(|v| v.verb == "round certify")
        .map(|v| {
            v.holds
                .iter()
                .filter(|h| matches!(h, crate::lock::Hold::Taken { verb, .. } if verb == "round certify"))
                .count()
        })
        .collect();
    assert!(
        certify.iter().all(|n| *n >= 2),
        "certify locks once to prepare and again to record, around the VM run: {certify:?}"
    );
}

#[test_id(t5xcsi7)]
#[test]
fn a_caller_that_holds_the_lock_across_the_verbs_as_the_batcher_does_shuts_the_hand_abandon_out() {
    let t = round_world();
    let _held = t.hold_lock();
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    let (verbs, tries) = drive_beside_a_hand_driver(&t);
    assert!(verbs.iter().all(|v| v.holds.iter().all(
        |h| matches!(h, crate::lock::Hold::Taken { verb, .. } | crate::lock::Hold::Released { verb, .. } if verb == "round abandon")
    )), "under SPIRA_QUEUE_LOCK_HELD=1 the verbs take no lock of their own");
    let e = hand_abandon_never_waits(&tries).unwrap_err();
    assert_eq!(e, "hand abandon 1 found the lock busy while a round VM ran");
    assert!(held_only_inside_their_verb(&verbs)
        .unwrap_err()
        .contains("took no lock of its own"));
}

#[test_id(tn9th3o)]
#[test]
fn eject_also_ejects_the_members_stacked_on_the_ejected_one() {
    let t = round_world();
    t.git
        .bases
        .borrow_mut()
        .insert(("ta".into(), "tb".into()), "own-commit-of-a".into());
    t.git
        .bases
        .borrow_mut()
        .insert(("ta".into(), "tc".into()), "b0".into());
    t.git.ancestor("ta", "tb");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "red on lint"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("ejected sp-b from round") && t.out().contains("stacked on sp-a"),
        "{}",
        t.out()
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with(&format!("eject-member {batch} sp-b "))
                && c.ends_with("stacked on sp-a")),
        "b's reason names a"
    );
    assert!(t.lib.has("bead_reopen sp-b eject "));
    let rec = kv_of(&t, "round");
    assert_eq!(
        (rec["members"].as_str(), rec["head"].as_str()),
        ("sp-c:tc", "merged-tc")
    );
    assert_eq!(rec["ejected"].trim(), "sp-a sp-b");
    assert!(
        !t.lib.has("bead_reopen sp-c eject "),
        "an independent member stays"
    );
}

fn share(t: &T, a: &str, b: &str, own: &str) {
    t.git.bases.borrow_mut().insert((a.into(), b.into()), own.into());
}

fn eject_one(t: &T, members: &str, id: &str) -> String {
    assert_eq!(t.run(&["round", "open", "--members", members]), 0, "{}", t.err());
    let batch = batch_of(t);
    assert_eq!(t.run(&["round", "eject", &batch, id, "--reason", "red"]), 0, "{}", t.err());
    batch
}

#[test_id(tkut09p)]
#[test]
fn ejecting_a_stacked_child_leaves_the_member_it_is_stacked_on() {
    let t = round_world();
    share(&t, "ta", "tb", "own-commit-of-a");
    t.git.ancestor("ta", "tb");
    eject_one(&t, "sp-a:ta,sp-b:tb,sp-c:tc", "sp-b");
    assert!(!t.lib.has("bead_reopen sp-a eject "), "the parent stays");
    assert!(!t.lib.has("bead_reopen sp-c eject "));
    assert_eq!(kv_of(&t, "round")["ejected"].trim(), "sp-b");
}

#[test_id(tsefi3m)]
#[test]
fn ejecting_one_of_two_siblings_on_the_same_parent_leaves_the_other_and_the_parent() {
    let t = round_world();
    share(&t, "ta", "tb", "own-commit-of-a");
    share(&t, "ta", "tc", "own-commit-of-a");
    share(&t, "tb", "tc", "own-commit-of-a");
    t.git.ancestor("ta", "tb");
    t.git.ancestor("ta", "tc");
    eject_one(&t, "sp-a:ta,sp-b:tb,sp-c:tc", "sp-b");
    assert!(!t.lib.has("bead_reopen sp-a eject "));
    assert!(!t.lib.has("bead_reopen sp-c eject "));
    assert_eq!(kv_of(&t, "round")["ejected"].trim(), "sp-b");
}

#[test_id(teuiy84)]
#[test]
fn eject_follows_a_stacked_child_of_a_blocked_dependant_transitively() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    share(&t, "tb", "tc", "own-commit-of-b");
    t.git.ancestor("tb", "tc");
    let batch = eject_one(&t, "sp-a:ta,sp-b:tb,sp-c:tc", "sp-a");
    assert!(t.lib.has("bead_reopen sp-b eject ") && t.lib.has("bead_reopen sp-c eject "));
    assert!(
        t.lc.calls.borrow().iter().any(|c| c.starts_with(&format!("eject-member {batch} sp-c ")) && c.ends_with("stacked on sp-b")),
        "the reason names the direct prerequisite"
    );
}

#[test_id(t6g012e)]
#[test]
fn eject_takes_a_child_whose_recorded_stack_names_a_retipped_parent() {
    let t = round_world();
    share(&t, "ta", "tb", "own-commit-of-a");
    set_row(&t, "sp-b", |r| {
        r.stack.insert("sp-a".into(), "old-ta".into());
    });
    eject_one(&t, "sp-a:ta,sp-b:tb,sp-c:tc", "sp-a");
    assert!(t.lib.has("bead_reopen sp-b eject "));
    assert!(!t.lib.has("bead_reopen sp-c eject "));
}

#[test_id(tw9rgem)]
#[test]
fn blocked_on_takes_precedence_when_a_member_is_also_stacked_on_the_ejected_one() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    share(&t, "ta", "tb", "own-commit-of-a");
    t.git.ancestor("ta", "tb");
    eject_one(&t, "sp-a:ta,sp-b:tb,sp-c:tc", "sp-a");
    assert!(t.out().contains("ejected sp-b from round") && t.out().contains("blocked on sp-a"), "{}", t.out());
    assert!(!t.out().contains("stacked on sp-a"), "{}", t.out());
}

fn events_of(t: &T, batch: &str) -> Vec<String> {
    let prefix = format!("event batch {batch} ");
    t.lc.calls
        .borrow()
        .iter()
        .filter_map(|c| c.strip_prefix(&prefix))
        .map(|c| c.splitn(3, ' ').nth(2).unwrap_or("").to_string())
        .collect()
}

#[test_id(tu4spmz)]
#[test]
fn a_red_pass_the_batch_row_refuses_is_a_fault_not_a_local_red() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_refused.set(Some("PassRed"));
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(
        t.err().contains("REFUSED") && t.err().contains("IllegalTransition"),
        "{}",
        t.err()
    );
    assert!(
        !kv_of(&t, "round").contains_key("phase"),
        "the round's phase is the lifecycle's alone"
    );
}

#[test_id(t9e5068)]
#[test]
fn certify_hands_the_batch_and_repo_to_round_vm_to_record_its_boundaries() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("round-vm ") && c.contains(&format!("round={batch}/"))),
        "{:?}",
        t.scripts.calls.borrow()
    );
}

#[test_id(ts2vaio)]
#[test]
fn a_second_certify_of_a_batch_already_being_certified_is_refused_naming_the_holder() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let g = match crate::lock::try_certify_lock(&t.s().queue_dir, "spira", &batch) {
        crate::lock::Acquire::Held(g) => g,
        _ => panic!("lock"),
    };
    let before = t.scripts.calls.borrow().len();
    assert_ne!(t.run(&["round", "certify", &batch]), 0);
    assert!(
        t.err().contains("already being certified") && t.err().contains("pid="),
        "{}",
        t.err()
    );
    assert_eq!(
        t.scripts.calls.borrow().len(),
        before,
        "no pass was started"
    );
    drop(g);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
}

#[test_id(tkl60cv)]
#[test]
fn a_round_through_a_red_pass_an_eject_and_a_green_pass_records_both_passes() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    *t.scripts.round_vm_meta.borrow_mut() = "build_wall_s=30\n".into();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-tc\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-b.sh\",\"n\":1}}",
            "{\"PassRed\":{\"n\":1,\"red_suites\":[\"test-b.sh\"],\"suites_s\":0,\"build_s\":30,\"runner\":{\"place\":\"unrecorded\",\"why\":\"the pass left no runner.place\"}}}",
        ],
        "pass 1 is started, enters its suites and ends red, naming the suite"
    );
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red on test-b.sh",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        events_of(&t, &batch)
            .last()
            .unwrap()
            .starts_with("{\"PassRebuilt\":{\"head\":"),
        "{:?}",
        events_of(&t, &batch)
    );

    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let events = events_of(&t, &batch);
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("{\"PassStarted\":") && e.contains("\"n\":2")),
        "{events:?}"
    );
    assert!(
        events
            .last()
            .unwrap()
            .starts_with("{\"PassGreen\":{\"n\":2"),
        "{events:?}"
    );
}

fn place_file(place: &str, why: &str) -> Vec<(String, String)> {
    vec![("runner.place".into(), format!("place={place}\nwhy={why}\nio_full_avg300=\ncpu_some_avg300=\n"))]
}

#[test_id(tv4fiug)]
#[test]
fn every_pass_names_its_runner_and_a_move_between_runners_in_verdict_log_event_and_status() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    *t.scripts.round_vm_files.borrow_mut() = place_file("host", "host io pressure full avg300 1.0 at or under 50.0 — on the host");
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        t.out().lines().any(|l| l.contains(" is RED at ") && l.contains("runner=host (host io pressure full avg300 1.0 at or under 50.0") && !l.contains("runner_changed")),
        "{}",
        t.out()
    );
    seed_suite(&t, "test-b.sh");
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red on test-b.sh", "--suites", "test-b.sh"]), 0, "{}", t.err());

    *t.scripts.round_vm_files.borrow_mut() = place_file("ec2", "host io pressure full avg300 55.0 above 50.0");
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "red 1 2 fp p e 1".into()), ("test-b.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(
        t.out().lines().any(|l| l.contains(" is RED at ") && l.contains("runner=ec2 (host io pressure full avg300 55.0 above 50.0)") && l.contains("runner_changed=host->ec2")),
        "{}",
        t.out()
    );
    let log = t.landing_log();
    let red = log.lines().find(|l| l.starts_with("QUEUE ROUND-RED ") && l.contains("runner=ec2")).unwrap_or_else(|| panic!("{log}"));
    assert!(red.contains("runner_changed=host->ec2") && red.contains("runner_why=host io pressure full avg300 55.0 above 50.0"), "{red}");
    assert!(
        events_of(&t, &batch).iter().any(|e| e.starts_with("{\"PassRed\":{\"n\":2") && e.contains("\"runner\":{\"place\":\"ec2\"") && e.contains("\"runner_changed\":\"host->ec2\"")),
        "{:?}",
        events_of(&t, &batch)
    );

    let before = t.out().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[before..].to_string();
    assert!(status.contains("pass1_runner=host") && status.contains("pass2_runner=ec2"), "{status}");
    assert!(status.contains("pass2_runner_why=host io pressure full avg300 55.0 above 50.0"), "{status}");

    seed_suite(&t, "test-a.sh");
    assert_eq!(t.run(&["round", "eject", &batch, "sp-a", "--reason", "red on test-a.sh", "--suites", "test-a.sh"]), 0, "{}", t.err());
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let log = t.landing_log();
    let green = log.lines().find(|l| l.starts_with("QUEUE ROUND-GREEN ")).unwrap_or_else(|| panic!("{log}"));
    assert!(green.contains("runner=ec2") && !green.contains("runner_changed"), "{green}");
}

#[test_id(tuxja87)]
#[test]
fn a_green_pass_on_the_host_names_the_host_in_its_verdict_and_landing_log_line() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "ok".into())];
    *t.scripts.round_vm_files.borrow_mut() = place_file("host", "host io pressure full avg300 1.0 at or under 50.0 — on the host");
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.out().lines().any(|l| l.contains(" is GREEN at ") && l.contains("runner=host (")), "{}", t.out());
    let log = t.landing_log();
    let green = log.lines().find(|l| l.starts_with("QUEUE ROUND-GREEN ")).unwrap_or_else(|| panic!("{log}"));
    assert!(green.contains("runner=host") && !green.contains("runner_changed"), "{green}");
}

const TREE_WITH_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TREE_WITHOUT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test_id(t3ox9g5)]
#[test]
fn the_round_record_names_the_tree_each_pass_built_from() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    t.git.trees.borrow_mut().insert("merged-tc".into(), TREE_WITH_B.into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert_eq!(kv_of(&t, "round")["pass-trees"], format!("1={TREE_WITH_B}"));

    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red on test-b.sh", "--suites", "test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    t.git.trees.borrow_mut().insert("merged-tc".into(), TREE_WITHOUT_B.into());
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(
        kv_of(&t, "round")["pass-trees"],
        format!("1={TREE_WITH_B} 2={TREE_WITHOUT_B}"),
        "each pass keeps the tree it built from"
    );
}

#[test_id(twmshra)]
#[test]
fn a_pass_the_vm_could_not_finish_is_incomplete_not_red_or_green() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "killed".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let events = events_of(&t, &batch);
    assert!(events.last().unwrap().starts_with("{\"PassIncomplete\":{\"n\":1,\"reason\":\"round-vm hung past its guard (guard=hang-guard elapsed="), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("{\"PassRed\"") || e.starts_with("{\"PassGreen\"")),
        "{events:?}"
    );
}

#[test_id(t3c1dck)]
#[test]
fn suites_started_re_reads_and_survives_repeated_stale_versions() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    t.lc.stale_batch_events.set(3);
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    let shows =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("event batch") && c.contains("SuitesStarted"))
            .count();
    assert_eq!(shows, 4, "{:?}", t.lc.calls.borrow());
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 9, 1, "build".into()));
    t.lc.stale_batch_events.set(100);
    assert_ne!(t.run(&["round", "suites-started", &batch]), 0);
}

#[test_id(tab81s7)]
#[test]
fn a_repeated_suites_started_for_one_pass_is_a_no_op_success() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    for _ in 0..2 {
        assert_eq!(
            t.run(&["round", "suites-started", &batch]),
            0,
            "{}",
            t.err()
        );
    }
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-ta\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
        ]
    );
}

#[test_id(tvns6pa)]
#[test]
fn suites_started_outside_the_build_phase_of_a_pass_still_refuses() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 9, 1, String::new()));
    t.lc.batch_event_refused.set(Some("SuitesStarted"));
    assert_ne!(t.run(&["round", "suites-started", &batch]), 0);
}

#[test_id(tonqfgq)]
#[test]
fn the_pass_verbs_record_a_hand_driven_pass_like_the_batchers() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "pass-verdict",
            &batch,
            "--verdict",
            "red",
            "--red-suites",
            "test-x.sh",
            "--suites-s",
            "90",
            "--build-s",
            "30"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-ta\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"PassRed\":{\"n\":1,\"red_suites\":[\"test-x.sh\"],\"suites_s\":90,\"build_s\":30}}",
        ]
    );
}

#[test_id(tayf2ab)]
#[test]
fn a_pass_verb_out_of_phase_is_sent_so_the_machine_can_refuse_it_by_name() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0);
    assert_eq!(
        t.run(&["round", "pass-start", &batch]),
        0,
        "the fake machine does not refuse; the verb must still have sent the event"
    );
    assert_eq!(
        events_of(&t, &batch)
            .iter()
            .filter(|e| e.starts_with("{\"PassStarted\""))
            .count(),
        2,
        "a strict verb never swallows a wrong-phase call"
    );
    assert_eq!(
        t.run(&["round", "pass-verdict", &batch, "--verdict", "incomplete"]),
        2,
        "an incomplete pass names why"
    );
}

#[test_id(t6ppm7k)]
#[test]
fn a_pass_left_open_is_closed_incomplete_so_the_next_certify_starts_cleanly() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 2, 1, "suites".into()));
    t.scripts.pass_alive.set(false);
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    t.run(&["round", "certify", &batch]);
    let events = events_of(&t, &batch);
    let at = |p: &str| events.iter().position(|e| e.starts_with(p));
    assert!(
        at("{\"PassIncomplete\":{\"n\":1").is_some()
            && at("{\"PassIncomplete\"") < at("{\"PassStarted\""),
        "{events:?}"
    );
}

fn pass_world() -> (T, String) {
    let t = round_world();
    t.git
        .anc
        .borrow_mut()
        .remove(&("tb".to_string(), "merged-tc".to_string()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 4, 1, "suites".into()));
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh", "test-c.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    let results = marker(&t, &batch, "results");
    fs::create_dir_all(&results).unwrap();
    fs::write(results.join("test-a.sh.result"), "ok\n").unwrap();
    fs::write(results.join("test-b.sh.result"), "red 1 2 fp p e 1\n").unwrap();
    fs::write(marker(&t, &batch, "pass"), "777").unwrap();
    t.scripts.pass_alive.set(true);
    (t, batch)
}

#[test_id(t9q1qr1)]
#[test]
fn an_eject_with_no_suite_and_no_red_reopens_with_cause_eject() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.env.stdin.borrow_mut() = "gate red".into();
    let rc = t.run(&["round", "eject", &batch, "sp-b", "spira", "--reason-file", "-", "--no-rebuild"]);
    assert_eq!(rc, 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-b eject"), "{:?}", t.lib.calls.borrow());
}

#[test_id(tuyhz9u)]
#[test]
fn a_rebuild_that_cannot_cut_the_base_restores_the_head_and_says_nothing_changed() {
    let (t, batch) = pass_world();
    *t.lc.batch_view.borrow_mut() = Some(("ATTRIBUTING".into(), 4, 1, String::new()));
    let rec = kv_of(&t, "round");
    let (base, head) = (rec["base"].to_string(), rec["head"].to_string());
    *t.git.add_refused.borrow_mut() = Some(base);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"]), 1);
    assert!(t.err().contains("cannot create the round worktree: fatal: invalid reference") && t.err().contains("— nothing changed"), "{}", t.err());
    let wt = kv_of(&t, "round")["worktree"].to_string();
    assert_eq!(t.git.refs.borrow().get(&format!("{wt}:HEAD")), Some(&head), "the worktree is back at the round's head");
    assert!(!t.lc.has("eject-member"));
}

fn round_record(t: &T, _batch: &str) -> Kv {
    Kv::parse(&fs::read_to_string(t.qfile("round")).unwrap())
}

fn save_round_record(t: &T, kv: &Kv) {
    fs::write(t.qfile("round"), kv.render()).unwrap();
}

fn real_round_world(dir: &testkit::TempDir) -> (T, String, PathBuf, [String; 3], String) {
    let t = round_world();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "main"]);
    let commit = |m: &str| testkit::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", m]);
    fs::write(repo.join("base.txt"), "b\n").unwrap();
    testkit::git(&repo, &["add", "."]);
    commit("base");
    let base = testkit::git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let mut tips = vec![];
    for id in ["sp-a", "sp-b", "sp-c"] {
        testkit::git(&repo, &["checkout", "-q", "-b", &format!("spira/{id}"), &base]);
        fs::write(repo.join(format!("{id}.txt")), "x\n").unwrap();
        testkit::git(&repo, &["add", "."]);
        commit(id);
        tips.push(testkit::git(&repo, &["rev-parse", "HEAD"]).trim().to_string());
    }
    testkit::git(&repo, &["checkout", "-q", "main"]);
    let wt = dir.path().join("round-wt");
    testkit::git(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), &base]);
    for tip in &tips {
        testkit::git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-ff", "--no-edit", tip]);
    }
    let head = testkit::git(&wt, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    {
        let mut r = t.lib.r.borrow_mut();
        r.path = Some(repo.clone());
        r.landref = Some("main".into());
    }
    let mut kv = round_record(&t, &batch);
    for (k, v) in [("worktree", wt.to_str().unwrap()), ("base", &base), ("head", &head)] {
        kv.remove(k);
        kv.push(k, v);
    }
    let members = [("sp-a", &tips[0]), ("sp-b", &tips[1]), ("sp-c", &tips[2])]
        .map(|(id, tip)| format!("{id}:{tip}"))
        .join(" ");
    kv.remove("members");
    kv.push("members", &members);
    save_round_record(&t, &kv);
    *t.lc.batch_view.borrow_mut() = Some(("ATTRIBUTING".into(), 4, 1, String::new()));
    (t, batch, wt, [tips[0].clone(), tips[1].clone(), tips[2].clone()], base)
}

#[test_id(tcs27ko)]
#[test]
fn an_eject_rebuilds_a_locked_round_tree_in_place_and_starts_the_second_pass() {
    let dir = testkit::TempDir::new("eject-locked");
    let (t, batch, wt, _tips, base) = real_round_world(&dir);
    let repo = dir.path().join("repo");
    testkit::git(&repo, &["worktree", "lock", wt.to_str().unwrap()]);
    let rc = t.run_on(&crate::real::RealGit, &["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "spira-lint"]);
    assert_eq!(rc, 0, "{}", t.err());
    assert!(wt.join("sp-a.txt").exists() && wt.join("sp-c.txt").exists() && !wt.join("sp-b.txt").exists());
    let head = testkit::git(&wt, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(testkit::git(&wt, &["rev-parse", "HEAD^1"]).trim(), "");
    assert_eq!(kv_of(&t, "round")["head"], head);
    assert_eq!(testkit::git(&wt, &["merge-base", &head, &base]).trim(), base);
    assert!(t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    t.run_on(&crate::real::RealGit, &["round", "certify", &batch]);
    assert!(t.err().contains("round-vm ran and left no verdicts"), "certify started the second pass: {}", t.err());
}

#[test_id(tk0y0eg)]
#[test]
fn an_eject_whose_reset_is_refused_quotes_gits_stderr_and_keeps_the_head() {
    let dir = testkit::TempDir::new("eject-refused");
    let (t, batch, wt, _tips, _base) = real_round_world(&dir);
    let head = round_record(&t, &batch).get("head").unwrap().to_string();
    let gitdir = testkit::git(&wt, &["rev-parse", "--absolute-git-dir"]);
    fs::write(Path::new(gitdir.trim()).join("index.lock"), "").unwrap();
    let rc = t.run_on(&crate::real::RealGit, &["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "spira-lint"]);
    assert_eq!(rc, 1, "{}", t.err());
    assert!(t.err().contains("cannot create the round worktree: fatal:") && t.err().contains("index.lock"), "{}", t.err());
    assert_eq!(testkit::git(&wt, &["rev-parse", "HEAD"]).trim(), head);
    assert!(!t.lc.has("eject-member"));
}

#[test_id(tio19bj)]
#[test]
fn an_eject_refused_because_a_pass_started_after_the_check_exits_pass_running() {
    let (t, batch) = pass_world();
    *t.lc.batch_view.borrow_mut() = Some(("ATTRIBUTING".into(), 4, 1, String::new()));
    let before = kv_of(&t, "round");
    t.lc.eject_refused_by_a_pass_starting.set(true);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"]), crate::ops::round::PASS_RUNNING, "{}", t.err());
    let e = t.err();
    assert!(e.contains("pass 2") && e.contains("phase suites"), "{e}");
    let after = kv_of(&t, "round");
    assert_eq!((after["head"].to_string(), after["members"].to_string()), (before["head"].to_string(), before["members"].to_string()));
}

fn build_phase_world() -> (T, String) {
    let (t, batch) = pass_world();
    t.scripts.pass_alive.set(false);
    t.scripts.round_legs.borrow_mut().clear();
    let v = t.lc.batch_view.borrow().as_ref().map(|v| v.1).unwrap();
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), v, 3, "build".into()));
    (t, batch)
}

fn assert_eject_refused_while_the_pass_builds(extra: &[&str]) {
    let (t, batch) = build_phase_world();
    let before = kv_of(&t, "round");
    let git_before = t.git.calls.borrow().len();
    let mut argv = vec!["round", "eject", batch.as_str(), "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"];
    argv.extend_from_slice(extra);
    assert_eq!(t.run(&argv), crate::ops::round::PASS_RUNNING, "{}", t.err());
    let e = t.err();
    assert!(e.contains("pass 3") && e.contains("phase build"), "{e}");
    assert!(e.contains("spira-lc hold") && e.contains("round abandon"), "{e}");
    let after = kv_of(&t, "round");
    assert_eq!((after["head"].to_string(), after["members"].to_string()), (before["head"].to_string(), before["members"].to_string()));
    assert!(!t.lc.has("eject-member"));
    assert_eq!(t.git.calls.borrow().len(), git_before, "no worktree call");
    assert_eq!(
        t.run(&["round", "pass-verdict", &batch, "--verdict", "red", "--red-suites", "test-b.sh"]),
        0,
        "the running pass's verdict is still accepted: {}",
        t.err()
    );
    assert!(events_of(&t, &batch).iter().any(|e| e.starts_with("{\"PassRed\"")), "PassRed is sent");
}

#[test_id(towvram)]
#[test]
fn eject_refuses_while_the_lifecycle_row_says_a_pass_is_running_though_no_scope_exists() {
    assert_eject_refused_while_the_pass_builds(&[]);
}

#[test_id(tw3lhvr)]
#[test]
fn the_batchers_lease_token_does_not_get_an_eject_past_a_running_pass() {
    let (t, batch) = build_phase_world();
    leased_by(&t, &batch, "round-recert", "12-0");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh", "--lease-token", "12-0"]),
        crate::ops::round::PASS_RUNNING,
        "{}",
        t.err()
    );
    assert!(!t.lc.has("eject-member"));
}

#[test_id(tcv6yan)]
#[test]
fn eject_refuses_when_the_lifecycle_row_cannot_be_read() {
    let (t, batch) = build_phase_world();
    t.lc.no_batch_row.set(true);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"]), 1);
    assert!(t.err().contains("cannot tell whether a pass of round"), "{}", t.err());
    assert!(!t.lc.has("eject-member"));
}

#[test_id(tsqpiri)]
#[test]
fn an_attributing_red_round_admits_an_eject_of_a_member() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb,sp-c:tc");
    seed_suite(&t, "test-b.sh");
    *t.lc.batch_view.borrow_mut() = Some(("ATTRIBUTING".into(), 4, 2, String::new()));
    let argv = ["round", "eject", batch.as_str(), "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"];
    assert_eq!(t.run(&argv), 0, "{}", t.err());
    assert!(t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tzojc90)]
#[test]
fn a_running_pass_refuses_eject_naming_the_env_exit_and_that_exit_abandons() {
    let (t, batch) = pass_world();
    let argv = ["round", "eject", batch.as_str(), "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"];
    assert_eq!(t.run(&argv), crate::ops::round::PASS_RUNNING, "{}", t.err());
    let e = t.err();
    assert!(e.contains("QUEUE_ABANDON_CONSIDERED"), "{e}");
    assert!(!e.contains("`queue round abandon <batch> <repo>`"), "no bare abandon is offered: {e}");
    t.env.vars.borrow_mut().insert("QUEUE_ABANDON_CONSIDERED".into(), "attribution is not converging".into());
    assert_eq!(t.run(&["round", "abandon", &batch, "--reason", "stall"]), 0, "{}", t.err());
    assert!(t.lc.has(&format!("abandon-batch {batch}")), "{:?}", t.lc.calls.borrow());
}

#[test_id(tsaobmx)]
#[test]
fn abandoning_a_red_round_whose_pass_runs_does_not_offer_eject() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb,sp-c:tc");
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 2, 1, "suites".into()));
    assert_eq!(t.run(&["round", "abandon", &batch, "--reason", "red"]), 1);
    let e = t.err();
    assert!(e.contains("QUEUE_ABANDON_CONSIDERED") && !e.contains("queue round eject"), "{e}");
    assert!(!t.lc.has("abandon-batch"), "a refused abandon changes nothing");
}

const T_STAGED: &str = "4444444444444444444444444444444444444444";
const T_MOVED: &str = "5555555555555555555555555555555555555555";

fn vm_runs(t: &T) -> usize {
    t.scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("round-vm "))
        .count()
}

/// A round `r1` (sp-a) open and certified GREEN, with sp-b staged behind it and tested.
fn staged_behind_green() -> (T, String, String) {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("GREEN".into(), "5".into()));
    t.io.out.borrow_mut().clear();
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-tb");
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), T_STAGED.into());
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    (t, n, "spira-staged".into())
}

#[test_id(tvk6pky)]
#[test]
fn a_stage_refuses_naming_the_gate_timeout_key_when_it_does_not_resolve_and_runs_no_gate() {
    let mut t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n, ("GREEN".into(), "5".into()));
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-tb");
    t.git.trees.borrow_mut().insert("merged-tb".into(), T_STAGED.into());
    t.lib.s.gate_timeout = None;
    let gates = |t: &T| t.scripts.calls.borrow().iter().filter(|c| c.starts_with("gate ")).count();
    let before = gates(&t);
    assert_ne!(t.run(&["round", "stage", "--members", "sp-b:tb", "--name", "spira-staged"]), 0);
    assert!(t.err().contains("SPIRA_GATE_TIMEOUT"), "{}", t.err());
    assert_eq!(gates(&t), before);
}

#[test_id(tnx7pdh)]
#[test]
fn a_staged_round_is_merged_onto_the_open_rounds_head_and_moves_no_bead() {
    let (t, n, s) = staged_behind_green();
    assert!(
        t.lc.has(&format!("stage {s} merged-tb merged-ta sp-b:tb behind {n}")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c == "merge tb spira: land sp-b"),
        "the merge is open's: land_subject, member tip"
    );
    assert!(
        t.scripts.calls.borrow().iter().any(|c| c.contains("gate")
            && c.contains("spira/round-staged/spira-staged")
            && c.ends_with("lock_wait=5400")),
        "the fences run on the staged head: {:?}",
        t.scripts.calls.borrow()
    );
    assert!(
        !t.lc.has("cut ")
            || t.lc
                .calls
                .borrow()
                .iter()
                .filter(|c| c.starts_with("cut "))
                .count()
                == 1,
        "only the open round was cut"
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "a staged member is not delivered"
    );
    assert!(recorded(&t, "stage", "batcher", &s), "{:?}", round_records(&t));
    assert!(t.landing_log().contains("QUEUE ROUND-STAGE ") && t.landing_log().contains(&format!("batch={s} by=batcher behind=")));
    let rec = kv_of(&t, "round-staged");
    assert_eq!(
        (
            rec["parent"].as_str(),
            rec["head"].as_str(),
            rec["base"].as_str(),
            rec["phase"].as_str()
        ),
        (n.as_str(), "merged-tb", "merged-ta", "staged")
    );
    assert_eq!(
        t.run(&["round", "stage", "--members", "sp-c"]),
        1,
        "one staged round at a time"
    );
    assert!(t.err().contains("already waiting"), "{}", t.err());
    let _ = t.run(&["round", "status"]);
    assert!(
        t.out().contains(&format!("staged_batch={s}")),
        "{}",
        t.out()
    );
}

#[test_id(t33i0ef)]
#[test]
fn stage_refuses_without_an_open_round_and_skips_the_open_rounds_own_members() {
    let t = round_world();
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("nothing to stage behind"), "{}", t.err());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "stage", "--members", "sp-a:ta"]), 1);
    assert!(t.out().contains("sp-a: already a member of"), "{}", t.out());
    assert!(!t.qfile("round-staged").exists());
}

#[test_id(tst8bs2)]
#[test]
fn fences_red_on_the_staged_head_stage_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    t.scripts.gate_rc.set(1);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("fences are red"), "{}", t.err());
    assert!(
        !t.qfile("round-staged").exists() && !t.lc.has("stage "),
        "a red fence leaves no staged round"
    );
}

#[test_id(t3xk4c3)]
#[test]
fn staging_runs_its_fences_with_the_queue_lock_free() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    *t.scripts.lock_probe.borrow_mut() = Some(t.s().queue_dir.clone());
    assert_eq!(
        t.run(&["round", "stage", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        *t.scripts.lock_free_at_gate.borrow(),
        [true],
        "a stage's gate run must not hold the lock the open round's driver retakes"
    );
}

#[test_id(txz06w0)]
#[test]
fn a_stage_refuses_to_start_while_another_stage_is_running_its_fences() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let held = crate::lock::try_stage_lock(&t.s().queue_dir, "spira");
    assert!(matches!(held, crate::lock::Acquire::Held(_)));
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 6);
    assert!(t.err().contains("another stage"), "{}", t.err());
    assert!(!t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("gate ")));
}

#[test_id(ts7hzir)]
#[test]
fn a_stage_whose_slot_was_taken_during_its_fences_is_not_staged_and_cleans_up() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let planted = t.qfile("round-staged");
    *t.scripts.during_gate.borrow_mut() = Some(Box::new(move || {
        std::fs::write(&planted, "batch_id=other\n").unwrap();
    }));
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("while the fences ran"), "{}", t.err());
    assert!(!t.lc.has("stage "), "nothing is staged on spira-lc");
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("branch -D sanctioned") && c.contains("round-staged/")),
        "the staged branch is deleted"
    );
}

#[test_id(th1j28a)]
#[test]
fn a_no_verdict_fence_is_infra_and_leaves_the_stage_retryable() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    t.scripts.gate_rc.set(75);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 75);
    assert!(
        t.err().contains("infra, not red") && !t.err().contains("fences are red"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("stage "));
}

#[test_id(t1uiikb)]
#[test]
fn the_staged_round_is_tested_only_once_the_round_ahead_is_green() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "stage", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert!(t.err().contains("not green"), "{}", t.err());
    assert_eq!(vm_runs(&t), 0);
}

#[test_id(tyy8ejg)]
#[test]
fn promotion_on_an_identical_tree_reuses_the_staged_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(
        vm_runs(&t),
        2,
        "the open round's pass and the staged round's pass"
    );
    let rec = kv_of(&t, "round-staged");
    assert_eq!(
        (rec["phase"].as_str(), rec["tested_tree"].as_str()),
        ("green", T_STAGED)
    );

    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("LANDED".into(), "9".into()));
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("CI_RUNNING".into(), "2".into()));
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.lc.has(&format!("promote {s} merged-tb merged-ta")),
        "base is the landed head: {:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(vm_runs(&t), 2, "promotion ran no pass of its own");
    let answer: serde_json::Value = serde_json::from_str(
        t.out()
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(answer["attested"], true, "{}", t.out());
    assert_eq!(answer["batch"], s.as_str());
    assert_eq!(answer["head"], "merged-tb", "{}", t.out());
    assert!(
        answer["members"].as_array().is_some_and(|m| !m.is_empty()
            && m.iter()
                .all(|x| x["id"].is_string() && x["tip"].is_string())),
        "{answer}"
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")),
        "the tested pass is recorded on the promoted round: {:?}",
        t.lc.calls.borrow()
    );
    assert!(
        gate::cert::path(&t.s().run.join("verdicts"), "spira", T_STAGED)
            .is_some_and(|p| p.exists()),
        "the tested tree is certified"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (rec["batch_id"].as_str(), rec["members"].as_str()),
        (s.as_str(), "sp-b:tb")
    );
    assert!(!rec.contains_key("phase"));
    assert!(!t.qfile("round-staged").exists());
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("GREEN".into(), "3".into()));
    assert_eq!(t.run(&["round", "land", &s]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test_id(t0nz35l)]
#[test]
fn promotion_onto_a_moved_base_gets_a_fresh_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("LANDED".into(), "9".into()));
    t.git.set("refs/heads/local/main", "moved");
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), T_MOVED.into());
    let merges = t
        .git
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("merge "))
        .count();
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("merge "))
            .count()
            > merges,
        "the members are merged again onto the moved base"
    );
    assert!(
        t.lc.has(&format!("promote {s} merged-tb moved")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(t.out().contains("\"attested\":false"), "{}", t.out());
    assert!(
        !t.lc
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")),
        "no pass is recorded without a run"
    );
    assert!(!kv_of(&t, "round").contains_key("phase"));
    assert!(
        !gate::cert::path(&t.s().run.join("verdicts"), "spira", T_MOVED)
            .is_some_and(|p| p.exists())
    );
}

#[test_id(tarob0m)]
#[test]
fn a_staged_pass_the_unit_tier_ended_before_any_suite_is_red_on_unit_tests() {
    let (t, _n, _s) = staged_behind_green();
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![];
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "unit.json".to_string(),
        "{\"stage\":\"unit-tests\",\"verdict\":\"red\",\"ran_before_suites\":true,\"cause\":\"exit status 101\",\"secs\":3,\"last_binary\":\"\",\"failed\":[\"a::broken\"]}\n".to_string(),
    )];
    assert_eq!(t.run(&["round", "stage-test"]), 1, "{}{}", t.out(), t.err());
    assert_eq!(kv_of(&t, "round-staged")["phase"], "red");
    assert!(
        t.out().contains("red=unit-tests") && t.out().contains("unit-tests-failed=a::broken"),
        "{}",
        t.out()
    );
    assert!(!t.err().contains("left no verdicts"), "{}", t.err());
}

#[test_id(tzj058f)]
#[test]
fn a_staged_round_whose_pass_was_red_is_not_attested() {
    let (t, n, s) = staged_behind_green();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert_eq!(kv_of(&t, "round-staged")["phase"], "red");
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("LANDED".into(), "9".into()));
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.out().contains("\"attested\":false") && t.lc.has(&format!("promote {s} ")),
        "{}",
        t.out()
    );
}

#[test_id(tg7jszi)]
#[test]
fn a_red_round_discards_the_stage_behind_it() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-a.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    t.git.set("HEAD", "merged-ta");
    assert_eq!(t.run(&["round", "certify", &n]), 1);
    assert!(t.lc.has("event batch spira-staged STAGED 0 {\"Discard\":{\"reason\":\"round spira-20260929T010203Z went red: test-a.sh\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(
        !t.qfile("round-staged").exists(),
        "the staged record is gone"
    );
    assert!(
        t.landing_log().contains("QUEUE ROUND-DISCARD")
            && t.landing_log().contains("batch=spira-staged")
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "discarding returns no member: none was delivered"
    );
}

#[test_id(tufciks)]
#[test]
fn an_abandoned_round_discards_the_stage_behind_it() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    assert_eq!(
        t.run(&["round", "abandon", &n, "--reason", "gave up"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("event batch spira-staged STAGED 0")
                && c.contains("Discard")
                && c.contains("gave up")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(!t.qfile("round-staged").exists());
}

#[test_id(ts2kfn2)]
#[test]
fn promote_discards_when_the_round_it_followed_did_not_land() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("STAGED".into(), "0".into()));
    let _ = fs::remove_file(t.qfile("round"));
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("ABANDONED".into(), "5".into()));
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(
        t.err().contains("is ABANDONED, not LANDED") && t.err().contains("discarded"),
        "{}",
        t.err()
    );
    assert!(!t.lc.has("promote ") && !t.qfile("round-staged").exists());
}

#[test_id(t10vjs8)]
#[test]
fn promote_waits_while_the_round_ahead_is_still_open() {
    let (t, _, _) = staged_behind_green();
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("still open"), "{}", t.err());
    assert!(
        t.qfile("round-staged").exists() && !t.lc.has("promote "),
        "the stage is kept"
    );
}

#[test_id(t0tzed6)]
#[test]
fn promote_discards_when_a_staged_member_moved() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s, ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("LANDED".into(), "9".into()));
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-b")
    {
        r.tip = Some("tb2".into());
    }
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("no longer admissible"), "{}", t.err());
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("promote "));
}

#[test_id(tl3uc50)]
#[test]
fn round_open_discards_a_stage_whose_round_is_gone() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round-staged").exists());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with(&format!("event batch {s} STAGED 0")) && c.contains("Discard")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(tdzou8f)]
#[test]
fn a_mender_bead_carries_the_resolved_scope_label_though_the_process_env_lacks_it() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    t.env
        .resolved
        .borrow_mut()
        .insert("SPIRA_SCOPE_LABEL".into(), "scope-fixture".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lib.calls.borrow();
    let bug = calls.iter().find(|c| c.starts_with("create_bug")).unwrap();
    assert!(bug.contains(" scope-fixture,mend-fixture,repo:"), "{bug}");
}

#[test_id(t0qw7jo)]
#[test]
fn a_mender_bead_gets_its_lifecycle_repo_at_creation() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("create-bead ") && c.ends_with(" --repo spira")),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("create-bead ") && !c.contains("--repo")),
        "{calls:?}"
    );
}

#[test_id(ttn3ebg)]
#[test]
fn each_red_suite_files_one_mender_lane_bead_carrying_its_output_pass_and_batch() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("test-z.sh.out"), "assertion Z failed\n").unwrap();

    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );

    let calls = t.lib.calls.borrow();
    let bugs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .collect();
    assert_eq!(
        bugs.len(),
        2,
        "one bead per failure, none for a repeated report: {bugs:?}"
    );
    assert!(
        bugs[0].contains(" mend-fixture,repo:") && bugs[0].contains("test-z.sh"),
        "{}",
        bugs[0]
    );
    assert!(
        bugs[0].contains("assertion Z failed")
            && bugs[0].contains(&format!("Batch: {batch}"))
            && bugs[0].contains("Pass: 1")
            && bugs[0].contains("Deadline:"),
        "{}",
        bugs[0]
    );
    assert!(
        bugs[1].contains("test-b.sh") && bugs[1].contains("left no output"),
        "{}",
        bugs[1]
    );
}

#[test_id(ty48v7i)]
#[test]
fn a_red_that_is_a_repo_config_refusal_files_no_mender_bead_in_any_round() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(
        results.join("ci.out"),
        ".spira/testenv.toml declares no ci_entry\n",
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            t.run(&["round", "failed", &batch, "--suite", "ci"]),
            0,
            "{}",
            t.err()
        );
    }
    let bugs = t
        .lib
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .count();
    assert_eq!(bugs, 0, "a refusal no mend can fix files nothing");
}

#[test_id(tov0f2g)]
#[test]
fn a_suite_going_red_mid_pass_is_a_failed_event_in_the_order_it_failed_and_the_pass_is_not_judged()
{
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());

    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-tc\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-z.sh\",\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-b.sh\",\"n\":1}}",
        ],
        "the suites phase opens on the first result, and no verdict is sent"
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "bad suite"]),
        1,
        "a suite name is an identifier"
    );
}

#[test_id(tugjjl5)]
#[test]
fn a_red_certify_with_no_batcher_files_one_mender_per_red_suite_with_the_attribution() {
    let mut t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lib.s.batcher_bin = Some(std::path::PathBuf::from("batcher"));
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "red".into()),
        ("test-b.sh".into(), "red".into()),
        ("test-c.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let calls = t.lib.calls.borrow();
    let bugs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .collect();
    assert_eq!(bugs.len(), 2, "one mender per red suite: {bugs:?}");
    assert!(
        bugs.iter().all(|b| b.contains("verdict=ambiguous")),
        "{bugs:?}"
    );
}

#[test_id(t00i788)]
#[test]
fn certify_reds_a_pass_whose_unit_tests_failed_though_every_suite_is_ok() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: "round-vm: UNIT-TESTS: FAIL (unit run rc=101)".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=unit-tests"), "{}", t.out());
    assert!(
        !certified_tree_exists(&t),
        "unit tests that failed certify nothing"
    );
}

#[test_id(t26ufi2)]
#[test]
fn certify_reds_a_failed_workspace_build_naming_the_first_compiler_error() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "round-vm: building\nerror[E0063]: missing field `cpus` in initializer of `RunRequest`\n   --> testenv/src/shots.rs:224:9\nerror: could not compile `testenv`\nround-vm: the round's workspace build failed";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 4,
        out: String::new(),
        err: err.into(),
    };
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=workspace-build"), "{}", t.out());
    assert!(
        t.out()
            .contains("build-error=error[E0063]: missing field `cpus`")
            && t.out().contains("testenv/src/shots.rs:224"),
        "{}",
        t.out()
    );
}

#[test_id(txgptq4)]
#[test]
fn certify_saves_the_compiler_errors_of_a_failed_workspace_build_and_quotes_the_first() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "round-vm: building\nerror[E0425]: cannot find value `x` in this scope\n   --> spira/sim/src/x.rs:12:5\n    |\n12  |     x\n\nerror[E0063]: missing field `cpus`\n   --> testenv/src/shots.rs:224:9\n\nerror: could not compile `testenv`\nround-vm: the round's workspace build failed";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 4,
        out: String::new(),
        err: err.into(),
    };
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(
        t.out()
            .contains("red-reason=workspace-build: error[E0425] at spira/sim/src/x.rs:12:5"),
        "{}",
        t.out()
    );
    let saved =
        find_file_named(&t.dir, "workspace-build.out").expect("workspace-build.out was written");
    assert!(
        saved.contains("cargo build") && saved.contains("exit: 4"),
        "{saved}"
    );
    assert!(
        saved.contains("error[E0425]") && saved.contains("error[E0063]"),
        "{saved}"
    );
}

#[test_id(tfd2z7o)]
#[test]
fn a_workspace_build_that_died_of_the_environment_is_not_the_members_red() {
    use crate::ops::round::build_environment_cause;
    assert_eq!(
        build_environment_cause(
            "round-vm: building\nerror: failed to write: No space left on device"
        ),
        Some("out of disk")
    );
    assert_eq!(
        build_environment_cause("error[E0425]: cannot find value\nNo space left on device"),
        None,
        "a compiler diagnostic makes it the candidate's"
    );
}

fn find_file_named(dir: &std::path::Path, name: &str) -> Option<String> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(s) = find_file_named(&p, name) {
                return Some(s);
            }
        } else if p.file_name().is_some_and(|f| f == name) {
            return std::fs::read_to_string(p).ok();
        }
    }
    None
}

#[test_id(tl0fzgg)]
#[test]
fn certify_judges_a_wall_killed_run_whose_results_and_unit_tests_are_complete() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "round-vm: UNIT-TESTS: PASS (3 test binaries)\nround-vm: INTEGRATION-TESTS: PASS (2 test binaries)".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.out().contains("GREEN"), "{}", t.out());
}

#[test_id(tlee974)]
#[test]
fn a_wall_killed_run_whose_integration_tests_never_reported_is_not_judged() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "round-vm: UNIT-TESTS: PASS (3 test binaries)".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(
        !t.out().contains("GREEN"),
        "the unit phase alone does not complete the run: {}",
        t.out()
    );
}

#[test_id(thur1b7)]
#[test]
fn an_integration_red_beside_green_suites_is_judged_red_on_unit_tests() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: "round-vm: INTEGRATION-TESTS: FAIL (integration run rc=101):\n".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "integration.json".to_string(),
        "{\"stage\":\"integration-tests\",\"verdict\":\"red\",\"ran_before_suites\":false,\"cause\":\"exit status 101\",\"secs\":61,\"last_binary\":\"journal_dolt\",\"failed\":[\"replays_in_order\"]}\n".to_string(),
    )];
    assert_eq!(
        t.run(&["round", "certify", &batch]),
        1,
        "{}{}",
        t.out(),
        t.err()
    );
    assert!(
        t.out().contains("unit-tests-failed=replays_in_order"),
        "{}",
        t.out()
    );
    assert!(
        t.out().contains(
            "unit-tests-cause=integration-tests: exit status 101 (beside the suites, after 61s)"
        ),
        "{}",
        t.out()
    );
    let events = events_of(&t, &batch);
    assert!(events.iter().any(|e| e.starts_with("{\"PassRed\"") && e.contains("\"red_suites\":[\"unit-tests\"]")), "{events:?}");
}

#[test_id(tekos3h)]
#[test]
fn a_green_write_that_times_out_after_applying_still_certifies_and_certify_is_idempotent() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_times_out.set(Some("PassGreen"));
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        certified_tree_exists(&t),
        "the certificate is written though the reply was lost"
    );

    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    t.run(&["round", "land", &batch]);
    assert!(
        t.err().contains("certified by round GREEN"),
        "land finds the certificate: {}",
        t.err()
    );
}

#[test_id(tvwte0v)]
#[test]
fn a_retry_after_a_timed_out_green_completes_the_missing_certificate() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("GREEN".into(), 1, 1, String::new()));
    let ran = t.scripts.calls.borrow().len();
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    assert_eq!(
        t.scripts.calls.borrow().len(),
        ran,
        "a GREEN round is finished, never re-tested: {:?}",
        t.scripts.calls.borrow()
    );
}

#[test_id(tmxyq40)]
#[test]
fn a_green_the_lifecycle_refuses_keeps_the_certificate_the_pass_earned() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_refused.set(Some("PassGreen"));
    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        certified_tree_exists(&t),
        "the full corpus passed on this tree; a refused reply must not lose that"
    );
    assert!(
        t.err().contains("certificate") && t.err().contains("kept"),
        "{}",
        t.err()
    );
}

#[test_id(tnb1xm9)]
#[test]
fn a_green_batch_at_another_head_is_still_refused_as_changed() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("GREEN".into(), 1, 1, String::new()));
    t.git.set("refs/heads/elsewhere", "merged-ta");
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "elsewhere",
            "--ran",
            "3"
        ]),
        1
    );
    assert!(!certified_tree_exists(&t));
}

fn guards_file(t: &T) -> String {
    let p = t.dir.path().join("statute-guards");
    std::fs::write(
        &p,
        "law-nonrebase | refuse | rebase by hand\nlaw-touch | show | allocator\n",
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

#[test_id(t3c4clx)]
#[test]
fn a_round_eject_reason_contradicting_a_statute_is_refused_and_ejects_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let g = guards_file(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), g);
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "needs a rebase by hand"
        ]),
        1
    );
    assert!(
        t.err().contains("law-nonrebase") && t.err().contains("BEAD_STATUTE_CONSIDERED"),
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round"), before);
    assert!(!t.lib.has("bead_reopen"));
}

#[test_id(tdtu35l)]
#[test]
fn a_round_eject_with_an_override_reason_proceeds_and_a_touch_is_shown() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let g = guards_file(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), g);
    t.env
        .vars
        .borrow_mut()
        .insert("BEAD_STATUTE_CONSIDERED".into(), "not that rebase".into());
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "rebase by hand"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-a",
            "--reason",
            "the allocator is red"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.err().contains("law-touch"), "{}", t.err());
}

#[test_id(tjggzmj)]
#[test]
fn an_unreadable_statute_registry_refuses_a_round_eject() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), testkit::TempDir::new("round-guards").join("nonexistent/guards").to_string_lossy().into_owned());
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "red"]),
        1
    );
    assert!(
        t.err().contains("cannot read the statute registry"),
        "{}",
        t.err()
    );
}

const T_FIX: &str = "4444444444444444444444444444444444444444";

fn rework_world(holds: &[&str]) -> T {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .retain(|r| r.bead_id != "sp-a");
    t.lc_row("sp-a", "REWORK", "ta", 100);
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.holds = holds.iter().map(|h| h.to_string()).collect();
    }
    t.git.set("refs/heads/spira/sp-a", T_FIX);
    t.git.ancestor(T_FIX, "merged-tb");
    t
}

fn ev_tip() -> String {
    format!("--evidence-tip={T_FIX}")
}

#[test_id(tc7ljq1)]
#[test]
fn a_rework_bead_held_manual_is_retipped_and_admitted_to_a_round() {
    let t = rework_world(&["manual"]);
    let tip_arg = format!("--tip={T_FIX}");
    t.run(&["round", "open", "--members", &format!("sp-a:{T_FIX}")]);
    assert!(
        t.err().contains("1 of 1 named members") && !t.qfile("round").exists(),
        "a REWORK row is not admitted: {}",
        t.err()
    );

    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!(
            "retip sp-a {T_FIX} concierge [hand-built fix] [npm test 1/0] {T_FIX}"
        )),
        "{:?}",
        t.lc.calls.borrow()
    );

    assert_eq!(
        t.run(&["round", "open", "--members", &format!("sp-a:{T_FIX}"), "--by", "concierge"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains(&format!("sp-a:{T_FIX}")), "{}", t.out());
}

fn set_row(t: &T, id: &str, f: impl Fn(&mut crate::model::LcBeadRow)) {
    t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().filter(|r| r.bead_id == id).for_each(f);
}

#[test_id(t5c4s7k)]
#[test]
fn resubmit_at_the_ejected_red_tip_withdraws_the_red_in_one_step() {
    let t = rework_world(&["manual"]);
    set_row(&t, "sp-a", |r| r.ejected_red_tip = Some(T_FIX.into()));
    fs::write(t.qfile("red-tips"), format!("sp-a {T_FIX}\n")).unwrap();
    let verdicts = t.s().run.join("verdicts");
    let red = verdicts.join("red");
    fs::create_dir_all(&red).unwrap();
    fs::write(red.join("k1"), gate::redcache::render(1, "branch-red", "s", "w", 1, "c", T_FIX, "m\n")).unwrap();
    assert!(gate::redcache::holds_tip(&verdicts, T_FIX), "positive control");
    assert!(fs::read_to_string(t.qfile("red-tips")).unwrap().contains(T_FIX), "positive control");

    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&["resubmit", "sp-a", &tip_arg, "--by=concierge", "--reason=infra red", "--evidence=clean rerun 9/0", &ev_tip()]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains(&format!("red at {T_FIX} withdrawn")), "{}", t.out());
    assert!(!fs::read_to_string(t.qfile("red-tips")).unwrap().contains(T_FIX));
    assert!(!gate::redcache::holds_tip(&verdicts, T_FIX));
    assert!(t.lib.has("comment sp-a Red at") && t.lib.calls.borrow().iter().any(|c| c.contains("clean rerun 9/0")), "{:?}", t.lib.calls.borrow());

    set_row(&t, "sp-a", |r| r.state = "CERTIFIED".into());
    let rc = t.run(&["round", "open", "--members", &format!("sp-a:{T_FIX}")]);
    assert_eq!(rc, 0, "{}{}", t.out(), t.err());
    assert!(!format!("{}{}", t.out(), t.err()).contains("ejected-red"), "{}{}", t.out(), t.err());
}

#[test_id(tmfvak3)]
#[test]
fn resubmit_says_a_held_bead_is_not_cut_by_the_batcher() {
    let args = |tip_arg: &str| {
        ["resubmit", "sp-a", tip_arg, "--by=concierge", "--reason=fix", "--evidence=e 1/0"].map(String::from).into_iter().chain([ev_tip()]).collect::<Vec<_>>()
    };
    let tip_arg = format!("--tip={T_FIX}");
    let held = rework_world(&["manual"]);
    let a = args(&tip_arg);
    assert_eq!(held.run(&a.iter().map(String::as_str).collect::<Vec<_>>()), 0, "{}", held.err());
    assert!(held.out().contains("stays held manual") && held.out().contains("it is screened and gated") && held.out().contains("queue round open --members sp-a:"), "{}", held.out());
    assert!(!held.out().contains("joins the next round"), "{}", held.out());

    let waiting = rework_world(&["wait"]);
    set_row(&waiting, "sp-a", |r| r.state = "READY".into());
    assert_eq!(waiting.run(&a.iter().map(String::as_str).collect::<Vec<_>>()), 0, "{}", waiting.err());
    assert!(waiting.out().contains("not gated until the hold lifts") && waiting.out().contains("spira-lc unhold sp-a <kind>"), "{}", waiting.out());
    assert!(!waiting.out().contains("its gate certifies it"), "{}", waiting.out());

    let mixed = rework_world(&["manual", "wait"]);
    assert_eq!(mixed.run(&a.iter().map(String::as_str).collect::<Vec<_>>()), 0, "{}", mixed.err());
    assert!(mixed.out().contains("not gated until the hold lifts") && !mixed.out().contains("its gate certifies it"), "{}", mixed.out());

    let ready = rework_world(&[]);
    set_row(&ready, "sp-a", |r| r.state = "READY".into());
    assert_eq!(ready.run(&a.iter().map(String::as_str).collect::<Vec<_>>()), 0, "{}", ready.err());
    assert!(ready.out().contains("the batcher cuts it as a provisional member"), "{}", ready.out());
}

#[test_id(tkmtfre)]
#[test]
fn resubmit_moves_the_local_branch_to_the_new_tip() {
    let t = rework_world(&["manual"]);
    t.git.set(
        "refs/heads/spira/sp-a",
        "9999999999999999999999999999999999999999",
    );
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/spira/sp-a").as_deref(), Some(T_FIX));
}

#[test_id(t94vxn5)]
#[test]
fn resubmit_leaves_a_branch_it_already_moved_alone_when_branch_f_would_refuse() {
    let t = rework_world(&["manual"]);
    t.git.branch_set_fails.set(true);
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/spira/sp-a").as_deref(), Some(T_FIX));
}

#[test_id(tqnsyta)]
#[test]
fn resubmit_fetches_an_absent_tip_and_refuses_when_the_fetch_brings_nothing() {
    let t = rework_world(&["manual"]);
    let absent = "6".repeat(40);
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &format!("--tip={absent}"),
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &format!("--evidence-tip={absent}")
        ]),
        1
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("fetch ") && c.ends_with("spira/sp-a")),
        "{:?}",
        t.git.calls.borrow()
    );
    assert_eq!(
        t.git.get("refs/heads/spira/sp-a").as_deref(),
        Some(T_FIX),
        "the branch is not moved"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tlsumec)]
#[test]
fn an_unclaimed_ready_bead_is_resubmitted_and_its_branch_set_to_the_tip() {
    let t = rework_world(&[]);
    t.git.set("refs/heads/spira/sp-a", "stale");
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "READY".into();
    }
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built P0",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!(
            "retip sp-a {T_FIX} concierge [hand-built P0] [npm test 1/0] {T_FIX}"
        )),
        "{:?}",
        t.lc.calls.borrow()
    );
    let state =
        t.lc.rows
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.bead_id == "sp-a")
            .map(|r| (r.state.clone(), r.tip.clone()))
            .unwrap();
    assert_eq!(state, ("SUBMITTED".to_string(), Some(T_FIX.to_string())));
    assert_eq!(
        t.git
            .rev_parse(Path::new("/"), "refs/heads/spira/sp-a")
            .as_deref(),
        Some(T_FIX)
    );
}

#[test_id(tp7hiyb)]
#[test]
fn a_bead_a_live_dwarf_holds_is_not_resubmitted_and_the_refusal_names_the_holder() {
    let t = rework_world(&[]);
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "WORKING".into();
        r.holder = Some("dwarf-7".into());
    }
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built P0",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("dwarf-7"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tqm78tl)]
#[test]
fn a_hand_mended_tip_that_fails_its_lint_is_not_retipped() {
    let t = rework_world(&["manual"]);
    t.scripts
        .lint_red
        .borrow_mut()
        .insert(T_FIX.into(), "src/lib.rs:3:1".into());
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("src/lib.rs:3:1"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tn41ch4)]
#[test]
fn a_bead_held_by_no_one_is_not_retipped() {
    let t = rework_world(&[]);
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("held by no one"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tmbmp3i)]
#[test]
fn resubmit_refuses_a_stranger_a_short_tip_an_unknown_commit_and_a_non_rework_row() {
    let t = rework_world(&["manual"]);
    let ok = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &ok,
            "--by=dwarf-x",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            "--tip=abc",
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &format!("--tip={}", "5".repeat(40)),
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-b",
            &ok,
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&["resubmit", "sp-a", &ok, "--by=concierge"]),
        2,
        "a reason is required"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tlbwcrv)]
#[test]
fn a_resubmit_without_evidence_or_with_an_evidence_tip_that_is_not_the_tip_is_refused() {
    let t = rework_world(&["manual"]);
    let tip_arg = format!("--tip={T_FIX}");
    let base = [
        "resubmit",
        "sp-a",
        tip_arg.as_str(),
        "--by=concierge",
        "--reason=hand-built fix",
    ];
    assert_eq!(t.run(&base), 2);
    assert!(t.err().contains("--evidence is required"), "{}", t.err());
    let other = format!("--evidence-tip={}", "7".repeat(40));
    assert_eq!(
        t.run(&[&base[..], &["--evidence=npm test 1/0", other.as_str()]].concat()),
        1
    );
    assert!(
        t.err().contains("--evidence-tip must equal --tip"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[&base[..], &["--evidence=npm test 1/0"]].concat()),
        1,
        "no evidence tip at all"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

fn concurrent_world() -> T {
    let mut t = round_world();
    t.lib.s.rounds_max = 2;
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    t
}

#[test_id(t08zk06)]
#[test]
fn a_second_round_with_disjoint_members_opens_beside_the_first() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert!(kv_of(&t, "round")["batch_id"] == "r-1" && kv_of(&t, "round.r-2")["batch_id"] == "r-2");
    assert_ne!(
        kv_of(&t, "round")["worktree"],
        kv_of(&t, "round.r-2")["worktree"],
        "each round has its own worktree"
    );
    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(
        status.contains("rounds=2") && status.contains("open_batches=r-1 r-2"),
        "{status}"
    );
    assert!(
        status.contains("batch_id=r-1")
            && status.contains("round2_batch_id=r-2")
            && status.contains("round2_members=sp-b:tb"),
        "{status}"
    );
}

#[test_id(tf70kf2)]
#[test]
fn open_names_how_long_each_step_took_on_the_log_line_and_stdout() {
    let t = round_world();
    let ms = t.clock.extra_ms.clone();
    *t.git.on_merge.borrow_mut() = Some(Box::new(move |tip| {
        if tip == "ta" {
            ms.set(ms.get() + 5_000);
        }
    }));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let steps = t
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("steps=").map(str::to_string))
        .unwrap_or_else(|| panic!("no steps= line: {}", t.out()));
    assert_eq!(
        steps,
        "reconcile:0,lock:0,admit:0,worktree:0,merge.sp-a:5000,merge.sp-b:0,collide:0,record:0"
    );
    assert!(
        t.landing_log()
            .lines()
            .any(|l| l.contains("QUEUE ROUND-OPEN ") && l.ends_with(&format!(" steps={steps}"))),
        "{}",
        t.landing_log()
    );
}

#[test_id(twfxouw)]
#[test]
fn a_refused_open_prints_the_steps_it_reached_on_stderr() {
    let t = concurrent_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "open", "--members", "sp-b,sp-a", "--name", "r-2"]), 1);
    assert!(t.err().lines().any(|l| l == "steps=reconcile:0,lock:0"), "{}", t.err());
}

#[test_id(tzivcr2)]
#[test]
fn a_second_round_naming_a_member_of_an_open_round_is_refused_naming_it() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b,sp-a", "--name", "r-2"]),
        1
    );
    assert!(
        t.err()
            .contains("sp-a is already a member of open round r-1"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round.r-2").exists());
}

#[test_id(tpp3ufr)]
#[test]
fn a_duplicate_identical_member_pair_opens_one_member() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-a:ta,sp-b:tb",
            "--name",
            "r-1"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["members"], "sp-a:ta sp-b:tb");
}

#[test_id(t7wafq3)]
#[test]
fn an_abbreviated_and_a_full_tip_of_one_id_are_one_member_at_the_longer() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:t,sp-b:tb,sp-a:ta,sp-a",
            "--name",
            "r-1"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["members"], "sp-a:ta sp-b:tb");
}

#[test_id(tbu4ssk)]
#[test]
fn one_id_at_two_tips_is_refused_naming_both() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb,sp-a:tz",
            "--name",
            "r-1"
        ]),
        2,
        "{}",
        t.err()
    );
    let e = t.err();
    assert!(
        e.contains("sp-a") && e.contains("ta") && e.contains("tz"),
        "{e}"
    );
    assert!(!t.qfile("round.r-1").exists());
}

#[test_id(to9mhjc)]
#[test]
fn the_open_past_the_limit_is_refused() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c", "--name", "r-3"]),
        1
    );
    assert!(
        t.err()
            .contains("2 rounds are already open for spira (limit 2)"),
        "{}",
        t.err()
    );
}

#[test_id(tvb0v6c)]
#[test]
fn a_round_is_addressed_by_its_batch_id_and_the_oldest_record_follows_the_landed() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "abandon", "r-2", "--reason", "x"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round.r-2").exists() && kv_of(&t, "round")["batch_id"] == "r-1");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-3"]),
        0,
        "{}",
        t.err()
    );
    t.git.ancestor("ta", "merged-ta");
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    assert_eq!(
        kv_of(&t, "round")["batch_id"],
        "r-3",
        "the surviving round takes over the round file"
    );
    assert!(!t.qfile("round.r-3").exists());
}

fn tree_of(t: &T, head: &str) {
    t.git.ancestor(head, head);
    t.git.trees.borrow_mut().insert(head.into(), T_ROUND.into());
}

fn open_and_certify(t: &T, member: &str, name: &str) {
    assert_eq!(
        t.run(&["round", "open", "--members", member, "--name", name]),
        0,
        "{}",
        t.err()
    );
}

fn two_green_stacked_rounds(t: &T) {
    open_and_certify(t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    t.git.heads_name_their_base.set(true);
    open_and_certify(t, "sp-b", "r-2");
    tree_of(t, "merged-tb@merged-ta");
    assert_eq!(t.run(&["round", "certify", "r-2"]), 0, "{}", t.err());
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("tb", "merged-tb@merged-ta");
}

#[test_id(th5ngh5)]
#[test]
fn a_later_round_is_cut_on_the_earlier_rounds_head_and_lands_after_it_with_one_certify() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    let second = kv_of(&t, "round.r-2");
    assert_eq!(
        (second["base"].as_str(), second["parent"].as_str()),
        ("merged-ta", "r-1"),
        "the second round is stacked on the first round's head"
    );
    let runs = vm_runs(&t);
    assert_eq!(t.run(&["round", "land", "r-2"]), crate::ops::LAND_STACKED);
    assert!(
        t.err()
            .contains("stacked on round r-1, which has not landed"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("b0"),
        "the stacked round lands only behind its parent"
    );
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", "r-2"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb@merged-ta"));
    assert_eq!(
        vm_runs(&t),
        runs,
        "landing the second round re-certified nothing"
    );
    assert!(!t.landing_log().contains("QUEUE ROUND-REMERGE"));
}

#[test_id(t5wxyd5)]
#[test]
fn status_keeps_reporting_the_other_open_round_after_one_lands() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    let both = t.out()[mark..].to_string();
    assert!(
        both.contains("open_batches=r-1 r-2") && both.contains("round2_batch_id=r-2"),
        "{both}"
    );
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    let after = t.out()[mark..].to_string();
    assert!(
        !after.contains("round=none")
            && after.contains("rounds=1")
            && after.contains("batch_id=r-2"),
        "{after}"
    );
}

#[test_id(t0yj8py)]
#[test]
fn open_does_not_count_a_round_the_lifecycle_abandoned_while_status_hides_it() {
    let t = concurrent_world();
    open_and_certify(&t, "sp-a", "r-1");
    t.lc.batch_states
        .borrow_mut()
        .insert("r-1".into(), ("ABANDONED".into(), "9".into()));
    open_and_certify(&t, "sp-a", "r-2");
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    let status = t.out()[mark..].to_string();
    assert!(
        status.contains("rounds=1") && status.contains("batch_id=r-2"),
        "{status}"
    );
}

fn stacked_pair(t: &T) {
    open_and_certify(t, "sp-a", "r-1");
    open_and_certify(t, "sp-b", "r-2");
    assert_eq!(
        kv_of(t, "round.r-2")["parent"],
        "r-1",
        "r-2 is stacked on r-1"
    );
}

fn voided(t: &T, child: &str) {
    assert!(
        t.lc.has(&format!("abandon-batch {child} ")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.qfile(&format!("round.{child}")).exists() && !t.qfile("round").exists(),
        "no round record survives"
    );
    assert!(
        t.out().contains(&format!(
            "voiding round {child}: it is stacked on round r-1"
        )),
        "{}",
        t.out()
    );
}

#[test_id(tc5yyqj)]
#[test]
fn open_refuses_to_stack_on_an_open_round_that_went_red() {
    let t = concurrent_world();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-a.sh".into(), "red 1 2 fp p e 1".into())];
    open_and_certify(&t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 1);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        1
    );
    assert!(
        t.err().contains("open round r-1 is red (test-a.sh)") && t.err().contains("refused"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round.r-2").exists());
    assert!(!t.lc.has("cut r-2"), "{:?}", t.lc.calls.borrow());
}

#[test_id(t2zo1qo)]
#[test]
fn open_refuses_to_stack_on_a_live_round_whose_own_parent_went_red() {
    let mut t = concurrent_world();
    t.lib.s.rounds_max = 3;
    stacked_pair(&t);
    let mut kv = crate::records::Kv::default();
    for (k, v) in kv_of(&t, "round") {
        kv.push(&k, &v);
    }
    kv.push("red", "test-a.sh");
    crate::records::write_atomic(&t.qfile("round"), &kv.render()).unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c", "--name", "r-3"]),
        1
    );
    assert!(
        t.err()
            .contains("open round r-1, under the newest open round r-2, is red"),
        "{}",
        t.err()
    );
    assert!(!t.lc.has("cut r-3"), "{:?}", t.lc.calls.borrow());
}

#[test_id(t2ih21u)]
#[test]
fn open_prints_the_base_it_stacked_on_and_its_chain() {
    let t = concurrent_world();
    stacked_pair(&t);
    let out = t.out();
    assert!(
        out.contains("base_of=local/main")
            && out.contains("base=merged-ta")
            && out.contains("base_of=round r-1")
            && out.contains("base_chain=merged-ta"),
        "{out}"
    );
}

#[test_id(t3pdtld)]
#[test]
fn abandoning_a_round_voids_the_round_stacked_on_it() {
    let t = concurrent_world();
    stacked_pair(&t);
    assert_eq!(
        t.run(&["round", "abandon", "r-1", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    voided(&t, "r-2");
}

#[test_id(tacd43o)]
#[test]
fn ejecting_a_rounds_last_member_voids_the_round_stacked_on_it() {
    let t = concurrent_world();
    stacked_pair(&t);
    assert_eq!(
        t.run(&["round", "eject", "r-1", "sp-a", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    voided(&t, "r-2");
}

#[test_id(tmkjtiq)]
#[test]
fn a_stacked_round_whose_parent_was_abandoned_is_rebuilt_without_it_and_certified_again() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    // The parent ended outside the queue's verbs, which would have voided the child; its record
    // and lock are left behind, and the child's settle removes them.
    drop(crate::lock::try_certify_lock(
        &t.s().queue_dir,
        "spira",
        "r-1",
    ));
    t.lc.batch_states
        .borrow_mut()
        .insert("r-1".into(), ("ABANDONED".into(), "9".into()));
    tree_of(&t, "merged-tb");
    let runs = vm_runs(&t);
    assert_eq!(
        t.run(&["round", "land", "r-2"]),
        crate::ops::LAND_BASE_MOVED
    );
    assert!(t.err().contains("it is not landed"), "{}", t.err());
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("b0"),
        "nothing landed on the parent's tree"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            rec["batch_id"].as_str(),
            rec["base"].as_str(),
            rec["head"].as_str()
        ),
        ("r-2", "b0", "merged-tb")
    );
    assert!(!rec.contains_key("parent"));
    assert_eq!(round_phase(&t), "REBUILDING");
    assert!(
        !certify_lock(&t, "r-1").exists(),
        "the settled parent leaves no certify lock"
    );
    t.git.ancestor("tb", "merged-tb");
    assert_eq!(t.run(&["round", "certify", "r-2"]), 0, "{}", t.err());
    assert_eq!(vm_runs(&t), runs + 1);
    assert_eq!(t.run(&["round", "land", "r-2"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test_id(tjkipwg)]
#[test]
fn a_stacked_round_is_recut_on_the_head_its_parent_was_rebuilt_to() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    let mut parent = kv_of(&t, "round");
    parent.insert("head".into(), "rebuilt-ta".into());
    let mut kv = crate::records::Kv::default();
    for (k, v) in &parent {
        kv.push(k, v);
    }
    crate::records::write_atomic(&t.qfile("round"), &kv.render()).unwrap();
    tree_of(&t, "merged-tb@rebuilt-ta");
    assert_eq!(
        t.run(&["round", "land", "r-2"]),
        crate::ops::LAND_BASE_MOVED
    );
    let rec = kv_of(&t, "round.r-2");
    assert_eq!(
        (
            rec["base"].as_str(),
            rec["head"].as_str(),
            rec["parent"].as_str()
        ),
        ("rebuilt-ta", "merged-tb@rebuilt-ta", "r-1")
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test_id(t5us46z)]
#[test]
fn a_member_that_conflicts_in_meaning_with_an_earlier_round_is_certified_on_that_rounds_tree() {
    let t = concurrent_world();
    open_and_certify(&t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    t.git.heads_name_their_base.set(true);
    open_and_certify(&t, "sp-b", "r-2");
    assert_eq!(
        t.git.cut_on.borrow().as_str(),
        "merged-ta",
        "the second member is merged into the first round's tree, where a duplicate definition fails the build"
    );
    t.git.ancestor("merged-tb@merged-ta", "merged-tb@merged-ta");
    t.git.trees.borrow_mut().insert(
        "merged-tb@merged-ta".into(),
        "4444444444444444444444444444444444444444".into(),
    );
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    t.git.ancestor("ta", "merged-ta");
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    *t.lc.batch_view.borrow_mut() = None;
    assert_eq!(t.run(&["round", "certify", "r-2"]), 1, "{}", t.out());
    assert_eq!(t.run(&["round", "land", "r-2"]), 1);
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-ta"),
        "the conflicting round never lands"
    );
}

#[test_id(t2qk9mb)]
#[test]
fn a_member_whose_tip_is_a_recorded_ejected_red_tip_is_not_offered_to_the_cut() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r.bead_id == "sp-b")
        .unwrap()
        .ejected_red_tip = Some("tb".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        1,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("sp-b: tb is a recorded ejected-red tip"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round").exists());
}

#[test_id(t9de2nw)]
#[test]
fn a_member_spira_lc_refuses_at_the_cut_refuses_the_whole_open_naming_it() {
    let t = round_world();
    t.lc.cut_refuses_member.set(Some("sp-b"));
    t.lc.no_batch_row.set(true);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        1,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("spira-lc cut refused") && t.err().contains("sp-b"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round").exists());
}

#[test_id(tjdac90)]
#[test]
fn rework_returns_an_unbatched_submitted_bead_and_refuses_a_round_member() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .retain(|r| r.bead_id != "sp-c");
    t.lc_row("sp-c", "SUBMITTED", "tc", 100);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );

    assert_eq!(
        t.run(&["rework", "sp-a", "--by=concierge", "--reason=red"]),
        1
    );
    assert!(
        t.err().contains("member of the open round spira-"),
        "the batch is named: {}",
        t.err()
    );
    assert!(!t.lc.has("\"GateRed\""), "{:?}", t.lc.calls.borrow());

    assert_eq!(
        t.run(&["rework", "sp-c", "--by=dwarf-x", "--reason=red"]),
        1
    );
    assert_eq!(
        t.run(&["rework", "sp-c", "--by=concierge"]),
        2,
        "a reason is required"
    );
    assert!(!t.lc.has("\"GateRed\""), "{:?}", t.lc.calls.borrow());

    assert_eq!(
        t.run(&[
            "rework",
            "sp-c",
            "--by=concierge",
            "--reason=red on test-install-dwarves"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("event bead sp-c "), "{:?}", t.lc.calls.borrow());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.contains("\"tip\":\"tc\"")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(t.lc.bead_state("sp-c").unwrap().0, "REWORK");
}

#[test_id(tst11cp)]
#[test]
fn a_successful_rework_records_a_send_back_fact_and_a_refused_one_none() {
    let t = round_world();
    t.lc.rows.borrow_mut().as_mut().unwrap().retain(|r| r.bead_id != "sp-c");
    t.lc_row("sp-c", "SUBMITTED", "tc", 100);
    let reason = "not ok 2 - \"a spike\"\nwanted [1] got [0]";
    assert_eq!(t.run(&["rework", "sp-c", "--by=dwarf-x", &format!("--reason={reason}")]), 1);
    assert!(!t.lc.has("fact sp-c send-back"), "{:?}", t.lc.calls.borrow());
    assert_eq!(t.run(&["rework", "sp-c", "--by=concierge", &format!("--reason={reason}")]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow();
    let facts: Vec<&String> = calls.iter().filter(|c| c.starts_with("fact sp-c send-back concierge [")).collect();
    assert_eq!(facts.len(), 1, "{calls:?}");
    let cause = facts[0].strip_prefix("fact sp-c send-back concierge [").unwrap().strip_suffix(']').unwrap();
    assert_eq!(spira_config::send_back::parse(cause), ("tc".to_string(), reason.to_string()));
}

fn proof_run(t: &T) -> std::path::PathBuf {
    t.s().run.clone()
}

#[test_id(tigo8yv)]
#[test]
fn a_resubmit_is_refused_naming_an_ejected_suite_that_does_not_pass_at_the_new_tip_and_accepted_when_it_does(
) {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["test-x.sh".to_string()])
        .unwrap();
    t.scripts.suite_red.borrow_mut().push("test-x.sh".into());
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=npm test 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 1);
    assert!(
        t.err().contains("test-x.sh failed at") && t.err().contains("refused"),
        "{}",
        t.err()
    );
    assert!(
        !t.lc.calls.borrow().iter().any(|c| c.starts_with("retip")),
        "a refused resubmit must not move the bead"
    );

    t.scripts.suite_red.borrow_mut().clear();
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(spira_config::proof::proven(
        &proof_run(&t),
        "sp-a",
        T_FIX,
        "test-x.sh"
    ));
}

#[test_id(tcldcj2)]
#[test]
fn a_resubmit_check_that_did_not_run_is_reported_not_verified_never_as_still_failing() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["test-x.sh".to_string()])
        .unwrap();
    t.scripts
        .suite_rc
        .borrow_mut()
        .insert("test-x.sh".into(), 2);
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=npm test 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 1);
    assert!(t.err().contains("test-x.sh not verified at"), "{}", t.err());
    assert!(!t.err().contains("still do not pass"), "{}", t.err());
    t.scripts
        .suite_rc
        .borrow_mut()
        .insert("test-x.sh".into(), 5);
    assert_eq!(t.run(&args), 1);
    assert!(!t.err().contains("still do not pass"), "{}", t.err());
    t.scripts
        .suite_rc
        .borrow_mut()
        .insert("test-x.sh".into(), 1);
    assert_eq!(t.run(&args), 1);
    assert!(
        !t.err().contains("still do not pass"),
        "a rc=1 FAULT is not a RED: {}",
        t.err()
    );
    let runs: Vec<String> = t
        .scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("suite_run test-x.sh"))
        .cloned()
        .collect();
    assert!(runs[0].contains("first=true "), "{runs:?}");
    assert!(
        runs[1..].iter().all(|c| c.contains("first=false ")),
        "{runs:?}"
    );
}

#[test_id(taqeode)]
#[test]
fn a_resubmit_runs_each_ejected_suite_as_a_first_check_of_the_new_tip_and_admits_it_when_it_passes()
{
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["test-x.sh".to_string()])
        .unwrap();
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=npm test 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with(&format!("suite_run test-x.sh {T_FIX} spira first=true deny="))));
}

#[test_id(tbxtpl7)]
#[test]
fn an_eject_red_on_unit_tests_records_the_step_as_the_requirement() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "unit-tests"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lib.has("bead_reopen sp-b eject-red unit-tests"),
        "{:?} {:?}",
        t.lib.calls.borrow(),
        t.lc.calls.borrow()
    );
}

#[test_id(t8e83k1)]
#[test]
fn a_bead_ejected_on_a_round_only_step_resubmits_with_a_note_not_a_refusal() {
    for step in ["spira-lint", "workspace-build", "fence-forge-batch-readers", "bare-suites", "image"] {
        let t = rework_world(&["manual"]);
        spira_config::proof::record_failing(&proof_run(&t), "sp-a", &[step.to_string()]).unwrap();
        let tip_arg = format!("--tip={T_FIX}");
        let args = [
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=cargo build 1/0",
            &ev_tip(),
        ];
        assert_eq!(t.run(&args), 0, "{step}: {}", t.err());
        assert!(
            t.out().contains(&format!(
                "queue.sh resubmit: {step} passes only in the round that carries {T_FIX}"
            )),
            "{step}: {}",
            t.out()
        );
        assert!(
            !t.scripts
                .calls
                .borrow()
                .iter()
                .any(|c| c.starts_with(&format!("suite_run {step}"))),
            "a round-only step is never run as a suite"
        );
    }
}

#[test_id(t1rvabg)]
#[test]
fn a_round_only_step_does_not_excuse_a_host_runnable_check_that_is_red() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(
        &proof_run(&t),
        "sp-a",
        &["spira-lint".to_string(), "unit-tests".to_string()],
    )
    .unwrap();
    t.scripts.suite_red.borrow_mut().push("unit-tests".into());
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=cargo test 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 1);
    assert!(t.err().contains("unit-tests failed at"), "{}", t.err());
}

fn resubmit_args_for(tip_arg: &str) -> Vec<String> {
    [
        "resubmit",
        "sp-a",
        tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=cargo build 1/0",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(ev_tip()))
    .collect()
}

#[test_id(t4xxziy)]
#[test]
fn a_bead_ejected_on_spira_lint_is_still_refused_when_the_host_lint_is_red_at_the_tip() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["spira-lint".to_string()])
        .unwrap();
    t.scripts
        .lint_red
        .borrow_mut()
        .insert(T_FIX.into(), "src/x.rs:1:1".into());
    let args = resubmit_args_for(&format!("--tip={T_FIX}"));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_eq!(t.run(&args), 1);
    assert!(t.err().contains("src/x.rs:1:1"), "{}", t.err());
}

#[test_id(t6iha66)]
#[test]
fn a_round_only_note_is_not_printed_for_a_resubmit_that_is_refused_later() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["spira-lint".to_string()])
        .unwrap();
    let args = resubmit_args_for(&format!("--tip={T_FIX}"));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .retain(|r| r.bead_id != "sp-a");
    t.lc_row("sp-a", "WORKING", "ta", 100);
    assert_eq!(t.run(&args), 1);
    assert!(t.err().contains("held by a live dwarf"), "{}", t.err());
    assert!(!t.out().contains("passes only in the round"), "{}", t.out());
}

#[test_id(tr01yfo)]
#[test]
fn a_bead_ejected_on_unit_tests_resubmits_after_a_green_runner_run_and_not_after_a_red_one() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["unit-tests".to_string()])
        .unwrap();
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=cargo test 1/0",
        &ev_tip(),
    ];
    t.scripts
        .suite_red
        .borrow_mut()
        .push("unit-tests".to_string());
    assert_eq!(t.run(&args), 1);
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("suite_run unit-tests")));
    t.scripts.suite_red.borrow_mut().clear();
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(!t.err().contains("--clear"), "{}", t.err());
}

#[test_id(tu1tkw0)]
#[test]
fn a_hand_eject_for_red_must_name_the_failing_suite() {
    let t = rework_world(&[]);
    assert_eq!(t.run(&["eject", "sp-a", "--red", "--reason", "red"]), 2);
    assert!(t.err().contains("--suites"), "{}", t.err());
}

#[test_id(t1umztf)]
#[test]
fn settle_landed_moves_a_certified_bead_whose_tip_is_on_the_landing_ref_to_landed() {
    let t = round_world();
    t.git.ancestor("ta", "b0");
    assert_eq!(
        t.run(&["settle-landed", "sp-a", "--by=concierge", "--evidence-tip=ta"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().join("\n");
    assert!(calls.contains("event bead sp-a CERTIFIED"), "{calls}");
    assert!(calls.contains("ContentOnBase"), "{calls}");
}

#[test_id(tue70he)]
#[test]
fn settle_landed_refuses_a_tip_off_the_landing_ref_naming_both_shas() {
    let t = round_world();
    assert_eq!(
        t.run(&["settle-landed", "sp-b", "--by=concierge", "--evidence-tip=tb"]),
        1
    );
    assert!(t.err().contains("tb") && t.err().contains("b0"), "{}", t.err());
    assert!(!t.lc.has("event bead sp-b"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tju26oz)]
#[test]
fn settle_landed_refuses_an_evidence_tip_that_is_not_the_recorded_tip() {
    let t = round_world();
    t.git.ancestor("ta", "b0");
    assert_eq!(
        t.run(&["settle-landed", "sp-a", "--by=concierge", "--evidence-tip=zz"]),
        1
    );
    assert!(!t.lc.has("event bead sp-a"), "{:?}", t.lc.calls.borrow());
}

#[test_id(td3e7og)]
#[test]
fn absolve_refuses_without_evidence_and_with_it_readmits_a_red_tip() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r.bead_id == "sp-b")
        .unwrap()
        .ejected_red_tip = Some("tb".into());
    fs::write(t.qfile("red-tips"), "sp-b tb\n").unwrap();
    assert_eq!(
        t.run(&["absolve", "sp-b", "--by=concierge", "--reason=infra red"]),
        2,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=infra red",
            "--evidence=sccache startup timeout",
            "--evidence-tip=other"
        ]),
        1,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=dwarf",
            "--reason=infra red",
            "--evidence=e",
            "--evidence-tip=tb"
        ]),
        1,
        "{}",
        t.err()
    );
    assert!(!t.lc.has("absolve"), "{:?}", t.lc.calls.borrow());
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert!(
        format!("{}{}", t.out(), t.err()).contains("sp-b: tb is a recorded ejected-red tip"),
        "positive control: {}{}",
        t.out(),
        t.err()
    );

    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=infra red",
            "--evidence=sccache startup timeout, no suite ran",
            "--evidence-tip=tb"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(
            "absolve sp-b tb concierge [infra red] [sccache startup timeout, no suite ran] tb"
        ),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(fs::read_to_string(t.qfile("red-tips")).unwrap().is_empty());
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=r",
            "--evidence=e",
            "--evidence-tip=tb"
        ]),
        1,
        "nothing left to absolve: {}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("members=sp-b:tb"), "{}", t.out());
}

#[test_id(tmzvnpc)]
#[test]
fn absolve_clears_a_red_only_the_gate_recorded() {
    let t = round_world();
    let red = t.s().run.join("verdicts/red");
    fs::create_dir_all(&red).unwrap();
    let rec = red.join("k1");
    fs::write(&rec, gate::redcache::render(1, "branch-red", "s", "w", 1, "c", "tb", "m\n")).unwrap();
    assert_eq!(
        t.run(&["absolve", "sp-b", "--by=concierge", "--reason=r", "--evidence=e", "--evidence-tip=tb"]),
        0,
        "{}",
        t.err()
    );
    assert!(!rec.exists());
}

const OWN_IMAGE: &str = "docker.io/library/rust:1.99.1-bookworm";

fn declare_own_testenv(t: &T) {
    let wt = PathBuf::from(&kv_of(t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(
        wt.join(".spira").join("testenv.toml"),
        format!("ci_entry = \"deploy/ci.sh all\"\nimage = \"{OWN_IMAGE}\"\n"),
    )
    .unwrap();
}

#[test_id(tsqf6ah)]
#[test]
fn a_repo_that_declares_its_own_testenv_certifies_and_lands_from_its_verdict() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: "round-vm must not run".into(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert!(
        calls.iter().any(|c| {
            c.starts_with("repo-round-vm ")
                && c.contains(&format!("entry=deploy/ci.sh all image={OWN_IMAGE} workdir=/work round="))
                && c.ends_with("/spira wall=900")
        }),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("round-vm")),
        "{calls:?}"
    );
    assert!(certified_tree_exists(&t));
    assert_eq!(round_phase(&t), "GREEN");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

fn prune_world(own_testenv: bool) -> (T, PathBuf) {
    let t = round_world();
    let store = t.lc.store.get_or_init(crate::batches::mem::MemDoor::default);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    if own_testenv {
        let kv = crate::batches::batch(store, "spira", &batch).unwrap().unwrap().kv;
        let wt = PathBuf::from(kv.get("worktree").unwrap());
        fs::create_dir_all(wt.join(".spira")).unwrap();
        fs::write(wt.join(".spira").join("testenv.toml"), format!("ci_entry = \"deploy/ci.sh all\"\nimage = \"{OWN_IMAGE}\"\n")).unwrap();
    }
    store.put_round("spira", "spira-open", "OPEN", &[("sp-c", "tc")], 10);
    let rounds = t.s().run.join("rounds");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400);
    for n in ["spira-old.results", "spira-open.results", "cubby-old.results"] {
        fs::create_dir_all(rounds.join(n)).unwrap();
        fs::File::open(rounds.join(n)).unwrap().set_modified(old).unwrap();
    }
    t.run(&["round", "certify", &batch]);
    (t, rounds)
}

#[test_id(tv8263f)]
#[test]
fn a_round_pass_prunes_the_repos_old_closed_round_results_and_keeps_open_ones() {
    let (t, rounds) = prune_world(false);
    assert!(!rounds.join("spira-old.results").exists(), "{}", t.err());
    assert!(rounds.join("spira-open.results").exists());
    assert!(rounds.join("cubby-old.results").exists());
}

#[test_id(t11zjk0)]
#[test]
fn an_own_testenv_round_pass_prunes_the_repos_old_closed_round_results_and_keeps_open_ones() {
    let (t, rounds) = prune_world(true);
    assert!(t.scripts.calls.borrow().iter().any(|c| c.starts_with("repo-round-vm ")), "{}", t.err());
    assert!(!rounds.join("spira-old.results").exists(), "{}", t.err());
    assert!(rounds.join("spira-open.results").exists());
    assert!(rounds.join("cubby-old.results").exists());
}

#[test_id(tnjdq6r)]
#[test]
fn a_worktree_moved_by_stabilization_certifies_only_when_the_batcher_names_its_head() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    t.git.set("HEAD", "stabilized");
    t.git
        .trees
        .borrow_mut()
        .insert("stabilized".into(), T_ROUND.into());
    t.git.ancestor("b0", "stabilized");

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        t.err().contains("is not at the round's head"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "certify", &batch, "--rehead", "merged-tb"]),
        1
    );
    assert!(
        t.err()
            .contains("--rehead merged-tb is not the round worktree's head"),
        "{}",
        t.err()
    );
    assert!(!certified_tree_exists(&t));

    assert_eq!(
        t.run(&["round", "certify", &batch, "--rehead", "stabilized"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["head"], "stabilized");
    assert_eq!(round_phase(&t), "GREEN");
}

#[test_id(t7l9nn5)]
#[test]
fn a_repo_that_declares_its_own_testenv_goes_red_and_does_not_land_on_a_failing_suite() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 1,
        out: "VERDICT RED ran=4 red=1".into(),
        err: String::new(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(t.out().contains("red=testenv"), "{}", t.out());
    assert!(
        t.err().contains("is RED at"),
        "a red exit says why on stderr: {}",
        t.err()
    );
    assert!(
        t.out()
            .lines()
            .any(|l| l.starts_with("red-output=") && l.ends_with("testenv.out")),
        "{}",
        t.out()
    );
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(t.run(&["round", "land", &batch]), 1);
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing landed");
}

#[test_id(tyu9vlu)]
#[test]
fn a_testenv_fault_on_an_own_testenv_repo_is_not_judged() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 2,
        out: "VERDICT FAULT rc=2".into(),
        err: "tier lint: podman: command not found".into(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(
        t.err().contains("CI entry exited 2")
            && t.err().contains("not judged")
            && t.err().contains("tier lint: podman: command not found"),
        "{}",
        t.err()
    );
}

#[test_id(tsztdwb)]
#[test]
fn a_single_ci_entry_pass_whose_ci_result_is_ok_is_green_whatever_the_run_exit_after_it() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: "round-vm run: pass record could not be written".into(),
    };
    *t.scripts.repo_ci_status.borrow_mut() = Some("ok".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(round_phase(&t), "GREEN");
}

#[test_id(td4xhxo)]
#[test]
fn a_red_ci_result_does_not_turn_a_faulted_run_into_a_judged_round() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: "round-vm run: fault".into(),
    };
    *t.scripts.repo_ci_status.borrow_mut() = Some("red".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
}

#[test_id(tkv36sp)]
#[test]
fn an_ok_ci_result_does_not_turn_a_failing_run_exit_into_a_green_round() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 3,
        out: String::new(),
        err: "round-vm run: fault".into(),
    };
    *t.scripts.repo_ci_status.borrow_mut() = Some("ok".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
}

#[test_id(t6trl4z)]
#[test]
fn an_own_testenv_repo_with_no_ci_entry_is_not_judged_and_runs_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(wt.join(".spira").join("testenv.toml"), "").unwrap();
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(t.err().contains("declares no ci_entry"), "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert!(!calls.iter().any(|c| c.contains("round-vm")), "{calls:?}");
}

#[test_id(ti1q20z)]
#[test]
fn an_own_testenv_repo_with_a_ci_entry_and_no_image_is_not_judged_and_runs_nothing() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(wt.join(".spira").join("testenv.toml"), "ci_entry = \"deploy/ci.sh all\"\n").unwrap();
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(t.err().contains("declares a ci_entry but no image"), "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert!(!calls.iter().any(|c| c.contains("round-vm")), "{calls:?}");
}

const STALE_ACQUIRE: &str = "round-vm run: acquire failed: EC2 launch failed (placement ec2-always)\nclone refused: the newest round AMI ami-1 carries recipe-sha aa but this release's recipe is bb; run the round-ami-build workflow\n";

fn stale_ami() -> RunOut {
    RunOut {
        rc: 2,
        out: String::new(),
        err: STALE_ACQUIRE.into(),
    }
}

fn stale_world() -> (T, String) {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    (t, batch)
}

#[test_id(t9x3kir)]
#[test]
fn a_stale_round_image_waits_for_the_build_in_progress_then_proceeds() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![stale_ami(), stale_ami(), RunOut::default()];
    *t.forge.ami_runs.borrow_mut() = vec![
        Some(vec!["381189175".into()]),
        Some(vec!["381189175".into()]),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.out().contains("waiting for round AMI 381189175"),
        "{}",
        t.out()
    );
    let calls = t.forge.calls.borrow().join("\n");
    assert!(!calls.contains("ami-dispatch"), "{calls}");
    assert_eq!(calls.matches("ami-runs").count(), 2, "{calls}");
}

#[test_id(txzrl3m)]
#[test]
fn a_stale_round_image_with_no_build_running_triggers_exactly_one() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![stale_ami(), stale_ami(), RunOut::default()];
    *t.forge.ami_runs.borrow_mut() = vec![Some(vec![]), Some(vec!["7".into()])];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().join("\n");
    assert_eq!(calls.matches("ami-dispatch").count(), 1, "{calls}");
}

#[test_id(tbhj8ts)]
#[test]
fn a_round_image_that_stays_stale_reports_the_acquire_error_itself() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm.borrow_mut() = stale_ami();

    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let err = t.err();
    assert!(
        err.contains("round AMI is stale")
            && err.contains("clone refused")
            && err.contains("round-ami-build workflow"),
        "{err}"
    );
    assert!(!err.contains("left no verdicts"), "{err}");
}

fn image_pending() -> RunOut {
    RunOut {
        rc: crate::ops::round::IMAGE_PENDING,
        out: String::new(),
        err: "round-vm run: the round image is still baking\n".into(),
    }
}

fn recipe_changing_world() -> (T, String) {
    let mut t = round_world();
    t.lib.s.round_spill = "ec2-always".into();
    *t.scripts.recipe_release.borrow_mut() = Some("release-recipe".into());
    *t.scripts.recipe_tree.borrow_mut() = Some("candidate-recipe-sha".into());
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    (t, batch)
}

#[test_id(tv839r1)]
#[test]
fn a_recipe_changing_round_builds_its_own_image_and_certifies_on_it() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending()];
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Missing, AmiLookup::Found("ami-cand".into())];
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Running("41".into())),
        Some(AmiBuild::Success("41".into())),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let branch = "spira-round-candidate/candidate-recipe-sha".to_string();
    let forge = t.forge.calls.borrow().join("\n");
    assert_eq!(
        forge.matches(&format!("ami-dispatch {branch}")).count(),
        1,
        "{forge}"
    );
    assert!(
        t.out()
            .contains("waiting for the candidate round image build 41"),
        "{}",
        t.out()
    );
    let git = t.lib.calls.borrow().join("\n");
    assert!(git.contains(&format!(":refs/heads/{branch}")), "{git}");
    let vm = t.scripts.calls.borrow().join("\n");
    assert!(vm.contains("ami=ami-cand"), "{vm}");
}

#[test_id(tbsyu49)]
#[test]
fn a_round_reheaded_after_an_eject_builds_a_new_image_instead_of_reusing_the_failed_one() {
    let mut t = round_world();
    t.lib.s.round_spill = "ec2-always".into();
    *t.scripts.recipe_release.borrow_mut() = Some("release-recipe".into());
    *t.scripts.recipe_tree.borrow_mut() = Some("candidate-recipe-sha".into());
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending(), image_pending()];
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Failure(
            "41".into(),
            vec!["build / Build, image and tag".into()],
        )),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "harness"]),
        0,
        "{}",
        t.err()
    );

    *t.scripts.recipe_tree.borrow_mut() = Some("survivors-recipe-sha".into());
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Success("42".into())),
    ];
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Missing, AmiLookup::Found("ami-new".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());

    let dispatched: std::collections::BTreeSet<String> = t
        .forge
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("ami-dispatch "))
        .cloned()
        .collect();
    assert_eq!(
        dispatched.len(),
        2,
        "a new head gets its own image build: {dispatched:?}"
    );
    let read: Vec<String> = t
        .forge
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("ami-result "))
        .cloned()
        .collect();
    let second = dispatched
        .iter()
        .last()
        .unwrap()
        .replace("ami-dispatch ", "ami-result ");
    assert!(read.contains(&second), "{read:?}");
    assert!(second.contains("survivors-re"), "{second}");
    assert!(t.scripts.calls.borrow().join("\n").contains("ami=ami-new"));
}

#[test_id(t04kg1n)]
#[test]
fn a_recipe_changing_round_placed_on_the_host_starts_its_pass_without_the_image() {
    let (t, batch) = recipe_changing_world();
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Running("41".into())),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let vm: Vec<String> = t
        .scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("round-vm "))
        .cloned()
        .collect();
    assert_eq!(vm.len(), 1, "{vm:?}");
    assert!(vm[0].contains("candidate-recipe=candidate-recipe-sha"), "{vm:?}");
    assert!(!vm[0].contains("ami="), "{vm:?}");
    let forge = t.forge.calls.borrow().join("\n");
    assert_eq!(forge.matches("ami-dispatch").count(), 1, "{forge}");
    assert!(
        !t.out().contains("waiting for the candidate round image build"),
        "{}",
        t.out()
    );
}

#[test_id(tuki3yw)]
#[test]
fn a_recipe_changing_round_placed_on_ec2_waits_for_its_image_then_runs_on_it() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending()];
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Missing, AmiLookup::Found("ami-cand".into())];
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Running("41".into())),
        Some(AmiBuild::Success("41".into())),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let vm: Vec<String> = t
        .scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("round-vm "))
        .cloned()
        .collect();
    assert_eq!(vm.len(), 2, "{vm:?}");
    assert!(!vm[0].contains("ami="), "{vm:?}");
    assert!(vm[1].contains("ami=ami-cand"), "{vm:?}");
    assert!(
        t.out()
            .contains("waiting for the candidate round image build 41"),
        "{}",
        t.out()
    );
}

#[test_id(t3s65fn)]
#[test]
fn two_rounds_of_one_recipe_share_one_image_build() {
    let (t, first) = recipe_changing_world();
    t.forge.ami_branches_dispatched_only.set(true);
    *t.forge.ami_results.borrow_mut() = vec![Some(AmiBuild::Running("41".into()))];

    assert_eq!(t.run(&["round", "certify", &first]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "abandon", &first, "--reason", "next round"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b"]),
        0,
        "{}",
        t.err()
    );
    let second = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &second]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().clone();
    assert_eq!(
        forge.iter().filter(|c| c.starts_with("ami-dispatch")).count(),
        1,
        "{forge:?}"
    );
    let read: std::collections::BTreeSet<&String> =
        forge.iter().filter(|c| c.starts_with("ami-result")).collect();
    assert_eq!(
        read.into_iter().collect::<Vec<_>>(),
        vec!["ami-result spira-round-candidate/candidate-recipe-sha"],
        "{forge:?}"
    );
}

#[test_id(t1xc2tq)]
#[test]
fn a_failed_bake_for_another_head_of_the_same_recipe_is_rebuilt_not_read() {
    let (t, batch) = recipe_changing_world();
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::Failure("40".into(), vec!["build / x".into()])),
        Some(AmiBuild::Running("41".into())),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert_eq!(forge.matches("ami-dispatch ").count(), 1, "{forge}");
    let git = t.lib.calls.borrow().join("\n");
    assert_eq!(
        git.matches("+merged-ta:refs/heads/spira-round-candidate/candidate-recipe-sha")
            .count(),
        1,
        "{git}"
    );
    assert!(!t.out().contains("red=workspace-build"), "{}", t.out());
}

#[test_id(tdyvtof)]
#[test]
fn an_ec2_pass_does_not_red_on_the_stale_failure_a_rebuild_has_not_yet_replaced() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending()];
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Missing, AmiLookup::Found("ami-cand".into())];
    let old = || Some(AmiBuild::Failure("40".into(), vec!["build / x".into()]));
    *t.forge.ami_results.borrow_mut() =
        vec![old(), old(), old(), Some(AmiBuild::Success("41".into()))];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert_eq!(forge.matches("ami-dispatch ").count(), 1, "{forge}");
    let vm = t.scripts.calls.borrow().join("\n");
    assert!(vm.contains("ami=ami-cand"), "{vm}");
    assert!(!t.out().contains("red=workspace-build"), "{}", t.out());
}

#[test_id(t7f7a9x)]
#[test]
fn a_recipe_already_imaged_dispatches_no_build() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Found("ami-have".into())];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert!(!forge.contains("ami-dispatch"), "{forge}");
    assert!(t.scripts.calls.borrow().join("\n").contains("ami=ami-have"));
    let git = t.lib.calls.borrow().join("\n");
    assert!(
        git.contains("push origin :refs/heads/spira-round-candidate/candidate-recipe-sha"),
        "{git}"
    );
}

#[test_id(tybk44b)]
#[test]
fn a_recipe_change_whose_image_fails_to_build_reds_the_round_naming_the_step() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending()];
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Failure(
            "41".into(),
            vec!["build / Build, image and tag".into()],
        )),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=workspace-build"), "{}", t.out());
    assert!(
        t.out()
            .contains("build-error=round image build 41 failed at build / Build, image and tag"),
        "{}",
        t.out()
    );
    let vm = t.scripts.calls.borrow().join("\n");
    assert_eq!(vm.matches("round-vm ").count(), 1, "{vm}");
    assert!(!vm.contains("ami="), "{vm}");
}

#[test_id(tks7r16)]
#[test]
fn a_round_whose_recipe_is_the_releases_never_touches_the_image_build() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.recipe_tree.borrow_mut() = Some("release-recipe".into());

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert!(!forge.contains("ami-"), "{forge}");
    assert!(!t.scripts.calls.borrow().join("\n").contains("ami="));
}

#[test_id(txpcjc1)]
#[test]
fn a_candidate_image_build_that_outlasts_its_wait_leaves_the_round_unjudged() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![image_pending()];
    *t.forge.ami_results.borrow_mut() = vec![Some(AmiBuild::Running("41".into()))];

    assert_eq!(
        t.run(&["round", "certify", &batch]),
        crate::ops::round::FAULT,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("did not finish within 5400s"),
        "{}",
        t.err()
    );
}

#[test_id(tg9o94t)]
#[test]
fn open_admits_a_recipe_changing_member_with_the_ordinary_ones() {
    let t = round_world();
    t.git.stat("b0", "tb", &[("round-vm/ami-setup.sh", 1, 1)]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("members=sp-a:ta sp-b:tb sp-c:tc") && !o.contains("solo:"),
        "{o}"
    );
}

const PREFLIGHT_OUT: &str = "round-image-preflight: recipe inputs touched: flake.nix\nround-image-preflight: RED rc=1 after 12s (image debian:12)\nround-image-preflight: failing setup line: + nix profile install .#round-deps\n";

/// A two-member round whose pass ends red on the image preflight fence, `recipe` naming the
/// members whose diff touches a round-recipe input.
fn preflight_red_round(recipe: &[&str]) -> (T, String) {
    let t = round_world();
    for tip in recipe {
        t.git.stat("b0", tip, &[("flake.nix", 1, 1)]);
    }
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("fence-round-image-preflight".into(), "red".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "fence-round-image-preflight.out".into(),
        PREFLIGHT_OUT.into(),
    )];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(
        t.out().contains("red=fence-round-image-preflight"),
        "{}",
        t.out()
    );
    (t, batch)
}

const IMAGE_OUT: &str = "round-vm: IMAGE-ABSENT: localhost/spira-testenv:abc is not on this VM\n";

/// A two-member round whose pass ends red on the image phase, `movers` naming the members whose
/// diff touches an image tag input.
fn image_red_round(movers: &[&str]) -> (T, String) {
    let t = round_world();
    for tip in movers {
        t.git.stat("b0", tip, &[("spira/deps.toml", 1, 1)]);
    }
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("fence-round-image".into(), "red".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![("fence-round-image.out".into(), IMAGE_OUT.into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    (t, batch)
}

#[test_id(tgetr36)]
#[test]
fn a_preflight_red_ejects_exactly_the_recipe_changing_member_with_the_fence_evidence() {
    let (t, _) = preflight_red_round(&["tb"]);
    assert!(
        t.lib.has("bead_reopen sp-b eject-red"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!t.lib.has("bead_reopen sp-a"), "{:?}", t.lib.calls.borrow());
    let comment = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("comment sp-b"))
        .cloned()
        .unwrap_or_default();
    assert!(
        comment.contains("failing setup line: + nix profile install .#round-deps")
            && comment.contains("flake.nix"),
        "{comment}"
    );
    let members = kv_of(&t, "round")
        .get("members")
        .cloned()
        .unwrap_or_default();
    assert!(
        members.contains("sp-a") && !members.contains("sp-b"),
        "{members}"
    );
}

#[test_id(tk09n3z)]
#[test]
fn a_preflight_red_with_no_recipe_changing_member_ejects_nobody() {
    let (t, _) = preflight_red_round(&[]);
    assert!(!t.lib.has("bead_reopen"), "{:?}", t.lib.calls.borrow());
    let members = kv_of(&t, "round")
        .get("members")
        .cloned()
        .unwrap_or_default();
    assert!(
        members.contains("sp-a") && members.contains("sp-b"),
        "{members}"
    );
}

fn touches(t: &T, tip: &str, file: &str) {
    t.git
        .stats
        .borrow_mut()
        .insert(("b0".into(), tip.into()), vec![(file.into(), 3, 1)]);
}

fn integration_bugs(t: &T) -> Vec<String> {
    t.lib
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("create_bug") && c.contains(" integrate "))
        .cloned()
        .collect()
}

#[test_id(tgenmqw)]
#[test]
fn open_files_one_integration_with_both_members_context_instead_of_skipping_one() {
    let t = round_world();
    *t.lib.bug_id.borrow_mut() = Some("sp-int".into());
    touches(&t, "ta", "child.rs");
    touches(&t, "tb", "other.rs");
    touches(&t, "tc", "child.rs");
    t.bd.rows.borrow_mut().push(crate::model::BeadRow {
        id: "sp-c".into(),
        title: Some("goal of c".into()),
        description: Some("ACCEPTANCE-OF-C".into()),
        ..Default::default()
    });
    t.git.merge_fail.borrow_mut().insert("tc".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tc".into(), vec!["child.rs".into()]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        1,
        "{}",
        t.err()
    );
    let bugs = integration_bugs(&t);
    assert_eq!(bugs.len(), 1, "{bugs:?}");
    let b = &bugs[0];
    let labels = b.lines().next().unwrap().split(' ').nth(3).unwrap();
    assert!(
        labels.split(',').any(|l| l == "plan-fixture")
            && labels.split(',').any(|l| l == "integrate")
            && !labels.split(',').any(|l| l == "mender-failure"),
        "{labels}"
    );
    assert!(
        b.contains("integrate sp-a + sp-c in spira")
            && b.contains("spira: land sp-a")
            && b.contains("spira: land sp-c")
            && b.contains("Tip: ta")
            && b.contains("Tip: tc")
            && b.contains("ACCEPTANCE-OF-C")
            && b.contains("paths: child.rs")
            && !b.contains("spira: land sp-b"),
        "{b}"
    );
    assert!(
        t.lc.has("create-bead sp-int --repo spira"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(!t.qfile("round").exists(), "a refused open cuts nothing");

    let t2 = round_world();
    t2.git.merge_fail.borrow_mut().insert("tc".into());
    t2.git.rebase_fail.borrow_mut().insert("tc".into(), vec![]);
    assert_eq!(
        t2.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        1,
        "{}",
        t2.err()
    );
    assert!(
        integration_bugs(&t2).is_empty(),
        "no readable overlap files nothing"
    );
}

#[test_id(ttf695y)]
#[test]
fn filing_an_integration_marks_each_member_record_carried_by_it() {
    let t = round_world();
    *t.lib.bug_id.borrow_mut() = Some("sp-int".into());
    touches(&t, "ta", "child.rs");
    touches(&t, "tb", "other.rs");
    touches(&t, "tc", "child.rs");
    t.git.merge_fail.borrow_mut().insert("tc".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tc".into(), vec!["child.rs".into()]);
    t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]);
    assert!(
        t.lc.has("set-carrier sp-a sp-c --by sp-int"),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(t9dym7z)]
#[test]
fn a_carried_member_is_not_admitted_and_a_second_integration_is_not_filed_while_one_is_open() {
    let t = round_world();
    *t.lib.bug_id.borrow_mut() = Some("sp-int".into());
    t.lc_row("sp-int", "READY", "ti", 100);
    fs::create_dir_all(t.s().queue_dir.join("spira")).unwrap();
    fs::write(t.qfile("integrations"), "sp-int sp-a sp-c\n").unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        1,
        "{}",
        t.err()
    );
    let o = t.err();
    assert!(
        o.contains("sp-a: carried by integration sp-int")
            && o.contains("sp-c: carried by integration sp-int"),
        "{o}"
    );
    assert!(
        integration_bugs(&t).is_empty(),
        "{:?}",
        integration_bugs(&t)
    );

    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .for_each(|r| {
            if r.bead_id == "sp-int" {
                r.state = "DROPPED".into();
            }
        });
    let t3 = round_world();
    fs::create_dir_all(t3.s().queue_dir.join("spira")).unwrap();
    fs::write(t3.qfile("integrations"), "sp-gone sp-a\n").unwrap();
    t3.lc_row("sp-gone", "DROPPED", "tg", 100);
    assert_eq!(
        t3.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t3.err()
    );
    assert!(
        t3.out().contains("members=sp-a:ta sp-b:tb"),
        "a dropped integration releases its members: {}",
        t3.out()
    );
}

#[test_id(tdedjs9)]
#[test]
fn a_red_that_only_the_combination_produces_files_the_integration_from_the_eject() {
    let t = round_world();
    touches(&t, "ta", "x.rs");
    touches(&t, "tb", "x.rs");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-x.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "sp-b renames what sp-a uses",
            "--red",
            "--suites",
            "test-x.sh",
            "--with",
            "sp-a"
        ]),
        0,
        "{}",
        t.err()
    );
    let bugs = integration_bugs(&t);
    assert_eq!(bugs.len(), 1, "{bugs:?}");
    assert!(
        bugs[0].contains("integrate sp-b + sp-a in spira")
            && bugs[0].contains("sp-b renames what sp-a uses")
            && bugs[0].contains("spira: land sp-a")
            && bugs[0].contains("spira: land sp-b"),
        "{}",
        bugs[0]
    );
    assert!(t.out().contains("members=sp-c:tc"), "{}", t.out());

    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0);
    let batch = batch_of(&t);
    seed_suite(&t, "test-x.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "r",
            "--red",
            "--suites",
            "test-x.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        integration_bugs(&t).is_empty(),
        "a single-member red keeps the eject-to-rework path"
    );
}

#[test_id(tho5d9o)]
#[test]
fn a_mender_calls_carries_a_failure_spec_its_verbs_can_read() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("test-z.sh.out"), "assertion Z failed\n").unwrap();
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );

    let calls = t.lib.calls.borrow();
    let bug = calls
        .iter()
        .find(|c| c.starts_with("create_bug"))
        .expect("a mender bead");
    let path = bug
        .lines()
        .find_map(|l| l.strip_prefix("spec: "))
        .unwrap_or_else(|| panic!("the call names no spec: {bug}"));
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(spec["batch"], batch.as_str());
    assert_eq!(spec["pass"], 1);
    assert_eq!(spec["suite"], "test-z.sh");
    assert_eq!(
        spec["output"],
        format!(
            "{}\nassertion Z failed",
            spira_config::sendback::NO_FAILURE_FOUND
        )
    );
    assert!(
        spec["repo"].as_str().is_some_and(|r| !r.is_empty()),
        "{spec}"
    );
    let members: Vec<(&str, &str)> = spec["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["bead"].as_str().unwrap(), m["tip"].as_str().unwrap()))
        .collect();
    assert_eq!(members, [("sp-a", "ta"), ("sp-b", "tb")]);
}

#[test_id(tcpfo4g)]
#[test]
fn a_failure_excerpt_names_the_failing_test_past_forty_crates_of_doc_tests() {
    use spira_config::sendback::failure_excerpt;
    let mut o = String::from("     Running unittests src/lib.rs (target/debug/deps/gate-abc)\n\nrunning 2 tests\ntest a ... ok\ntest b ... FAILED\n\nfailures:\n\n---- b stdout ----\nthread 'b' panicked at gate/src/lib.rs:42:9:\nassertion failed: x\n\nfailures:\n    b\n\ntest result: FAILED. 1 passed; 1 failed\n\n");
    for i in 0..40 {
        o.push_str(&format!(
            "   Doc-tests crate{i}\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed\n\n"
        ));
    }
    o.push_str("error: 1 target failed:\n    `-p gate --lib`\n");
    let x = failure_excerpt(&o);
    assert!(
        x.contains("---- b stdout ----")
            && x.contains("gate/src/lib.rs:42:9")
            && x.contains("test result: FAILED"),
        "{x}"
    );
    assert!(x.contains("error: 1 target failed"), "{x}");
    assert!(!x.contains("crate7"), "{x}");
}

#[test_id(t15xvwj)]
#[test]
fn a_failure_excerpt_of_a_shell_suite_keeps_its_fail_lines() {
    use spira_config::sendback::failure_excerpt;
    let x = failure_excerpt("ok one\nFAIL: two\n  expected 1\n  got 2\nok three\nok four\n");
    assert!(
        x.starts_with("FAIL: two") && x.contains("got 2") && !x.contains("ok four"),
        "{x}"
    );
}

#[test_id(tw2pinq)]
#[test]
fn a_failure_excerpt_of_nextest_output_keeps_each_failing_tests_panic_block_once() {
    use spira_config::sendback::failure_excerpt;
    let fail = "FAIL spira-lc::bin/spira-lc control::store::rounds::tests::list_batches_reads_only_the_rounds_it_shows";
    let text = format!(
        "PASS spira-lc::bin/spira-lc a::b\n{fail}\n    thread '...shows' (3) panicked at spira-lc/src/control/store/rounds/tests.rs:1163:9:\n    61 reads: [(\"range_newest_opened\", \"batches/\", ...), ...]\nPASS spira-lc::bin/spira-lc c::d\n------------\n     Summary [   1.0s] 2 tests run: 1 passed, 1 failed\n{fail}\nerror: test run failed\nerror: test run failed\n"
    );
    let x = failure_excerpt(&text);
    assert!(x.contains("panicked at spira-lc/src/control/store/rounds/tests.rs:1163:9") && x.contains("61 reads:"), "{x}");
    assert_eq!(x.matches(fail).count(), 1, "{x}");
    assert!(!x.contains("PASS"), "{x}");
}

#[test_id(t23370q)]
#[test]
fn a_failure_excerpt_keeps_the_same_test_name_failing_in_two_crates() {
    use spira_config::sendback::failure_excerpt;
    let x = failure_excerpt(
        "        FAIL [   0.1s] (1/2) a t::x\n            panicked at a.rs:1:1:\n        FAIL [   0.1s] (2/2) b t::x\n            panicked at b.rs:2:2:\nerror: test run failed\n",
    );
    assert!(x.contains("panicked at a.rs:1:1") && x.contains("panicked at b.rs:2:2"), "{x}");
}

#[test_id(tc420l6)]
#[test]
fn a_failure_excerpt_keeps_not_ok_lines_beside_fail_blocks() {
    use spira_config::sendback::failure_excerpt;
    let x = failure_excerpt("FAIL x\n  why x\nnot ok 3 - y\n");
    assert!(x.contains("why x") && x.contains("not ok 3 - y"), "{x}");
}

#[test_id(thx2ava)]
#[test]
fn a_recorded_mend_moves_the_next_pass_head_to_the_mended_tip() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    t.lc.mend_tips
        .borrow_mut()
        .insert("sp-a".into(), "ta2".into());
    t.git.ancestor("b0", "ta2");

    t.run(&["round", "certify", &batch]);
    let kv = kv_of(&t, "round");
    assert!(
        kv["members"].contains("sp-a:ta2") && !kv["members"].contains("sp-a:ta:"),
        "{}",
        kv["members"]
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("merge ta2")),
        "the head is rebuilt from the mended tip: {:?}",
        t.git.calls.borrow()
    );
}

fn forget_pass_ledger(t: &T) {
    for e in fs::read_dir(t.s().run.join("rounds")).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("pass-ledger-") {
            fs::remove_file(e.path()).unwrap();
        }
    }
}

const INSTALL_FAULT: &str = "FIXTURE-FAULT: install rc=7: install failed — harness fault\ninstall rc=7 output tail:\nunits-install: no such unit\n";

fn fault_next_pass(t: &T) {
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 3,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_files.borrow_mut() =
        vec![("fixture-fault.out".into(), INSTALL_FAULT.into())];
}

#[test_id(t5mkdg8)]
#[test]
fn a_member_in_two_consecutive_faulted_passes_is_ejected_with_the_fault_text() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    fault_next_pass(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        !t.lib.has("bead_reopen sp-a"),
        "one faulted pass still leaves the member in"
    );

    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.lib.has("bead_reopen sp-a eject-red"),
        "{:?}",
        t.lib.calls.borrow()
    );
    let comment = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("comment sp-a"))
        .cloned()
        .unwrap_or_default();
    assert!(
        comment.contains("units-install: no such unit") && comment.contains("2 consecutive passes"),
        "{comment}"
    );
    assert!(kv_of(&t, "round")
        .get("members")
        .is_none_or(|m| !m.contains("sp-a")));
}

const SETTINGS_FAULT: &str = "ROUND-FAULT: FAULT settings-refused phase=suites rc=2: batch: SPIRA_NEW_KEY is not declared — every key is required; refusing
settings-key: SPIRA_NEW_KEY
--- the last 40 lines of the VM's script log ---
round-vm: PHASE suites
batch: SPIRA_NEW_KEY is not declared — every key is required; refusing
VERDICT FAULT rc=2 ran=0 reason=settings-refused
";

fn holds_key(t: &T, rev: &str, text: &str) {
    t.git.greps.borrow_mut().insert(
        format!("{rev}:SPIRA_NEW_KEY"),
        vec!["spira/conf.d/new.toml".into()],
    );
    t.git
        .files
        .borrow_mut()
        .insert(format!("{rev}:spira/conf.d/new.toml"), text.into());
}

fn vm_names_the_fault(t: &T, file: &str) {
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_files.borrow_mut() = vec![("round-fault.out".into(), file.into())];
}

#[test_id(tu8s4ia)]
#[test]
fn a_pass_the_vm_ended_with_no_verdict_is_its_named_fault_with_the_logs_tail() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    vm_names_the_fault(&t, "ROUND-FAULT: FAULT phase=release rc=2: round-vm: cannot apply the head's config delta\n--- the last 40 lines of the VM's script log ---\nround-vm: built the round in 12s\nround-vm: cannot apply the head's config delta\n");
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.out().contains(
            "fault=FAULT phase=release rc=2: round-vm: cannot apply the head's config delta"
        ),
        "{}",
        t.out()
    );
    let err = t.err();
    assert!(err.contains("FAULT phase=release rc=2: round-vm: cannot apply the head's config delta; the round is not judged"), "{err}");
    assert!(
        err.contains("round-vm: built the round in 12s"),
        "the log's tail is shown: {err}"
    );
    assert!(!err.contains("round-vm exited 2"), "{err}");
}

#[test_id(ty1lg8p)]
#[test]
fn a_settings_refusal_ejects_the_member_whose_diff_adds_the_key_and_no_other() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    holds_key(&t, "tb", "SPIRA_NEW_KEY = 1\n");
    holds_key(&t, "local/main", "SPIRA_NEW_KEY_ROOT = 1\n");
    vm_names_the_fault(&t, SETTINGS_FAULT);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(t.out().contains("fault-owner=sp-b"), "{}", t.out());
    assert!(
        t.lib.has("bead_reopen sp-b eject-red"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!t.lib.has("bead_reopen sp-a"), "{:?}", t.lib.calls.borrow());
    let comment = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("comment sp-b"))
        .cloned()
        .unwrap_or_default();
    assert!(
        comment.contains("adds SPIRA_NEW_KEY") && comment.contains("reason=settings-refused"),
        "{comment}"
    );
}

#[test_id(tljpxqr)]
#[test]
fn a_member_stacked_on_the_one_that_adds_the_key_is_not_ejected_for_it() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    holds_key(&t, "ta", "SPIRA_NEW_KEY = 1\n");
    holds_key(&t, "tb", "SPIRA_NEW_KEY = 1\n");
    t.git.ancestor("ta", "tb");
    vm_names_the_fault(&t, SETTINGS_FAULT);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.lib.has("bead_reopen sp-a eject-red"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(!t.lib.has("bead_reopen sp-b"), "{:?}", t.lib.calls.borrow());
}

#[test_id(tgs95r5)]
#[test]
fn a_settings_refusal_no_member_adds_names_the_harness_and_counts_against_none() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    holds_key(&t, "local/main", "SPIRA_NEW_KEY = 1\n");
    holds_key(&t, "ta", "SPIRA_NEW_KEY = 1\n");
    for _ in 0..3 {
        vm_names_the_fault(&t, SETTINGS_FAULT);
        assert_eq!(t.run(&["round", "certify", &batch]), 4);
        assert!(t.out().contains("fault-owner=harness"), "{}", t.out());
        assert!(t.err().contains("the harness is the cause"), "{}", t.err());
    }
    assert!(!t.lib.has("bead_reopen sp-a"), "{:?}", t.lib.calls.borrow());
}

const THROTTLED: &str = "aws ec2: An error occurred (InsufficientInstanceCapacity) when calling the RunInstances operation: throttled due to an operational issue in availability zone usw2-az2\n";

fn launch_refused_next_pass(t: &T) {
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: THROTTLED.into(),
    };
    *t.scripts.round_vm_files.borrow_mut() = vec![];
}

#[test_id(tc7pyow)]
#[test]
fn a_pass_whose_vm_never_launched_ejects_nobody_however_often_it_repeats() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    launch_refused_next_pass(&t);
    for _ in 0..4 {
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    }
    assert!(
        !t.lib.has("bead_reopen"),
        "no suite ran: {:?}",
        t.lib.calls.borrow()
    );
    assert!(kv_of(&t, "round")
        .get("members")
        .is_some_and(|m| m.contains("sp-a")));
}

#[test_id(tjyxhv8)]
#[test]
fn repeated_launch_faults_page_the_infrastructure_cause_not_the_bead() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    launch_refused_next_pass(&t);
    for _ in 0..2 {
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    }
    let paged: Vec<String> = t
        .lib
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("notify") && c.contains("cannot be placed"))
        .cloned()
        .collect();
    assert_eq!(paged.len(), 1, "{:?}", t.lib.calls.borrow());
    assert!(
        paged[0].contains("usw2-az2") && !paged[0].contains("sp-a"),
        "{}",
        paged[0]
    );
}

#[test_id(tahn4tq)]
#[test]
fn a_launch_fault_between_two_member_faults_does_not_reset_or_extend_their_count() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    fault_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    launch_refused_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(!t.lib.has("bead_reopen"), "{:?}", t.lib.calls.borrow());
    fault_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(t.lib.has("bead_reopen sp-a eject-red"));
}

#[test_id(tajsswj)]
#[test]
fn a_green_pass_between_two_faults_makes_the_second_a_first_again() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    fault_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);

    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 0,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_files.borrow_mut() = vec![];
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(!t.lib.has("bead_reopen sp-a"));
}

fn pass_ledger_text(t: &T) -> String {
    fs::read_dir(t.s().run.join("rounds"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("pass-ledger-"))
        .map(|e| fs::read_to_string(e.path()).unwrap_or_default())
        .collect()
}

fn green_by_round_vm_next_pass(t: &T, results: &[(&str, &str)]) {
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 0,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = results
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    *t.scripts.round_vm_files.borrow_mut() =
        vec![("run.events".into(), "1 verdict GREEN -\n".into())];
}

#[test_id(t9jsv4a)]
#[test]
fn a_green_pass_whose_deny_listed_suites_report_a_non_blocking_word_lands_green() {
    for word in ["disabled", "skip-req", "quarantined-red"] {
        let t = round_world();
        assert_eq!(
            t.run(&["round", "open", "--members", "sp-a"]),
            0,
            "{}",
            t.err()
        );
        let batch = batch_of(&t);
        green_by_round_vm_next_pass(&t, &[("test-a.sh", "ok"), ("test-b.sh", word)]);
        assert_eq!(
            t.run(&["round", "certify", &batch]),
            0,
            "{word}: {}",
            t.err()
        );
        assert!(
            events_of(&t, &batch)
                .iter()
                .any(|e| e.starts_with("{\"PassGreen\"")),
            "{word}: {:?}",
            events_of(&t, &batch)
        );
        let ledger = pass_ledger_text(&t);
        assert!(ledger.contains("judged sp-a"), "{word}: {ledger}");
        assert!(!ledger.contains("fault"), "{word}: {ledger}");
    }
}

#[test_id(t7s67ws)]
#[test]
fn a_pass_round_vm_judged_but_the_queue_cannot_read_ejects_nobody_and_tells_the_operator() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    green_by_round_vm_next_pass(&t, &[("test-a.sh", "garbage")]);
    for _ in 0..2 {
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    }
    assert!(
        !pass_ledger_text(&t).contains("fault sp-a"),
        "{}",
        pass_ledger_text(&t)
    );
    assert!(!t.lib.has("bead_reopen sp-a"), "{:?}", t.lib.calls.borrow());
    assert!(
        t.lib
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("notify") && c.contains("could not read it")),
        "{:?}",
        t.lib.calls.borrow()
    );
}

#[test_id(txwmyvx)]
#[test]
fn faulted_passes_with_the_same_members_aboard_blame_none_of_them() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    fault_next_pass(&t);
    for _ in 0..3 {
        assert_eq!(t.run(&["round", "certify", &batch]), 4);
    }
    assert!(!t.lib.has("bead_reopen"), "{:?}", t.lib.calls.borrow());
}

#[test_id(t4rraz8)]
#[test]
fn a_round_vm_lost_mid_pass_exits_75_incomplete_and_never_counts_against_a_member() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 75,
        out: String::new(),
        err: "round-vm run: INCOMPLETE reason=vm-lost\n".into(),
    };
    for _ in 0..3 {
        assert_eq!(t.run(&["round", "certify", &batch]), 75, "{}", t.err());
        assert!(t.err().contains("reason=vm-lost"), "{}", t.err());
    }
    assert!(
        !t.lib.has("bead_reopen sp-a"),
        "a lost VM is not the member's fault: {:?}",
        t.lib.calls.borrow()
    );
    assert!(!t.out().contains("red-output="), "never named a red");
}

fn no_output_next_pass(t: &T) {
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 77, out: String::new(), err: "round-vm run: INCOMPLETE reason=FAULT vm-no-output: VM produced no output within 180s of ssh being up\n".into() };
}

#[test_id(tqum029)]
#[test]
fn a_round_vm_that_produced_no_output_exits_77_named_counts_on_the_round_and_pages_on_the_second() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    no_output_next_pass(&t);
    let paged = |t: &T| {
        t.lib
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("notify") && c.contains(&format!("Round {batch} parked")))
            .count()
    };
    assert_eq!(t.run(&["round", "certify", &batch]), 77, "{}", t.err());
    assert!(
        t.out().contains("fault=vm-no-output") && t.err().contains("VM produced no output"),
        "{} {}",
        t.out(),
        t.err()
    );
    assert!(
        kv_of(&t, "round")["vm-no-output"].ends_with(":1"),
        "{:?}",
        kv_of(&t, "round")
    );
    assert_eq!(paged(&t), 0, "one fault is re-driven, not paged");
    assert_eq!(t.run(&["round", "certify", &batch]), 77, "{}", t.err());
    assert!(
        kv_of(&t, "round")["vm-no-output"].ends_with(":2"),
        "{:?}",
        kv_of(&t, "round")
    );
    assert_eq!(paged(&t), 1, "the second fault parks the round and pages");
    assert!(
        !t.lib.has("bead_reopen"),
        "a VM that never started the round is no member's fault: {:?}",
        t.lib.calls.borrow()
    );
    assert!(!t.out().contains("red-output="), "never named a red");
}

#[test_id(ty21vzi)]
#[test]
fn a_red_round_whose_latest_pass_was_a_no_output_vm_abandons_without_the_override() {
    let t = round_world();
    let batch = red_round(&t, "sp-a:ta,sp-b:tb");
    no_output_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 77, "{}", t.err());
    assert_eq!(
        t.run(&["round", "abandon", &batch, "--reason", "vm"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!("abandon-batch {batch}")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test_id(tle88s2)]
#[test]
fn a_red_pass_after_a_no_output_vm_clears_it_so_the_abandon_guard_holds_again() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    no_output_next_pass(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 77, "{}", t.err());
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(t.run(&["round", "abandon", &batch, "--reason", "red"]), 1);
    assert!(t.err().contains("QUEUE_ABANDON_CONSIDERED"), "{}", t.err());
}

const ENDED: [(&str, &str); 3] = [
    ("r-abandoned", "ABANDONED"),
    ("r-landed", "LANDED"),
    ("r-settled", "SETTLED"),
];

/// Moves the open round `r-open` to `round.r-open` and plants the records of three ended rounds
/// beside it, the abandoned one in `round` where the batcher leaves the oldest.
fn ended_records_beside_the_open_round(t: &T) {
    let open = fs::read_to_string(t.qfile("round")).unwrap();
    assert!(open.contains("batch_id=r-open\n"), "{open}");
    fs::rename(t.qfile("round"), t.qfile("round.r-open")).unwrap();
    for (i, (name, state)) in ENDED.iter().enumerate() {
        let file = if i == 0 {
            "round".to_string()
        } else {
            format!("round.{name}")
        };
        let rec: String = open
            .lines()
            .map(|l| match l.split_once('=') {
                Some(("batch_id", _)) => format!("batch_id={name}\n"),
                Some(("opened", _)) => format!("opened={}\n", i + 1),
                _ => format!("{l}\n"),
            })
            .collect();
        fs::write(t.qfile(&file), rec).unwrap();
        t.lc.batch_states
            .borrow_mut()
            .insert(name.to_string(), (state.to_string(), "9".into()));
    }
}

fn open_beside_ended(t: &T, members: &str) {
    assert_eq!(
        t.run(&["round", "open", "--members", members, "--name", "r-open"]),
        0,
        "{}",
        t.err()
    );
    ended_records_beside_the_open_round(t);
}

fn no_ended_record_left(t: &T) {
    for (name, _) in ENDED {
        assert!(
            !t.qfile(&format!("round.{name}")).exists(),
            "{name}'s record is gone"
        );
    }
    if t.qfile("round").exists() {
        assert_eq!(
            kv_of(t, "round")["batch_id"],
            "r-open",
            "only a live round holds `round`"
        );
    }
}

#[test_id(t7s6zxf)]
#[test]
fn status_lists_only_the_open_round_beside_landed_settled_and_abandoned_records() {
    let t = round_world();
    open_beside_ended(&t, "sp-a:ta");
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    let status = t.out()[mark..].to_string();
    assert!(
        status.contains("rounds=1\n")
            && status.contains("open_batches=r-open\n")
            && status.contains("batch_id=r-open\n"),
        "{status}"
    );
    assert!(
        t.qfile("round.r-landed").exists(),
        "status reads without the lock and removes nothing"
    );
}

#[test_id(t9d37ob)]
#[test]
fn certify_acts_on_the_open_round_and_a_wrong_batch_is_told_only_the_open_one() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    open_beside_ended(&t, "sp-a:ta");
    assert_eq!(t.run(&["round", "certify", "r-gone"]), 1);
    assert!(
        t.err()
            .contains("the open round for spira is r-open, not r-gone"),
        "{}",
        t.err()
    );
    no_ended_record_left(&t);
    ended_records_beside_the_open_round_again(&t);
    assert_eq!(t.run(&["round", "certify", "r-open"]), 0, "{}", t.err());
    assert_eq!(t.lc.batch_state("r-open").unwrap().0, "GREEN");
    no_ended_record_left(&t);
}

/// The ended records again, after a verb swept them, beside `r-open` wherever it now is.
fn ended_records_beside_the_open_round_again(t: &T) {
    if !t.qfile("round").exists() {
        fs::rename(t.qfile("round.r-open"), t.qfile("round")).unwrap();
    }
    ended_records_beside_the_open_round(t);
}

#[test_id(tdqrzll)]
#[test]
fn eject_acts_on_the_open_round_beside_ended_records() {
    let t = round_world();
    open_beside_ended(&t, "sp-a:ta,sp-b:tb");
    assert_eq!(
        t.run(&["round", "eject", "r-open", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    no_ended_record_left(&t);
    assert_eq!(kv_of(&t, "round")["members"], "sp-a:ta");
}

#[test_id(tfgyzqa)]
#[test]
fn land_acts_on_the_open_round_and_promotes_no_ended_record() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    open_beside_ended(&t, "sp-a:ta");
    assert_eq!(t.run(&["round", "certify", "r-open"]), 0, "{}", t.err());
    ended_records_beside_the_open_round_again(&t);
    t.git.ancestor("ta", "merged-ta");
    assert_eq!(t.run(&["round", "land", "r-open"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-ta"));
    no_ended_record_left(&t);
    assert!(!t.qfile("round").exists() && !t.qfile("round.r-open").exists());
}

#[test_id(t2s9yra)]
#[test]
fn abandon_acts_on_the_open_round_and_promotes_no_ended_record() {
    let t = round_world();
    open_beside_ended(&t, "sp-a:ta");
    assert_eq!(
        t.run(&["round", "abandon", "r-open", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    no_ended_record_left(&t);
    assert!(!t.qfile("round").exists() && !t.qfile("round.r-open").exists());
}

#[test_id(tnulst6)]
#[test]
fn abandon_of_a_landed_round_refuses_and_removes_its_record() {
    let t = round_world();
    open_beside_ended(&t, "sp-a:ta");
    assert_eq!(
        t.run(&["round", "abandon", "r-landed", "--reason", "stale"]),
        1
    );
    assert!(
        t.err()
            .contains("the open round for spira is r-open, not r-landed"),
        "{}",
        t.err()
    );
    assert!(
        !t.lc.has("abandon-batch r-landed"),
        "{:?}",
        t.lc.calls.borrow()
    );
    no_ended_record_left(&t);
    assert_eq!(kv_of(&t, "round")["batch_id"], "r-open");
}

#[test_id(tk6c1mt)]
#[test]
fn open_counts_only_the_open_round_toward_the_limit_and_stacks_on_it() {
    let mut t = round_world();
    t.lib.s.rounds_max = 2;
    open_beside_ended(&t, "sp-a:ta");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b:tb", "--name", "r-next"]),
        0,
        "{}",
        t.err()
    );
    no_ended_record_left(&t);
    let next = kv_of(&t, "round.r-next");
    assert_eq!(next["parent"], "r-open", "{next:?}");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c:tc", "--name", "r-third"]),
        1
    );
    assert!(
        t.err()
            .contains("2 rounds are already open for spira (limit 2): r-open"),
        "{}",
        t.err()
    );
}

#[test_id(txve6fs)]
#[test]
fn stage_stages_behind_the_open_round_not_an_ended_record() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    open_beside_ended(&t, "sp-a:ta");
    t.lc.batch_states
        .borrow_mut()
        .insert("r-open".into(), ("GREEN".into(), "5".into()));
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-tb");
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), T_ROUND.into());
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "r-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round-staged")["parent"], "r-open");
    no_ended_record_left(&t);
}

#[test_id(tb13fm1)]
#[test]
fn a_round_the_lifecycle_abandoned_is_closed_out_by_the_next_locked_verb() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta", "--name", "r-gone"]),
        0,
        "{}",
        t.err()
    );
    let wt = kv_of(&t, "round")["worktree"].clone();
    t.lc.batch_states
        .borrow_mut()
        .insert("r-gone".into(), ("ABANDONED".into(), "9".into()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b:tb", "--name", "r-next"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["batch_id"], "r-next");
    assert!(!t.qfile("round.r-gone").exists() && !marker(&t, "r-gone", "running").exists());
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("worktree") && c.contains(&wt)),
        "the ended round's worktree is removed: {:?}",
        t.git.calls.borrow()
    );
    let lines = result_lines(&t, "r-gone");
    assert!(
        lines
            .iter()
            .any(|(_, cause, detail)| cause == "abandoned:swept"
                && detail.starts_with("ended: ABANDONED on the lifecycle")),
        "the sweep closes the record out with a typed cause: {lines:?}"
    );
}

/// Another holder's live lease on `batch` of the fixture's repo.
fn leased_by(t: &T, batch: &str, holder: &str, token: &str) {
    t.lc.leases.borrow_mut().insert(
        format!("spira/{batch}"),
        Ok(Some(RoundLease {
            holder: Some(holder.into()),
            token: token.into(),
            ttl_left_secs: 240,
        })),
    );
}

/// What one call wrote to stderr, without the calls before it.
fn err_of(t: &T, f: impl FnOnce() -> i32) -> (i32, String) {
    let mark = t.io.err.borrow().len();
    let rc = f();
    (rc, t.io.err.borrow()[mark..].to_string())
}

#[test_id(t0sk93s)]
#[test]
fn a_round_leased_by_another_holder_refuses_every_verb_that_touches_it_naming_holder_token_and_ttl()
{
    let (t, batch) = certified_round();
    seed_suite(&t, "test-b.sh");
    leased_by(&t, &batch, "round-recert", "7-0");
    let before = kv_of(&t, "round");
    let runs = vm_runs(&t);
    let verbs: [&[&str]; 4] = [
        &["round", "abandon", &batch, "--reason", "batcher: stale"],
        &[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh",
        ],
        &["round", "land", &batch],
        &["round", "certify", &batch],
    ];
    for argv in verbs {
        let (rc, e) = err_of(&t, || t.run(argv));
        assert_eq!(rc, 1, "{argv:?}: {e}");
        assert!(
            e.contains(&format!(
                "round {batch} is leased by round-recert under token 7-0 with 240s left"
            )) && e.contains("--lease-token 7-0"),
            "{argv:?}: {e}"
        );
    }
    assert_eq!(kv_of(&t, "round"), before, "nothing changed");
    assert_eq!(vm_runs(&t), runs, "no pass ran");
    for write in ["abandon-batch", "eject-member", "land "] {
        assert!(!t.lc.has(write), "{write}: {:?}", t.lc.calls.borrow());
    }
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(t.lc.has(&format!("lock-show round/spira/{batch}")));
}

#[test_id(twp15iz)]
#[test]
fn the_holders_lease_token_lets_each_verb_through() {
    let (t, batch) = certified_round();
    leased_by(&t, &batch, "round-recert", "7-0");
    assert_eq!(
        t.run(&["round", "certify", &batch, "--lease-token", "7-0"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "land", &batch, "--lease-token", "7-0"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));

    let t = round_world();
    let open = [
        "round",
        "open",
        "--members",
        "sp-a:ta,sp-b:tb",
        "--name",
        "r-1",
    ];
    assert_eq!(t.run(&open), 0, "{}", t.err());
    seed_suite(&t, "test-b.sh");
    leased_by(&t, "r-1", "round-recert", "12-0");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            "r-1",
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh",
            "--lease-token",
            "12.0",
        ]),
        0,
        "the store's spelling of the token is the same token: {}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "abandon",
            "r-1",
            "--reason",
            "hand",
            "--lease-token",
            "12-0"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("abandon-batch r-1"));
}

#[test_id(tv2og24)]
#[test]
fn an_unleased_round_is_touched_as_before_and_a_lapsed_token_or_an_unread_lease_refuses() {
    let t = round_world();
    let open = [
        "round",
        "open",
        "--members",
        "sp-a:ta,sp-b:tb",
        "--name",
        "r-1",
    ];
    assert_eq!(t.run(&open), 0, "{}", t.err());
    let (rc, e) = err_of(&t, || {
        t.run(&[
            "round",
            "abandon",
            "r-1",
            "--reason",
            "x",
            "--lease-token",
            "3-0",
        ])
    });
    assert_eq!(rc, 1, "{e}");
    assert!(
        e.contains("holds no lease, so --lease-token 3-0 no longer holds it"),
        "{e}"
    );
    t.lc.leases.borrow_mut().insert(
        "spira/r-1".into(),
        Err("spira-lc lock show round/spira/r-1 exited Some(2): cannot tell".into()),
    );
    let (rc, e) = err_of(&t, || t.run(&["round", "abandon", "r-1", "--reason", "x"]));
    assert_eq!(rc, 1, "{e}");
    assert!(
        e.contains("cannot tell whether round r-1 is leased") && e.contains("exited Some(2)"),
        "{e}"
    );
    assert!(!t.lc.has("abandon-batch"), "{:?}", t.lc.calls.borrow());
    t.lc.leases.borrow_mut().clear();
    assert_eq!(
        t.run(&["round", "abandon", "r-1", "--reason", "x"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("abandon-batch r-1"));
}

/// `queue round land` as the Lander calls it (the argv the landing pass drives): the
/// exit the Lander classifies its refusal by, and stderr's first word, which is for the operator.
fn land_as_the_lander(t: &T, batch: &str, token: Option<&str>) -> (i32, String) {
    let mut argv = vec!["round", "land", batch, "spira"];
    argv.extend(token.map(|t| ["--lease-token", t]).into_iter().flatten());
    let (rc, e) = err_of(t, || t.run(&argv));
    let word = e.split_whitespace().next().unwrap_or("");
    (rc, word.trim_end_matches(':').to_string())
}

#[test_id(tbjb469)]
#[test]
fn each_land_refusal_exits_with_the_code_the_lander_acts_on() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    for b in ["r-1", "r-2"] {
        leased_by(&t, b, "round-recert", "5-0");
    }
    let lander = |token| land_as_the_lander(&t, "r-2", Some(token));
    assert_eq!(
        lander("5-0"),
        (crate::ops::LAND_STACKED, "stacked".into()),
        "{}",
        t.err()
    );
    assert_eq!(
        lander("6-0"),
        (1, "refused".into()),
        "another holder's lease"
    );
    // The parent ends outside the queue's verbs, which would have voided the child with it.
    t.lc.batch_states
        .borrow_mut()
        .insert("r-1".into(), ("ABANDONED".into(), "9".into()));
    tree_of(&t, "merged-tb");
    assert_eq!(
        lander("5-0"),
        (crate::ops::LAND_BASE_MOVED, "base-moved".into()),
        "the parent ended unlanded, so the round was rebuilt on the landing ref: {}",
        t.err()
    );
    assert_eq!(
        lander("5-0"),
        (1, "refused".into()),
        "rebuilt and not certified again, it is not green"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    t.git.ancestor("tb", "merged-tb");
    let certify = ["round", "certify", "r-2", "--lease-token", "5-0"];
    assert_eq!(t.run(&certify), 0, "{}", t.err());
    let (rc, e) = err_of(&t, || {
        t.run(&["round", "land", "r-2", "spira", "--lease-token", "5-0"])
    });
    assert_eq!(rc, 0, "{e}");
    assert!(
        !["stacked", "base-moved", "refused"]
            .iter()
            .any(|w| e.starts_with(w)),
        "{e}"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test_id(tbxdm0c)]
#[test]
fn a_round_cut_before_its_base_moved_refuses_land_as_base_moved() {
    let (t, batch) = certified_round();
    t.git.set("refs/heads/local/main", "b1");
    t.git.ancestor("b0", "b1");
    assert_eq!(
        land_as_the_lander(&t, &batch, None),
        (crate::ops::LAND_BASE_MOVED, "base-moved".into()),
        "{}",
        t.err()
    );
    assert!(t.err().contains("re-merged"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b1"));
}

#[test_id(tp18tq2)]
#[test]
fn land_driven_again_on_a_landed_round_answers_landed_instead_of_no_open_round() {
    let (t, batch) = certified_round();
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    t.git.ancestor("merged-tb", "local/main");
    let landed_calls =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("land ") || c.starts_with("event "))
            .count();
    let (rc, e) = err_of(&t, || t.run(&["round", "land", &batch]));
    assert_eq!(rc, 0, "{e}");
    assert!(!e.contains("no open round"), "{e}");
    assert!(
        t.out()
            .contains(&format!("round {batch} already landed at merged-tb")),
        "{}",
        t.out()
    );
    let after =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("land ") || c.starts_with("event "))
            .count();
    assert_eq!(after, landed_calls, "a repeat land records nothing");
}

#[test_id(tfh9w4m)]
#[test]
fn land_driven_again_refuses_when_the_record_is_not_on_the_landing_ref_or_names_another_batch() {
    let (t, batch) = certified_round();
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    let (rc, e) = err_of(&t, || t.run(&["round", "land", &batch]));
    assert_eq!(rc, 1, "a head off the landing ref is not landed: {e}");
    assert!(e.contains("no open round"), "{e}");
    t.git.ancestor("merged-tb", "local/main");
    let (rc, e) = err_of(&t, || t.run(&["round", "land", "r-other"]));
    assert_eq!(rc, 1, "another batch has no landing record: {e}");
}

#[test_id(tef6qut)]
#[test]
fn a_preflight_red_ejects_every_recipe_changing_member_and_names_them() {
    let (t, _) = preflight_red_round(&["ta", "tb"]);
    assert!(
        t.lib.has("bead_reopen sp-a eject-red") && t.lib.has("bead_reopen sp-b eject-red"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(t.out().contains("ejected=sp-a,sp-b"), "{}", t.out());
}

/// sp-822b8l: a refused open names each member it did not admit as a typed stdout record
/// (`refused=<id> kind=<kind> why=<line>`), so the batcher re-cuts without them by kind rather
/// than abandoning on a members list that differs from the one it named.
#[test_id(t1nc3bj)]
#[test]
fn a_refused_open_prints_one_typed_record_per_member_it_did_not_admit() {
    let t = round_world();
    *t.lib.bug_id.borrow_mut() = Some("sp-int".into());
    t.lc_row("sp-int", "READY", "ti", 100);
    fs::create_dir_all(t.s().queue_dir.join("spira")).unwrap();
    fs::write(t.qfile("integrations"), "sp-int sp-a\n").unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    let o = t.out();
    let records: Vec<&str> = o.lines().filter(|l| l.starts_with("refused=")).collect();
    assert_eq!(records.len(), 1, "{o}");
    assert!(
        records[0]
            .starts_with("refused=sp-a kind=integration why=sp-a: carried by integration sp-int"),
        "{o}"
    );

    let t2 = round_world();
    touches(&t2, "ta", "child.rs");
    touches(&t2, "tc", "child.rs");
    t2.git.merge_fail.borrow_mut().insert("tc".into());
    t2.git
        .rebase_fail
        .borrow_mut()
        .insert("tc".into(), vec!["child.rs".into()]);
    assert_eq!(
        t2.run(&["round", "open", "--members", "sp-a,sp-c"]),
        1,
        "{}",
        t2.err()
    );
    let o = t2.out();
    assert!(
        o.lines().any(|l| l
            .starts_with("refused=sp-c kind=round-conflict why=sp-c: conflicts with the round")
            && l.contains("paths: child.rs")),
        "{o}"
    );
    assert!(
        !o.lines().any(|l| l.starts_with("refused=sp-a")),
        "the admissible member is not refused: {o}"
    );
}

fn image_world(files: &[(&str, &str)]) -> T {
    let t = round_world();
    t.git.ancestor("b0", "ta");
    for (tip, file) in files {
        touches(&t, tip, file);
    }
    *t.scripts.recipe_tree.borrow_mut() = Some("tree-recipe".into());
    t.scripts
        .image_check
        .borrow_mut()
        .insert("ta".into(), ImageCheck::Absent("tagz".into()));
    t
}

#[test_id(t9n3pix)]
#[test]
fn open_skips_by_name_a_member_whose_image_nothing_holds_and_cuts_the_rest() {
    let t = image_world(&[("ta", "spira/deps.toml")]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 1, "{}", t.err());
    let o = t.out();
    let line = o.lines().find(|l| l.starts_with("refused=sp-a kind=image-absent")).unwrap_or_else(|| panic!("{o}"));
    assert!(line.contains("localhost/spira-testenv:tagz") && line.contains(spira_config::recipe::HAND_BUILD), "{line}");
    assert!(!o.lines().any(|l| l.starts_with("refused=sp-b")), "the others are cut: {o}");
    assert!(t.scripts.calls.borrow().iter().any(|c| c == "ami-for tree-recipe"), "the AMI was asked after the host");
}

#[test_id(txa4b5l)]
#[test]
fn open_admits_a_mover_whose_image_a_round_ami_holds() {
    let t = image_world(&[("ta", "spira/deps.toml")]);
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Found("ami-1".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(!t.out().contains("image-absent"));
}

#[test_id(tphsups)]
#[test]
fn open_admits_a_mover_whose_image_the_host_holds_without_asking_for_an_ami() {
    let t = image_world(&[("ta", "spira/deps.toml")]);
    t.scripts.image_check.borrow_mut().insert("ta".into(), ImageCheck::Present);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("ami-for")));
}

#[test_id(t7z9r6d)]
#[test]
fn open_skips_a_recipe_changing_mover_without_hashing_a_tree_that_lacks_its_change() {
    let t = image_world(&[("ta", "spira/testenv/Containerfile")]);
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Found("ami-old".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 1, "{}", t.err());
    assert!(t.out().contains("refused=sp-a kind=image-absent"), "{}", t.out());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("ami-for")));
}

#[test_id(tnhn7bh)]
#[test]
fn open_admits_a_mover_whose_image_check_cannot_be_read_and_says_so() {
    let t = image_world(&[("ta", "spira/deps.toml")]);
    t.scripts.image_check.borrow_mut().insert("ta".into(), ImageCheck::Unreadable("no testenv".into()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(t.err().contains("cannot check sp-a's testenv image (no testenv)"), "{}", t.err());
}

#[test_id(tkyspre)]
#[test]
fn open_checks_no_image_for_a_member_touching_no_tag_input() {
    let t = image_world(&[("ta", "src/other.rs")]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("image-check")), "no check ran");
}

#[test_id(tjlo9p0)]
#[test]
fn open_skips_a_text_only_member_and_says_it_lands_by_lint() {
    let t = round_world();
    t.git
        .stat("local/main", "ta", &[("law/1-general/x.toml", 2, 0)]);
    t.git.ancestor("b0", "ta");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("sp-a: text-only: lands by lint"),
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("refused=sp-a kind=text-only"),
        "{}",
        t.out()
    );
    assert!(!t.err().contains("sp-b:") && !t.qfile("round").exists());
}

#[test_id(tjtmbh4)]
#[test]
fn open_admits_a_text_only_harness_member_the_release_in_force_cannot_lend_to() {
    let mut t = round_world();
    t.lib.s.home_repo = "spira".into();
    t.lib.s.text_only.push("README.md".into());
    t.git.stat("local/main", "ta", &[("README.md", 1, 0)]);
    t.git.ancestor("b0", "ta");
    let rel = t.s().releases.clone().unwrap();
    std::fs::create_dir_all(rel.join("r1/bin")).unwrap();
    testkit::write_exe(rel.join("r1/bin/release"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(rel.join("r1"), rel.join("current")).unwrap();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    assert!(!t.err().contains("text-only"), "{}", t.err());
}

#[test_id(tkv6yp3)]
#[test]
fn open_admits_a_member_that_touches_a_source_file() {
    let t = round_world();
    t.git.stat(
        "local/main",
        "ta",
        &[("law/x.toml", 2, 0), ("queue/src/lib.rs", 1, 0)],
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.err().contains("text-only"), "{}", t.err());
}

#[test_id(t7cgbbs)]
#[test]
fn open_admits_a_text_only_member_that_has_fallen_behind_the_landing_ref() {
    let t = round_world();
    t.git.stat("local/main", "ta", &[("law/x.toml", 2, 0)]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.err().contains("text-only"), "{}", t.err());
}

#[test_id(t12zgp3)]
#[test]
fn ejecting_every_member_on_an_integration_test_red_returns_each_red_and_logs_each() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(
            t.run(&[
                "round",
                "eject",
                &batch,
                id,
                "--reason",
                "integration red",
                "--no-rebuild",
                "--suites",
                "integration-tests"
            ]),
            0,
            "{id}: {}",
            t.err()
        );
    }
    let log = t.landing_log();
    for id in ["sp-a", "sp-b"] {
        assert!(
            log.contains(&format!("batch={batch} by=batcher id={id} cause=eject-red")),
            "{id}: {log}"
        );
        assert!(
            t.lc.calls
                .borrow()
                .iter()
                .any(|c| c.contains(&format!("event bead {id} IN_DELIVERY"))
                    && c.contains("batch-ejected-red")),
            "{id}: {:?}",
            t.lc.calls.borrow()
        );
        assert!(
            t.lib.has(&format!("bead_reopen {id} eject-red unit-tests")),
            "{id}"
        );
    }
    assert!(t.lc.has(&format!("abandon-batch {batch} OPEN")));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        1,
        "{}",
        t.err()
    );
    for (id, tip) in [("sp-a", "ta"), ("sp-b", "tb")] {
        assert!(
            t.err()
                .contains(&format!("{id}: {tip} is a recorded ejected-red tip")),
            "{}",
            t.err()
        );
    }
}

#[test_id(tduk2n9)]
#[test]
fn absolve_deletes_the_gate_workers_stored_verdict_for_the_tip() {
    use landing_pass::gateq::GateQueue;
    let t = round_world();
    t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().find(|r| r.bead_id == "sp-b").unwrap().ejected_red_tip = Some("tb".into());
    fs::write(t.qfile("red-tips"), "sp-b tb\n").unwrap();
    file_gate_verdict(&t, "sp-b", "tb", 1);
    let red = t.s().run.join("verdicts/red");
    fs::create_dir_all(&red).unwrap();
    fs::write(red.join("aaa"), "status=1\nreason=branch-red\ntip=tb\n---\n").unwrap();
    fs::write(red.join("bbb"), "status=1\nreason=branch-red\ntip=other\n---\n").unwrap();
    let q = GateQueue::new(&t.s().run);
    assert!(q.peek_done("spira", "spira/sp-b", "tb").is_some(), "positive control: the stored verdict is there");
    assert_eq!(
        t.run(&["absolve", "sp-b", "--by=concierge", "--reason=infra red", "--evidence=no suite ran", "--evidence-tip=tb"]),
        0,
        "{}",
        t.err()
    );
    assert!(q.peek_done("spira", "spira/sp-b", "tb").is_none(), "absolved: the next gate judges the tip afresh");
    assert!(!red.join("aaa").exists(), "the gate's own red record for the tip is gone");
    assert!(red.join("bbb").exists(), "another tip's record is kept");
}

const T_HAND: &str = "5555555555555555555555555555555555555555";

fn failed_round_with_mender(t: &T, suite: &str, mender: &str) -> String {
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "failed", &batch, "--suite", suite]), 0, "{}", t.err());
    fs::write(marker(t, &batch, &format!("mender-1-{suite}")), format!("{mender}\n")).unwrap();
    let base = kv_of(t, "round")["base"].clone();
    t.git.commits.borrow_mut().insert(T_HAND.into());
    t.git.ancestor("ta", T_HAND);
    t.git.stat("ta", T_HAND, &[("queue/src/x.rs", 1, 1)]);
    t.git.stat(&base, "ta", &[("queue/src/x.rs", 1, 1)]);
    t.git.files.borrow_mut().insert(format!("{base}:spira/mend-fences"), String::new());
    t.git.files.borrow_mut().insert(format!("{T_HAND}:queue/Cargo.toml"), String::new());
    batch
}

#[test_id(tgwysrn)]
#[test]
fn a_return_names_the_member_it_sends_back_and_one_outside_the_round_or_none_is_refused() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    for members in [vec![], vec!["--members=sp-z"], vec!["--members=sp-a,sp-z"]] {
        let mut argv = vec!["round", "return", &batch, "test-z.sh", "--reason=bad at x.rs:9"];
        argv.extend(members);
        assert_ne!(t.run(&argv), 0);
        assert!(t.err().contains("sp-a"), "the refusal names the round's members: {}", t.err());
    }
    assert!(!t.lc.has("mending-event"), "nothing was recorded: {:?}", t.lc.calls.borrow());
    assert_eq!(t.run(&["round", "return", &batch, "test-z.sh", "--members=sp-a", "--reason=bad at x.rs:9"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow();
    let ev = calls.iter().find(|c| c.contains("Return")).unwrap();
    assert!(ev.contains(r#""members":["sp-a"]"#), "{ev}");
}

#[test_id(tgmvao0)]
#[test]
fn a_concierge_hand_mends_a_waiting_failure_with_provenance_and_the_mender_bead_is_retired() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    let tip = format!("--tips=sp-a:{T_HAND}");
    let ev_tip = format!("--evidence-tip={T_HAND}");
    assert_eq!(
        t.run(&[
            "round", "mended", &batch, "test-z.sh", &tip, "--checked=crate:queue",
            "--by=concierge", "--reason=hand-built fix", "--evidence=cargo test -p queue 9/0", &ev_tip,
        ]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow();
    let mend = calls.iter().find(|c| c.starts_with("mend ")).unwrap();
    assert!(mend.ends_with("by=concierge"), "{mend}");
    assert!(
        mend.contains(r#""hand":{"reason":"hand-built fix","evidence":"cargo test -p queue 9/0","evidence_tip":"#),
        "{mend}"
    );
    assert!(t.lib.has("bead_close sp-m1"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, &batch, "mender-1-test-z.sh.retired").exists());
}

#[test_id(t1mwjfv)]
#[test]
fn a_flaky_verdict_records_flaky_and_runs_no_testenv() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    assert_eq!(t.run(&["round", "flaky", &batch, "test-z.sh", "--reason=x"]), 0, "{}", t.err());
    assert!(t.lc.calls.borrow().iter().any(|c| c.contains("Flaky")), "{:?}", t.lc.calls.borrow());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("observe-flake")), "{:?}", t.scripts.calls.borrow());
}

#[test_id(ttempcd)]
#[test]
fn a_by_that_is_not_the_concierge_or_operator_is_refused_before_anything_is_recorded() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    let tip = format!("--tips=sp-a:{T_HAND}");
    assert_eq!(
        t.run(&["round", "mended", &batch, "test-z.sh", &tip, "--checked=crate:queue", "--by=dwarf-1"]),
        2,
        "{}",
        t.err()
    );
    assert!(!t.lc.has("mend "));
    assert!(!t.lib.has("bead_close"));
}

#[test_id(t0rxb72)]
#[test]
fn a_mender_that_mends_its_own_failure_keeps_its_bead() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    let tip = format!("--tips=sp-a:{T_HAND}");
    assert_eq!(t.run(&["round", "mended", &batch, "test-z.sh", &tip, "--checked=crate:queue"]), 0, "{}", t.err());
    assert!(!t.lib.has("bead_close"), "{:?}", t.lib.calls.borrow());
}

#[test_id(tpxp2kx)]
#[test]
fn a_hand_mend_without_its_reason_evidence_or_evidence_tip_is_refused_before_spira_lc_hears_of_it() {
    let t = round_world();
    let batch = failed_round_with_mender(&t, "test-z.sh", "sp-m1");
    let tip = format!("--tips=sp-a:{T_HAND}");
    let ev_tip = format!("--evidence-tip={T_HAND}");
    for extra in [
        vec!["--reason=r", "--evidence=e"],
        vec!["--reason=r", &ev_tip],
        vec!["--evidence=e", &ev_tip],
    ] {
        let mut argv = vec!["round", "mended", &batch, "test-z.sh", &tip, "--checked=crate:queue", "--by=operator"];
        argv.extend(extra);
        assert_eq!(t.run(&argv), 2, "{argv:?}: {}", t.err());
    }
    assert!(!t.lc.has("mend "));
}

#[test_id(ts3afc2)]
#[test]
fn stop_records_owner_reason_and_release_condition_and_status_prints_them() {
    let t = round_world();
    let args = [
        "round", "stop", "--by", "concierge", "--reason", "fixes unlanded", "--until",
        "bead:sp-byw57t=LANDED",
    ];
    assert_eq!(t.run(&args), 0, "{}", t.err());
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    let shown = t.out()[mark..].to_string();
    assert!(
        shown.contains("stop_owner=concierge")
            && shown.contains("stop_reason=fixes unlanded")
            && shown.contains("stop_until=bead:sp-byw57t=LANDED"),
        "{shown}"
    );
}

#[test_id(tz9ppdh)]
#[test]
fn a_stop_stands_after_a_round_lands_until_start_lifts_it() {
    let t = concurrent_world();
    open_and_certify(&t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    t.git.ancestor("ta", "merged-ta");
    let args = ["round", "stop", "--by", "concierge", "--reason", "hold", "--until", "fix landed"];
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    let c = t.run(&["round", "status"]);
    assert_eq!(c, 0, "{}", t.err());
    assert!(t.out().contains("stop=in-force"), "the landing lifted the stop: {}", t.out());
    assert_eq!(
        t.run(&["round", "start", "--by", "concierge", "--reason", "fixes landed"]),
        0,
        "{}",
        t.err()
    );
    let mark = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    assert!(!t.out()[mark..].contains("stop="), "{}", &t.out()[mark..]);
}

#[test_id(t5lies4)]
#[test]
fn stop_without_a_release_condition_is_refused() {
    let t = round_world();
    assert_eq!(t.run(&["round", "stop", "--by", "concierge", "--reason", "x"]), 2);
}

#[test_id(tyi7po0)]
#[test]
fn start_with_no_stop_in_force_says_so_and_changes_nothing() {
    let t = round_world();
    let args = ["round", "start", "--by", "concierge", "--reason", "x"];
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(t.out().contains("no stop of its rounds in force"), "{}", t.out());
}

fn open_two_and_eject_without_rebuild(t: &T) -> String {
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "lint red", "--red", "--suites", "lint", "--no-rebuild"]),
        0,
        "{}",
        t.err()
    );
    batch
}

fn an_ejected_member_still_in_the_worktree(t: &T) -> String {
    let batch = open_two_and_eject_without_rebuild(t);
    declare_own_testenv(t);
    let head = kv_of(t, "round")["head"].clone();
    t.git.ancestor("tb", &head);
    batch
}

#[test_id(t1avj9u)]
#[test]
fn certify_refuses_a_worktree_that_still_holds_an_ejected_member() {
    let t = round_world();
    let batch = an_ejected_member_still_in_the_worktree(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(t.err().contains("sp-b") && t.err().contains("no pass ran"), "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.contains("round-vm")), "{:?}", t.scripts.calls.borrow());
    assert!(!events_of(&t, &batch).iter().any(|e| e.contains("PassStarted")));
}

#[test_id(te12snc)]
#[test]
fn certify_attest_and_rehead_refuse_a_worktree_that_still_holds_an_ejected_member() {
    let t = round_world();
    let batch = an_ejected_member_still_in_the_worktree(&t);
    let head = kv_of(&t, "round")["head"].clone();

    for argv in [
        vec!["round", "certify", &batch, "--rehead", &head],
        vec!["round", "certify", &batch, "--attest", &head, "--ran", "1"],
    ] {
        assert_eq!(t.run(&argv), 1, "{argv:?}");
        assert!(t.err().contains("sp-b") && t.err().contains("no pass ran"), "{argv:?}: {}", t.err());
    }
    assert!(!events_of(&t, &batch).iter().any(|e| e.contains("PassStarted")));
}

#[test_id(tgs1eu9)]
#[test]
fn pass_start_refuses_a_worktree_that_still_holds_an_ejected_member() {
    let t = round_world();
    let batch = an_ejected_member_still_in_the_worktree(&t);

    assert_eq!(t.run(&["round", "pass-start", &batch, "--resume"]), 1);
    assert!(t.err().contains("sp-b") && t.err().contains("no pass ran"), "{}", t.err());
    assert!(!events_of(&t, &batch).iter().any(|e| e.contains("PassStarted")));
}

#[test_id(tqvqfgl)]
#[test]
fn an_eject_that_rebuilds_records_the_rebuilt_tree_as_the_rounds_head() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "lint"]),
        0,
        "{}",
        t.err()
    );
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    assert_eq!(
        Some(kv_of(&t, "round")["head"].clone()),
        t.git.rev_parse(&wt, "HEAD")
    );
    assert_eq!(kv_of(&t, "round")["head"], "merged-ta");
}

#[test_id(tn5qbmp)]
#[test]
fn a_worktree_rebuilt_from_the_surviving_members_after_an_eject_is_adopted_by_certify() {
    let t = round_world();
    let batch = open_two_and_eject_without_rebuild(&t);
    declare_own_testenv(&t);
    t.git.set("HEAD", "merged-ta");
    t.git.ancestor("ta", "merged-ta");
    assert_ne!(kv_of(&t, "round")["head"], "merged-ta");

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.out().contains("adopted"), "{}{}", t.out(), t.err());
    assert_eq!(kv_of(&t, "round")["head"], "merged-ta");
    assert_eq!(round_phase(&t), "GREEN");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-ta"));
}

#[test_id(toz71hx)]
#[test]
fn a_worktree_carrying_a_commit_no_member_owns_is_not_adopted_by_certify() {
    let t = round_world();
    let batch = open_two_and_eject_without_rebuild(&t);
    t.git.set("HEAD", "merged-ta");
    t.git.ancestor("ta", "merged-ta");
    *t.git.log.borrow_mut() = vec![RangeCommit {
        sha: "stray".into(),
        parents: vec!["b0".into()],
        subject: "not a member's".into(),
    }];

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(t.err().contains("is not at the round's head"), "{}", t.err());
    assert_ne!(kv_of(&t, "round")["head"], "merged-ta");
}

#[test_id(tj0ezqp)]
#[test]
fn a_worktree_carrying_a_fold_commit_is_certified_when_rehead_names_its_head() {
    let t = round_world();
    let batch = open_two_and_eject_without_rebuild(&t);
    declare_own_testenv(&t);
    t.git.set("HEAD", "merged-ta");
    t.git.ancestor("ta", "merged-ta");
    *t.git.log.borrow_mut() = vec![RangeCommit {
        sha: "fold".into(),
        parents: vec!["b0".into()],
        subject: "round: fold 1 config delta fragment(s) into spira/config-delta.toml".into(),
    }];

    assert_eq!(t.run(&["round", "certify", &batch, "--rehead", "merged-ta"]), 0, "{}", t.err());
    assert_eq!(kv_of(&t, "round")["head"], "merged-ta");
    assert_eq!(round_phase(&t), "GREEN");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-ta"));
}

#[test_id(tmr445a)]
#[test]
fn resubmit_drops_a_forced_suite_the_landing_ref_deleted_and_records_why() {
    let t = rework_world(&["manual"]);
    let dir = testkit::TempDir::new("deleted-suite");
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join("spira")).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("spira/test-x.sh"), "").unwrap();
    testkit::git(&repo, &["add", "."]);
    testkit::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]);
    testkit::git(&repo, &["rm", "-q", "spira/test-x.sh"]);
    testkit::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "delete x"]);
    {
        let mut r = t.lib.r.borrow_mut();
        r.path = Some(repo.clone());
        r.landref = Some("main".into());
    }
    let run = t.s().run.clone();
    spira_config::proof::record_failing(&run, "sp-a", &["test-x.sh".to_string()]).unwrap();
    fs::create_dir_all(run.join("ejected")).unwrap();
    fs::write(run.join("ejected/sp-a"), "test-x.sh").unwrap();
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&["resubmit", "sp-a", &tip_arg, "--by=concierge", "--reason=r", "--evidence=e", &ev_tip()]),
        0,
        "{}",
        t.err()
    );
    let commit = testkit::git(&repo, &["rev-parse", "main"]);
    assert!(
        t.lc.has(&format!(
            "fact sp-a suite-dropped concierge [suite test-x.sh deleted on main at {}; requirement dropped]",
            commit.trim()
        )),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(spira_config::proof::failing(&run, "sp-a").is_empty() && !run.join("ejected/sp-a").exists());
}

const TEST_A: &str = "test:t0wal00";
const TEST_B: &str = "test:t0other";

fn unit_red_world(ids: &[&str], deny: &str) -> T {
    let t = rework_world(&["manual"]);
    let run = t.s().run.clone();
    spira_config::proof::record_failing(&run, "sp-a", &["unit-tests".to_string()]).unwrap();
    let ids: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    spira_config::proof::record_failing_tests(&run, "sp-a", "unit-tests", &ids).unwrap();
    fs::create_dir_all(run.join("ejected")).unwrap();
    fs::write(run.join("ejected/sp-a"), "unit-tests").unwrap();
    *t.scripts.deny_list.borrow_mut() = Some(Ok(format!("# test-registry deny-list at=1 rows=1\n{deny}")));
    t
}

fn resubmit_unit_red(t: &T) -> i32 {
    let tip_arg = format!("--tip={T_FIX}");
    t.run(&["resubmit", "sp-a", &tip_arg, "--by=concierge", "--reason=r", "--evidence=e", &ev_tip()])
}

#[test_id(txns00y)]
#[test]
fn resubmit_runs_nothing_when_every_test_the_eject_named_is_now_disabled() {
    let t = unit_red_world(&[TEST_A], &format!("disabled\t{TEST_A}\tsp-gm7sti\t9999999999\n"));
    assert_eq!(resubmit_unit_red(&t), 0, "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("suite_run")), "{:?}", t.scripts.calls.borrow());
    let facts: Vec<String> = t.lc.calls.borrow().iter().filter(|c| c.starts_with("fact sp-a check-waived-disabled concierge")).cloned().collect();
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert!(facts[0].contains(TEST_A) && facts[0].contains("sp-gm7sti"), "{facts:?}");
    let run = t.s().run.clone();
    assert!(spira_config::proof::failing(&run, "sp-a").is_empty() && !run.join("ejected/sp-a").exists());
}

#[test_id(t2vrkrr)]
#[test]
fn resubmit_runs_only_the_still_active_tests_when_some_named_tests_are_disabled() {
    let t = unit_red_world(&[TEST_A, TEST_B], &format!("disabled\t{TEST_A}\tsp-gm7sti\t9999999999\n"));
    assert_eq!(resubmit_unit_red(&t), 0, "{}", t.err());
    let runs: Vec<String> = t.scripts.calls.borrow().iter().filter(|c| c.starts_with("suite_run")).cloned().collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].starts_with(&format!("suite_run unit-tests {T_FIX} spira first=true tests=::t0other deny=")), "{runs:?}");
    assert!(t.out().contains(&format!("running unit-tests at {T_FIX} via testenv --tests ::t0other")), "{}", t.out());
}

#[test_id(tn6femf)]
#[test]
fn resubmit_reports_testenvs_over_cap_cause_with_the_verdict() {
    let t = unit_red_world(&[], "");
    t.scripts.suite_out.borrow_mut().insert(
        "unit-tests".into(),
        (2, "over-cap: unit-tests was still running at the 600s cap (x)\nVERDICT FAULT rc=2 ran=0 reason=over-cap".into()),
    );
    assert_eq!(resubmit_unit_red(&t), 1);
    assert!(t.err().contains("over-cap: unit-tests was still running at the 600s cap") && t.err().contains("VERDICT FAULT rc=2"), "{}", t.err());
}

#[test_id(tzcufm5)]
#[test]
fn resubmit_runs_the_whole_check_when_the_eject_recorded_no_test_ids() {
    let t = unit_red_world(&[], "");
    assert_eq!(resubmit_unit_red(&t), 0, "{}", t.err());
    let runs: Vec<String> = t.scripts.calls.borrow().iter().filter(|c| c.starts_with("suite_run")).cloned().collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].starts_with(&format!("suite_run unit-tests {T_FIX} spira first=true deny=")), "{runs:?}");
}

fn deny_path_of(t: &T) -> String {
    let runs: Vec<String> = t.scripts.calls.borrow().iter().filter(|c| c.starts_with("suite_run")).cloned().collect();
    runs[0].split(" deny=").nth(1).unwrap().to_string()
}

#[test_id(tux2f78)]
#[test]
fn resubmit_hands_a_whole_run_the_deny_rows_when_the_eject_recorded_no_test_ids() {
    let row = "disabled\ttest:t0other\tsp-quvpmf\t9999999999\n";
    let t = unit_red_world(&[], row);
    assert_eq!(resubmit_unit_red(&t), 0, "{}", t.err());
    let text = fs::read_to_string(deny_path_of(&t)).unwrap();
    assert!(text.contains(row), "{text}");
}

#[test_id(t8anhrt)]
#[test]
fn resubmit_refuses_and_runs_nothing_when_the_deny_list_cannot_be_read() {
    let t = unit_red_world(&[], "");
    *t.scripts.deny_list.borrow_mut() = Some(Err("registry busy".into()));
    assert_eq!(resubmit_unit_red(&t), 1);
    assert!(t.err().contains("the deny list could not be resolved: registry busy"), "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("suite_run")));
}

#[test_id(ttl5v0c)]
#[test]
fn resubmit_keeps_a_red_runs_whole_output_and_names_its_failing_tests() {
    let t = unit_red_world(&[], "");
    let text = "noise\nunit-tests: 1 failing test(s):\n  a::t\nVERDICT RED ran=1 red=1 first=unit-tests.sh: exit 101";
    t.scripts.suite_out.borrow_mut().insert("unit-tests".into(), (1, text.into()));
    assert_eq!(resubmit_unit_red(&t), 1);
    let err = t.err();
    assert!(err.contains("a::t") && err.contains("output: "), "{err}");
    let path = err.lines().find_map(|l| l.trim().strip_prefix("output: ")).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), text);
}

fn round_records(t: &T) -> Vec<(String, String, String)> {
    let get = |r: &[(String, serde_json::Value)], k: &str| {
        r.iter()
            .find(|(n, _)| n == k)
            .and_then(|(_, v)| v["stringValue"].as_str().map(str::to_string))
            .unwrap_or_default()
    };
    spira_otel::logs::read_all(&spira_otel::dir_of(&t.s().run), "queue")
        .iter()
        .map(|r| (get(r, "kind"), get(r, "by"), get(r, "batch")))
        .collect()
}

fn recorded(t: &T, kind: &str, by: &str, batch: &str) -> bool {
    round_records(t).contains(&(kind.into(), by.into(), batch.into()))
}

#[test_id(t3vcjyg)]
#[test]
fn a_hand_eject_names_the_concierge_in_the_landing_log_and_the_log_record() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "r",
            "--red",
            "--no-rebuild",
            "--suites",
            "integration-tests",
            "--by",
            "concierge"
        ]),
        0,
        "{}",
        t.err()
    );
    let log = t.landing_log();
    assert!(
        log.lines().any(|l| l.starts_with("QUEUE ROUND-EJECT ")
            && l.contains(&format!("batch={batch} by=concierge id=sp-b"))),
        "{log}"
    );
    assert!(
        recorded(&t, "eject", "concierge", &batch),
        "{:?}",
        round_records(&t)
    );
}

#[test_id(txug3of)]
#[test]
fn a_batcher_eject_names_the_batcher_in_both_places() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "r",
            "--red",
            "--no-rebuild",
            "--suites",
            "integration-tests",
            "--by",
            "batcher"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t
        .landing_log()
        .contains(&format!("batch={batch} by=batcher id=sp-b")));
    assert!(recorded(&t, "eject", "batcher", &batch));
    assert!(!recorded(&t, "eject", "concierge", &batch));
}

#[test_id(tt93u65)]
#[test]
fn open_land_and_abandon_record_their_actor_in_both_places() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok 3 1 fp p e 0".into()),
        ("test-b.sh".into(), "skip".into()),
    ];
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--by",
            "batcher"
        ]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "certify", &batch, "--by", "batcher"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "land", &batch, "--by", "batcher"]),
        0,
        "{}",
        t.err()
    );
    let log = t.landing_log();
    for kind in ["OPEN", "LAND"] {
        assert!(
            log.lines()
                .any(|l| l.starts_with(&format!("QUEUE ROUND-{kind} "))
                    && l.contains(&format!("batch={batch} by=batcher "))),
            "{kind}: {log}"
        );
    }
    assert!(recorded(&t, "open", "batcher", &batch) && recorded(&t, "land", "batcher", &batch));

    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "abandon", &batch, "--reason", "why", "--by", "operator"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.landing_log().contains("QUEUE ROUND-ABANDON ")
            && t.landing_log()
                .contains(&format!("batch={batch} by=operator reason=why"))
    );
    assert!(recorded(&t, "abandon", "operator", &batch));
}

#[test_id(teaht9w)]
#[test]
fn a_settings_refusal_inside_certify_records_the_certifying_actor_on_its_eject() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    holds_key(&t, "tb", "SPIRA_NEW_KEY = 1\n");
    holds_key(&t, "local/main", "SPIRA_NEW_KEY_ROOT = 1\n");
    vm_names_the_fault(&t, SETTINGS_FAULT);
    assert_eq!(t.run(&["round", "certify", &batch, "--by", "batcher"]), 4);
    assert!(
        recorded(&t, "eject", "batcher", &batch),
        "{:?}",
        round_records(&t)
    );
}

#[test_id(tzwi94j)]
#[test]
fn a_round_verb_without_a_known_actor_is_usage_and_writes_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let before = t.landing_log();
    for args in [
        vec![
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "r",
            "--red",
            "--no-rebuild",
            "--suites",
            "integration-tests",
        ],
        vec![
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "r",
            "--red",
            "--no-rebuild",
            "--suites",
            "integration-tests",
            "--by",
            "dwarf",
        ],
    ] {
        assert_eq!(t.run_bare(&args), 2);
        assert!(
            t.err()
                .contains("--by batcher|concierge|operator is required"),
            "{}",
            t.err()
        );
        assert_eq!(t.landing_log(), before, "nothing is written");
    }
}

#[test_id(tns2gfa)]
#[test]
fn open_whose_cut_answers_cannot_tell_and_applies_later_keeps_its_worktree_and_one_round() {
    let t = round_world();
    t.lc.store.get_or_init(crate::batches::mem::MemDoor::default);
    t.git.materialise.set(true);
    t.lc.cut_applies_late.set(true);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("cannot tell whether round") && !t.err().contains("cut refused"),
        "{}",
        t.err()
    );
    assert!(!t.lc.has("requeue-orphans"), "{:?}", t.lc.calls.borrow());
    let tree = t
        .git
        .refs
        .borrow()
        .keys()
        .find_map(|k| k.strip_suffix(":HEAD").map(std::path::PathBuf::from))
        .expect("POSITIVE CONTROL: the open created a round worktree");
    assert!(tree.exists(), "the round's worktree {} was removed", tree.display());

    t.lc.apply_late_cut();
    t.lc.cut_applies_late.set(false);
    let before = t.git.calls.borrow().len();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 1);
    assert!(
        t.err().contains("a round is already open") || t.err().contains("already a member"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.git.calls.borrow().len(),
        before,
        "the second open leaves the first round's worktree alone: {:?}",
        t.git.calls.borrow()
    );
    let d = t.lc.store.get().unwrap();
    let held = crate::batches::open_batches(d, "spira").unwrap();
    assert_eq!(held.len(), 1, "one round: {}", t.err());
    let ids: std::collections::BTreeSet<String> = t
        .lc
        .calls
        .borrow()
        .iter()
        .filter_map(|c| c.strip_prefix("cut ").and_then(|r| r.split(' ').next()).map(String::from))
        .collect();
    assert_eq!(ids.len(), 1, "one cut id reaches the store: {ids:?}");
    let wt = std::path::PathBuf::from(held[0].kv.get("worktree").unwrap_or(""));
    assert!(wt.exists(), "the recorded worktree exists");
    assert_eq!(wt, tree, "the round records the tree the first open kept");
}

fn plant_batcher_red_tip(t: &T, id: &str, tip: &str) -> PathBuf {
    let p = spira_config::red_tip_record::path(&t.s().run, "spira", id);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, format!("{tip} filed\n")).unwrap();
    p
}

#[test_id(t8xfbi1)]
#[test]
fn absolve_clears_the_batchers_own_red_tip_record() {
    let t = round_world();
    set_row(&t, "sp-b", |r| r.ejected_red_tip = Some("tb".into()));
    fs::write(t.qfile("red-tips"), "sp-b tb\n").unwrap();
    let rec = plant_batcher_red_tip(&t, "sp-b", "tb");
    let other = plant_batcher_red_tip(&t, "sp-c", "tc");
    assert_eq!(
        t.run(&["absolve", "sp-b", "--by=concierge", "--reason=infra red", "--evidence=e", "--evidence-tip=tb"]),
        0,
        "{}",
        t.err()
    );
    assert!(!rec.exists(), "the absolved tip must not stay withheld from the cut");
    assert!(other.exists(), "another bead's record is untouched");
}

#[test_id(te77mnd)]
#[test]
fn resubmit_at_the_ejected_red_tip_clears_the_batchers_own_red_tip_record() {
    let t = rework_world(&["manual"]);
    set_row(&t, "sp-a", |r| r.ejected_red_tip = Some(T_FIX.into()));
    let rec = plant_batcher_red_tip(&t, "sp-a", T_FIX);
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&["resubmit", "sp-a", &tip_arg, "--by=concierge", "--reason=infra red", "--evidence=clean rerun", &ev_tip()]),
        0,
        "{}",
        t.err()
    );
    assert!(!rec.exists());
}

#[test_id(tm6o8ng)]
#[test]
fn an_integration_red_hands_the_mender_the_integration_output_and_the_failed_test_names() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    fs::create_dir_all(&results).unwrap();
    let test = "the_wal_stays_within_three_checkpoint_intervals_while_reads_never_stop";
    fs::write(results.join("integration-tests.out"), format!("running 1 test\nFAIL {test}\n")).unwrap();
    fs::write(
        results.join("integration.json"),
        format!(r#"{{"stage":"integration-tests","verdict":"red","failed":["{test}"],"cause":"x","secs":3}}"#),
    )
    .unwrap();
    assert_eq!(t.run(&["round", "failed", &batch, "--suite", "unit-tests"]), 0, "{}", t.err());
    let spec = fs::read_dir(t.lib.s.run.join("rounds"))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().contains(".spec-1-unit-tests.json"))
        .expect("a failure spec");
    let spec: serde_json::Value = serde_json::from_str(&fs::read_to_string(spec.path()).unwrap()).unwrap();
    let out = spec["output"].as_str().unwrap();
    assert!(out.contains(&format!("FAIL {test}")) && out.contains(&format!("integration-tests failed: {test}")), "{out}");
}

fn long_running_integration_red(t: &T, batch: &str) -> std::path::PathBuf {
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    fs::create_dir_all(&results).unwrap();
    fs::write(results.join("integration-tests.out"), "INTEGRATION-TESTS-RED: TIMEOUT binary=scratch_gate\nrunning\nFAIL later_poll ran over 60 s, then passed\n").unwrap();
    fs::write(
        results.join("integration.json"),
        r#"{"stage":"integration-tests","verdict":"red","failed":["later_poll"],"long_running":[{"test":"later_poll","over_secs":60,"end":"passed"}],"cause":"x","secs":3}"#,
    )
    .unwrap();
    results
}

#[test_id(tb75qhf)]
#[test]
fn a_round_red_by_a_long_running_test_logs_the_test_how_long_it_ran_and_how_it_ended() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![
        ("integration-tests.out".into(), "INTEGRATION-TESTS-RED: TIMEOUT binary=scratch_gate\nFAIL later_poll ran over 60 s, then passed\n".into()),
        (
            "integration.json".into(),
            r#"{"stage":"integration-tests","verdict":"red","ran_before_suites":false,"failed":["later_poll"],"long_running":[{"test":"later_poll","over_secs":60,"end":"passed"}],"cause":"x","secs":3}"#.into(),
        ),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}{}", t.out(), t.err());
    let log = t.landing_log();
    let red = log.lines().find(|l| l.starts_with("QUEUE ROUND-RED ")).unwrap_or_else(|| panic!("{log}"));
    assert!(red.ends_with("reason=unit-tests: later_poll ran over 60 s, then passed"), "{red}");
    assert!(t.out().contains("unit-tests: later_poll ran over 60 s, then passed"), "{}", t.out());
}

#[test_id(tqklkco)]
#[test]
fn an_integration_red_names_its_phase_and_first_failing_line_in_the_round_red_row() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![
        ("integration-tests.out".into(), "running 2 tests\nFAIL replays_in_order\n".into()),
        (
            "integration.json".into(),
            r#"{"stage":"integration-tests","verdict":"red","ran_before_suites":false,"failed":["replays_in_order"],"cause":"x","secs":3}"#.into(),
        ),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}{}", t.out(), t.err());
    let log = t.landing_log();
    let red = log.lines().find(|l| l.starts_with("QUEUE ROUND-RED ")).unwrap_or_else(|| panic!("{log}"));
    assert!(red.contains(" red=unit-tests "), "{red}");
    assert!(red.contains(" phase=integration-tests "), "{red}");
    assert!(red.ends_with("reason=unit-tests: FAIL replays_in_order"), "{red}");
}

#[test_id(ta8jsza)]
#[test]
fn a_red_step_the_round_runs_is_named_with_its_first_output_line() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("release".into(), "red".into())];
    *t.scripts.round_vm_files.borrow_mut() = vec![(
        "release.out".into(),
        "bash: spira/release.sh: No such file or directory\n".into(),
    )];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}{}", t.out(), t.err());
    let log = t.landing_log();
    let red = log.lines().find(|l| l.starts_with("QUEUE ROUND-RED ")).unwrap_or_else(|| panic!("{log}"));
    assert!(red.contains(" red=release "), "{red}");
    assert!(red.contains(" phase=release "), "{red}");
    assert!(red.ends_with("reason=release: bash: spira/release.sh: No such file or directory"), "{red}");
}

#[test_id(t901s3f)]
#[test]
fn a_phase_that_failed_with_no_suite_red_is_the_red_the_round_names() {
    for (phase, red) in [("release", "release"), ("workspace-build", "unit-tests")] {
        let t = round_world();
        assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
        let batch = batch_of(&t);
        *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
        *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
        *t.scripts.round_vm_files.borrow_mut() = vec![("round-phase".into(), format!("phase={phase} rc=127\n"))];
        assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}{}", t.out(), t.err());
        let log = t.landing_log();
        let row = log.lines().find(|l| l.starts_with("QUEUE ROUND-RED ")).unwrap_or_else(|| panic!("{log}"));
        assert!(row.contains(&format!(" red={red} ")), "{row}");
    }
}

#[test_id(tnzdc2b)]
#[test]
fn a_mend_item_for_a_long_running_test_names_it_first_and_is_not_a_no_failure_found() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = long_running_integration_red(&t, &batch);
    fs::write(results.join("integration-tests.out"), "running\nnothing matched\n").unwrap();
    assert_eq!(t.run(&["round", "failed", &batch, "--suite", "unit-tests"]), 0, "{}", t.err());
    let spec = fs::read_dir(t.lib.s.run.join("rounds")).unwrap().flatten().find(|e| e.file_name().to_string_lossy().contains(".spec-1-unit-tests.json")).expect("a failure spec");
    let spec: serde_json::Value = serde_json::from_str(&fs::read_to_string(spec.path()).unwrap()).unwrap();
    let out = spec["output"].as_str().unwrap();
    assert!(out.starts_with("later_poll ran over 60 s, then passed"), "{out}");
    assert!(!out.contains("NO_FAILURE_FOUND"), "{out}");
}

#[test_id(t12f194)]
#[test]
fn a_kept_unit_tests_evidence_names_the_long_running_test_and_how_it_ended() {
    let t = round_world();
    t.env.resolved.borrow_mut().insert("SPIRA_SENDBACK_KEEP_DAYS".into(), "7".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let results = long_running_integration_red(&t, &batch);
    let ev = kept_evidence_after_eject(&t, &batch, "unit-tests");
    fs::remove_dir_all(&results).ok();
    assert!(fs::read_to_string(&ev).unwrap().contains("later_poll ran over 60 s, then passed"));
}

#[test_id(tfpoukr)]
#[test]
fn an_ejects_red_evidence_is_kept_where_the_next_pass_start_does_not_wipe_it() {
    let t = round_world();
    t.env.resolved.borrow_mut().insert("SPIRA_SENDBACK_KEEP_DAYS".into(), "7".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    fs::create_dir_all(&results).unwrap();
    fs::write(results.join("test-b.sh.out"), "ok 1\nFAIL: b_broke_the_wal\n").unwrap();
    let eject = ["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"];
    assert_eq!(t.run(&eject), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().join("\n");
    let kept = calls
        .split("Red evidence kept at ")
        .nth(1)
        .and_then(|r| r.split(".\n").next().or(Some(r)))
        .map(|r| r.split(" Red").next().unwrap().trim_end_matches(['.', '"', ' ']).to_string())
        .unwrap_or_else(|| panic!("the reason names no kept evidence: {calls}"));
    let _ = t.run(&["round", "pass-start", &batch]);
    fs::remove_dir_all(&results).ok();
    assert!(fs::read_to_string(&kept).is_ok_and(|e| e.contains("FAIL: b_broke_the_wal")), "{kept}");
}

fn kept_evidence_after_eject(t: &T, batch: &str, suites: &str) -> std::path::PathBuf {
    let eject = ["round", "eject", batch, "sp-b", "--reason", "red", "--red", "--suites", suites];
    assert_eq!(t.run(&eject), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().join("\n");
    let p = calls
        .split("Red evidence kept at ")
        .nth(1)
        .and_then(|r| r.split(".\n").next())
        .map(|r| r.split(" Red").next().unwrap().trim_end_matches(['.', '"', ' ']).to_string())
        .unwrap_or_else(|| panic!("the reason names no kept evidence: {calls}"));
    std::path::PathBuf::from(p)
}

#[test_id(tx8av7s)]
#[test]
fn an_ejects_kept_evidence_holds_each_red_suites_whole_output() {
    let t = round_world();
    t.env.resolved.borrow_mut().insert("SPIRA_SENDBACK_KEEP_DAYS".into(), "7".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    fs::create_dir_all(&results).unwrap();
    let full = "ok 1\nnot ok 2 - b broke\n# wanted [x] in [y]\nmore\n";
    fs::write(results.join("test-b.sh.out"), full).unwrap();
    let ev = kept_evidence_after_eject(&t, &batch, "test-b.sh");
    let _ = t.run(&["round", "pass-start", &batch]);
    fs::remove_dir_all(&results).ok();
    assert_eq!(fs::read(ev.with_file_name("test-b.sh.out")).unwrap(), full.as_bytes());
    let text = fs::read_to_string(&ev).unwrap();
    assert!(text.contains("not ok 2 - b broke") && text.contains("test-b.sh.out"), "{text}");
}

#[test_id(t53pj44)]
#[test]
fn a_unit_tests_ejects_kept_evidence_holds_each_red_phases_whole_output() {
    let t = round_world();
    t.env.resolved.borrow_mut().insert("SPIRA_SENDBACK_KEEP_DAYS".into(), "7".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    fs::create_dir_all(&results).unwrap();
    let full = "running 1 test\nFAIL t1\nafter the failure\n";
    fs::write(results.join("integration-tests.out"), full).unwrap();
    fs::write(
        results.join("integration.json"),
        r#"{"stage":"integration-tests","verdict":"red","failed":["t1"],"cause":"x","secs":3}"#,
    )
    .unwrap();
    let ev = kept_evidence_after_eject(&t, &batch, "unit-tests");
    fs::remove_dir_all(&results).ok();
    assert_eq!(fs::read(ev.with_file_name("integration-tests.out")).unwrap(), full.as_bytes());
    assert!(fs::read_to_string(&ev).unwrap().contains("integration-tests.out"));
}

fn deploy_faulting_round(detached: bool) -> (T, String) {
    let (t, batch) = certified_round();
    if detached {
        *t.scripts.caller_unit.borrow_mut() = Some("spira-runtime-prod.service".into());
    }
    t.scripts.leave_current.set(true);
    (t, batch)
}

#[test_id(tujm874)]
#[test]
fn a_detached_activation_that_exits_0_leaving_current_behind_is_a_deploy_fault() {
    let (t, batch) = deploy_faulting_round(true);
    assert_eq!(t.run(&["round", "land", &batch]), crate::ops::DEPLOY_FAULT, "{}", t.err());
    assert_eq!(t.scripts.detached_keys.borrow().len(), 1);
    let m = fs::read_to_string(t.qfile("deploy-fault")).unwrap();
    assert!(m.contains("exited 0 but current is"), "{m}");
}

#[test_id(tgcdkij)]
#[test]
fn a_deferred_landing_whose_activation_faults_mails_once_after_it_with_the_fault() {
    let (t, batch) = deploy_faulting_round(false);
    assert_eq!(t.run(&["round", "land", &batch]), crate::ops::DEPLOY_FAULT, "{}", t.err());
    let mails = t.lib.mails.borrow();
    let landing: Vec<_> = mails.iter().filter(|(s, _)| s.starts_with("local landing")).collect();
    assert_eq!(landing.len(), 1, "{mails:?}");
    assert!(landing[0].1.contains("DEPLOY FAULT:"), "{}", landing[0].1);
}

#[test_id(tju6cdq)]
#[test]
fn a_deferred_landing_that_activates_is_a_landing_log_record_and_no_mail() {
    let (t, batch) = certified_round();
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    let mails = t.lib.mails.borrow();
    assert!(!mails.iter().any(|(s, _)| s.starts_with("local landing")), "{mails:?}");
    let log = t.landing_log();
    let lines: Vec<_> = log.lines().filter(|l| l.starts_with("QUEUE LANDED ")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains(" round=") && lines[0].contains(" head=") && lines[0].contains(" release="), "{}", lines[0]);
    assert!(!lines[0].contains("release=none") && !lines[0].contains("not activated"), "{}", lines[0]);
}

#[test_id(t8aguwf)]
#[test]
fn a_green_round_proves_every_listed_round_step_for_each_member() {
    let mut t = round_world();
    t.lib.s.home_repo = "spira".into();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().filter(|r| r.bead_id == "sp-a") {
        r.tip = Some(T_FIX.into());
    }
    t.git.ancestor(T_FIX, "merged-tb");
    t.git.ancestor(T_FIX, "merged-tc");
    let members = format!("sp-a:{T_FIX},sp-b");
    assert_eq!(t.run(&["round", "open", "--members", &members]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    for step in spira_config::round_step::names() {
        assert!(spira_config::proof::proven(&proof_run(&t), "sp-a", T_FIX, step), "{step} not proven for sp-a");
    }
}

#[test_id(t8qp4wn)]
#[test]
fn a_green_round_clears_its_members_registry_rows_before_the_release_activates() {
    let (t, batch) = certified_round();
    *t.scripts.clear_landed.borrow_mut() = Some(Ok(vec!["test:t0aaaaa".into()]));
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    let calls = t.scripts.calls.borrow();
    let clears: Vec<_> = calls.iter().enumerate().filter(|(_, c)| c.starts_with("clear-landed ")).collect();
    assert_eq!(clears.len(), 1, "{calls:?}");
    assert_eq!(clears[0].1, "clear-landed sp-a,sp-b");
    let activate = calls.iter().position(|c| c == "release activate").expect("the release activated");
    assert!(clears[0].0 < activate, "{calls:?}");
    assert!(t.out().contains("queue.sh land-local: landing cleared registry row(s) test:t0aaaaa"), "{}", t.out());
}

#[test_id(tu2mmj7)]
#[test]
fn a_landing_whose_registry_clear_fails_still_lands_and_says_so() {
    let (t, batch) = certified_round();
    *t.scripts.clear_landed.borrow_mut() = Some(Err("registry busy".into()));
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
    assert!(t.err().contains("queue.sh land-local: landed but registry rows were not cleared: registry busy"), "{}", t.err());
}

#[test_id(tibws6k)]
#[test]
fn a_landing_with_no_registry_rows_prints_no_clearing_line() {
    let (t, batch) = certified_round();
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(!t.out().contains("registry row"), "{}", t.out());
    assert!(!t.err().contains("registry row"), "{}", t.err());
}

const FLIP_ROW: &str = "disabled\tsuite:test-x.sh\tsp-flip\t9999999999\n";
const ID_ROW: &str = "disabled\ttest:t0abcde\tsp-flip\t9999999999\n";
const DENY_AT_CUT: &str = "# test-registry deny-list at=7 rows=0\n";
const INTEG_RED: &str = r#"{"stage":"integration-tests","verdict":"red","ran_before_suites":false,"cause":"exit status 1","secs":1,"failed":["FAIL bin mod::t0abcde"]}"#;

fn vm_answers(t: &T, rc: i32, results: &[(&str, &str)], files: &[(&str, &str)]) {
    *t.scripts.round_vm.borrow_mut() = RunOut { rc, out: String::new(), err: String::new() };
    *t.scripts.round_vm_results.borrow_mut() = results.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect();
    *t.scripts.round_vm_files.borrow_mut() = files.iter().map(|(n, b)| (n.to_string(), b.to_string())).collect();
}

fn only_calls(t: &T) -> Vec<String> {
    t.scripts.calls.borrow().iter().filter(|c| c.starts_with("round-vm ") && c.contains(" only=")).cloned().collect()
}

fn vm_calls(t: &T) -> usize {
    t.scripts.calls.borrow().iter().filter(|c| c.starts_with("round-vm ")).count()
}

/// A round whose first pass was red on `results`/`files`, and whose attribution then kept `added` in the deny list.
fn flip_world(results: &[(&str, &str)], files: &[(&str, &str)], added: &str) -> (T, String) {
    let t = round_world();
    *t.scripts.deny_list.borrow_mut() = Some(Ok(DENY_AT_CUT.into()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    vm_answers(&t, 1, results, files);
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let kept = marker(&t, &batch, "results").join("deny-list");
    fs::write(&kept, format!("{DENY_AT_CUT}{added}")).unwrap();
    (t, batch)
}

fn suite_flip_world() -> (T, String) {
    flip_world(&[("test-x.sh", "red"), ("test-y.sh", "ok")], &[], FLIP_ROW)
}

fn integration_flip_world(added: &str) -> (T, String) {
    flip_world(&[("test-y.sh", "ok")], &[("integration.json", INTEG_RED), ("integration-tests.out", "boom\n")], added)
}

#[test_id(tcr3r6k)]
#[test]
fn a_pass_red_only_on_a_suite_now_disabled_runs_that_suite_alone_and_lands_on_its_green() {
    let (t, batch) = suite_flip_world();
    vm_answers(&t, 0, &[], &[]);
    *t.scripts.only_results.borrow_mut() = vec![("test-x.sh".into(), "disabled".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(only_calls(&t).len(), 1, "{:?}", t.scripts.calls.borrow());
    assert!(only_calls(&t)[0].ends_with(" only=test-x.sh"), "{:?}", only_calls(&t));
    assert_eq!(vm_calls(&t), 2, "{:?}", t.scripts.calls.borrow());
    assert!(t.out().contains(" only=test-x.sh of_pass=1"), "{}", t.out());
    let log = fs::read_to_string(t.s().run.join("landing.log")).unwrap_or_default();
    assert!(log.lines().any(|l| l.contains("ROUND-GREEN") && l.contains("only=test-x.sh") && l.contains("of_pass=1")), "{log}");
    let kept = marker(&t, &batch, "results");
    assert_eq!(fs::read_to_string(kept.join("test-x.sh.result")).unwrap().trim(), "disabled");
    assert_eq!(fs::read_to_string(kept.join("test-y.sh.result")).unwrap().trim(), "ok", "the pass's other greens are kept");
}

#[test_id(t7q2dvm)]
#[test]
fn a_pass_red_only_on_an_integration_test_now_disabled_runs_the_integration_step_alone_and_lands() {
    let (t, batch) = integration_flip_world(ID_ROW);
    vm_answers(&t, 0, &[], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(only_calls(&t).len(), 1, "{:?}", t.scripts.calls.borrow());
    assert!(only_calls(&t)[0].ends_with(" only=integration-tests"), "{:?}", only_calls(&t));
    let kept = marker(&t, &batch, "results");
    assert!(!kept.join("integration.json").exists(), "the earlier red record is replaced");
    assert!(kept.join("test-y.sh.result").exists());
}

#[test_id(tm5wk1x)]
#[test]
fn a_pass_whose_tree_changed_since_the_red_is_a_full_pass() {
    let (t, batch) = suite_flip_world();
    for head in ["merged-ta", "merged-tb", "merged-tc"] {
        t.git.trees.borrow_mut().insert(head.into(), "4444444444444444444444444444444444444444".into());
    }
    vm_answers(&t, 0, &[("test-x.sh", "disabled"), ("test-y.sh", "ok")], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(only_calls(&t).is_empty(), "{:?}", t.scripts.calls.borrow());
    assert_eq!(vm_calls(&t), 2);
}

#[test_id(txn8h0c)]
#[test]
fn a_pass_with_a_second_red_beside_the_flip_is_a_full_pass() {
    let (t, batch) = flip_world(&[("test-x.sh", "red"), ("test-y.sh", "red")], &[], FLIP_ROW);
    vm_answers(&t, 0, &[("test-x.sh", "disabled"), ("test-y.sh", "ok")], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(only_calls(&t).is_empty(), "{:?}", t.scripts.calls.borrow());
}

#[test_id(tp4ev9s)]
#[test]
fn a_pass_whose_failing_test_has_no_disabled_row_is_a_full_pass() {
    let (t, batch) = integration_flip_world("disabled\ttest:t0other\tsp-flip\t9999999999\n");
    vm_answers(&t, 0, &[("test-y.sh", "ok")], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(only_calls(&t).is_empty(), "{:?}", t.scripts.calls.borrow());
    let (t, batch) = integration_flip_world("");
    vm_answers(&t, 0, &[("test-y.sh", "ok")], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(only_calls(&t).is_empty(), "{:?}", t.scripts.calls.borrow());
}

#[test_id(tu6bz2n)]
#[test]
fn a_pass_a_unit_red_ended_before_any_suite_is_a_full_pass() {
    let unit = INTEG_RED.replace("integration-tests", "unit-tests").replace("\"ran_before_suites\":false", "\"ran_before_suites\":true");
    let (t, batch) = flip_world(&[], &[("unit.json", &unit)], ID_ROW);
    vm_answers(&t, 0, &[("test-y.sh", "ok")], &[]);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(only_calls(&t).is_empty(), "{:?}", t.scripts.calls.borrow());
}

#[test_id(tw8cf3j)]
#[test]
fn a_rerun_that_is_red_makes_the_round_red() {
    let (t, batch) = suite_flip_world();
    vm_answers(&t, 1, &[], &[]);
    *t.scripts.only_results.borrow_mut() = vec![("test-x.sh".into(), "red".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(only_calls(&t).len(), 1);
    assert!(t.out().contains("red=test-x.sh"), "{}", t.out());
}

#[test_id(t0j15od)]
#[test]
fn an_integration_rerun_that_is_red_replaces_the_earlier_record_and_makes_the_round_red() {
    let (t, batch) = integration_flip_world(ID_ROW);
    vm_answers(&t, 1, &[], &[]);
    let again = INTEG_RED.replace("t0abcde", "t0zzzzz");
    *t.scripts.only_files.borrow_mut() = vec![("integration.json".into(), again.clone())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert_eq!(only_calls(&t).len(), 1);
    assert_eq!(fs::read_to_string(marker(&t, &batch, "results").join("integration.json")).unwrap(), again);
    assert!(t.out().contains("red=unit-tests"), "{}", t.out());
}

#[test_id(t45gmo8)]
#[test]
fn a_comment_only_containerfile_edit_is_no_recipe_input_touched_and_a_run_edit_is() {
    const CF: &str = "spira/testenv/Containerfile";
    let t = round_world();
    t.git.stat("b0", "ta", &[(CF, 1, 1)]);
    t.git.stat("b0", "tb", &[(CF, 1, 1)]);
    t.git.stat("b0", "tc", &[(CF, 1, 1), ("flake.nix", 1, 1)]);
    for (tip, head) in [("ta", "# new\nFROM x\n"), ("tb", "FROM x\nRUN y\n"), ("tc", "# new\nFROM x\n")] {
        t.git.files.borrow_mut().insert(format!("b0:{CF}"), "# old\nFROM x\n".into());
        t.git.files.borrow_mut().insert(format!("{tip}:{CF}"), head.into());
    }
    t.with_world(|w| {
        let touched = |tip: &str| crate::ops::round::recipe_inputs_touched(w, std::path::Path::new("r"), "b0", tip);
        assert!(touched("ta").is_empty());
        assert_eq!(touched("tb"), vec![CF.to_string()]);
        assert_eq!(touched("tc"), vec!["flake.nix".to_string()]);
    });
}

const FORCED_FENCES: &str = "step scripts/listed.sh\n";

fn forced_reopen(t: &T, bead: &str) -> String {
    t.lib
        .calls
        .borrow()
        .iter()
        .find_map(|c| c.strip_prefix(&format!("bead_reopen {bead} eject-red")).map(|r| r.trim().to_string()))
        .unwrap_or_else(|| panic!("no reopen of {bead}: {:?}", t.lib.calls.borrow()))
}

fn assert_forced_comment(t: &T, forced: &str) {
    let calls = t.lib.calls.borrow();
    let c = calls.iter().find(|c| c.starts_with("comment sp-b") && c.contains("will force")).unwrap_or_else(|| panic!("no force comment: {calls:?}"));
    assert!(c.trim_end().ends_with(&format!("suites regardless of SPIRA_CERTIFY_SUITES: {forced}")), "{c}");
}

#[test_id(tfyxs0f)]
#[test]
fn a_round_eject_forces_a_listed_fence_and_drops_an_unlisted_one_and_release_verify() {
    let t = round_world();
    t.git.files.borrow_mut().insert("tb:gate.steps".into(), FORCED_FENCES.into());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let args = ["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "fence-listed,fence-unlisted,release-verify,test-b.sh"];
    seed_suite(&t, "test-b.sh");
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert_eq!(forced_reopen(&t, "sp-b"), "fence-listed,test-b.sh");
    assert_forced_comment(&t, "fence-listed,test-b.sh");
}

#[test_id(tatd4l1)]
#[test]
fn a_withdrawal_forces_a_listed_fence_and_drops_an_unlisted_one_and_release_verify() {
    let t = round_world();
    t.git.files.borrow_mut().insert("tb:gate.steps".into(), FORCED_FENCES.into());
    assert_eq!(
        t.run(&["eject", "sp-b", "--red", "--reason", "red", "--suites", "fence-listed,fence-unlisted,release-verify,test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(forced_reopen(&t, "sp-b"), "fence-listed,test-b.sh");
    assert_forced_comment(&t, "fence-listed,test-b.sh");
}

const INTEGRATION_RED_FILE: &str = "{\"stage\":\"integration-tests\",\"verdict\":\"red\",\"ran_before_suites\":false,\"cause\":\"exit status 101\",\"secs\":4,\"last_binary\":\"b\",\"failed\":[\"x::y\"]}\n";

#[test_id(tpq6w3a)]
#[test]
fn certify_on_a_tree_with_no_suite_certifies_on_the_unit_tier_result_alone() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm_results.borrow_mut() = vec![("unit-tests".into(), "ok 1 0 - - - 0".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(certified_tree_exists(&t));
}


#[test_id(t9dtrl7)]
#[test]
fn certify_on_a_tree_with_no_suite_judges_an_integration_red_red_naming_integration_tests() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm_results.borrow_mut() = vec![("unit-tests".into(), "ok 1 0 - - - 0".into())];
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_files.borrow_mut() = vec![("integration.json".to_string(), INTEGRATION_RED_FILE.to_string())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}{}", t.out(), t.err());
    assert!(t.out().contains("unit-tests-cause=integration-tests: exit status 101"), "{}", t.out());
    assert!(!certified_tree_exists(&t));
}

#[test_id(t9f2m8d)]
#[test]
fn stage_test_on_a_tree_with_no_suite_passes_on_the_unit_tier_result_alone() {
    let (t, _n, _s) = staged_behind_green();
    *t.scripts.round_vm_results.borrow_mut() = vec![("unit-tests".into(), "ok 1 0 - - - 0".into())];
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(kv_of(&t, "round-staged")["phase"], "green");
}


#[test_id(tdmcqex)]
#[test]
fn stage_test_on_a_tree_with_no_suite_judges_an_integration_red_red_naming_integration_tests() {
    let (t, _n, _s) = staged_behind_green();
    *t.scripts.round_vm_results.borrow_mut() = vec![("unit-tests".into(), "ok 1 0 - - - 0".into())];
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 1, out: String::new(), err: String::new() };
    *t.scripts.round_vm_files.borrow_mut() = vec![("integration.json".to_string(), INTEGRATION_RED_FILE.to_string())];
    assert_eq!(t.run(&["round", "stage-test"]), 1, "{}{}", t.out(), t.err());
    assert_eq!(kv_of(&t, "round-staged")["phase"], "red");
    assert!(t.out().contains("unit-tests-cause=integration-tests: exit status 101"), "{}", t.out());
}
