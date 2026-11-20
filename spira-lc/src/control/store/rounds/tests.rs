use std::cell::Cell;

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchState};
use lifecycle::bead::BeadState;
use lifecycle::delivery::DeliveryState;
use runtime::ports::VirtualClock;
use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};
use state_store_sqlite::SqliteBackend;

use super::{Cut, Planned, Ran, Store, Turned};
use crate::control::store::counting::Counting;
use crate::control::keys;
use crate::control::records::Audit;
use crate::control::state::{self, Call};

const AT: i64 = 1_767_225_600;
const T1: &str = "1111111111111111111111111111111111111111";
const T2: &str = "2222222222222222222222222222222222222222";

struct Rig<B: Backend = MemBackend> {
    s: Store<B>,
    n: Cell<u64>,
}

fn clock() -> VirtualClock {
    VirtualClock::new(AT as u64 * 1_000)
}

impl Rig {
    fn new() -> Self {
        Rig::on(MemBackend::new(), &clock())
    }
}

impl Rig<SqliteBackend> {
}

impl<B: Backend> Rig<B> {
    fn on(b: B, clock: &VirtualClock) -> Self {
        let c = Contract::open(
            b,
            Ports::virtual_clock(clock),
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

    fn with_refs(self, pairs: &[(&str, &str)]) -> Self {
        let held: std::collections::BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Rig { s: self.s.with_refs(std::sync::Arc::new(move |id: &str| Ok(held.get(id).cloned()))), n: self.n }
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
                request: &req.id, inputs: &crate::control::claimable::FIXED,
            },
        )
    }

    fn ok(&self, argv: &[&str]) {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
    }

    fn certified(&self, id: &str, tip: &str) {
        self.certified_with(
            id,
            tip,
            r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#,
        );
    }

    fn certified_with(&self, id: &str, tip: &str, claim: &str) {
        self.submitted_with(id, tip, claim);
        let pass = format!(r#"{{"GatePass":{{"tip":"{tip}","gate_key":"k1"}}}}"#);
        self.bead_event(id, "SUBMITTED", "2", &pass);
    }

    fn submitted_with(&self, id: &str, tip: &str, claim: &str) {
        self.ok(&["create-bead", id, "--repo", "spira", "--ready"]);
        let submit = format!(r#"{{"Submit":{{"tip":"{tip}"}}}}"#);
        self.bead_event(id, "READY", "0", claim);
        self.bead_event(id, "WORKING", "1", &submit);
    }

    fn bead_event(&self, id: &str, expect: &str, version: &str, kind: &str) {
        self.ok(&[
            "event",
            "bead",
            id,
            "--expect",
            expect,
            "--version",
            version,
            "--actor",
            "test",
            "--kind",
            kind,
        ]);
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

fn batch_event<B: Backend>(r: &Rig<B>, expect: BatchState, version: u64, kind: BatchEventKind) {
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

fn green_round<B: Backend>(r: &Rig<B>) {
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

const T3: &str = "3333333333333333333333333333333333333333";
const CAP: usize = state_store::limits::MAX_CUT_MEMBERS;

fn ids(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| (format!("sp-m{i:02}"), T1.to_string()))
        .collect()
}

#[test]
fn a_round_id_is_taken_in_every_repo_once_any_repo_cuts_it() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    let theirs = vec![("sp-b".to_string(), T2.to_string())];
    let other = Cut {
        repo: "other",
        ..r.cut_spec(&theirs)
    };
    let p = r.s.plan_cut(&other).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3], "the index compared absent loses");
    let (key, _) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(key, keys::batch("other", "b-1"));
    assert!(r
        .s
        .contract()
        .get(&keys::batch("spira", "b-1"))
        .unwrap()
        .is_none());
}

fn batch_ev(expect: BatchState, version: u64, kind: BatchEventKind) -> BatchEvent {
    BatchEvent {
        expect,
        version,
        kind,
        actor: "test".into(),
    }
}

fn round_state(r: &Rig) -> BatchState {
    r.s.batch("b-1").unwrap().unwrap().1.value.row.state
}

#[test]
fn an_abandon_whose_member_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let ev = batch_ev(
        BatchState::Green,
        4,
        BatchEventKind::Abandon {
            reason: "stopped".into(),
        },
    );
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "abandon").unwrap() else {
        panic!("the abandon did not run");
    };
    assert_eq!(applied, vec![false; 5], "Abandon, and two exits per member");
    assert_eq!(round_state(&r), BatchState::Green);
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "abandon").unwrap() else {
        panic!("the abandon did not run");
    };
    assert_eq!(applied, vec![true; 5]);
    assert_eq!(round_state(&r), BatchState::Abandoned);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::Certified);
    }
}

#[test]
fn an_abandon_notes_its_reason_on_each_released_member() {
    let r = Rig::new();
    green_round(&r);
    let ev = batch_ev(
        BatchState::Green,
        4,
        BatchEventKind::Abandon {
            reason: "attribution leg died".into(),
        },
    );
    let req = r.req();
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    r.s.run_planned(p, &req, "abandon").unwrap();
    for id in ["sp-a", "sp-b"] {
        let noted = r
            .s
            .history(id, Some("bead"))
            .unwrap()
            .into_iter()
            .any(|c| c.audit.evidence.get("note").and_then(|n| n.as_str()) == Some("attribution leg died"));
        assert!(noted, "{id} carries the abandon's reason");
    }
}

fn open_round<B: Backend>(r: &Rig<B>) {
    r.certified("sp-a", T1);
    let one = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&one)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
}

#[test]
fn a_stack_whose_member_moved_after_the_plan_writes_nothing_and_none_past_the_cap_is_taken() {
    let r = Rig::new();
    open_round(&r);
    r.certified("sp-c", T3);
    let more = vec![("sp-c".to_string(), T3.to_string())];
    let p = r.s.plan_stack("b-1", &more, "test", AT).unwrap();
    r.ok(&["set-repo", "sp-c", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "stack").unwrap() else {
        panic!("the stack did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.members.len(), 1);
    assert_eq!(r.state("sp-c"), BeadState::Certified);
    let full = ids(CAP);
    let p = r.s.plan_stack("b-1", &full, "test", AT).unwrap();
    assert!(
        matches!(p, Planned::Ran(Ran::Turned(super::Turned::Refused(ref m))) if m.contains(&format!("would hold {}", CAP + 1)))
    );
    let p = r.s.plan_stack("b-1", &more, "test", AT).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "stack").unwrap() else {
        panic!("the stack did not run");
    };
    assert_eq!(applied, vec![true; 3]);
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.members.len(), 2);
}

#[test]
fn a_promotion_whose_member_moved_after_the_plan_leaves_the_round_staged() {
    let r = Rig::new();
    open_round(&r);
    r.certified("sp-c", T3);
    r.ok(&[
        "stage",
        "b-2",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-c:{T3}"),
        "--actor",
        "test",
        "--parent",
        "b-1",
    ]);
    assert_eq!(
        r.state("sp-c"),
        BeadState::Certified,
        "staging delivers nothing"
    );
    let req = r.req();
    let p =
        r.s.plan_promote("b-2", "H2", "B2", "test", AT, &req)
            .unwrap();
    r.ok(&["set-repo", "sp-c", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "promote").unwrap() else {
        panic!("the promotion did not run");
    };
    assert_eq!(applied, vec![false; 4]);
    let (_, staged) = r.s.batch("b-2").unwrap().unwrap();
    assert_eq!(staged.value.row.state, BatchState::Staged);
    assert_eq!(r.state("sp-c"), BeadState::Certified);
    let (rc, out) = r.lc(&[
        "promote", "b-2", "--head", "H2", "--base", "B2", "--actor", "test",
    ]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(r.state("sp-c"), BeadState::InDelivery);
    let (rc, out) = r.lc(&[
        "stage",
        "b-2",
        "--repo",
        "other",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-a:{T1}"),
        "--actor",
        "test",
        "--parent",
        "b-1",
    ]);
    assert_eq!(
        (rc, out.as_str()),
        (2, "stage: a batch row for b-2 already exists")
    );
}

#[test]
fn a_round_that_opened_and_never_began_is_abandoned_and_its_members_requeued() {
    let r = Rig::new();
    open_round(&r);
    let req = r.req();
    let early =
        r.s.requeue_orphans("test", true, 1800, AT + 60, &none(), &req)
            .unwrap();
    assert!(early.phantom_batches.is_empty() && early.orphans.is_empty());
    let late =
        r.s.requeue_orphans("test", false, 1800, AT + 3600, &none(), &req)
            .unwrap();
    assert_eq!(
        (late.phantom_batches.as_slice(), late.orphans.as_slice()),
        (
            ["b-1".to_string()].as_slice(),
            ["sp-a".to_string()].as_slice()
        )
    );
    assert_eq!(
        round_state(&r),
        BatchState::Open,
        "a dry run writes nothing"
    );
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(
        (applied.abandoned, applied.requeued, applied.failed),
        (vec!["b-1".to_string()], vec!["sp-a".to_string()], vec![])
    );
    assert_eq!(round_state(&r), BatchState::Abandoned);
    assert_eq!(r.state("sp-a"), BeadState::Certified);
    let noted = r.s.history("sp-a", Some("bead")).unwrap().into_iter().any(|c| {
        c.audit.evidence.get("note").and_then(|n| n.as_str()).is_some_and(|n| n.contains("never began"))
    });
    assert!(noted, "the sweep's reason rides on the released member");
}

#[test]
fn a_round_with_an_ejected_member_and_no_phase_write_is_not_abandoned_as_never_began() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::Open,
        version: rec.value.row.version,
        kind: BatchEventKind::Eject { bead_id: "sp-a".into(), reason: "red".into() },
        actor: "test".into(),
    };
    let (p, _) = r.s.plan_eject("b-1", "sp-a", &ev, AT, &r.req()).unwrap();
    r.s.run_planned(p, &r.req(), "eject").unwrap();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-b"), BeadState::InDelivery);
}

#[test]
fn a_round_with_a_recorded_pass_and_no_live_leg_is_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    let send = |expect: BatchState, kind: BatchEventKind| {
        let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
        let ev = BatchEvent { expect, version: rec.value.row.version, kind, actor: "test".into() };
        r.s.batch_event_with("b-1", &ev, AT + 10, &r.req(), None).unwrap();
    };
    send(BatchState::Open, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
    send(BatchState::CiRunning, BatchEventKind::PassIncomplete { n: 1, reason: "lost".into() });
    send(BatchState::Attributing, BatchEventKind::PassRebuilt { head: "h2".into() });
    assert_eq!(round_state(&r), BatchState::Open);
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty() && applied.requeued.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}

fn set_round_fields(r: &Rig, fields: serde_json::Value) {
    let key = keys::batch("spira", "b-1");
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["round"] = fields;
    let t = state_store::Txn::new()
        .when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev)))
        .put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
}

#[test]
fn a_forge_round_with_a_recorded_pr_is_begun_and_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    set_round_fields(&r, serde_json::json!({ "forge.phase": "armed", "pr": "351" }));
    let applied =
        r.s.requeue_orphans("test", true, 120, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty() && applied.requeued.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}

#[test]
fn a_forge_round_still_opening_with_no_pr_is_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    set_round_fields(&r, serde_json::json!({ "forge.phase": "opening" }));
    let applied =
        r.s.requeue_orphans("test", true, 120, AT + 3600, &none(), &r.req())
            .unwrap();
    assert!(applied.running.is_empty(), "{applied:?}");
    assert_eq!(round_state(&r), BatchState::Abandoned);
}

#[test]
fn at_121_seconds_a_round_with_a_started_pass_is_left_and_one_with_none_is_abandoned() {
    let begun = Rig::new();
    open_round(&begun);
    let (_, rec) = begun.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::Open,
        version: rec.value.row.version,
        kind: BatchEventKind::PassStarted { n: 1, head: "h1".into() },
        actor: "test".into(),
    };
    begun.s.batch_event_with("b-1", &ev, AT + 10, &begun.req(), None).unwrap();
    let left = begun.s.requeue_orphans("test", true, 120, AT + 121, &none(), &begun.req()).unwrap();
    assert!(left.phantom_batches.is_empty() && left.abandoned.is_empty() && left.requeued.is_empty(), "{left:?}");
    assert_ne!(round_state(&begun), BatchState::Abandoned);

    let never = Rig::new();
    open_round(&never);
    let gone = never.s.requeue_orphans("test", true, 120, AT + 121, &none(), &never.req()).unwrap();
    assert_eq!(gone.abandoned, vec!["b-1".to_string()]);
}

fn none() -> std::collections::BTreeSet<String> {
    Default::default()
}

#[test]
fn a_round_with_a_live_leg_unit_is_begun_and_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    let live: std::collections::BTreeSet<String> = ["b-1".to_string()].into();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &live, &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty()
            && applied.abandoned.is_empty()
            && applied.requeued.is_empty()
            && applied.orphans.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
    let dry =
        r.s.requeue_orphans("test", false, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(dry.repos.get("b-1").map(String::as_str), Some("spira"));
}

#[test]
fn the_requeue_orphans_command_leaves_the_round_it_is_told_is_live() {
    let r = Rig::new();
    open_round(&r);
    let later = |argv: &[&str]| {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = r.req();
        state::run(&r.s, &args, &Call { now: AT + 3600, request: &req.id, inputs: &crate::control::claimable::FIXED })
    };
    let (rc, dry) = later(&["requeue-orphans", "--actor", "test"]);
    assert_eq!(rc, 0, "{dry}");
    assert!(dry.contains(r#""repos":{"b-1":"spira"}"#), "{dry}");
    let (rc, out) = later(&["requeue-orphans", "--actor", "test", "--apply", "--live-round", "b-1"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(r#""running":["b-1"]"#) && out.contains(r#""abandoned":[]"#), "{out}");
    assert_eq!(round_state(&r), BatchState::Open);
}

#[test]
fn the_requeue_orphans_command_abandons_a_round_with_no_driver_after_two_minutes_by_default() {
    let r = Rig::new();
    open_round(&r);
    let at = |now: i64| {
        let args: Vec<String> = ["requeue-orphans", "--actor", "test"].iter().map(|s| s.to_string()).collect();
        let req = r.req();
        state::run(&r.s, &args, &Call { now, request: &req.id, inputs: &crate::control::claimable::FIXED })
    };
    let (rc, early) = at(AT + 60);
    assert_eq!(rc, 0, "{early}");
    assert!(!early.contains(r#""b-1""#), "a minute is not yet a stall: {early}");
    let (rc, late) = at(AT + 180);
    assert_eq!(rc, 0, "{late}");
    assert!(late.contains(r#""repos":{"b-1":"spira"}"#), "{late}");
    assert_eq!(round_state(&r), BatchState::Open, "a dry run writes nothing");
    let args: Vec<String> = ["requeue-orphans", "--actor", "test", "--apply"].iter().map(|s| s.to_string()).collect();
    let req = r.req();
    let (rc, out) = state::run(&r.s, &args, &Call { now: AT + 180, request: &req.id, inputs: &crate::control::claimable::FIXED });
    assert_eq!(rc, 0, "{out}");
    assert_eq!(round_state(&r), BatchState::Abandoned);
}

#[test]
fn requeue_orphans_leaves_a_leased_round_that_never_began_open_and_lists_it_leased() {
    let r = Rig::new();
    open_round(&r);
    r.s.contract()
        .lock("round/spira/b-1", 600_000, &r.req())
        .unwrap();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.leased, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty()
            && applied.abandoned.is_empty()
            && applied.requeued.is_empty()
            && applied.orphans.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}

#[test]
fn the_queues_fields_of_a_round_survive_every_machine_step_and_the_landing() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    set_round_fields(&r, serde_json::json!({ "worktree": "wt/b-1", "red": "test-a.sh" }));
    batch_event(
        &r,
        BatchState::Open,
        2,
        BatchEventKind::CiStarted { run: "r1".into() },
    );
    batch_event(&r, BatchState::CiRunning, 3, BatchEventKind::Green);
    let landed = r.s.plan_land("b-1", &fast_forward(4), AT, &r.req()).unwrap();
    r.s.run_planned(landed, &r.req(), "land").unwrap();
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.row.state, BatchState::Landed);
    assert_eq!(
        round.value.round,
        [("red", "test-a.sh"), ("worktree", "wt/b-1")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into()
    );
}

#[test]
fn the_queues_fields_given_to_a_cut_or_a_batch_event_commit_in_that_steps_own_txn() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let key = keys::batch("spira", "b-1");
    let at_head = |r: &Rig| {
        let head = r.s.contract().head().unwrap();
        let item = r.s.contract().get(&key).unwrap().unwrap();
        (item.mod_rev.main == head, item.value.data["round"].clone())
    };
    let before = r.s.contract().head().unwrap();
    r.ok(&[
        "cut", "b-1", "--repo", "spira", "--head", "H", "--base", "B", "--members",
        &format!("sp-a:{T1}"), "--actor", "test", "--round", r#"{"worktree":"wt/b-1","actor":"q"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the cut and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"actor": "q", "worktree": "wt/b-1"})));
    let before = r.s.contract().head().unwrap();
    r.ok(&[
        "event", "batch", "b-1", "--expect", "OPEN", "--version", "1", "--actor", "test",
        "--kind", r#"{"CiStarted":{"run":"r1"}}"#, "--round", r#"{"worktree":"wt/b-1","red":"x"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the event and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"red": "x", "worktree": "wt/b-1"})));
    let (rc, out) = r.lc(&[
        "event", "batch", "b-1", "--expect", "CI_RUNNING", "--version", "2", "--actor", "test",
        "--kind", r#""Green""#, "--round", "[1]",
    ]);
    assert!(rc != 0 && out.contains("--round must be one JSON object"), "{rc}: {out}");
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.row.state, BatchState::CiRunning);
}

#[test]
fn fields_with_no_write_of_their_round_to_ride_in_are_an_error_not_dropped() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let members = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let fields = [("worktree".to_string(), "wt".to_string())].into();
    let e = p.with_round("b-9", Some(&fields)).err().expect("no write of b-9");
    assert!(e.to_string().contains("b-9"), "{e}");
}

#[test]
fn a_list_with_after_and_limit_answers_one_page_of_rows_in_id_order() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-c", T1, "b-3"), ("sp-a", T2, "b-1"), ("sp-b", T1, "b-2")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    let page = |argv: &[&str], key: &str| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x[key].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(page(&["list", "--limit", "2"], "bead_id"), ["sp-a", "sp-b"]);
    assert_eq!(page(&["list", "--limit", "2", "--after", "sp-b"], "bead_id"), ["sp-c"]);
    assert_eq!(page(&["list", "--batches", "--limit", "2"], "batch_id"), ["b-1", "b-2"]);
    assert_eq!(page(&["list", "--batches", "--limit", "2", "--after", "b-2"], "batch_id"), ["b-3"]);
    assert_eq!(page(&["list", "--batches", "--after", "b-1"], "batch_id"), ["b-2", "b-3"]);
    for bad in [&["list", "--limit", "0"][..], &["list", "--batches", "--limit", "x"]] {
        assert_eq!(r.lc(bad).0, crate::CANNOT_TELL, "{bad:?}");
    }
}

#[test]
fn a_batch_filter_answers_the_round_for_an_id_with_a_slash_and_for_a_plain_id() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-a", T1, "spira/queue/x"), ("sp-b", T2, "b-1")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    r.s.set_progress("spira", "spira/queue/x", serde_json::json!({"done": 1}), &r.req())
        .unwrap();
    let ids = |argv: &[&str]| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x["batch_id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&["list", "--batches", "--batch", "spira/queue/x"]), ["spira/queue/x"]);
    assert_eq!(ids(&["list", "--batches", "--batch", "b-1"]), ["b-1"]);
    assert_eq!(ids(&["list", "--batches", "--limit", "10"]), ["b-1", "spira/queue/x"]);
    assert_eq!(super::batch_key("batches/spira/spira/queue/x"), Some(("spira", "spira/queue/x")));
    assert_eq!(super::batch_key("batches/spira/b-1"), Some(("spira", "b-1")));
    assert_eq!(super::batch_key("batches/spira/spira/queue/x/progress"), None);
    assert_eq!(super::batch_key("batches/spira/b-1/progress"), None);
}

/// law: every list paginates in the store. `list --state X --limit N --after ID` over many
/// records is one state-index read bounded by the page plus a point read per row it answers;
/// it never ranges over `lifecycle/`, `readiness/` or the claim locks.
#[test]
fn a_state_filtered_page_is_one_bounded_read_of_the_state_index() {
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..120 {
        r.ok(&["create-bead", &format!("sp-{i:03}"), "--repo", "spira", "--ready"]);
    }
    for i in (0..120).step_by(3) {
        r.bead_event(&format!("sp-{i:03}"), "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    }
    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--state", "WORKING", "--limit", "5", "--after", "sp-030"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = rows.as_array().unwrap().iter().map(|x| x["bead_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["sp-033", "sp-036", "sp-039", "sp-042", "sp-045"]);
    let reads = r.s.contract().backend().take();
    let ranges: Vec<_> = reads.iter().filter(|(k, _, _)| !k.starts_with("get")).collect();
    assert_eq!(
        ranges,
        [&("range_in_state", format!("{}Working", keys::LIFECYCLE), 5)],
        "one bounded read of the state index: {reads:?}"
    );
    let gets = reads.len() - ranges.len();
    assert!(gets <= 3 * ids.len(), "{gets} point reads for {} rows: {reads:?}", ids.len());
}

/// `ROUNDS` rounds cut `100 * i` seconds before `AT`, all but `open` ended.
const ROUNDS: usize = 60;

fn many_rounds<B: Backend>(r: &Rig<B>, from: usize, to: usize, open: &[usize]) {
    use lifecycle::batch::BatchState;
    for i in from..to {
        let (id, round) = (format!("sp-{i:03}"), format!("b-{i:03}"));
        r.certified(&id, T1);
        let members = vec![(id, T1.to_string())];
        let cut = Cut { batch_id: &round, at: AT - 100 * i as i64, ..r.cut_spec(&members) };
        let p = r.s.plan_cut(&cut).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
        if !open.contains(&i) {
            let key = keys::batch("spira", &round);
            let (_, mut rec) = r.s.batch(&round).unwrap().unwrap();
            rec.value.row.state = BatchState::Landed;
            let put = state_store::Txn::new().put(&key, state_store::Object::new(crate::control::records::encode(&rec.value).unwrap()));
            r.s.contract().txn(&put, &r.req()).unwrap();
        }
    }
}

/// law-list-reads-paginate-in-the-database: `list --batches` reads the rounds its answer shows
/// (the newest eight, a window's, the open ones, a page's), never the `batches/` prefix.
#[test]
fn list_batches_reads_only_the_rounds_it_shows() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, ROUNDS, &[25, 33]);
    let ids = |argv: &[&str]| -> (Vec<String>, Vec<(&'static str, String, usize)>) {
        r.s.contract().backend().take();
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ids = rows.as_array().unwrap().iter().map(|x| x["batch_id"].as_str().unwrap().to_string()).collect();
        (ids, r.s.contract().backend().take())
    };
    let of = |ns: &[usize]| -> Vec<String> { ns.iter().map(|i| format!("b-{i:03}")).collect() };
    let bounded = |reads: &[(&'static str, String, usize)]| {
        assert!(
            reads.iter().all(|(k, p, limit)| *k == "get" || !(p.starts_with(keys::BATCHES) && *k == "range" && *limit > 1_000)),
            "an unbounded read of the rounds: {reads:?}"
        );
        assert!(reads.len() <= 60, "{} reads: {reads:?}", reads.len());
    };

    let (bare, reads) = ids(&["list", "--batches"]);
    assert_eq!(bare, of(&[0, 1, 2, 3, 4, 5, 6, 7]));
    assert!(reads.iter().any(|(k, _, limit)| *k == "range_newest_opened" && *limit == 8), "{reads:?}");
    bounded(&reads);

    let (windowed, reads) = ids(&["list", "--batches", "--window", "1000"]);
    assert_eq!(windowed, of(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 25, 33]));
    assert!(reads.iter().any(|(k, _, _)| *k == "range_newest_opened"), "{reads:?}");
    bounded(&reads);

    let (one, reads) = ids(&["list", "--batches", "--batch", "b-042"]);
    assert_eq!(one, of(&[42]));
    assert!(reads.iter().all(|(k, p, _)| *k == "get" || !p.starts_with(keys::BATCHES)), "{reads:?}");

    let (page, reads) = ids(&["list", "--batches", "--limit", "3", "--after", "b-010"]);
    assert_eq!(page, of(&[11, 12, 13]));
    bounded(&reads);
}

/// On the real store, what `list --batches` costs does not grow with the rounds behind it: the
/// VM steps of a bare list and of a window are the same over 40 rounds and over 240.
#[test]
fn list_batches_costs_the_same_over_a_larger_history_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-batches-steps");
    let b = SqliteBackend::open(&dir.path().join("c.db"), state_store_sqlite::Options::default()).unwrap();
    let r = Rig::on(b, &clock());
    let steps = |argv: &[&str]| -> u64 {
        r.s.contract().backend().take_read_steps();
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        r.s.contract().backend().take_read_steps()
    };
    let probes = [&["list", "--batches"][..], &["list", "--batches", "--window", "300"], &["list", "--batches", "--limit", "5", "--after", "b-010"]];
    many_rounds(&r, 0, 40, &[35]);
    let small: Vec<u64> = probes.iter().map(|p| steps(p)).collect();
    r.s.contract().backend().take_read_steps();
    many_rounds(&r, 40, 240, &[]);
    let large: Vec<u64> = probes.iter().map(|p| steps(p)).collect();
    for ((p, s), l) in probes.iter().zip(&small).zip(&large) {
        assert!(*l <= *s + *s / 4 + 50, "{p:?}: {s} VM steps over 40 rounds, {l} over 240");
    }
}

/// Every state the round machine can be in is either listed as open or is terminal: a round in
/// a state `list --batches --window` forgot would vanish from it.
#[test]
fn every_batch_state_is_listed_as_open_or_is_terminal() {
    use lifecycle::batch::BatchState::*;
    for s in [Staged, Open, CiRunning, Green, Landing, Deploying, Attributing, Rebuilding, Landed, Settled, Abandoned, Discarded] {
        match s {
            Staged | Open | CiRunning | Green | Landing | Deploying | Attributing | Rebuilding => {
                assert!(super::OPEN_BATCH_STATES.contains(&s) && !s.is_terminal(), "{s:?}")
            }
            Landed | Settled | Abandoned | Discarded => {
                assert!(!super::OPEN_BATCH_STATES.contains(&s) && s.is_terminal(), "{s:?}")
            }
        }
    }
}

/// law-hot-reads-answer-in-500ms: an unfiltered `list` reads its rows' readiness by key in
/// batched reads, never by ranging `readiness/`, and the rows are the ones a read by key answers.
#[test]
fn a_whole_table_list_reads_readiness_by_key_never_by_range() {
    const BEADS: usize = 600;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..BEADS {
        r.ok(&["create-bead", &format!("sp-{i:04}"), "--repo", "spira", "--ready"]);
    }
    r.bead_event("sp-0002", "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    let blocked = crate::control::records::Readiness {
        blocked_by: vec!["sp-0001".into(), "sp-0002".into(), "sp-gone".into()],
        ..Default::default()
    };
    let seed = state_store::Txn::new().put(
        &keys::readiness("sp-0500"),
        state_store::Object::new(crate::control::records::encode(&blocked).unwrap()),
    );
    r.s.contract().txn(&seed, &r.req()).unwrap();

    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--live"]);
    assert_eq!(rc, 0, "{out}");
    let reads = r.s.contract().backend().take();
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), BEADS);
    assert!(!reads.iter().any(|(k, key, _)| *k == "range" && key.starts_with(keys::READINESS)), "{reads:?}");
    let readiness_keys = reads.iter().filter(|(k, key, _)| k.starts_with("get") && key.starts_with(keys::READINESS)).count();
    assert!(readiness_keys <= BEADS, "{readiness_keys} readiness keys read for {BEADS} rows");

    let probe = ["sp-0000", "sp-0002", "sp-0007", "sp-0014", "sp-0500", "sp-0599"];
    let (rc, by_key) = r.lc(&["list", "--ids", &probe.join(",")]);
    assert_eq!(rc, 0, "{by_key}");
    let by_key: serde_json::Value = serde_json::from_str(&by_key).unwrap();
    let ranged: Vec<&serde_json::Value> =
        rows.iter().filter(|x| probe.contains(&x["bead_id"].as_str().unwrap())).collect();
    assert_eq!(ranged, by_key.as_array().unwrap().iter().collect::<Vec<_>>());
    assert_eq!(ranged[4]["blocked_by"], serde_json::json!(["sp-0001", "sp-0002"]), "a blocker with no record no longer blocks");
}

fn seed_readiness(r: &Rig<impl Backend>, ids: impl Iterator<Item = String>) {
    let blocked = crate::control::records::Readiness { blocked_by: vec!["sp-x".into()], ..Default::default() };
    let value = crate::control::records::encode(&blocked).unwrap();
    let ids: Vec<String> = ids.collect();
    for chunk in ids.chunks(100) {
        let mut txn = state_store::Txn::new();
        for id in chunk {
            txn = txn.put(&keys::readiness(id), state_store::Object::new(value.clone()));
        }
        r.s.contract().txn(&txn, &r.req()).unwrap();
    }
}

/// law-list-reads-paginate-in-the-database: the readiness of a few hundred ids spread across a
/// large `readiness/` prefix is read by their keys, never by ranging the prefix.
#[test]
fn readiness_of_spread_ids_reads_their_keys_and_never_ranges_the_table() {
    let r = Rig::on(Counting::default(), &clock());
    seed_readiness(&r, (0..3_000).map(|i| format!("sp-{i:04}")));
    let ids: Vec<String> = (0..3_000).step_by(10).map(|i| format!("sp-{i:04}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let head = r.s.contract().head().unwrap();
    r.s.contract().backend().take();
    let got = r.s.readiness_of(&refs, head).unwrap();
    let reads = r.s.contract().backend().take();
    assert!(!reads.iter().any(|(k, key, _)| *k == "range" && key.starts_with(keys::READINESS)), "{reads:?}");
    let keys_read = reads.iter().filter(|(k, _, _)| k.starts_with("get")).count();
    assert!(keys_read <= ids.len(), "{keys_read} keys read for {} ids", ids.len());
    assert_eq!(got.len(), ids.len());
    for id in &refs {
        assert_eq!(Some(&got[*id]), r.s.readiness_at(id, head).unwrap().as_ref(), "{id}");
    }
}

/// On the real store, `list --live` costs what its rows cost, not what the readiness table behind
/// them holds.
#[test]
fn list_live_costs_the_same_over_a_larger_readiness_table_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-live-steps");
    let b = SqliteBackend::open(&dir.path().join("c.db"), state_store_sqlite::Options::default()).unwrap();
    let r = Rig::on(b, &clock());
    for i in 0..300 {
        r.ok(&["create-bead", &format!("sp-{:05}", i * 11), "--repo", "spira", "--ready"]);
    }
    let steps = || -> u64 {
        r.s.contract().backend().take_read_steps();
        let (rc, out) = r.lc(&["list", "--live"]);
        assert_eq!(rc, 0, "{out}");
        r.s.contract().backend().take_read_steps()
    };
    let small = steps();
    seed_readiness(&r, (0..3_300).filter(|i| i % 11 != 0).map(|i| format!("sp-{i:05}")));
    let large = steps();
    assert!(large <= small + small / 4 + 50, "{small} VM steps over 300 rows, {large} with 3000 more readiness rows");
}

const DELIVERIES: usize = 3_000;
const AUDITS: usize = 50;

/// `DELIVERIES` queued deliveries, each with `AUDITS` history entries, written to the backend
/// as the import writes them: each delivery's `entered` beside its row, its history under
/// `history/<id>/`. Answers the `entered_at` each should list with.
fn many_deliveries(b: &Counting) -> Vec<(String, i64)> {
    use crate::control::records::{fold_entered, ENTERED};
    use state_store::{Object, Rev, Write};
    let mut writes = Vec::new();
    let mut want = Vec::new();
    for n in 0..DELIVERIES {
        let id = format!("sp-{n:05}");
        let audits: Vec<Audit> = (0..AUDITS)
            .map(|k| Audit {
                machine: if k % 2 == 0 { "delivery" } else { "bead" }.into(),
                lc_key: id.clone(),
                event: "Probe".into(),
                expect: "QUEUED".into(),
                from_state: "QUEUED".into(),
                to_state: "QUEUED".into(),
                applied: true,
                refusal: None,
                evidence: serde_json::Value::Null,
                actor: "test".into(),
                at: AT + (n * AUDITS + k) as i64,
            })
            .collect();
        let mut entered = serde_json::Map::new();
        fold_entered(&mut entered, &id, &audits);
        want.push((id.clone(), entered["QUEUED"].as_i64().unwrap()));
        let mut row = serde_json::to_value(lifecycle::delivery::DeliveryRow::start_queue(id.as_str())).unwrap();
        row[ENTERED] = serde_json::Value::Object(entered);
        writes.push((keys::delivery(&id), Object::new(row)));
        for (k, a) in audits.into_iter().enumerate() {
            let seq = (n * AUDITS + k) as u64;
            let entry = serde_json::json!({ "seq": seq, "audit": a });
            writes.push((keys::history_entry(&id, 0, &format!("import/{seq:020}")), Object::new(entry)));
        }
    }
    for chunk in writes.chunks(20_000) {
        let head = b.inner.head().unwrap();
        let batch: Vec<Write> = chunk
            .iter()
            .enumerate()
            .map(|(sub, (key, v))| Write::Put {
                key: key.clone(),
                value: v.encode(),
                create_rev: Rev::new(head + 1, sub as u32),
                prev: None,
            })
            .collect();
        b.inner.commit(head, &batch).unwrap();
    }
    want
}

/// law: a list reads its page from the current record, in the store. `list --delivery` over
/// 3k deliveries of 50 audits each reads the deliveries alone, a page at a time, and never a
/// delivery's history; a page of 100 is one bounded read.
#[test]
fn list_delivery_reads_entered_at_from_the_record_never_history() {
    let r = Rig::on(Counting::default(), &clock());
    let want = many_deliveries(r.s.contract().backend());
    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--delivery"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let got: Vec<(String, i64)> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["bead_id"].as_str().unwrap().to_string(), x["entered_at"].as_str().unwrap().parse().unwrap()))
        .collect();
    assert_eq!(got, want);
    let reads = r.s.contract().backend().take();
    let elsewhere: Vec<_> = reads.iter().filter(|(_, k, _)| k != keys::DELIVERIES).take(3).collect();
    assert!(elsewhere.is_empty(), "{} reads, some outside deliveries/: {elsewhere:?}", reads.len());
    assert!(reads.len() <= DELIVERIES / 1_000 + 1, "{} reads for {DELIVERIES} deliveries", reads.len());

    let (rc, out) = r.lc(&["list", "--delivery", "--state", "QUEUED", "--limit", "100", "--after", "sp-01000"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 100);
    assert_eq!(rows[0]["bead_id"], "sp-01001");
    assert_eq!(
        r.s.contract().backend().take(),
        [("range_in_state", format!("{}Queued", keys::DELIVERIES), 100)],
        "one bounded read of the state index"
    );
}

/// Every txn that moves a delivery keeps its `entered` current, so the record answers
/// `entered_at` as its history does.
#[test]
fn a_deliverys_entered_at_from_its_record_matches_its_history() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-a", T1, "b-1"), ("sp-b", T2, "b-2")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    let (rc, out) = r.lc(&["list", "--delivery"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{out}");
    for row in rows.as_array().unwrap() {
        let id = row["bead_id"].as_str().unwrap();
        let state = row["state"].as_str().unwrap();
        let from_history = r
            .s
            .history(id, Some("delivery"))
            .unwrap()
            .into_iter()
            .filter(|c| c.audit.applied && c.audit.to_state == state)
            .map(|c| c.audit.at)
            .max();
        assert!(from_history.is_some(), "{id}: no applied event into {state}");
        assert_eq!(row["entered_at"], serde_json::json!(from_history.map(|t| t.to_string())), "{id}");
        let item = r.s.contract().get(&keys::delivery(id)).unwrap().unwrap();
        assert!(item.value.data.get(crate::control::records::ENTERED).is_some(), "{id}: no entered on the record");
    }
}

/// law-list-reads-run-on-indexed-tables: `batches-of` over a store of hundreds of rounds reads the
/// bead's own membership range and the rounds it names, never the `batches/` prefix.
#[test]
fn batches_of_reads_the_beads_own_rounds_not_every_round() {
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..600 {
        let (id, round) = (format!("sp-{i:04}"), format!("b-{i:04}"));
        r.certified(&id, T1);
        let members = vec![(id, T1.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: &round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    r.s.contract().backend().take();
    let rows = r.s.batches_of("sp-0234").unwrap();
    assert_eq!(rows.iter().map(|b| b.batch_id.as_str()).collect::<Vec<_>>(), ["b-0234"]);
    assert!(r.s.batches_of("sp-absent").unwrap().is_empty());
    let reads = r.s.contract().backend().take();
    assert!(reads.len() <= 8, "{} reads for one bead among 600 rounds: {reads:?}", reads.len());
    assert!(
        reads.iter().all(|(k, key, _)| *k == "get" || !key.starts_with(keys::BATCHES)),
        "a range over the rounds: {reads:?}"
    );
}

/// A member added by `stack` is found by `batches-of` as one named at the cut is.
#[test]
fn batches_of_finds_a_member_stacked_into_an_open_round() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let first = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&first)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    r.certified("sp-b", T2);
    r.certified("sp-c", T2);
    let added = [("sp-b".to_string(), T2.to_string()), ("sp-c".to_string(), T2.to_string())];
    let p = r.s.plan_stack("b-1", &added, "test", AT).unwrap();
    r.s.run_planned(p, &r.req(), "stack").unwrap();
    for bead in ["sp-a", "sp-b", "sp-c"] {
        assert_eq!(r.s.batches_of(bead).unwrap().len(), 1, "{bead}");
    }
}

#[test]
fn create_bead_refuses_the_planning_fields_it_used_to_keep() {
    let r = Rig::new();
    for flag in ["--title", "--priority"] {
        let (rc, why) = r.lc(&["create-bead", "sp-plan", flag, "x", "--ready"]);
        assert_eq!(rc, crate::REFUSED, "{flag}: {why}");
        assert!(why.contains(flag), "{why}");
    }
    assert_eq!(r.lc(&["show", "sp-plan"]).0, 1, "a refused create leaves no record");
    // The positive control: the same call without the planning flag creates the record.
    r.ok(&["create-bead", "sp-plan", "--ready"]);
    assert_eq!(r.lc(&["show", "sp-plan"]).0, 0);
}

fn record_round(r: &Rig, row: lifecycle::batch::BatchRow, opened_at: i64) {
    let key = keys::batch(&row.repo, &row.batch_id);
    let rec = crate::control::records::BatchRecord { row, members: vec![], opened_at, round: Default::default() };
    let mut c = super::Cascade::new();
    r.s.new_round(&mut c, &rec.row.repo, &rec.row.batch_id).unwrap().unwrap();
    r.s.put_round(&mut c, &key, &rec).unwrap();
    r.s.commit(c, &r.req(), "cut").unwrap();
}

#[test]
fn dwell_reports_the_newest_round_cut_per_repo_from_the_round_records_whatever_the_audit_log_holds() {
    let r = Rig::new();
    open_round(&r);
    record_round(&r, lifecycle::batch::BatchRow::cut("b-late", "spira", "H", "B"), AT + 30);
    record_round(&r, lifecycle::batch::BatchRow::staged("b-staged", "spira", "H", "B", "b-1"), AT + 50);
    record_round(&r, lifecycle::batch::BatchRow::cut("b-other", "other", "H", "B"), AT + 20);
    let d = r.s.dwell(AT + 100, 3_600, None).unwrap();
    assert_eq!(d.last["spira"]["Deliver"], AT + 30, "a staged round was not cut: {:?}", d.last);
    assert_eq!(d.last["other"]["Deliver"], AT + 20, "{:?}", d.last);
    let old = r.s.dwell(AT + 100_000, 3_600, None).unwrap();
    assert!(old.last.get("spira").is_none_or(|k| !k.contains_key("Deliver")), "a cut older than the window is not reported: {:?}", old.last);
}

#[test]
fn a_cut_ships_each_step_it_committed_under_its_machine() {
    use spira_otel::vlogs::fake::FakeLogs;
    use std::time::Duration;
    let logs = FakeLogs::start();
    let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock()), Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let r = Rig { s: Store::new(c, 1_000).with_shipper(spira_otel::vlogs::Shipper::start(&logs.url())), n: Cell::new(0) };
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let Planned::Cascade(c) = r.s.plan_cut(&r.cut_spec(&members)).unwrap() else { panic!("the cut was turned away") };
    r.s.run_planned(Planned::Cascade(c), &r.req(), "cut").unwrap();
    let cut: Vec<(String, String)> = logs
        .wait_until(|got| got.iter().filter(|rec| rec["stream"] == "lifecycle.delivery").count() >= 4, Duration::from_secs(5))
        .into_iter()
        .filter(|rec| rec["to"] == "IN_DELIVERY" || rec["stream"] == "lifecycle.delivery")
        .map(|rec| (rec["stream"].as_str().unwrap().to_string(), rec["bead_id"].as_str().unwrap().to_string()))
        .collect();
    assert!(cut.iter().any(|(s, b)| s == "lifecycle.bead" && b == "sp-a"), "{cut:?}");
    assert!(cut.iter().any(|(s, b)| s == "lifecycle.delivery" && b == "sp-b"), "the cut's delivery steps ship under their own machine: {cut:?}");
}

fn claim_view_of(r: &Rig, id: &str) -> serde_json::Value {
    let (rc, out) = r.lc(&["list", "--ids", id]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    rows[0]["claim_view"].clone()
}

#[test]
fn list_answers_a_beads_claimability_from_its_prerequisite_when_it_is_read() {
    let r = Rig::new();
    for id in ["sp-a", "sp-b"] {
        r.ok(&["create-bead", id, "--repo", "spira", "--ready"]);
    }
    r.ok(&["set-claim-inputs", "sp-a"]);
    r.ok(&["set-claim-inputs", "sp-b", "--block", "sp-a"]);

    let b = claim_view_of(&r, "sp-b");
    assert_eq!((b["verdict"].as_str(), b["blocker"].as_str()), (Some("blocked"), Some("sp-a")));
    assert_eq!(claim_view_of(&r, "sp-a")["verdict"], "claimable");

    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable", "no write-back stands between the landing and the next list");
    assert!(claim_view_of(&r, "sp-a").is_null(), "a LANDED record has no claim view");
}

#[test]
fn a_ready_record_without_a_repo_says_the_repo_is_what_its_claim_view_lacks() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-a"]);
    let v = claim_view_of(&r, "sp-a");
    assert_eq!(v["verdict"], "cannot-tell");
    assert_eq!(v["missing"], serde_json::json!(["repo"]));
}

#[test]
fn list_keeps_a_pilgrimage_blocked_while_its_blocker_is_in_delivery_and_claimable_once_it_lands() {
    let r = Rig::new();
    open_round(&r);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
    r.ok(&["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-b", "--block", "sp-a", "--label", "pilgrimage"]);

    let b = claim_view_of(&r, "sp-b");
    assert_eq!((b["verdict"].as_str(), b["blocker"].as_str()), (Some("blocked"), Some("sp-a")));
    assert_eq!(b["runes"], serde_json::json!([]));

    r.bead_event("sp-a", "IN_DELIVERY", "4", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable");
}

#[test]
fn list_with_unreadable_view_inputs_still_lists_and_each_ready_row_says_it_cannot_tell() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.ok(&["create-bead", "sp-w", "--repo", "spira"]);
    let args: Vec<String> = ["list", "--live"].iter().map(|s| s.to_string()).collect();
    let req = r.req();
    let (rc, out) = state::run(&r.s, &args, &Call { now: AT, request: &req.id, inputs: &crate::control::claimable::UNREADABLE });
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let by = |id: &str| rows.as_array().unwrap().iter().find(|r| r["bead_id"] == id).unwrap().clone();
    assert_eq!(by("sp-a")["claim_view"]["verdict"], "cannot-tell");
    assert!(by("sp-a")["claim_view"]["error"].as_str().unwrap().contains("SPIRA_STACK_MAX_DEPTH"));
    assert!(by("sp-w")["claim_view"].is_null(), "a record the view does not cover is untouched");
}

fn repo_of(r: &Rig, id: &str) -> serde_json::Value {
    let (rc, out) = r.lc(&["list", "--ids", id]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    rows[0]["repo"].clone()
}

#[test]
fn create_bead_with_a_repo_fills_the_repo_of_a_record_another_writer_made_without_one() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-a"]);
    assert_eq!(claim_view_of(&r, "sp-a")["verdict"], "cannot-tell");
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(repo_of(&r, "sp-a"), "spira");
    assert_ne!(claim_view_of(&r, "sp-a")["verdict"], "cannot-tell");
    let (rc, _) = r.lc(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(rc, 0);
}

#[test]
fn create_bead_never_overwrites_a_repo_the_record_already_names() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "deck"]);
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(repo_of(&r, "sp-a"), "deck");
}

#[test]
fn create_bead_leaves_a_working_record_without_a_repo_and_still_exits_zero() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Working);
    let (rc, out) = r.lc(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(rc, 0, "{out}");
    assert!(repo_of(&r, "sp-a").is_null());
}

#[test]
fn a_round_landing_enqueues_one_closeout_for_each_github_member_and_none_for_the_rest() {
    let r = Rig::new().with_refs(&[("sp-a", "github:o/r#7")]);
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    r.s.run_planned(p, &req, "land").unwrap();
    let rows = r.s.gh_closeouts().unwrap();
    assert_eq!(rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-a"], "{rows:?}");
    assert_eq!((rows[0].1["outcome_state"].as_str(), rows[0].1["external_ref"].as_str()), (Some("LANDED"), Some("github:o/r#7")));
    r.s.gh_closeout_done("sp-a", &r.req()).unwrap();
    let again = r.lc(&["event", "bead", "sp-a", "--expect", "LANDED", "--version", "99", "--actor", "test", "--kind", r#"{"ContentOnBase":{"proof":"p"}}"#]);
    assert_ne!(again.0, 0, "a landed bead refuses a second landing: {}", again.1);
    assert!(r.s.gh_closeouts().unwrap().is_empty(), "a repeated landing event enqueues nothing");
}

#[test]
fn a_content_on_base_landing_enqueues_one_closeout() {
    let r = Rig::new().with_refs(&[("sp-a", "github:o/r#7")]);
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(r.s.gh_closeouts().unwrap().len(), 1);
}

#[test]
fn a_list_that_would_read_every_row_is_refused_naming_the_filters_that_do_not() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    r.ok(&["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    for argv in [
        &["list"][..],
        &["list", "--hold", "wait"],
        &["list", "--repo", "spira"],
        &["list", "--express"],
        &["list", "--since", "x"],
        &["list", "--state", "LANDED"],
        &["list", "--state", "READY,LANDED"],
    ] {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, crate::REFUSED, "{argv:?}: {out}");
        for exit in ["--live", "--ids", "--after", "--limit"] {
            assert!(out.contains(exit), "{argv:?} names {exit}: {out}");
        }
    }
    let ids = |argv: &[&str]| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x["bead_id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&["list", "--live"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--state", "READY"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--ids", "sp-a"]), ["sp-a"]);
    assert_eq!(ids(&["list", "--after", "sp-a", "--limit", "2"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--hold", "wait", "--live"]), Vec::<String>::new());
}

#[test]
fn a_watch_past_compacted_history_answers_with_the_resume_revision_and_reads_no_row() {
    let r = Rig::on(Counting::default(), &clock());
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    let old = r.s.contract().head().unwrap();
    for i in 0..5 {
        r.ok(&["create-bead", &format!("sp-b{i}"), "--repo", "spira", "--ready"]);
    }
    let head = r.s.contract().head().unwrap();
    r.s.contract().compact(head).unwrap();
    r.s.contract().backend().take();
    let fed = r.s.watch(&[keys::LIFECYCLE, keys::READINESS], old, 0).unwrap();
    assert!(matches!(fed, crate::control::feed::Feed::Relist { list_rev, .. } if list_rev == head), "too old resumes from the head");
    let reads = r.s.contract().backend().take();
    assert!(reads.iter().all(|(k, ..)| *k != "range" && *k != "range_in_state"), "no row is read: {reads:?}");
}

#[test]
fn a_round_cut_with_one_member_twice_writes_no_discontinuous_delivery_event() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let members = vec![("sp-a".to_string(), T1.to_string()); 2];
    let head = r.s.contract().head().unwrap();
    let planned = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let Planned::Ran(Ran::Turned(Turned::CannotTell(why))) = &planned else {
        panic!("a cut naming a member twice must be refused whole");
    };
    assert!(why.contains("sp-a is named twice"), "{why}");
    assert_eq!(r.s.contract().head().unwrap(), head, "a refused cut writes nothing");
    assert!(r.s.delivery("sp-a").unwrap().is_none());
}

#[test]
fn eject_member_of_a_round_whose_pass_is_running_refuses_with_its_exit_and_changes_nothing() {
    let r = Rig::new();
    open_round(&r);
    let send = |expect: BatchState, kind: BatchEventKind| {
        let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
        let ev = BatchEvent { expect, version: rec.value.row.version, kind, actor: "test".into() };
        r.s.batch_event_with("b-1", &ev, AT + 10, &r.req(), None).unwrap();
    };
    send(BatchState::Open, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
    let (_, before) = r.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::CiRunning,
        version: before.value.row.version,
        kind: BatchEventKind::Eject { bead_id: "sp-a".into(), reason: "red".into() },
        actor: "concierge".into(),
    };
    let (p, _) = r.s.plan_eject("b-1", "sp-a", &ev, AT + 20, &r.req()).unwrap();
    let Planned::Ran(Ran::Refused { refusal: lifecycle::Refusal::PassRunning { exit, .. }, .. }) = p else {
        panic!("a running pass refuses the eject");
    };
    assert!(exit.contains("round abandon") && exit.contains("hold"), "{exit}");
    let (_, after) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(after.value.row, before.value.row);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}

fn ranges_of_rounds(reads: &[(&'static str, String, usize)]) -> usize {
    reads.iter().filter(|(k, p, _)| *k == "range" && p.starts_with(keys::BATCHES)).count()
}

fn end_round<B: Backend>(r: &Rig<B>, round: &str, state: BatchState) {
    let (_, mut rec) = r.s.batch(round).unwrap().unwrap();
    rec.value.row.state = state;
    let put = state_store::Txn::new().put(&keys::batch("spira", round), state_store::Object::new(crate::control::records::encode(&rec.value).unwrap()));
    r.s.contract().txn(&put, &r.req()).unwrap();
}

/// law-list-reads-paginate-in-the-database: the orphan sweep reads the open rounds through the
/// state index, never the `batches/` prefix.
#[test]
fn requeue_orphans_reads_the_open_rounds_by_state_not_the_batches_prefix() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, 240, &[3, 7]);
    r.certified("sp-orph", T1);
    let members = vec![("sp-orph".to_string(), T1.to_string())];
    let cut = Cut { batch_id: "b-orph", at: AT - 100_000, ..r.cut_spec(&members) };
    let p = r.s.plan_cut(&cut).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    end_round(&r, "b-orph", BatchState::Abandoned);
    r.s.contract().backend().take();
    let found = r.s.requeue_orphans("test", false, 1800, AT + 3600, &Default::default(), &r.req()).unwrap();
    let reads = r.s.contract().backend().take();
    assert_eq!(ranges_of_rounds(&reads), 0, "{reads:?}");
    assert!(reads.iter().any(|(k, _, _)| *k == "range_in_state"), "{reads:?}");
    let mut expected: Vec<String> = (0..240).map(|i| format!("sp-{i:03}")).collect();
    expected.push("sp-orph".into());
    assert_eq!(found.orphans, expected, "the answer the full scan gave");
    assert_eq!(found.phantom_batches, ["b-003", "b-007"]);
}

/// The dwell verb's rounds come from the `opened_at` index, and a Staged round is still skipped.
#[test]
fn dwell_reads_the_rounds_in_its_window_from_the_opened_at_index() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, 240, &[3, 7]);
    end_round(&r, "b-000", BatchState::Staged);
    r.s.contract().backend().take();
    let d = r.s.dwell(AT, 1000, None).unwrap();
    let reads = r.s.contract().backend().take();
    assert_eq!(ranges_of_rounds(&reads), 0, "{reads:?}");
    assert_eq!(reads.iter().filter(|(k, _, _)| *k == "range_newest_opened").count(), 1, "{reads:?}");
    assert_eq!(d.last["spira"]["Deliver"], AT - 100, "b-000 is staged and skipped; b-001 is next");
}

#[test]
fn dwell_with_a_window_holding_the_cap_of_rounds_errors_naming_it() {
    let r = Rig::new();
    many_rounds(&r, 0, crate::control::dwell::DWELL_ROUNDS_MAX, &[]);
    let e = r.s.dwell(AT, 100 * crate::control::dwell::DWELL_ROUNDS_MAX as u64, None).unwrap_err();
    assert!(e.to_string().contains("DWELL_ROUNDS_MAX"), "{e}");

fn seed<T: serde::Serialize>(r: &Rig<Counting>, key: &str, rec: &T) {
    r.s.contract()
        .create(key, state_store::Object::new(crate::control::records::encode(rec).unwrap()), &r.req())
        .unwrap();
}

fn ask_rec(id: &str, state: lifecycle::ask::AskState, at: i64) -> crate::control::store::asks::AskRecord {
    let mut row = lifecycle::ask::AskRow::open(id, None);
    row.state = state;
    crate::control::store::asks::AskRecord { row, opened_at: at, closed_at: None }
}

fn reads_of(r: &Rig<Counting>) -> Vec<(&'static str, String)> {
    r.s.contract().backend().take().into_iter().map(|(k, p, _)| (k, p)).collect()
}

/// law: a list reads the live set. `list-asks` over many closed asks reads the OPEN state index
/// only, and `--state ALL` still answers every ask.
#[test]
fn list_asks_reads_only_the_open_asks_and_all_reads_every_one() {
    use lifecycle::ask::AskState;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..300 {
        let s = if i % 2 == 0 { AskState::Answered } else { AskState::DefaultTaken };
        seed(&r, &keys::ask(&format!("a-{i:03}")), &ask_rec(&format!("a-{i:03}"), s, 1_000 + i));
    }
    seed(&r, &keys::ask("a-z2"), &ask_rec("a-z2", AskState::Open, 5_000));
    seed(&r, &keys::ask("a-z1"), &ask_rec("a-z1", AskState::Open, 4_000));
    reads_of(&r);
    let (rc, out) = r.lc(&["list-asks"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = rows.as_array().unwrap().iter().map(|a| a["ask_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a-z1", "a-z2"]);
    let reads = reads_of(&r);
    assert!(reads.iter().any(|(k, p)| *k == "range_in_state" && p == "asks/Open"), "{reads:?}");
    assert!(!reads.iter().any(|(k, _)| *k == "range"), "{reads:?}");
    let (rc, out) = r.lc(&["list-asks", "--state", "ALL"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap().as_array().unwrap().len(), 302);
}

/// law: a list reads the live set. `mending list --live` over many returned rows ranges the
/// WAITING and MENDING state indexes only, and reads one ask per live row at most.
#[test]
fn mending_list_live_reads_only_the_waiting_and_mending_rows() {
    use lifecycle::mending::MendingState;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..300 {
        let mut rec = crate::control::store::mending::MendingRecord::failed("b-old", 1, &format!("s{i:03}"), 1_000 + i);
        rec.row.state = MendingState::Returned;
        seed(&r, &keys::mending(&rec.row.key()), &rec);
    }
    let mut waiting = crate::control::store::mending::MendingRecord::failed("b-1", 1, "unit", 2_000);
    waiting.row.awaiting_ask = Some("a-1".into());
    seed(&r, &keys::mending(&waiting.row.key()), &waiting);
    let mut mending = crate::control::store::mending::MendingRecord::failed("b-1", 1, "lint", 3_000);
    mending.row.state = MendingState::Mending;
    seed(&r, &keys::mending(&mending.row.key()), &mending);
    seed(&r, &keys::ask("a-1"), &ask_rec("a-1", lifecycle::ask::AskState::Open, 1));
    reads_of(&r);
    let (rc, out) = r.lc(&["mending", "list", "--live"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let got: Vec<(&str, bool)> = rows.as_array().unwrap().iter().map(|m| (m["suite"].as_str().unwrap(), m["ask_open"].as_bool().unwrap())).collect();
    assert_eq!(got, [("unit", true), ("lint", false)]);
    let reads = reads_of(&r);
    assert!(!reads.iter().any(|(k, p)| *k == "range" && p.starts_with("mendings/")), "{reads:?}");
    assert!(reads.iter().filter(|(k, _)| *k == "range_in_state").all(|(_, p)| p == "mendings/Waiting" || p == "mendings/Mending"), "{reads:?}");
    assert!(reads.iter().filter(|(_, p)| p.starts_with("asks/")).count() <= 2, "{reads:?}");
}

/// A round prints the largest `pr` among its members' deliveries that belong to it, else null.
#[test]
fn list_batches_prints_the_pr_of_a_members_delivery_in_the_round() {
    let r = Rig::on(Counting::default(), &clock());
    open_round(&r);
    let pr_of = |r: &Rig<Counting>| {
        let (rc, out) = r.lc(&["list", "--batches"]);
        assert_eq!(rc, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()[0]["pr"].clone()
    };
    assert_eq!(pr_of(&r), serde_json::Value::Null);
    let key = keys::delivery("sp-a");
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["pr"] = serde_json::json!(42);
    let t = state_store::Txn::new().when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev))).put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    assert_eq!(pr_of(&r), serde_json::json!(42));
}
