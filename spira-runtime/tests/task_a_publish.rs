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
    budget: 4,
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

/// The publish PR is gone: retire it and open the next.
fn busy() -> Snapshot {
    with(repo(Pr::Gone))
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_opening_x_tick_is_refused_naming_opening() {
    let refused = step(State::Opening, Trigger::Tick).expect_err("opening × tick is illegal");
    assert!(refused.contains("Opening"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// The `decide` table tests (`ops/publish.rs:391`).
#[test]
fn t5_the_settle_table() {
    assert_eq!(settle(Some(&Pr::Gone)), Settle::Retire);
    assert_eq!(settle(Some(&Pr::Open { ci: "pending".into(), behind: false, tries: 0 })), Settle::Wait);
    assert_eq!(settle(Some(&Pr::Open { ci: "red".into(), behind: true, tries: 0 })), Settle::Supersede);
    assert_eq!(settle(Some(&Pr::Open { ci: "untested".into(), behind: false, tries: 0 })), Settle::Rerun);
    assert_eq!(settle(Some(&Pr::Open { ci: "untested".into(), behind: false, tries: 1 })), Settle::Alarm);
}

/// `tests.rs:3808-3888`: a gone PR is retired, then the next one opens.
#[test]
fn t5_gone_retires_then_opens() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert!(effects(&acts).contains(&CANCEL_RUNS) || effects(&acts).contains(&PR_CLOSE), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Superseding);
}

/// Red and behind supersedes: cancel the runs, close the PR.
#[test]
fn t5_red_and_behind_supersedes() {
    let (_, acts) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "red".into(), behind: true, tries: 0 })));
    assert_eq!(effects(&acts), [CANCEL_RUNS, PR_CLOSE], "{acts:?}");
}

/// Untested reruns once; after the rerun it alarms with a record.
#[test]
fn t5_untested_reruns_then_alarms() {
    let (_, first) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "untested".into(), behind: false, tries: 0 })));
    assert_eq!(effects(&first), [RERUN], "{first:?}");
    let (_, then) = decide(&Carry::default(), &with(repo(Pr::Open { ci: "untested".into(), behind: false, tries: 1 })));
    assert!(effects(&then).contains(&NOTIFY) && effects(&then).contains(&RECORD), "{then:?}");
}

/// `:506-527`: a round holding the hypervisor defers the act and counts the tick.
#[test]
fn t5_a_round_on_the_hypervisor_defers() {
    let held = with(Repo { round_holds_hypervisor: true, ..busy().repos["spira"].clone() });
    let (carry, acts) = decide(&Carry::default(), &held);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!((carry.repos["spira"].state, carry.repos["spira"].deferred_ticks), (State::Deferred, 1));
}

/// The record a PR's creation writes is the one queue's `settle_publish` reads: every field it
/// refuses a record without is there, and the red path (`settle_publish_red`, `kv.members()`)
/// finds the range's members.
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
