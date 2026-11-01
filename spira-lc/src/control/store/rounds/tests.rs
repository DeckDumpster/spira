use std::cell::Cell;

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchState};
use lifecycle::bead::BeadState;
use lifecycle::delivery::DeliveryState;
use runtime::ports::VirtualClock;
use state_store::{Config, Contract, MemBackend, Ports, Request};

use super::{Cut, Planned, Ran, Store};
use crate::control::keys;
use crate::control::records::Audit;
use crate::control::state::{self, Call};

const AT: i64 = 1_767_225_600;
const T1: &str = "1111111111111111111111111111111111111111";
const T2: &str = "2222222222222222222222222222222222222222";

struct Rig {
    s: Store<MemBackend>,
    n: Cell<u64>,
}

impl Rig {
    fn new() -> Self {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        Rig {
            s: Store::new(c, 1_000),
            n: Cell::new(0),
        }
    }

    fn req(&self) -> Request {
        self.n.set(self.n.get() + 1);
        Request::new(&format!("r{}", self.n.get()), "test", "t")
    }

    fn lc(&self, argv: &[&str]) -> (i32, String) {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = self.req();
        state::run(
            &self.s,
            &args,
            &Call {
                now: AT,
                request: &req.id,
            },
        )
    }

    fn ok(&self, argv: &[&str]) {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
    }

    fn certified(&self, id: &str, tip: &str) {
        self.ok(&["create-bead", id, "--repo", "spira"]);
        let claim = r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#;
        let submit = format!(r#"{{"Submit":{{"tip":"{tip}"}}}}"#);
        let pass = format!(r#"{{"GatePass":{{"tip":"{tip}","gate_key":"k1"}}}}"#);
        let event = |expect: &str, v: &str, kind: &str| {
            self.ok(&[
                "event",
                "bead",
                id,
                "--expect",
                expect,
                "--version",
                v,
                "--actor",
                "test",
                "--kind",
                kind,
            ])
        };
        event("READY", "0", claim);
        event("WORKING", "1", &submit);
        event("SUBMITTED", "2", &pass);
    }

    fn cut_spec<'a>(&self, members: &'a [(String, String)]) -> Cut<'a> {
        Cut {
            batch_id: "b-1",
            repo: "spira",
            head: "H",
            base: "B",
            parent: None,
            members,
            actor: "test",
            at: AT,
        }
    }

    fn state(&self, id: &str) -> BeadState {
        self.s.record(id).unwrap().unwrap().value.row.state
    }

    fn audits_at(&self, rev: u64) -> (bool, Vec<Audit>) {
        let e = self
            .s
            .contract()
            .get(&state_store::events_key(rev))
            .unwrap()
            .unwrap();
        let refused = e.value.data["refused"].as_bool().unwrap();
        let branch = if refused { "else" } else { "then" };
        (
            refused,
            serde_json::from_value(e.value.data["evidence"][branch].clone()).unwrap(),
        )
    }
}

fn two() -> Vec<(String, String)> {
    vec![("sp-a".into(), T1.into()), ("sp-b".into(), T2.into())]
}

#[test]
fn a_cut_of_an_id_another_cut_took_after_the_plan_loses_and_leaves_that_round_whole() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    r.ok(&[
        "cut",
        "b-1",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-b:{T2}"),
        "--actor",
        "other",
    ]);
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.members.len(), 1);
    assert_eq!(round.value.members[0].bead_id, "sp-b");
    assert_eq!(r.state("sp-a"), BeadState::Certified);
    assert!(r.s.delivery("sp-a").unwrap().is_none());
}

#[test]
fn a_delivery_written_after_the_plan_makes_the_cut_lose() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    // Another writer gives sp-a a delivery the plan never saw.
    let born = lifecycle::delivery::DeliveryRow::start_push("sp-a");
    let t = state_store::Txn::new().put(
        &keys::delivery("sp-a"),
        state_store::Object::new(serde_json::to_value(&born).unwrap()),
    );
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    assert_eq!(
        r.s.delivery("sp-a").unwrap().unwrap().value.state,
        DeliveryState::Pushing
    );
}

#[test]
fn a_verdict_pass_naming_an_id_twice_keeps_its_last_entry() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let body = r#"{"computed_at":"2026-01-01T00:00:00Z","entries":[{"id":"sp-a","verdict":"blocked"},{"id":"sp-a","verdict":"claimable"}]}"#;
    r.ok(&["claim-verdicts", "--json", body]);
    let v = r.s.readiness("sp-a").unwrap().unwrap().value;
    assert_eq!(v.verdict.as_deref(), Some("claimable"));
}

#[test]
fn a_cut_is_one_txn_writing_the_round_with_its_members_and_each_members_delivery() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let Planned::Cascade(c) = r.s.plan_cut(&r.cut_spec(&members)).unwrap() else {
        panic!("the cut was turned away");
    };
    assert_eq!(c.steps(), 6, "MemberAdded, Deliver and Cut for each member");
    let head = r.s.contract().head().unwrap();
    let Ran::Done { rev, applied } =
        r.s.run_planned(Planned::Cascade(c), &r.req(), "cut")
            .unwrap()
    else {
        panic!("the cut did not run");
    };
    assert_eq!(
        (rev, applied),
        (head + 1, vec![true; 6]),
        "one revision for the whole cut"
    );
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.row.state, BatchState::Open);
    assert_eq!(round.value.row.version, 2);
    assert_eq!(round.value.members.len(), 2);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::InDelivery);
        let d = r.s.delivery(id).unwrap().unwrap().value;
        assert_eq!(
            (d.state, d.batch_id.as_deref()),
            (DeliveryState::Batched, Some("b-1"))
        );
    }
    let (refused, audits) = r.audits_at(rev);
    assert!(!refused);
    let events: Vec<(&str, &str)> = audits
        .iter()
        .map(|a| (a.machine.as_str(), a.event.as_str()))
        .collect();
    assert_eq!(
        events,
        [
            ("batch", "MemberAdded"),
            ("bead", "Deliver"),
            ("delivery", "Enqueued"),
            ("delivery", "Cut"),
            ("batch", "MemberAdded"),
            ("bead", "Deliver"),
            ("delivery", "Enqueued"),
            ("delivery", "Cut"),
        ]
    );
    let history = r.s.history("sp-a", Some("delivery")).unwrap();
    assert_eq!(
        history.len(),
        2,
        "the delivery's birth and its cut, in its own history"
    );
}

#[test]
fn a_cut_whose_member_moved_after_the_plan_writes_nothing_and_audits_every_step_as_lost() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let planned = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let a_before = r.s.contract().get("lifecycle/sp-a").unwrap().unwrap();
    // Another writer touches the second member between the plan and the commit.
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { rev, applied } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 6]);
    assert!(r.s.batch("b-1").unwrap().is_none(), "no round");
    for id in ["sp-a", "sp-b"] {
        assert!(r.s.delivery(id).unwrap().is_none(), "no delivery for {id}");
        assert_eq!(r.state(id), BeadState::Certified);
    }
    let a_after = r.s.contract().get("lifecycle/sp-a").unwrap().unwrap();
    assert_eq!(
        a_after.mod_rev, a_before.mod_rev,
        "the unmoved member is untouched too"
    );
    let (refused, lost) = r.audits_at(rev);
    assert!(refused);
    assert_eq!(lost.len(), 6);
    assert!(lost.iter().all(|a| !a.applied
        && a.refusal.as_deref() == Some("StaleVersion")
        && a.from_state == a.to_state));
    let (rc, out) = r.lc(&["show-batch", "b-1"]);
    assert_eq!((rc, out.as_str()), (1, "{}"));
}

fn batch_event(r: &Rig, expect: BatchState, version: u64, kind: BatchEventKind) {
    let ev = BatchEvent {
        expect,
        version,
        kind,
        actor: "test".into(),
    };
    let ran = r.s.batch_event("b-1", &ev, AT, &r.req()).unwrap();
    assert!(
        matches!(&ran, Ran::Done { applied, .. } if applied == &[true]),
        "{ran:?}"
    );
}

fn green_round(r: &Rig) {
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    batch_event(
        r,
        BatchState::Open,
        2,
        BatchEventKind::CiStarted { run: "r1".into() },
    );
    batch_event(r, BatchState::CiRunning, 3, BatchEventKind::Green);
}

fn fast_forward(version: u64) -> BatchEvent {
    BatchEvent {
        expect: BatchState::Green,
        version,
        kind: BatchEventKind::FastForward { sha: "M1".into() },
        actor: "test".into(),
    }
}

#[test]
fn a_landing_moves_the_round_and_every_member_in_one_txn() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "land").unwrap() else {
        panic!("the landing did not run");
    };
    assert_eq!(
        applied,
        vec![true; 5],
        "FastForward, and Delivered twice per member"
    );
    assert_eq!(
        r.s.batch("b-1").unwrap().unwrap().1.value.row.state,
        BatchState::Landed
    );
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::Landed);
        let d = r.s.delivery(id).unwrap().unwrap().value;
        assert_eq!(
            (d.state, d.merge_sha.as_deref()),
            (DeliveryState::Exited, Some("M1"))
        );
    }
}

#[test]
fn a_landing_whose_member_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "land").unwrap() else {
        panic!("the landing did not run");
    };
    assert_eq!(applied, vec![false; 5]);
    assert_eq!(
        r.s.batch("b-1").unwrap().unwrap().1.value.row.state,
        BatchState::Green
    );
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::InDelivery);
        assert_eq!(
            r.s.delivery(id).unwrap().unwrap().value.state,
            DeliveryState::Batched
        );
    }
}

#[test]
fn a_batch_event_its_machine_refuses_writes_no_round_and_is_audited() {
    let r = Rig::new();
    green_round(&r);
    let before =
        r.s.contract()
            .get(&keys::batch("spira", "b-1"))
            .unwrap()
            .unwrap();
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(3), AT, &req).unwrap();
    let Planned::Ran(Ran::Refused { rev, refusal }) = p else {
        panic!("a stale landing was planned");
    };
    assert_eq!(
        refusal,
        lifecycle::Refusal::StaleVersion {
            given: 3,
            current: 4
        }
    );
    let after =
        r.s.contract()
            .get(&keys::batch("spira", "b-1"))
            .unwrap()
            .unwrap();
    assert_eq!(after.mod_rev, before.mod_rev);
    let (_, audits) = r.audits_at(rev);
    assert_eq!(audits[0].refusal.as_deref(), Some("StaleVersion"));
    assert_eq!(r.s.history("b-1", Some("batch")).unwrap().len(), 5);
}

#[test]
fn a_cut_refuses_a_member_not_at_its_tip_or_still_in_another_round_before_writing() {
    let r = Rig::new();
    green_round(&r);
    r.certified("sp-c", T1);
    let head = r.s.contract().head().unwrap();
    let wrong_tip = vec![("sp-c".to_string(), T2.to_string())];
    let p =
        r.s.plan_cut(&Cut {
            batch_id: "b-2",
            ..r.cut_spec(&wrong_tip)
        })
        .unwrap();
    assert!(
        matches!(p, Planned::Ran(Ran::Turned(super::Turned::Refused(ref m))) if m.contains("certified tip does not match"))
    );
    let (rc, out) = r.lc(&[
        "cut",
        "b-2",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-c:{T1},sp-a:{T1}"),
        "--actor",
        "test",
    ]);
    assert_eq!(
        (rc, out.as_str()),
        (3, "refused: sp-a is IN_DELIVERY not SUBMITTED or CERTIFIED")
    );
    assert_eq!(r.s.contract().head().unwrap(), head, "nothing written");
}
