//! Acceptance T1–T6 for `publish`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/publish.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide`, `settle` and `step` are `unimplemented!`, and the row waits in
//! `tasks::a::PENDING`. T6 runs two virtual days (the contract asks fourteen); its idle budget is
//! the contract's 4/h.

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: publish,
    registration: Publish::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    test_ids: [ #[test_id(t116j9f)] #[test_id(t95x84v)] #[test_id(tnyrdg7)] #[test_id(tuvqza5)] #[test_id(tvme2zj)] ],
}

fn repo(pr: Pr) -> Repo {
    Repo {
        mode: "queue.local".into(),
        publish_mode: "pr".into(),
        publish_rec: Some(PublishRec { pr: 31, head: "h1".into(), branch: "publish/h1".into(), ..PublishRec::default() }),
        pr,
        open_publish_prs: vec![31],
        forge_sha: Some("f0".into()),
        head_sha: "h1".into(),
        diverged: Some(false),
        ..Repo::default()
    }
}

fn with(repo: Repo) -> Snapshot {
    Snapshot { repos: [("spira".to_string(), repo)].into(), world_running: true }
}

/// The contract's idle fixture: a record open, CI pending, not behind.
fn idle() -> Snapshot {
    with(repo(Pr::Open { ci: "pending".into(), behind: false, tries: 0 }))
}

/// The publish PR is red and behind: supersede it.
fn behind() -> Repo {
    repo(Pr::Open { ci: "red".into(), behind: true, tries: 0 })
}

/// The publish PR is gone: retire it and open the next.
fn busy() -> Snapshot {
    with(repo(Pr::Gone))
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test_id(twhjkd5)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(tlw4qd0)]
#[test]
fn t3_opening_x_tick_is_refused_naming_opening() {
    let refused = step(State::Opening, Trigger::Tick).expect_err("opening × tick is illegal");
    assert!(refused.contains("Opening"), "{refused}");
}

#[test_id(tgfbjab)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// The `decide` table tests (`ops/publish.rs:391`).
#[test_id(tgo9l0e)]
#[test]
fn t5_the_settle_table() {
    assert_eq!(settle(Some(&Pr::Gone)), Settle::Retire);
    assert_eq!(settle(Some(&Pr::Open { ci: "pending".into(), behind: false, tries: 0 })), Settle::Wait);
    assert_eq!(settle(Some(&Pr::Open { ci: "red".into(), behind: true, tries: 0 })), Settle::Supersede);
    assert_eq!(settle(Some(&Pr::Open { ci: "untested".into(), behind: false, tries: 0 })), Settle::Rerun);
    assert_eq!(settle(Some(&Pr::Open { ci: "untested".into(), behind: false, tries: 1 })), Settle::Alarm);
    let no_run = |behind, opened, ci_started| {
        let rec = PublishRec { pr: 31, opened, ci_started, ..PublishRec::default() };
        settle_with(&Policy { no_run_secs: Some(600), ..Policy::default() }, Some(&rec), 10_000, Some(&Pr::Open { ci: "no_run".into(), behind, tries: 0 }))
    };
    assert_eq!(no_run(true, 1_000, 0), Settle::Supersede);
    assert_eq!(no_run(false, 1_000, 0), Settle::StartCi);
    assert_eq!(no_run(false, 9_999, 0), Settle::Wait);
    assert_eq!(no_run(false, 1_000, 9_999), Settle::Wait);
    assert_eq!(no_run(false, 1_000, 5_000), Settle::Parked);
    assert_eq!(no_run(false, 0, 0), Settle::Wait, "no parseable opened is not past");
}

fn bound() -> Policy {
    Policy { no_run_secs: Some(600), ..Policy::default() }
}

fn no_run_repo(behind: bool, ci_started: u64) -> Snapshot {
    let mut r = repo(Pr::Open { ci: "no_run".into(), behind, tries: 0 });
    r.publish_rec = Some(PublishRec { pr: 31, head: "h1".into(), branch: "publish/h1".into(), opened: 1_000, ci_started, ..PublishRec::default() });
    r.observed_at = 10_000;
    with(r)
}

/// No run past the bound, local not ahead: CI started once, one page, then parked.
#[test_id(teicjen)]
#[test]
fn t5_no_run_past_the_bound_starts_ci_once_then_parks() {
    let (carry, first) = decide_with(&bound(), &Carry::default(), &no_run_repo(false, 0));
    assert_eq!(effects(&first), [START_CI, NOTIFY, RECORD], "{first:?}");
    assert!(first[0].input.contains("publish/h1"), "{first:?}");
    assert_eq!(carry.repos["spira"].state, State::Open);
    let (_, then) = decide_with(&bound(), &Carry::default(), &no_run_repo(false, 5_000));
    assert!(!effects(&then).contains(&START_CI), "{then:?}");
}

#[test_id(t3ltg3p)]
#[test]
fn t5_no_run_past_the_bound_and_behind_supersedes() {
    let (_, acts) = decide_with(&bound(), &Carry::default(), &no_run_repo(true, 0));
    assert_eq!(effects(&acts), [NOTIFY, RECORD, CANCEL_RUNS, PR_CLOSE], "{acts:?}");
    assert!(acts[1].input.contains("no_run_paged"), "{acts:?}");
}

#[test_id(tx4u2yu)]
#[test]
fn t5_no_run_with_an_unresolved_bound_waits_and_names_the_key() {
    let policy = Policy { no_run_secs: None, ..Policy::default() };
    let (carry, acts) = decide_with(&policy, &Carry::default(), &no_run_repo(false, 0));
    assert_eq!(effects(&acts), [NOTIFY], "{acts:?}");
    assert!(acts[0].input.contains("SPIRA_PUBLISH_NO_RUN_SECS"), "{acts:?}");
    let (carry, again) = decide_with(&policy, &carry, &no_run_repo(false, 0));
    assert!(again.is_empty(), "paged once per PR: {again:?}");
    assert_eq!(carry.repos["spira"].no_run_key_noted, Some(31));
}

/// A gone PR's record is retired and the next PR opens on the same tick, without closing it again.
#[test_id(txa3qt0)]
#[test]
fn t5_gone_retires_and_opens_on_one_tick() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert_eq!(effects(&acts), [RETIRE, PUSH_BRANCH], "{acts:?}");
    let retire: Pub = serde_json::from_str(&acts[0].input).expect("a Pub");
    let push: Pub = serde_json::from_str(&acts[1].input).expect("a Pub");
    assert_eq!((retire.pr, retire.subject.as_str(), push.head.as_str()), (31, "Retire", "h1"));
}

#[test_id(top9ct8)]
#[test]
fn a_gone_publish_pr_is_retired_and_pushed_by_the_engine_in_one_tick() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    let names: Vec<String> = r.requested().iter().map(|q| q.name.clone()).collect();
    assert!(names.iter().any(|n| n == "publish.retire") && names.iter().any(|n| n == "publish.push_branch"), "{names:?}");
    assert!(!names.iter().any(|n| n == "publish.pr_close"), "{names:?}");
}

#[test_id(tya0yen)]
#[test]
fn a_publish_step_held_by_a_halt_is_not_driven_again_while_it_is_held() {
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_HALTED, "test")).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 3).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let held: Vec<String> = r.journaled().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_HELD).map(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).unwrap()["spec"]["name"].as_str().unwrap().to_string()).collect();
    let count = |name: &str| held.iter().filter(|n| *n == name).count();
    assert_eq!((count("publish.retire"), count(PUSH_BRANCH)), (1, 1), "{held:?}");
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_RESUMED, "")).unwrap();
    assert_eq!(r.requested().iter().filter(|q| q.name == PUSH_BRANCH).count(), 1);
}

/// A close that succeeded and an open step that failed must not leave the stale record in charge.
#[test_id(t8hg37g)]
#[test]
fn a_failed_open_after_a_close_does_not_wedge_the_stale_record() {
    let superseding = Carry { repos: [("spira".to_string(), RepoCarry { state: State::Superseding, ..RepoCarry::default() })].into() };
    let p = Pub { repo: "spira".into(), pr: 31, head: "h1".into(), base: "f0".into(), branch: "publish/h1".into(), ..Pub::default() };
    let (opening, _) = on_outcome(&superseding, PR_CLOSE, true, "{}", &p);
    let (refused, _) = on_outcome(&opening, PR_CREATE, false, "boom", &p);
    assert_eq!(refused.repos["spira"].state, State::Refused);
    let (_, acts) = decide(&refused, &busy());
    assert_eq!(effects(&acts), [RETIRE, PUSH_BRANCH], "{acts:?}");
}

#[test_id(t0zp2df)]
#[test]
fn a_gone_pr_whose_forge_is_already_at_head_retires_and_pushes_nothing() {
    let at_head = with(Repo { forge_sha: Some("h1".into()), ..busy().repos["spira"].clone() });
    let (carry, acts) = decide(&Carry::default(), &at_head);
    assert_eq!(effects(&acts), [RETIRE], "{acts:?}");
    assert_eq!(carry.repos.get("spira").map(|c| c.state).unwrap_or_default(), State::Idle);
}

#[test_id(tbqrbol)]
#[test]
fn a_failed_retire_is_noted_for_the_operator() {
    let opening = Carry { repos: [("spira".to_string(), RepoCarry { state: State::Opening, ..RepoCarry::default() })].into() };
    let p = Pub { repo: "spira".into(), pr: 31, subject: "Retire".into(), ..Pub::default() };
    let (after, acts) = on_outcome(&opening, RETIRE, false, "lock busy", &p);
    assert_eq!(effects(&acts), [NOTIFY], "{acts:?}");
    assert_eq!(after.repos["spira"].state, State::Opening, "a failed retire does not disturb the open in flight");
}

#[test_id(tzhyygz)]
#[test]
fn a_closed_supersede_retires_its_record_before_pushing() {
    let superseding = Carry { repos: [("spira".to_string(), RepoCarry { state: State::Superseding, ..RepoCarry::default() })].into() };
    let p = Pub { repo: "spira".into(), pr: 31, head: "h1".into(), base: "f0".into(), branch: "publish/h1".into(), ..Pub::default() };
    let (_, acts) = on_outcome(&superseding, PR_CLOSE, true, "{}", &p);
    assert_eq!(effects(&acts), [RETIRE, PUSH_BRANCH], "{acts:?}");
    let retire: Pub = serde_json::from_str(&acts[0].input).expect("a Pub");
    assert_eq!((retire.pr, retire.subject.as_str()), (31, "Supersede"));
}

#[test_id(tj4psn5)]
#[test]
fn a_saved_repo_carry_with_the_retired_pr_field_still_loads() {
    let rc: RepoCarry = serde_json::from_str(r#"{"state":"Idle","deferred_ticks":0,"retired":1125}"#).expect("an old carry loads");
    assert_eq!(rc.state, State::Idle);
}

fn retire_world(dir: &testkit::TempDir, recorded: Option<u64>, retiring: u64) -> (bool, Result<String, String>) {
    if let Some(pr) = recorded {
        let p = Pub { repo: "spira".into(), pr, head: "h1".into(), base: "f0".into(), branch: "publish/h1".into(), ..Pub::default() };
        let fresh = edge::Fresh { remote: "origin".into(), forge_branch: "main".into(), members: vec![] };
        edge::write_record(dir.path(), &p, Some(&fresh), 1).expect("the record is written");
    }
    let run = testkit::TempDir::new("publish-retire-run");
    let e = spira_runtime::Effects { world: spira_runtime::world::World { queue_dir: dir.path().to_path_buf(), run_dir: run.path().to_path_buf(), ..Default::default() }, ..Default::default() };
    let spec = runtime::event::EffectSpec::keyed(RETIRE, "k", serde_json::to_string(&Pub { repo: "spira".into(), pr: retiring, subject: "Retire".into(), ..Pub::default() }).unwrap(), 30_000);
    let out = edge::retire(&e, "k", &spec);
    let log = std::fs::read_to_string(run.path().join("landing.log")).unwrap_or_default();
    (dir.path().join("spira").join("publish").exists(), out.map(|_| log))
}

#[test_id(ttx3ps5)]
#[test]
fn retire_removes_the_record_naming_its_pr() {
    let dir = testkit::TempDir::new("publish-retire");
    let (left, out) = retire_world(&dir, Some(42), 42);
    let log = out.expect("retire answers Ok");
    assert!(!left && log.contains("QUEUE PUBLISH_SUPERSEDED") && log.contains("repo=spira pr=42 action=Retire"), "{left} {log}");
}

#[test_id(tebyyvo)]
#[test]
fn retire_leaves_a_record_naming_another_pr() {
    let dir = testkit::TempDir::new("publish-retire");
    let (left, out) = retire_world(&dir, Some(43), 42);
    assert!(left && out.expect("retire answers Ok").is_empty());
}

#[test_id(t60g8rw)]
#[test]
fn retire_with_the_queue_lock_held_is_an_error_and_keeps_the_record() {
    let dir = testkit::TempDir::new("publish-retire");
    let _held = match queue::lock::try_lock_as(dir.path(), "spira", "other") { queue::lock::Acquire::Held(g) => g, _ => panic!("lock is free") };
    let (left, out) = retire_world(&dir, Some(42), 42);
    assert!(left && out.is_err(), "{left} {out:?}");
}

#[test_id(tlxxuye)]
#[test]
fn retire_logs_the_action_word_it_was_given() {
    let dir = testkit::TempDir::new("publish-retire");
    let p = Pub { repo: "spira".into(), pr: 42, head: "h1".into(), base: "f0".into(), branch: "publish/h1".into(), ..Pub::default() };
    edge::write_record(dir.path(), &p, Some(&edge::Fresh { remote: "origin".into(), forge_branch: "main".into(), members: vec![] }), 1).unwrap();
    let run = testkit::TempDir::new("publish-retire-run");
    let e = spira_runtime::Effects { world: spira_runtime::world::World { queue_dir: dir.path().to_path_buf(), run_dir: run.path().to_path_buf(), ..Default::default() }, ..Default::default() };
    let spec = runtime::event::EffectSpec::keyed(RETIRE, "k", serde_json::to_string(&Pub { repo: "spira".into(), pr: 42, subject: "Supersede".into(), ..Pub::default() }).unwrap(), 30_000);
    edge::retire(&e, "k", &spec).expect("ok");
    let log = std::fs::read_to_string(run.path().join("landing.log")).unwrap();
    assert!(log.contains("repo=spira pr=42 action=Supersede"), "{log}");
}

#[test_id(txifzh8)]
#[test]
fn retire_with_no_record_is_ok() {
    let dir = testkit::TempDir::new("publish-retire");
    let (left, out) = retire_world(&dir, None, 42);
    assert!(!left && out.expect("retire answers Ok").is_empty());
}

/// Red and behind supersedes: cancel the runs, close the PR.
#[test_id(tl2q2mi)]
#[test]
fn t5_red_and_behind_supersedes() {
    let (_, acts) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "red".into(), behind: true, tries: 0 })));
    assert_eq!(effects(&acts), [CANCEL_RUNS, PR_CLOSE], "{acts:?}");
}

/// Untested reruns once; after the rerun it alarms with a record.
#[test_id(tq2sesl)]
#[test]
fn t5_untested_reruns_then_alarms() {
    let (_, first) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "untested".into(), behind: false, tries: 0 })));
    assert_eq!(effects(&first), [RERUN], "{first:?}");
    let (_, then) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "untested".into(), behind: false, tries: 1 })));
    assert!(effects(&then).contains(&NOTIFY) && effects(&then).contains(&RECORD), "{then:?}");
}

/// `:506-527`: a round holding the hypervisor defers the act and counts the tick.
#[test_id(tjw6j9b)]
#[test]
fn t5_a_round_on_the_hypervisor_defers() {
    let held = with(Repo { round_holds_hypervisor: true, ..behind() });
    let (carry, acts) = decide(&Carry::default(), &held);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!((carry.repos["spira"].state, carry.repos["spira"].deferred_ticks), (State::Deferred, 1));
}

/// The record a PR's creation writes is the one queue's `settle_publish` reads: every field it
/// refuses a record without is there, and the red path (`settle_publish_red`, `kv.members()`)
/// finds the range's members.
#[test_id(tzyjyt1)]
#[test]
fn the_record_a_created_pr_writes_passes_settle_publish_and_names_its_members() {
    let opening = Carry { repos: [("spira".to_string(), RepoCarry { state: State::Opening, ..RepoCarry::default() })].into() };
    let pushed = Pub { repo: "spira".into(), head: "h1".into(), base: "f0".into(), branch: "spira/publish/1700000000".into(), ..Pub::default() };
    let (_, acts) = on_outcome(&opening, PR_CREATE, true, r#"{"pr":42}"#, &pushed);
    let act = acts.iter().find(|a| a.effect == RECORD).expect("a created PR is recorded");
    let p: Pub = serde_json::from_str(&act.input).expect("the record's input is a Pub");
    let dir = testkit::TempDir::new("publish-record");
    let fresh = edge::Fresh { remote: "origin".into(), forge_branch: "main".into(), members: vec![queue::model::Member { id: "sp-a1".into(), tip: "t1".into() }, queue::model::Member { id: "sp-b2".into(), tip: "t2".into() }] };
    edge::write_record(dir.path(), &p, Some(&fresh), 1_700_000_100).expect("the record is written");
    let kv = queue::records::read_kv(&dir.path().join("spira").join("publish")).expect("readable").expect("the record exists");
    assert_eq!(queue::ops::verdict::publish_record_missing(&kv), None, "settle_publish takes it: {kv:?}");
    assert_eq!((kv.get("pr"), kv.get("head"), kv.get("base"), kv.get("remote"), kv.get("forge_branch")), (Some("42"), Some("h1"), Some("f0"), Some("origin"), Some("main")));
    assert_eq!(kv.members().into_iter().map(|m| (m.id, m.tip)).collect::<Vec<_>>(), [("sp-a1".to_string(), "t1".to_string()), ("sp-b2".to_string(), "t2".to_string())], "the red path's members");
    // A later rerun sets its field on the whole record and keeps the rest.
    let rerun = Pub { repo: "spira".into(), pr: 42, record: [("rerun".to_string(), "1".to_string())].into(), ..Pub::default() };
    edge::write_record(dir.path(), &rerun, None, 1_700_000_200).expect("the rerun is recorded");
    let kv = queue::records::read_kv(&dir.path().join("spira").join("publish")).expect("readable").expect("the record exists");
    assert_eq!((queue::ops::verdict::publish_record_missing(&kv), kv.get("rerun"), kv.members().len()), (None, Some("1"), 2), "{kv:?}");
}

/// law-lifecycle-state-is-not-text (sp-mijstw): the fix-forward bead is live only while its typed
/// row is not terminal. A DROPPED or SUPERSEDED fix-forward read as live left the publish red
/// with no live fix, never filed again.
#[test_id(tlto3je)]
#[test]
fn t7_a_terminal_fix_forward_is_not_live() {
    let show = |state: &str| format!(r#"{{"bead":{{"bead_id":"sp-ff","state":"{state}"}},"delivery":null}}"#);
    for state in ["LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
        assert!(!fix_forward_live(&show(state)), "{state} is terminal, not a live fix-forward");
    }
    for state in ["READY", "WORKING", "SUBMITTED", "IN_DELIVERY"] {
        assert!(fix_forward_live(&show(state)), "{state} is live");
    }
    assert!(!fix_forward_live("cannot tell: deadline"), "no answer is not a live bead");
}

/// sp-hw3qio: publish PR #989 went green on its latest runs, while older runs at the same head
/// had failed, and sat open 16 h: `decide` answered Wait and left it to queue.step, which only
/// settles batches. One publish pass now settles it: the probe sees it green and asks
/// `publish.settle`, whose success leaves the repo idle for the next publish.
#[test_id(te8e9oh)]
#[test]
fn a_green_open_publish_pr_settles_in_one_pass() {
    let green = with(repo(Pr::Open { ci: "green".into(), behind: false, tries: 0 }));
    let (carry, acts) = decide(&Carry::default(), &green);
    assert_eq!(effects(&acts), ["publish.settle"], "{acts:?}");
    let p: Pub = serde_json::from_str(&acts[0].input).expect("the settle's input is a Pub");
    assert_eq!((p.pr, p.head.as_str(), p.branch.as_str()), (31, "h1", "publish/h1"));
    let (after, more) = on_outcome(&carry, "publish.settle", true, "{}", &p);
    assert!(more.is_empty(), "{more:?}");
    assert_eq!(after.repos.get("spira").map_or(State::Idle, |rc| rc.state), State::Idle);
    let mut r = rig(1, green);
    r.advance(CADENCE_MS).unwrap();
    assert!(r.requested().iter().any(|q| q.name == "publish.settle"), "{:?}", r.requested());
}

/// The PR #989 path: no record, one publish PR open. Adopting it settles it in the same pass;
/// a failed or timed-out adoption is retried on the next tick instead of wedging in Adopting.
#[test_id(tejgdjv)]
#[test]
fn an_adopted_publish_pr_settles_and_a_failed_adoption_is_retried() {
    let orphan = with(Repo { publish_rec: None, pr: Pr::Unknown, open_publish_prs: vec![989], ..repo(Pr::Unknown) });
    let (adopting, acts) = decide(&Carry::default(), &orphan);
    assert_eq!((adopting.repos["spira"].state, effects(&acts)), (State::Adopting, vec![RECORD]));
    let p: Pub = serde_json::from_str(&acts[0].input).unwrap();
    let (_, chained) = on_outcome(&adopting, RECORD, true, "{}", &p);
    assert_eq!(effects(&chained), ["publish.settle"], "an adoption settles at once: {chained:?}");
    let (failed, _) = on_outcome(&adopting, RECORD, false, "deadline", &p);
    let (_, retried) = decide(&failed, &orphan);
    assert_eq!(effects(&retried), [RECORD], "a failed adoption is retried on the next tick");
    let (_, lost) = decide(&adopting, &orphan);
    assert_eq!(effects(&lost), [RECORD], "an adoption whose outcome was lost is retried on the next tick");
    let deadline = ACTS.iter().find(|(e, _)| *e == RECORD).map(|(_, d)| *d).unwrap_or_default();
    assert!(deadline >= OBSERVE_DEADLINE_MS, "an adoption fetches and reads the range's members: {deadline} ms is too short");
}

#[test_id(t0i9upv)]
#[test]
fn the_carry_stays_under_the_journal_column_for_the_longest_repo_names_and_the_rest_wait() {
    let held = Repo { round_holds_hypervisor: true, ..busy().repos["spira"].clone() };
    let name = |i: usize| format!("{i:04}-{}", "r".repeat(250));
    let snap = Snapshot { repos: (0..600).map(|i| (name(i), held.clone())).collect(), world_running: true };
    let (carry, _) = decide(&Carry::default(), &snap);
    let bytes = serde_json::to_string(&carry).unwrap().len();
    assert!(bytes < 65_535 && !carry.repos.is_empty() && carry.repos.len() < 600, "{} repos, {bytes} bytes", carry.repos.len());
    let (again, _) = decide(&carry, &snap);
    assert!(carry.repos.keys().all(|k| again.repos.contains_key(k)), "a carried repo keeps its entry");
    assert!(serde_json::to_string(&again).unwrap().len() < 65_535);
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(t8tv5os)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let mut held = rig(1, with(Repo { round_holds_hypervisor: true, ..behind() }));
    held.advance(CADENCE_MS).unwrap();
    let rows = kit::assert_kept_as_rows(&held, NAME);
    assert!(rows.iter().any(|k| k.starts_with("repos/")), "{rows:?}");
}

#[test_id(t2kp827)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(ts0ami2)]
#[test]
fn t5_the_probe_reads_a_no_run_status_and_the_record_fields() {
    assert_eq!(edge::ci_label("pending\nno-run: Gate\n"), "no_run");
    assert_eq!(edge::ci_label("pending\n"), "pending");
    assert_eq!(edge::ci_label("green\n"), "green");
    let kv: std::collections::BTreeMap<String, String> = [("pr", "31"), ("opened", "900"), ("ci_started", "1200"), ("no_run_paged", "1")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let rec = edge::publish_rec(&kv).expect("a record with a pr");
    assert_eq!((rec.opened, rec.ci_started, rec.no_run_paged), (900, 1200, true));
}

fn world_of(repos: [(&str, Repo); 2]) -> std::sync::Arc<std::sync::Mutex<Snapshot>> {
    std::sync::Arc::new(std::sync::Mutex::new(Snapshot { repos: repos.into_iter().map(|(n, r)| (n.to_string(), r)).collect(), world_running: true }))
}

fn rig_over(world: &std::sync::Arc<std::sync::Mutex<Snapshot>>) -> TaskRig {
    let w = world.clone();
    rig_with(1, move |_| Ok(serde_json::to_string(&*w.lock().unwrap()).unwrap()))
}

fn wake(r: &mut TaskRig) {
    r.tick(STREAM, EventKind::new(spira_runtime::tasks::a::queue_step::PUBLISH_WANTED, r#"{"repo":"a","mode":"queue.local"}"#)).unwrap();
    kit::assert_unpoisoned(r, STREAM);
}

fn carried(r: &TaskRig) -> Carry {
    let e = r.journaled().into_iter().rev().find(|e| e.kind.tag == STATE).expect("a carry was recorded");
    serde_json::from_str(&e.kind.body).unwrap()
}

fn push_repo() -> Repo {
    Repo { publish_mode: "push".into(), publish_rec: None, pr: Pr::Gone, open_publish_prs: vec![], round_holds_hypervisor: true, ..repo(Pr::Gone) }
}

fn pr_repo() -> Repo {
    Repo { round_holds_hypervisor: true, ..behind() }
}

#[test_id(t6ire33)]
#[test]
fn a_publish_wake_pushes_a_push_repo_past_a_round_and_defers_a_pr_repo() {
    let idle = Repo { stopped: true, ..push_repo() };
    let world = world_of([("a", idle.clone()), ("b", Repo { stopped: true, ..pr_repo() })]);
    let mut r = rig_over(&world);
    let asked = r.requested().len();
    *world.lock().unwrap() = Snapshot { repos: [("a".to_string(), push_repo()), ("b".to_string(), pr_repo())].into(), world_running: true };
    wake(&mut r);
    let acts: Vec<(String, String)> = r.requested()[asked..].iter().filter(|q| q.name != OBSERVE).map(|q| (q.name.clone(), q.input.clone())).collect();
    assert_eq!(acts.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [PUSH_FORGE], "{acts:?}");
    assert!(acts[0].1.contains("\"a\""), "{acts:?}");
    assert_eq!(carried(&r).repos["b"].state, State::Deferred);
}

#[test_id(ttdk8yo)]
#[test]
fn wakes_inside_half_a_cadence_do_not_spend_a_deferral() {
    let held = |at: u64| Repo { observed_at: at, ..pr_repo() };
    let world = world_of([("a", Repo { stopped: true, ..push_repo() }), ("b", held(1_000))]);
    let mut r = rig_over(&world);
    assert_eq!(carried(&r).repos["b"].deferred_ticks, 1);
    for at in [1_100, 1_500, 1_000 + CADENCE_MS / 2000 - 1] {
        world.lock().unwrap().repos.insert("b".into(), held(at));
        wake(&mut r);
        assert_eq!(carried(&r).repos["b"].deferred_ticks, 1, "a wake at +{}s", at - 1_000);
    }
    world.lock().unwrap().repos.insert("b".into(), held(1_000 + CADENCE_MS / 2000));
    wake(&mut r);
    assert_eq!(carried(&r).repos["b"].deferred_ticks, 2);
}

fn rerun_world(code: i32, err: &str) -> (Result<(), String>, String, Option<String>) {
    let q = testkit::TempDir::new("publish-rerun-q");
    let run = testkit::TempDir::new("publish-rerun-run");
    let p = Pub { repo: "spira".into(), pr: 42, head: "h1".into(), base: "f0".into(), branch: "publish/h1".into(), ..Pub::default() };
    let fresh = edge::Fresh { remote: "origin".into(), forge_branch: "main".into(), members: vec![] };
    edge::write_record(q.path(), &p, Some(&fresh), 1).expect("the record is written");
    let reply = |code: i32, out: &str, err: &str| Ok(forge::client::Reply { code, out: out.into(), err: err.into() });
    let call = |verb: &str, _: &[&str]| match verb {
        "run-id" => reply(0, "9\n", ""),
        "check-status" => reply(0, "harness_fault\nrunner-lost: suites / Download binaries\nrunner-market: spot\n", ""),
        _ => reply(code, "", err),
    };
    let rerun = Pub { record: [("rerun".to_string(), "1".to_string())].into(), ..p };
    let out = edge::rerun_with(&call, q.path(), run.path(), &rerun);
    let log = std::fs::read_to_string(run.path().join("landing.log")).unwrap_or_default();
    let flag = queue::records::read_kv(&q.path().join("spira").join("publish")).unwrap().and_then(|kv| kv.get("rerun").map(str::to_string));
    (out, log, flag)
}

#[test_id(tk7vb3n)]
#[test]
fn a_started_rerun_writes_its_line_and_marks_the_record() {
    let (out, log, flag) = rerun_world(0, "");
    assert!(out.is_ok() && flag.as_deref() == Some("1"), "{out:?} {flag:?}");
    assert!(log.contains("QUEUE PUBLISH_RERUN ") && log.contains("repo=spira pr=42 run=9 cause=suites / Download binaries market=spot"), "{log}");
}

#[test_id(tw2ho8e)]
#[test]
fn a_refused_rerun_writes_its_line_and_leaves_the_record_unmarked() {
    let (out, log, flag) = rerun_world(3, "no input changed\nmore");
    assert!(out.is_err() && flag.is_none(), "{out:?} {flag:?}");
    assert!(log.contains("QUEUE PUBLISH_RERUN_REFUSED ") && log.contains("run=9 why=code=3 no input changed") && !log.contains("more"), "{log}");
}

type World = std::sync::Arc<std::sync::Mutex<Snapshot>>;

fn lost_world(world: Snapshot) -> World {
    std::sync::Arc::new(std::sync::Mutex::new(Snapshot { repos: world.repos.into_iter().map(|(n, r)| (n, Repo { round_holds_hypervisor: false, ..r })).collect(), ..world }))
}

fn lost_rig(world: &World, reg: fn() -> runtime::event::Registration, hang: &str) -> runtime::taskrig::TaskRig {
    let w = world.clone();
    let mut r = runtime::taskrig::TaskRig::new(1, reg()).unwrap().hanging(hang).double(OBSERVE, move |_| Ok(serde_json::to_string(&*w.lock().unwrap()).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r
}

fn tick(r: &mut runtime::taskrig::TaskRig) {
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    kit::assert_unpoisoned(r, STREAM);
}

fn asked(r: &runtime::taskrig::TaskRig, name: &str) -> usize {
    r.requested().iter().filter(|q| q.name == name).count()
}

fn landing(round: bool) -> Repo {
    Repo { publish_rec: None, open_publish_prs: vec![], round_holds_hypervisor: round, ..repo(Pr::Gone) }
}

/// A push lost at a restart is pushed again on the next pass, and the publish then settles.
#[test_id(tbq2f5x)]
#[test]
fn a_push_lost_at_a_restart_is_pushed_again_and_the_green_pr_then_settles() {
    let world = lost_world(with(landing(true)));
    let mut r = lost_rig(&world, registration, PUSH_BRANCH);
    tick(&mut r);
    assert_eq!(asked(&r, PUSH_BRANCH), 1, "a landing opens past a round: {:?}", r.requested());
    wake(&mut r);
    assert_eq!(asked(&r, PUSH_BRANCH), 1, "a push in flight is waited for");
    r.reopen(registration()).unwrap();
    tick(&mut r);
    assert_eq!((asked(&r, PUSH_BRANCH), asked(&r, PR_CREATE)), (2, 1), "{:?}", r.requested());
    *world.lock().unwrap() = with(repo(Pr::Open { ci: "green".into(), behind: false, tries: 0 }));
    wake(&mut r);
    assert_eq!(asked(&r, SETTLE), 1, "{:?}", r.requested());
}

/// A PR created before its answer was lost is adopted, not created twice.
#[test_id(tm9c1ue)]
#[test]
fn a_pr_created_before_its_answer_was_lost_is_adopted_not_created_again() {
    let world = lost_world(with(landing(false)));
    let mut r = lost_rig(&world, registration, PR_CREATE);
    tick(&mut r);
    assert_eq!(asked(&r, PR_CREATE), 1, "{:?}", r.requested());
    r.reopen(registration()).unwrap();
    world.lock().unwrap().repos.get_mut("spira").unwrap().open_publish_prs = vec![40];
    tick(&mut r);
    let records: Vec<_> = r.requested().iter().filter(|q| q.name == RECORD).collect();
    assert!(records.len() == 1 && records[0].input.contains("adopt"), "{:?}", r.requested());
    assert_eq!(asked(&r, PR_CREATE), 1, "no second create");
}

/// A close lost at a restart, its PR since gone, goes on to push the next publish.
#[test_id(tr4k7zd)]
#[test]
fn a_close_lost_at_a_restart_goes_on_to_push_once_the_pr_is_gone() {
    let world = lost_world(with(repo(Pr::Open { ci: "red".into(), behind: true, tries: 0 })));
    let mut r = lost_rig(&world, registration, PR_CLOSE);
    tick(&mut r);
    assert_eq!(asked(&r, PR_CLOSE), 1, "{:?}", r.requested());
    r.reopen(registration()).unwrap();
    world.lock().unwrap().repos.get_mut("spira").unwrap().pr = Pr::Gone;
    tick(&mut r);
    assert_eq!(asked(&r, PUSH_BRANCH), 1, "{:?}", r.requested());
}

fn ci_bound() -> runtime::event::Registration {
    Publish::new([("SPIRA_PUBLISH_NO_RUN_SECS".to_string(), "600".to_string())].into()).registration()
}

/// CI started and answered, the PR since green: the next pass settles it instead of waiting in Open.
#[test_id(tw8n3ph)]
#[test]
fn a_started_ci_that_goes_green_is_settled_on_the_next_pass() {
    let world = lost_world(no_run_repo(false, 0));
    let mut r = lost_rig(&world, ci_bound, "none");
    tick(&mut r);
    assert_eq!(asked(&r, START_CI), 1, "{:?}", r.requested());
    assert_eq!(carried(&r).repos["spira"].state, State::Open);
    let green = Repo { pr: Pr::Open { ci: "green".into(), behind: false, tries: 0 }, ..no_run_repo(false, 5_000).repos["spira"].clone() };
    world.lock().unwrap().repos.insert("spira".into(), green);
    tick(&mut r);
    assert_eq!(asked(&r, SETTLE), 1, "{:?}", r.requested());
}

/// An Opening carry with nothing in flight and the forge already at head is Idle in one pass.
#[test_id(tz1v6ya)]
#[test]
fn an_opening_carry_with_nothing_in_flight_and_the_forge_at_head_is_idle_in_one_pass() {
    let world = lost_world(with(landing(false)));
    let mut r = lost_rig(&world, registration, PUSH_BRANCH);
    tick(&mut r);
    assert_eq!(carried(&r).repos["spira"].state, State::Opening);
    r.reopen(registration()).unwrap();
    world.lock().unwrap().repos.get_mut("spira").unwrap().forge_sha = Some("h1".into());
    tick(&mut r);
    assert_eq!(carried(&r).repos.get("spira").map(|rc| rc.state), None, "Idle is no row: {:?}", carried(&r));
    let asked_before = r.requested().len();
    tick(&mut r);
    assert!(carried(&r).repos.get("spira").is_none_or(|rc| rc.state == State::Idle), "{:?}", carried(&r));
    assert_eq!(asked(&r, PUSH_BRANCH), 1, "nothing is pushed again: {:?}", &r.requested()[asked_before..]);
}

/// A landing opens at once with a round open; only a rerun, a supersede or a CI start waits.
#[test_id(tk5s0gc)]
#[test]
fn a_landing_with_a_round_open_opens_at_once() {
    let (carry, acts) = decide(&Carry::default(), &with(landing(true)));
    assert_eq!(effects(&acts), [PUSH_BRANCH], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opening);
}

/// A wake, the production trigger, recovers a lost push as a tick does.
#[test_id(tp3d8jm)]
#[test]
fn a_wake_after_a_restart_pushes_a_lost_push_again() {
    let world = lost_world(with(landing(false)));
    let mut r = lost_rig(&world, registration, PUSH_BRANCH);
    tick(&mut r);
    r.reopen(registration()).unwrap();
    wake(&mut r);
    assert_eq!(asked(&r, PUSH_BRANCH), 2, "{:?}", r.requested());
}

/// A PR that is gone is retired and the next opened at once, with a round open.
#[test_id(tn6w0cq)]
#[test]
fn a_gone_pr_with_a_round_open_is_retired_and_pushed_at_once() {
    let (carry, acts) = decide(&Carry::default(), &with(Repo { round_holds_hypervisor: true, ..busy().repos["spira"].clone() }));
    assert_eq!(effects(&acts), [RETIRE, PUSH_BRANCH], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opening);
}
