use serde_json::json;
use state_store::{Object, Txn};

use super::*;
use crate::outcome::Kind;
use crate::ports::Fields;
use crate::sim::bench::Bench;
use crate::sim::git::GitSeed;
use crate::sim::verbs::{Refuse, VerbScript};
use crate::sim::{FakeVerbs, FakeVm};
use runtime::ports::Clock;

fn ok(verb: &str, batch: &str) -> VerbScript {
    VerbScript {
        verb: verb.into(),
        repo: None,
        batch: Some(batch.into()),
        ok: Some(Fields::new()),
        refuse: None,
        lock_hold_ms: 0,
    }
}

fn refuse(batch: &str, code: Option<i32>, text: &str) -> VerbScript {
    VerbScript {
        verb: "land".into(),
        repo: None,
        batch: Some(batch.into()),
        ok: None,
        refuse: Some(Refuse {
            code,
            text: text.into(),
        }),
        lock_hold_ms: 0,
    }
}

/// A bench whose lane is on `base-0`, with git holding `base-0` and `scripts` for the queue.
fn bench(scripts: Vec<VerbScript>) -> Bench {
    let mut b = Bench::new();
    let profile = crate::sim::profiles().unwrap().remove(0);
    let vm = FakeVm::new(vec![], profile, b.clock.clone());
    b.verbs = FakeVerbs::new(scripts, vm, b.clock.clone())
        .unwrap()
        .with_store(b.store.clone())
        .with_git(b.git.clone());
    b.git.seed(&GitSeed {
        repo: "lane".into(),
        landing: "base-0".into(),
        history: vec!["old-0".into()],
    });
    b.seed(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "d", "harness_version": "h" }),
    );
    b
}

fn round(b: &Bench, id: &str, state: &str, head: &str, tested_base: &str, parent: Option<&str>) {
    b.seed(
        &round_key("lane", id),
        json!({ "batch_id": id, "repo": "lane", "state": state, "head": head, "base": tested_base,
                "parent": parent, "pass": 1, "version": 3, "tested_base": tested_base,
                "members": [{ "id": format!("sp-{id}"), "tip": "t1" }], "worktree": "/wt" }),
    );
}

fn put(b: &Bench, key: &str, data: serde_json::Value) {
    let head = b.store.head().unwrap();
    b.store
        .txn(
            &Txn::new().put(key, Object::new(data)),
            &state_store::Request::new(&format!("t-{key}-{head}"), "t", "t"),
        )
        .unwrap();
}

fn state(b: &Bench, id: &str) -> BatchState {
    RoundView::read(&*b.store, "lane", id)
        .unwrap()
        .unwrap()
        .round
        .row
        .state
}

fn landing_ref(b: &Bench) -> String {
    stack::lane(&b.ports(), "lane")
        .unwrap()
        .unwrap()
        .landing_ref
}

fn status(b: &Bench, id: &str) -> Option<Status> {
    read_status(&b.ports(), &status_key("lane", id)).unwrap()
}

fn lands(b: &Bench) -> usize {
    b.verbs
        .calls()
        .iter()
        .filter(|c| c.verb == Verb::Land)
        .count()
}

#[test]
fn a_green_round_on_the_ref_lands_in_one_txn_that_moves_the_lanes_landing_ref() {
    let b = bench(vec![ok("land", "k1")]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    let got = land(&b.ports(), "lane").unwrap();
    assert_eq!(got, [("k1".into(), Landing::Landed { at: "k1-h".into() })]);
    assert_eq!(state(&b, "k1"), BatchState::Landed);
    assert_eq!(landing_ref(&b), "k1-h");
    let rec = b.store.get("batches/lane/k1").unwrap().unwrap().value.data;
    assert_eq!(
        (
            rec["reason"].as_str(),
            rec["worktree"].as_str(),
            rec["version"].as_u64()
        ),
        (Some("k1-h"), Some("/wt"), Some(4)),
        "the landing is FastForward on the lifecycle row, and the record keeps its own fields"
    );
    let o = b.log.outcomes();
    assert_eq!(o.len(), 1);
    assert_eq!((o[0].kind.word(), o[0].cause.word()), ("landed", "dwell"));
    assert_eq!(
        b.verbs.calls()[0].batch.as_deref(),
        Some("k1"),
        "the land verb is called for the round"
    );
    assert!(
        holder_of(&b.ports(), &Resource::round("lane", "k1"))
            .unwrap()
            .is_none(),
        "the Lander's lease is given back"
    );
}

#[test]
fn a_head_already_on_the_ref_is_recorded_from_git_and_no_land_is_called() {
    let b = bench(vec![]);
    round(&b, "k1", "Green", "old-0", "base-0", None);
    let got = land(&b.ports(), "lane").unwrap();
    assert_eq!(
        got,
        [(
            "k1".into(),
            Landing::FastForward {
                at: "base-0".into()
            }
        )]
    );
    assert_eq!(lands(&b), 0);
    assert_eq!(state(&b, "k1"), BatchState::Landed);
    assert_eq!(landing_ref(&b), "base-0");
    assert_eq!(b.log.outcomes()[0].kind, Kind::FastForward);
}

#[test]
fn a_round_that_is_not_green_is_never_landed_and_a_repo_with_no_lane_is_left_alone() {
    let b = bench(vec![]);
    for (id, s) in [
        ("o", "Open"),
        ("c", "CiRunning"),
        ("a", "Attributing"),
        ("l", "Landing"),
    ] {
        round(&b, id, s, &format!("{id}-h"), "base-0", None);
    }
    assert_eq!(land(&b.ports(), "lane").unwrap(), []);
    assert_eq!(land(&b.ports(), "nowhere").unwrap(), []);
    assert!(b.verbs.calls().is_empty() && b.log.outcomes().is_empty());
}

#[test]
fn a_round_stacked_on_a_live_round_is_held_from_the_store_without_a_call_until_its_parent_lands() {
    let b = bench(vec![ok("land", "k2")]);
    round(&b, "k1", "Attributing", "k1-h", "base-0", None);
    round(&b, "k2", "Green", "k2-h", "k1-h", Some("k1"));
    let got = land(&b.ports(), "lane").unwrap();
    let held = Blocker::StackedOnUnlanded {
        parent: "k1".into(),
        parent_state: Some("ATTRIBUTING".into()),
        parent_leased: false,
    };
    assert_eq!(got, [("k2".into(), Landing::Held(held.clone()))]);
    assert_eq!(lands(&b), 0);
    let s = status(&b, "k2").unwrap();
    assert_eq!(s.blocker, held);
    assert_eq!(
        (s.inputs.base.as_str(), s.inputs.parent_head.as_deref()),
        ("base-0", Some("k1-h"))
    );
    assert_eq!(state(&b, "k2"), BatchState::Green, "held, never abandoned");

    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k2".into(), Landing::Unchanged)]
    );
    assert_eq!(
        b.log.outcomes().len(),
        1,
        "an unchanged hold is not told again"
    );

    let mut k1 = b.store.get("batches/lane/k1").unwrap().unwrap().value.data;
    k1["state"] = json!("Landed");
    put(&b, "batches/lane/k1", k1);
    put(
        &b,
        "lanes/lane",
        json!({ "landing_ref": "k1-h", "image_digest": "d", "harness_version": "h" }),
    );
    b.git.advance("lane", "k1-h");
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k2".into(), Landing::Landed { at: "k2-h".into() })]
    );
    assert_eq!(status(&b, "k2"), None, "a landing clears the blocker");
    assert_eq!(landing_ref(&b), "k2-h");
}

#[test]
fn a_parent_and_its_child_land_in_one_pass_lowest_first() {
    let b = bench(vec![ok("land", "k2"), ok("land", "k1")]);
    round(&b, "k2", "Green", "k2-h", "k1-h", Some("k1"));
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    let got = land(&b.ports(), "lane").unwrap();
    assert_eq!(
        got.iter().map(|(b, _)| b.as_str()).collect::<Vec<_>>(),
        ["k1", "k2"]
    );
    assert_eq!(landing_ref(&b), "k2-h");
    assert_eq!(state(&b, "k2"), BatchState::Landed);
}

#[test]
fn a_refusal_is_classified_by_its_first_word_and_a_held_round_is_retried_only_on_a_changed_input() {
    let b = bench(vec![refuse(
        "k1",
        Some(1),
        "refused: the gate is red at k1-h\n",
    )]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    let refused = Blocker::Refused {
        text: "refused: the gate is red at k1-h".into(),
    };
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k1".into(), Landing::Held(refused.clone()))]
    );
    for _ in 0..3 {
        assert_eq!(
            land(&b.ports(), "lane").unwrap(),
            [("k1".into(), Landing::Unchanged)]
        );
    }
    assert_eq!(
        lands(&b),
        1,
        "no second call while nothing it was refused on changed"
    );
    assert_eq!(state(&b, "k1"), BatchState::Green);

    put(
        &b,
        "lanes/lane",
        json!({ "landing_ref": "base-1", "image_digest": "d", "harness_version": "h" }),
    );
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [],
        "a round no longer grounded is supersede's, not the Lander's"
    );
}

#[test]
fn a_changed_input_retries_a_held_round_and_its_first_green_sighting_is_kept() {
    let b = bench(vec![
        refuse("k1", Some(1), "refused: no"),
        refuse("k1", Some(1), "refused: still no"),
    ]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    land(&b.ports(), "lane").unwrap();
    let seen = status(&b, "k1").unwrap().green_seen_ms;
    let mut k1 = b.store.get("batches/lane/k1").unwrap().unwrap().value.data;
    k1["parent"] = json!("gone");
    put(&b, "batches/lane/k1", k1);
    b.clock.advance_to(b.clock.now_ms() + 60_000);
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [(
            "k1".into(),
            Landing::Held(Blocker::Refused {
                text: "refused: still no".into()
            })
        )]
    );
    let s = status(&b, "k1").unwrap();
    assert_eq!(
        (
            s.green_seen_ms,
            s.inputs.parent.as_deref(),
            s.inputs.parent_state
        ),
        (seen, Some("gone"), None)
    );
    assert_eq!(lands(&b), 2);
}

#[test]
fn a_stacked_refusal_holds_naming_the_parent_and_an_unknown_word_holds_as_refused() {
    let b = bench(vec![
        refuse(
            "k1",
            Some(1),
            "stacked: round k1 is on round k0, which has not landed",
        ),
        refuse("k2", Some(1), "the queue lock is busy"),
    ]);
    round(&b, "k1", "Green", "k1-h", "base-0", Some("k0"));
    round(&b, "k2", "Green", "k2-h", "base-0", None);
    let got = land(&b.ports(), "lane").unwrap();
    assert_eq!(
        got,
        [
            (
                "k1".into(),
                Landing::Held(Blocker::StackedOnUnlanded {
                    parent: "k0".into(),
                    parent_state: None,
                    parent_leased: false
                })
            ),
            (
                "k2".into(),
                Landing::Held(Blocker::Refused {
                    text: "the queue lock is busy".into()
                })
            )
        ]
    );
    let words: Vec<_> = b.log.outcomes().iter().map(|o| o.cause.word()).collect();
    assert_eq!(words, ["stacked-on-unlanded", "refused"]);
}

#[test]
fn base_moved_supersedes_the_round_through_the_verb_and_never_abandons_it() {
    let b = bench(vec![
        refuse(
            "k1",
            Some(1),
            "base-moved: local/main is at base-1, not base-0",
        ),
        ok("supersede", "k1"),
    ]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k1".into(), Landing::Superseded)]
    );
    let verbs: Vec<_> = b.verbs.calls().iter().map(|c| c.verb).collect();
    assert_eq!(verbs, [Verb::Land, Verb::Supersede]);
    assert!(b.store.get("batches/lane/k1").unwrap().is_none());
    assert_eq!(b.log.outcomes()[0].kind.word(), "superseded");

    let b = bench(vec![refuse("k1", Some(1), "base-moved")]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    assert!(matches!(
        land(&b.ports(), "lane"),
        Err(Refusal::Verb { refusal, .. }) if refusal.verb == Verb::Supersede
    ));
}

#[test]
fn a_deploy_fault_is_distinct_from_a_land_failure_landed_when_the_ref_moved_held_when_it_did_not() {
    let b = bench(vec![refuse(
        "k1",
        Some(DEPLOY_FAULT_EXIT),
        "activate failed\n",
    )]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    let fault = Blocker::DeployFault {
        why: "activate failed".into(),
    };
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k1".into(), Landing::Held(fault.clone()))],
        "the ref did not move, so the round stays GREEN"
    );
    assert_eq!(state(&b, "k1"), BatchState::Green);

    let b = bench(vec![refuse(
        "k2",
        Some(DEPLOY_FAULT_EXIT),
        "activate failed",
    )]);
    round(&b, "k2", "Green", "k2-h", "base-0", None);
    let p = b.ports();
    // The queue pushed before its activation faulted.
    struct Pushed<'a>(&'a Bench);
    impl crate::ports::RoundVerbs for Pushed<'_> {
        fn open(&self, r: &crate::ports::OpenReq) -> Result<crate::ports::Opened, VerbRefusal> {
            self.0.verbs.open(r)
        }
        fn status(&self, repo: &str) -> Result<crate::ports::Status, VerbRefusal> {
            self.0.verbs.status(repo)
        }
        fn certify(
            &self,
            repo: &str,
            batch: &str,
            t: Option<&str>,
        ) -> Result<Box<dyn crate::ports::Child>, VerbRefusal> {
            self.0.verbs.certify(repo, batch, t)
        }
        fn eject(&self, r: &crate::ports::EjectReq) -> Result<crate::ports::Ejected, VerbRefusal> {
            self.0.verbs.eject(r)
        }
        fn land(
            &self,
            repo: &str,
            batch: &str,
            t: Option<&str>,
        ) -> Result<crate::ports::Done, VerbRefusal> {
            self.0.git.advance(repo, "k2-h");
            self.0.verbs.land(repo, batch, t)
        }
        fn abandon(
            &self,
            repo: &str,
            batch: &str,
            why: &str,
        ) -> Result<crate::ports::Done, VerbRefusal> {
            self.0.verbs.abandon(repo, batch, why)
        }
        fn pass_start(&self, repo: &str, batch: &str) -> Result<crate::ports::Done, VerbRefusal> {
            self.0.verbs.pass_start(repo, batch)
        }
        fn supersede(
            &self,
            repo: &str,
            batch: &str,
            t: Option<&str>,
        ) -> Result<crate::ports::Superseded, VerbRefusal> {
            self.0.verbs.supersede(repo, batch, t)
        }
    }
    let pushed = Pushed(&b);
    let p = Ports {
        verbs: &pushed,
        ..p
    };
    assert_eq!(
        land(&p, "lane").unwrap(),
        [("k2".into(), Landing::Landed { at: "k2-h".into() })]
    );
    assert_eq!(state(&b, "k2"), BatchState::Landed);
    assert_eq!(landing_ref(&b), "k2-h");
    assert_eq!(
        status(&b, "k2").unwrap().blocker,
        Blocker::DeployFault {
            why: "activate failed".into()
        },
        "the landing stands and the fault is kept beside it"
    );
    assert_eq!(b.log.outcomes()[0].cause.word(), "deploy-fault");
}

#[test]
fn a_land_that_never_ran_stores_nothing_so_the_next_pass_tries_again() {
    let b = bench(vec![refuse("k1", None, "queue exceeded its 120s deadline")]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    assert!(matches!(
        land(&b.ports(), "lane"),
        Err(Refusal::Verb { refusal, .. }) if refusal.code.is_none()
    ));
    assert_eq!(status(&b, "k1"), None);
    step(&b.ports(), "lane");
    assert_eq!(lands(&b), 2, "the step tried it again");
}

#[test]
fn a_round_leased_by_another_holder_is_left_to_them_and_an_unreadable_git_lands_nothing() {
    let b = bench(vec![]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    let theirs = acquire(&b.ports(), &Resource::round("lane", "k1"), "runner").unwrap();
    assert_eq!(
        land(&b.ports(), "lane").unwrap(),
        [("k1".into(), Landing::Leased)]
    );
    theirs.release(&b.ports()).unwrap();

    b.git.break_repo("lane", "index.lock exists");
    assert_eq!(
        land(&b.ports(), "lane"),
        Err(Refusal::Unreadable {
            key: git_key("lane"),
            text: "index.lock exists".into()
        })
    );
    assert!(b.verbs.calls().is_empty());
    assert_eq!(state(&b, "k1"), BatchState::Green);
}

#[test]
fn a_store_that_cannot_answer_is_a_refusal_naming_the_key() {
    let b = Bench::unserved();
    assert!(matches!(
        land(&b.ports(), "lane"),
        Err(Refusal::Unreadable { key, .. }) if key == "lanes/lane"
    ));
    let b = bench(vec![]);
    b.seed("batches/lane/k1/status", json!({ "blocker": "nonsense" }));
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    assert!(matches!(
        land(&b.ports(), "lane"),
        Err(Refusal::Unreadable { key, .. }) if key == "batches/lane/k1/status"
    ));
}

#[test]
fn a_landing_whose_records_keep_moving_is_refused_after_its_tries() {
    let b = bench(vec![ok("land", "k1")]);
    round(&b, "k1", "Green", "k1-h", "base-0", None);
    // The lease's acquire and token write, then the landing's txns lose their CAS.
    b.racing_writes_after("lanes/lane", 2);
    let r = land(&b.ports(), "lane");
    assert!(
        matches!(&r, Err(Refusal::Unreadable { text, .. }) if text.contains("moved under 3 tries")),
        "{r:?}"
    );
}

#[test]
fn a_landed_value_is_the_fast_forward_of_a_green_row_and_a_landed_one_stays_landed() {
    let mut v = json!({ "batch_id": "k1", "repo": "lane", "state": "Green", "head": "h", "pass": 1,
                        "version": 2, "extra": 1 });
    landed_value(&mut v, "h").unwrap();
    assert_eq!(
        (v["state"].as_str(), v["extra"].as_u64()),
        (Some("Landed"), Some(1))
    );
    let again = v.clone();
    landed_value(&mut v, "h").unwrap();
    assert_eq!(v, again);
    let mut open =
        json!({ "batch_id": "k1", "repo": "lane", "state": "Open", "pass": 1, "version": 2 });
    assert!(landed_value(&mut open, "h")
        .unwrap_err()
        .contains("FastForward refused"));
    assert!(landed_value(&mut json!(3), "h").is_err());
}

#[test]
fn every_blocker_has_its_own_word() {
    let all = [
        Blocker::StackedOnUnlanded {
            parent: "k".into(),
            parent_state: None,
            parent_leased: true,
        },
        Blocker::Refused { text: "t".into() },
        Blocker::DeployFault { why: "w".into() },
    ];
    let words: Vec<_> = all.iter().map(Blocker::word).collect();
    assert_eq!(words, Blocker::WORDS);
    let s = Status {
        blocker: all[0].clone(),
        inputs: Inputs {
            head: "h".into(),
            base: "b".into(),
            parent: None,
            parent_state: None,
            parent_head: None,
        },
        green_seen_ms: 5,
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["blocker"], "stacked-on-unlanded");
    assert_eq!(serde_json::from_value::<Status>(v).unwrap(), s);
}
