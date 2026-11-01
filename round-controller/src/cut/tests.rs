use std::sync::{Arc, Mutex};

use runtime::ports::{SeededRng, SysProc, VirtualClock};
use serde_json::json;
use state_store::{Config, Contract, MemBackend, Object, Request, Txn};

use super::*;
use crate::ledger::{mark_proven, record_attempt, FaultPolicy};
use crate::outcome::{Kind, MemLog};
use crate::ports::Fields;
use crate::refusal::standing_key;
use crate::resources::{Attempt, Cause, Class, InitiatedBy};
use crate::sim::verbs::{Refuse, VerbScript};
use crate::sim::{FakeVerbs, FakeVm};

struct World {
    clock: VirtualClock,
    store: Arc<Contract<MemBackend>>,
    rng: Mutex<SeededRng>,
    log: MemLog,
    verbs: FakeVerbs,
    slot: SlotGrant,
    units: crate::sim::bench::FakeUnits,
    vms: crate::sim::bench::FakeVms,
    launch: crate::sim::bench::FakeLaunch,
    leases: crate::liveness::LeaseSettings,
    slots: crate::slots::SlotSettings,
    ledger: crate::sim::judge::FakeLedger,
}

const POLICY: FaultPolicy = FaultPolicy {
    backoff_secs: 60,
    retries: 1,
};

fn open_ok(batch: &str) -> VerbScript {
    VerbScript {
        verb: "open".into(),
        repo: None,
        ok: Some(Fields::from([
            ("batch".to_string(), batch.to_string()),
            ("head".to_string(), format!("{batch}-head")),
        ])),
        refuse: None,
        lock_hold_ms: 0,
    }
}

impl World {
    fn new(scripts: Vec<VerbScript>) -> World {
        let clock = VirtualClock::new(1_800_000_000_000);
        let store = Arc::new(
            Contract::open(
                MemBackend::new(),
                state_store::Ports::virtual_clock(&clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .unwrap(),
        );
        let profile = crate::sim::profiles().unwrap().remove(0);
        let vm = FakeVm::new(vec![], profile, clock.clone());
        let verbs = FakeVerbs::new(scripts, vm, clock.clone())
            .unwrap()
            .with_store(store.clone());
        let slot = crate::slots::Slot {
            pool: crate::slots::Pool::Proxmox,
            n: 0,
        };
        let fence = store
            .lock(&slot.lock(), 600_000, &Request::new("slot", "t", "l"))
            .unwrap();
        let w = World {
            vms: crate::sim::bench::FakeVms::new(clock.clone()),
            clock,
            store,
            rng: Mutex::new(SeededRng::new(1)),
            log: MemLog::default(),
            verbs,
            slot: SlotGrant::of_fence(slot, fence),
            units: crate::sim::bench::FakeUnits::default(),
            launch: crate::sim::bench::FakeLaunch::default(),
            leases: crate::sim::bench::bench_settings(),
            slots: crate::sim::bench::bench_slot_settings(),
            ledger: crate::sim::judge::FakeLedger::default(),
        };
        w.put(
            "lanes/lane",
            json!({ "landing_ref": "base-0", "image_digest": "sha256:i0", "harness_version": "h0" }),
        );
        w
    }

    fn ports(&self) -> Ports<'_> {
        Ports {
            clock: &self.clock,
            proc_: &SysProc,
            rng: &self.rng,
            store: &*self.store,
            verbs: &self.verbs,
            ledger: &self.ledger,
            log: &self.log,
            units: &self.units,
            vms: &self.vms,
            launcher: &self.launch,
            leases: &self.leases,
            slots: &self.slots,
        }
    }

    fn put(&self, key: &str, data: serde_json::Value) {
        let head = self.store.head().unwrap();
        self.store
            .txn(
                &Txn::new().put(key, Object::new(data)),
                &Request::new(&format!("put-{key}-{head}"), "t", "t"),
            )
            .unwrap();
    }

    fn member(&self, id: &str, extra: serde_json::Value) {
        let mut v = json!({ "repo": "lane", "state": "CERTIFIED", "tip": format!("{id}-t") });
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        self.put(&format!("lifecycle/{id}"), v);
    }

    fn round(&self, batch: &str, state: &str, tested_base: &str, members: &[&str]) {
        let ms: Vec<_> = members
            .iter()
            .map(|m| json!({ "id": m, "tip": format!("{m}-t") }))
            .collect();
        self.put(
            &format!("batches/lane/{batch}"),
            json!({ "batch_id": batch, "repo": "lane", "state": state, "head": format!("{batch}-head"),
                    "pass": 1, "version": 1, "tested_base": tested_base, "members": ms }),
        );
    }

    fn cut(&self) -> Result<Option<Opened>, Refusal> {
        cut(&self.ports(), "lane", &self.slot)
    }

    fn opened_members(&self) -> Vec<String> {
        let r = crate::resources::RoundView::read(&*self.store, "lane", "r1")
            .unwrap()
            .unwrap();
        r.round.members.into_iter().map(|m| m.id).collect()
    }
}

fn mem(id: &str) -> Member {
    Member {
        id: id.into(),
        tip: format!("{id}-t"),
    }
}

fn ident(members: &[&str], base: &str) -> PassIdentity {
    PassIdentity::new(
        "lane",
        members.iter().map(|m| mem(m)).collect(),
        base,
        "sha256:i0",
        "h0",
        "all",
    )
    .unwrap()
}

fn attempt(class: Class) -> Attempt {
    Attempt {
        class,
        initiated_by: InitiatedBy::Planner,
        sub_passes: vec![],
        evidence: String::new(),
        at: 1_800_000_000_000,
    }
}

#[test]
fn with_its_slot_held_a_cut_opens_the_pool_on_the_landing_ref() {
    let w = World::new(vec![open_ok("r1")]);
    w.member("sp-a", json!({}));
    w.member("sp-b", json!({ "state": "SUBMITTED" }));
    w.member("sp-other", json!({ "repo": "elsewhere" }));
    w.member("sp-working", json!({ "state": "WORKING" }));
    w.member("sp-held", json!({ "holds": ["operator"] }));
    w.member("sp-ejected", json!({ "ejected_red_tip": "sp-ejected-t" }));
    w.member("sp-tipless", json!({ "tip": "" }));
    let o = w.cut().unwrap().unwrap();
    assert_eq!(o.batch, "r1");
    assert_eq!(w.opened_members(), ["sp-a", "sp-b"]);
    let call = &w.verbs.calls()[0];
    assert_eq!(call.place.as_deref(), Some("on-base"));
    let r = crate::resources::RoundView::read(&*w.store, "lane", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(r.round.tested_base.as_deref(), Some("base-0"));
}

#[test]
fn a_member_of_a_live_round_is_not_in_the_pool_and_an_empty_pool_cuts_nothing() {
    let w = World::new(vec![]);
    w.member("sp-a", json!({}));
    w.round("k1", "CiRunning", "base-0", &["sp-a"]);
    assert_eq!(w.cut(), Ok(None));
    assert!(w.verbs.calls().is_empty());
}

#[test]
fn a_cut_stacks_on_the_top_live_round_and_cuts_nothing_once_the_stack_is_at_its_depth() {
    let w = World::new(vec![open_ok("r1")]);
    w.round("k1", "CiRunning", "base-0", &[]);
    w.member("sp-a", json!({}));
    w.cut().unwrap().unwrap();
    assert_eq!(w.verbs.calls()[0].place.as_deref(), Some("stack-on:k1"));
    let r = crate::resources::RoundView::read(&*w.store, "lane", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(r.round.tested_base.as_deref(), Some("k1-head"));
    assert_eq!(r.round.row.parent.as_deref(), Some("k1"));
    w.member("sp-b", json!({}));
    assert_eq!(w.cut(), Ok(None), "k1 and r1 make the default depth 2");
    w.put(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "sha256:i0", "harness_version": "h0", "concurrency": 1 }),
    );
    w.store
        .txn(
            &Txn::new().delete("batches/lane/r1"),
            &Request::new("drop-r1", "t", "t"),
        )
        .unwrap();
    assert_eq!(w.cut(), Ok(None), "depth 1 is full with k1 alone");
}

#[test]
fn a_terminal_round_is_not_live_and_holds_no_member() {
    let w = World::new(vec![open_ok("r1")]);
    w.round("k0", "Landed", "base-0", &["sp-a"]);
    w.member("sp-a", json!({}));
    w.cut().unwrap().unwrap();
    assert_eq!(w.verbs.calls()[0].place.as_deref(), Some("on-base"));
}

#[test]
fn a_cut_on_a_slot_it_does_not_hold_is_slot_not_held() {
    let w = World::new(vec![]);
    w.member("sp-a", json!({}));
    w.store
        .release(w.slot.fence(), &Request::new("rel", "t", "r"))
        .unwrap();
    assert_eq!(
        w.cut(),
        Err(Refusal::SlotNotHeld {
            key: "locks/runner-slot/proxmox/0".into()
        })
    );
}

#[test]
fn a_repo_with_no_lane_record_is_lane_unknown() {
    let w = World::new(vec![]);
    let r = cut(&w.ports(), "nowhere", &w.slot);
    assert_eq!(
        r,
        Err(Refusal::LaneUnknown {
            key: "lanes/nowhere".into()
        })
    );
    w.put("lanes/broken", json!({ "landing_ref": 1 }));
    let r = cut(&w.ports(), "broken", &w.slot).unwrap_err();
    assert!(
        matches!(&r, Refusal::Unreadable { key, .. } if key == "lanes/broken"),
        "{r}"
    );
}

#[test]
fn an_undecodable_round_is_unreadable_and_its_child_keys_are_not_rounds() {
    let w = World::new(vec![open_ok("r1")]);
    w.member("sp-a", json!({}));
    w.put("batches/lane/k1/progress", json!({ "anything": 1 }));
    w.cut().unwrap().unwrap();
    w.put("batches/lane/k9", json!({ "state": 3 }));
    let r = w.cut().unwrap_err();
    assert!(
        matches!(&r, Refusal::Unreadable { key, .. } if key == "batches/lane/k9"),
        "{r}"
    );
    assert_eq!(r.word(), "unreadable");
    assert!(
        r.to_string().starts_with("unreadable batches/lane/k9: "),
        "{r}"
    );
}

#[test]
fn a_standing_lane_fault_refuses_before_selecting_and_a_new_digest_admits() {
    let w = World::new(vec![open_ok("r1")]);
    w.member("sp-a", json!({}));
    w.put(
        "faults/lane/base-0/sha256:i0",
        json!({ "cause": { "cause": "base" }, "since": 0 }),
    );
    assert_eq!(
        w.cut(),
        Err(Refusal::LaneFault {
            key: "faults/lane/base-0/sha256:i0".into()
        })
    );
    w.put(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "sha256:i1", "harness_version": "h0" }),
    );
    assert!(w.cut().unwrap().is_some());
}

#[test]
fn a_member_proven_red_at_its_tip_is_dropped_and_the_rest_reselected() {
    let w = World::new(vec![open_ok("r1")]);
    for m in ["sp-a", "sp-b", "sp-c"] {
        w.member(m, json!({}));
    }
    let red = ident(&["sp-a", "sp-b"], "base-0");
    let p = w.ports();
    record_attempt(
        &p,
        &red,
        "old",
        &attempt(Class::Red { step: "s".into() }),
        &POLICY,
    )
    .unwrap();
    mark_proven(&p, &red, &[mem("sp-b")], 0).unwrap();
    w.cut().unwrap().unwrap();
    assert_eq!(w.opened_members(), ["sp-a", "sp-c"]);
}

#[test]
fn a_red_identity_and_a_backed_off_one_open_nothing() {
    let w = World::new(vec![]);
    w.member("sp-a", json!({}));
    let id = ident(&["sp-a"], "base-0");
    let p = w.ports();
    record_attempt(
        &p,
        &id,
        "old",
        &attempt(Class::Red { step: "s".into() }),
        &POLICY,
    )
    .unwrap();
    assert_eq!(
        w.cut(),
        Err(Refusal::RedIdentity {
            key: summary_key(&id)
        })
    );
    let w = World::new(vec![]);
    w.member("sp-a", json!({}));
    let p = w.ports();
    record_attempt(
        &p,
        &id,
        "old",
        &attempt(Class::Fault { cause: Cause::Vm }),
        &POLICY,
    )
    .unwrap();
    assert_eq!(
        w.cut(),
        Err(Refusal::BackingOff {
            key: summary_key(&id)
        })
    );
    assert!(w.verbs.calls().is_empty());
}

#[test]
fn an_open_the_queue_refuses_is_a_verb_refusal_keyed_on_its_code() {
    let w = World::new(vec![VerbScript {
        verb: "open".into(),
        repo: None,
        ok: None,
        refuse: Some(Refuse {
            code: Some(3),
            text: "locked".into(),
        }),
        lock_hold_ms: 0,
    }]);
    w.member("sp-a", json!({}));
    let r = w.cut().unwrap_err();
    assert_eq!(
        (r.word(), r.key()),
        ("verb-refused", "verb/open/-/3".to_string())
    );
    assert_eq!(r.to_string(), "queue round open refused: locked");
}

#[test]
fn a_cut_at_max_members_plus_one_drops_the_last_member_in_round_order() {
    let w = World::new(vec![open_ok("r1")]);
    for i in 0..=MAX_MEMBERS {
        w.member(&format!("sp-{i:03}"), json!({ "since": i }));
    }
    w.cut().unwrap().unwrap();
    let got = w.opened_members();
    assert_eq!(got.len(), MAX_MEMBERS);
    assert_eq!(got.last().unwrap(), &format!("sp-{:03}", MAX_MEMBERS - 1));
    assert!(!got.contains(&format!("sp-{MAX_MEMBERS:03}")));
}

#[test]
fn a_member_never_judged_red_is_ordered_exactly_as_select_round_orders_it() {
    let w = World::new(vec![open_ok("r1")]);
    w.member("sp-z", json!({ "since": 1 }));
    w.member("sp-y", json!({ "since": 2, "priority": 0 }));
    w.member("sp-x", json!({ "since": 3, "express": true }));
    w.member("sp-w", json!({ "since": 0, "stack": { "sp-z": "sp-z-t" } }));
    let pool = pool(&w.ports(), "lane", &[]).unwrap();
    let (want, _) = core::select_round(core::topo_order(&pool));
    w.cut().unwrap().unwrap();
    let want: Vec<String> = want.into_iter().map(|m| m.id).collect();
    assert_eq!(w.opened_members(), want);
    assert_eq!(want, ["sp-x", "sp-y", "sp-z", "sp-w"]);
}

#[test]
fn a_prerequisite_in_an_unleased_round_makes_its_dependents_wait() {
    let w = World::new(vec![]);
    w.round("k1", "CiRunning", "base-0", &["sp-a"]);
    w.member("sp-b", json!({ "blocked_by": ["sp-a"] }));
    w.member("sp-c", json!({ "stack": { "sp-a": "sp-a-t" } }));
    assert_eq!(w.cut(), Ok(None));
}

#[test]
fn a_prerequisite_in_a_leased_round_below_rides_and_a_stale_stack_tip_waits() {
    let w = World::new(vec![open_ok("r1")]);
    w.round("k1", "CiRunning", "base-0", &["sp-a"]);
    w.store
        .lock("round/lane/k1", 600_000, &Request::new("lease", "t", "l"))
        .unwrap();
    w.member("sp-b", json!({ "blocked_by": ["sp-a"] }));
    w.member("sp-c", json!({ "stack": { "sp-a": "sp-a-t" } }));
    w.member("sp-d", json!({ "stack": { "sp-a": "older" } }));
    w.member("sp-e", json!({ "blocked_by": ["sp-d"] }));
    w.cut().unwrap().unwrap();
    assert_eq!(w.verbs.calls()[0].place.as_deref(), Some("stack-on:k1"));
    assert_eq!(w.opened_members(), ["sp-b", "sp-c"]);
}

#[test]
fn an_explicit_cut_opens_exactly_its_members_and_refuses_a_judged_red_one() {
    let w = World::new(vec![open_ok("r1")]);
    let p = w.ports();
    let o = explicit(&p, "lane", vec![mem("sp-q")]).unwrap();
    assert_eq!(o.batch, "r1");
    assert_eq!(w.opened_members(), ["sp-q"]);
    let red = ident(&["sp-a", "sp-b"], "r1-head");
    record_attempt(
        &p,
        &red,
        "old",
        &attempt(Class::Red { step: "s".into() }),
        &POLICY,
    )
    .unwrap();
    assert_eq!(
        explicit(&p, "lane", vec![mem("sp-b"), mem("sp-a")]),
        Err(Refusal::CutOfJudgedRed {
            key: summary_key(&red)
        }),
        "the identity on the base it would be tested on: r1's head, on which it stacks"
    );
    let red = ident(&["sp-a", "sp-b"], "base-0");
    let w = World::new(vec![]);
    let p = w.ports();
    record_attempt(
        &p,
        &red,
        "old",
        &attempt(Class::Red { step: "s".into() }),
        &POLICY,
    )
    .unwrap();
    mark_proven(&p, &red, &[mem("sp-a")], 0).unwrap();
    assert_eq!(
        explicit(&p, "lane", vec![mem("sp-a"), mem("sp-c")]),
        Err(Refusal::CutOfJudgedRed {
            key: "verdicts/lane/by-member/sp-a/sp-a-t".into()
        })
    );
    assert_eq!(
        explicit(&p, "lane", vec![]),
        Err(Refusal::BadMembers {
            key: "no-members".into()
        })
    );
}

#[test]
fn an_explicit_cut_on_a_full_stack_or_a_faulted_lane_is_refused_by_name() {
    let w = World::new(vec![]);
    let p = w.ports();
    w.put(
        "faults/lane/base-0/sha256:i0",
        json!({ "cause": { "cause": "base" }, "since": 0 }),
    );
    assert_eq!(
        explicit(&p, "lane", vec![mem("sp-a")]),
        Err(Refusal::LaneFault {
            key: "faults/lane/base-0/sha256:i0".into()
        })
    );
    w.round("k1", "CiRunning", "base-0", &[]);
    w.round("k2", "CiRunning", "k1-head", &[]);
    assert_eq!(
        explicit(&p, "lane", vec![mem("sp-a")]),
        Err(Refusal::StackFull {
            key: "lanes/lane/depth/2".into()
        })
    );
    assert_eq!(
        explicit(&p, "nowhere", vec![mem("sp-a")]),
        Err(Refusal::LaneUnknown {
            key: "lanes/nowhere".into()
        })
    );
}

#[test]
fn a_repeated_refusal_is_told_once_a_new_one_replaces_it_and_an_open_clears_it() {
    let w = World::new(vec![open_ok("r1")]);
    let p = w.ports();
    let fault = || {
        Err(Refusal::LaneFault {
            key: "faults/lane/b/d".into(),
        })
    };
    for _ in 0..3 {
        record(&p, "lane", &fault());
    }
    record(&p, "lane", &Ok(None));
    let lines: Vec<String> = w.log.outcomes().iter().map(|o| o.line()).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with("declined lane-fault repo=lane batch=- key=faults/lane/b/d"),
        "{lines:?}"
    );
    let skey = standing_key("lane", STEP);
    assert!(
        w.store.get(&skey).unwrap().is_some(),
        "nothing to cut leaves it"
    );
    record(
        &p,
        "lane",
        &Err(Refusal::SlotNotHeld {
            key: "locks/runner-slot/proxmox/0".into(),
        }),
    );
    assert_eq!(w.log.outcomes().len(), 2);
    let opened = Opened {
        batch: "r1".into(),
        head: "h".into(),
        members: String::new(),
    };
    record(&p, "lane", &Ok(Some(opened)));
    let last = w.log.outcomes().pop().unwrap();
    assert_eq!(last.kind.word(), "opened");
    assert_eq!(last.batch.as_deref(), Some("r1"));
    assert!(w.store.get(&skey).unwrap().is_none(), "an open clears it");
    let refused = Err(Refusal::Verb {
        batch: None,
        refusal: crate::ports::VerbRefusal {
            verb: Verb::Open,
            code: None,
            text: "deadline".into(),
        },
    });
    record(&p, "lane", &refused);
    record(&p, "lane", &refused);
    let outs = w.log.outcomes();
    assert_eq!(outs.len(), 4);
    assert_eq!(outs[3].kind, Kind::Refused(Verb::Open));
    assert!(
        outs[3].line().contains("refused not-run"),
        "{}",
        outs[3].line()
    );
}

#[test]
fn every_refusal_has_its_own_word_and_a_key() {
    let k = || "k".to_string();
    let all = [
        Refusal::SlotNotHeld { key: k() },
        Refusal::LaneUnknown { key: k() },
        Refusal::LaneFault { key: k() },
        Refusal::RedIdentity { key: k() },
        Refusal::BackingOff { key: k() },
        Refusal::CutOfJudgedRed { key: k() },
        Refusal::StackFull { key: k() },
        Refusal::BadMembers { key: k() },
        Refusal::Unreadable {
            key: k(),
            text: "t".into(),
        },
    ];
    let words: std::collections::BTreeSet<&str> = all.iter().map(Refusal::word).collect();
    assert_eq!(words.len(), all.len());
    assert!(all.iter().all(|r| r.key() == "k"));
    assert_eq!(all[2].to_string(), "lane-fault k");
}

#[test]
fn a_store_that_cannot_answer_is_unreadable_at_every_read() {
    let w = World::new(vec![]);
    let p = Ports {
        store: &crate::real::Unserved,
        ..w.ports()
    };
    assert!(matches!(
        cut(&p, "lane", &w.slot),
        Err(Refusal::Unreadable { .. })
    ));
    assert!(matches!(
        explicit(&p, "lane", vec![mem("sp-a")]),
        Err(Refusal::Unreadable { .. })
    ));
}

fn supersede_ok() -> VerbScript {
    VerbScript {
        verb: "supersede".into(),
        repo: None,
        ok: Some(Fields::from([("released".to_string(), "x".to_string())])),
        refuse: None,
        lock_hold_ms: 0,
    }
}

#[test]
fn supersede_takes_every_round_that_can_no_longer_land_and_leaves_the_grounded_stack() {
    let w = World::new(vec![supersede_ok(), supersede_ok()]);
    w.round("k1", "CiRunning", "base-0", &["sp-a"]);
    w.round("k2", "Green", "k1-head", &["sp-b"]);
    w.round("k3", "Attributing", "k2-head", &["sp-c"]);
    w.round("s1", "Green", "base-0", &["sp-s"]);
    w.round("x1", "Green", "gone-head", &["sp-x"]);
    w.round("x2", "Rebuilding", "x1-head", &["sp-y"]);
    w.round("o1", "Open", "gone-head", &[]);
    w.round("l1", "Landing", "gone-head", &[]);
    w.put(
        "batches/lane/legacy",
        json!({ "batch_id": "legacy", "repo": "lane", "state": "Green", "pass": 1, "version": 1 }),
    );
    w.round("done", "Abandoned", "gone-head", &[]);
    let p = w.ports();
    assert_eq!(
        crate::supersede::check(&p, "lane"),
        Ok(vec!["x1".to_string(), "x2".to_string()])
    );
    let calls: Vec<_> = w.verbs.calls().into_iter().map(|c| c.batch).collect();
    assert_eq!(calls, [Some("x1".to_string()), Some("x2".to_string())]);
    let lines: Vec<String> = w.log.outcomes().iter().map(|o| o.line()).collect();
    assert!(
        lines[0].ends_with("superseded ok repo=lane batch=x1"),
        "{lines:?}"
    );
    assert_eq!(
        crate::supersede::check(&p, "lane"),
        Ok(vec![]),
        "gone from the live set"
    );
}

#[test]
fn supersede_hands_the_verb_the_rounds_lease_token_and_a_refusal_stops_the_step() {
    let w = World::new(vec![
        VerbScript {
            verb: "supersede".into(),
            repo: None,
            ok: None,
            refuse: Some(Refuse {
                code: Some(1),
                text: "leased by another holder".into(),
            }),
            lock_hold_ms: 0,
        };
        3
    ]);
    w.put(
        "batches/lane/x1",
        json!({ "batch_id": "x1", "repo": "lane", "state": "Green", "head": "h", "pass": 1,
                "version": 1, "tested_base": "gone", "last_lease_token": { "main": 41, "sub": 0 }, "members": [] }),
    );
    w.round("x2", "Green", "gone", &[]);
    let p = w.ports();
    let r = crate::supersede::check(&p, "lane").unwrap_err();
    assert_eq!(r.key(), "verb/supersede/x1/1");
    assert_eq!(w.verbs.calls().len(), 1, "x2 waits for the next pass");
    crate::supersede::step(&p, "lane");
    crate::supersede::step(&p, "lane");
    let declined = w
        .log
        .outcomes()
        .iter()
        .filter(|o| o.kind == Kind::Refused(Verb::Supersede))
        .count();
    assert_eq!(declined, 1, "told once while it stands");
}

#[test]
fn supersede_leaves_a_repo_with_no_lane_alone_and_names_an_unreadable_one() {
    let w = World::new(vec![]);
    let p = w.ports();
    assert_eq!(crate::supersede::check(&p, "nowhere"), Ok(vec![]));
    w.put("batches/lane/bad", json!({ "state": 3 }));
    assert!(matches!(
        crate::supersede::check(&p, "lane"),
        Err(Refusal::Unreadable { .. })
    ));
    crate::supersede::step(&p, "nowhere");
    assert!(w.log.outcomes().is_empty());
}

#[test]
fn the_explicit_cut_reads_its_repo_and_members_and_refuses_anything_else_by_name() {
    assert_eq!(
        parse_explicit(&["--members", "sp-a@1,sp-b@2", "--repo", "lane"]),
        Ok((
            "lane".into(),
            vec![
                Member {
                    id: "sp-a".into(),
                    tip: "1".into()
                },
                Member {
                    id: "sp-b".into(),
                    tip: "2".into()
                },
            ]
        ))
    );
    for (args, want) in [
        (vec!["--repo"], "--repo needs a value"),
        (vec!["--repo", "a", "--repo", "b"], "--repo given twice"),
        (vec!["--members", "sp-a@1"], "--repo is required"),
        (vec!["--repo", "a"], "--members is required"),
        (
            vec!["--repo", "a", "--members", "sp-a"],
            "member \"sp-a\" is not <id>@<tip>",
        ),
        (
            vec!["--repo", "a", "--members", "@1"],
            "member \"@1\" is not <id>@<tip>",
        ),
        (vec!["--force"], "unknown argument \"--force\""),
    ] {
        assert_eq!(parse_explicit(&args).unwrap_err(), want, "{args:?}");
    }
}

#[test]
fn a_lane_fault_on_the_landing_ref_refuses_a_stacked_cut_too() {
    let w = World::new(vec![]);
    w.round("k1", "CiRunning", "base-0", &[]);
    w.member("sp-a", json!({}));
    w.put(
        "faults/lane/base-0/sha256:i0",
        json!({ "cause": { "cause": "base" }, "since": 0 }),
    );
    assert_eq!(
        w.cut(),
        Err(Refusal::LaneFault {
            key: "faults/lane/base-0/sha256:i0".into()
        })
    );
    w.put(
        "faults/lane/k1-head/sha256:i1",
        json!({ "cause": { "cause": "base" }, "since": 0 }),
    );
    w.put(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "sha256:i1", "harness_version": "h0" }),
    );
    assert_eq!(
        w.cut(),
        Err(Refusal::LaneFault {
            key: "faults/lane/k1-head/sha256:i1".into()
        }),
        "and one on the base it would stack on"
    );
}

#[test]
fn serde_spelt_states_pool_and_a_finished_prerequisite_blocks_nothing() {
    let w = World::new(vec![open_ok("r1")]);
    w.member("sp-a", json!({ "state": "Certified" }));
    w.member("sp-done", json!({ "state": "LANDED" }));
    w.member(
        "sp-b",
        json!({ "blocked_by": ["sp-done"], "stack": { "sp-done": "x" } }),
    );
    w.member("sp-c", json!({ "blocked_by": ["sp-unknown"] }));
    w.cut().unwrap().unwrap();
    assert_eq!(w.opened_members(), ["sp-a", "sp-b"]);
}

#[test]
fn an_explicit_cut_of_a_member_already_in_a_live_round_is_refused() {
    let w = World::new(vec![]);
    w.round("k1", "CiRunning", "base-0", &["sp-a"]);
    assert_eq!(
        explicit(&w.ports(), "lane", vec![mem("sp-a")]),
        Err(Refusal::BadMembers {
            key: "in-a-live-round/sp-a".into()
        })
    );
    let dup = explicit(&w.ports(), "lane", vec![mem("sp-b"), mem("sp-b")]).unwrap_err();
    assert_eq!(dup.key(), "duplicate-member/sp-b");
}

#[test]
fn the_members_a_supersede_releases_are_re_cut_on_the_new_head_in_the_same_pass() {
    let w = World::new(vec![supersede_ok(), open_ok("r1")]);
    w.round("k1", "Attributing", "base-0", &["sp-d"]);
    w.round("k2", "Green", "old-k1-head", &["sp-b", "sp-c"]);
    for m in ["sp-b", "sp-c", "sp-d"] {
        w.member(m, json!({}));
    }
    let p = w.ports();
    crate::supersede::step(&p, "lane");
    w.cut().unwrap().unwrap();
    assert_eq!(w.opened_members(), ["sp-b", "sp-c"]);
    let r = crate::resources::RoundView::read(&*w.store, "lane", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(r.round.tested_base.as_deref(), Some("k1-head"));
}
