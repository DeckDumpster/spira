use std::cell::Cell;

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchState};
use lifecycle::bead::BeadState;
use lifecycle::delivery::DeliveryState;
use runtime::ports::VirtualClock;
use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};
use state_store_sqlite::SqliteBackend;

use super::{Cut, Planned, Ran, Store};
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
        self.certified_with(
            id,
            tip,
            r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#,
        );
    }

    /// CERTIFIED at `tip`, stacked on `on`'s current tip.
    fn certified_on(&self, id: &str, tip: &str, on: (&str, &str)) {
        let claim = format!(
            r#"{{"Claim":{{"holder":"dwarf-1","lease_until":1767229200,"stack":{{"{}":"{}"}},"stack_depth":1,"stack_max_depth":3}}}}"#,
            on.0, on.1
        );
        self.certified_with(id, tip, &claim);
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

/// A round of sp-a and sp-b, sp-b stacked on sp-a, red and awaiting attribution.
fn attributing(r: &Rig) {
    r.certified("sp-a", T1);
    r.certified_on("sp-b", T2, ("sp-a", T1));
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    batch_event(
        r,
        BatchState::Open,
        2,
        BatchEventKind::CiStarted { run: "r1".into() },
    );
    batch_event(r, BatchState::CiRunning, 3, BatchEventKind::Red);
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
fn a_settle_returns_the_red_member_and_carries_out_the_member_stacked_on_it() {
    let r = Rig::new();
    attributing(&r);
    let (rc, out) = r.lc(&[
        "settle",
        "b-1",
        "--expect",
        "ATTRIBUTING",
        "--version",
        "4",
        "--actor",
        "test",
        "--eject",
        "sp-a",
    ]);
    assert_eq!(rc, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["base_withdrawn"], 1);
    assert_eq!(v["stacked_on"]["sp-b"], "sp-a");
    assert_eq!(round_state(&r), BatchState::Settled);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::Rework);
    }
    let reasons: Vec<Option<String>> = ["sp-a", "sp-b"]
        .iter()
        .map(|id| r.s.record(id).unwrap().unwrap().value.row.reason)
        .collect();
    assert_eq!(
        reasons,
        [
            Some("batch-ejected-red".into()),
            Some("base-withdrawn".into())
        ]
    );
    let returned = r.s.history("sp-b", Some("bead")).unwrap();
    let last = &returned.last().unwrap().audit;
    assert_eq!(
        (last.event.as_str(), &last.evidence["stacked_on"]),
        ("Returned", &serde_json::json!("sp-a"))
    );
}

#[test]
fn a_settle_whose_member_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    attributing(&r);
    let req = r.req();
    let ev = batch_ev(BatchState::Attributing, 4, BatchEventKind::Settle);
    let (p, _) =
        r.s.plan_settle("b-1", &ev, &["sp-a".into()], &[], AT, &req)
            .unwrap();
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "settle").unwrap() else {
        panic!("the settle did not run");
    };
    assert!(!applied.is_empty() && applied.iter().all(|a| !a));
    assert_eq!(round_state(&r), BatchState::Attributing);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::InDelivery);
        assert_eq!(
            r.s.delivery(id).unwrap().unwrap().value.state,
            DeliveryState::Batched
        );
    }
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
fn an_eject_requeues_the_member_and_withdraws_its_dependent_in_one_txn_or_not_at_all() {
    let r = Rig::new();
    attributing(&r);
    let ev = batch_ev(
        BatchState::Attributing,
        4,
        BatchEventKind::Eject {
            bead_id: "sp-a".into(),
            reason: "red".into(),
        },
    );
    let req = r.req();
    let (p, withdrawn) = r.s.plan_eject("b-1", "sp-a", &ev, AT, &req).unwrap();
    assert_eq!(withdrawn, [("sp-b".to_string(), "sp-a".to_string())]);
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "eject").unwrap() else {
        panic!("the eject did not run");
    };
    assert!(applied.iter().all(|a| !a));
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
    let (p, _) = r.s.plan_eject("b-1", "sp-a", &ev, AT, &req).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "eject").unwrap() else {
        panic!("the eject did not run");
    };
    assert_eq!(applied, vec![true; 5]);
    assert_eq!(
        (r.state("sp-a"), r.state("sp-b")),
        (BeadState::Certified, BeadState::Rework)
    );
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(
        round.value.members.len(),
        2,
        "an ejected member keeps its place"
    );
}

fn open_round(r: &Rig) {
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
        r.s.requeue_orphans("test", true, 1800, AT + 60, &req)
            .unwrap();
    assert!(early.phantom_batches.is_empty() && early.orphans.is_empty());
    let late =
        r.s.requeue_orphans("test", false, 1800, AT + 3600, &req)
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
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &r.req())
            .unwrap();
    assert_eq!(
        (applied.abandoned, applied.requeued, applied.failed),
        (vec!["b-1".to_string()], vec!["sp-a".to_string()], vec![])
    );
    assert_eq!(round_state(&r), BatchState::Abandoned);
    assert_eq!(r.state("sp-a"), BeadState::Certified);
}

#[test]
fn requeue_orphans_leaves_a_leased_round_that_never_began_open_and_lists_it_leased() {
    let r = Rig::new();
    open_round(&r);
    r.s.contract()
        .lock("round/spira/b-1", 600_000, &r.req())
        .unwrap();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &r.req())
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

/// The gate a claim would compare on `id`, as the store holds it.
fn stored_gate<B: Backend>(r: &Rig<B>, id: &str) -> spira_config::edges::Gate {
    let item = r.s.contract().get(&keys::gate(id)).unwrap().unwrap();
    serde_json::from_value(item.value.data).unwrap()
}

#[test]
fn a_landing_and_a_settle_move_each_members_gate_with_its_record() {
    use spira_config::edges::GateClass;
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    r.s.run_planned(p, &req, "land").unwrap();
    for id in ["sp-a", "sp-b"] {
        assert_eq!(stored_gate(&r, id).class, GateClass::Finished, "{id}");
    }
    let r = Rig::new();
    attributing(&r);
    r.ok(&[
        "settle",
        "b-1",
        "--expect",
        "ATTRIBUTING",
        "--version",
        "4",
        "--actor",
        "test",
        "--eject",
        "sp-a",
    ]);
    for id in ["sp-a", "sp-b"] {
        let rec = r.s.record(id).unwrap().unwrap().value;
        assert_eq!(stored_gate(&r, id), rec.gate(), "{id}");
        assert_eq!(stored_gate(&r, id).class, GateClass::Unfinished, "{id}");
    }
}

#[test]
fn the_queues_fields_of_a_round_survive_every_machine_step_and_the_landing() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let key = keys::batch("spira", "b-1");
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["round"] = serde_json::json!({ "worktree": "/wt/b-1", "red": "test-a.sh" });
    let t = state_store::Txn::new()
        .when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev)))
        .put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
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
        [("red", "test-a.sh"), ("worktree", "/wt/b-1")]
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
        &format!("sp-a:{T1}"), "--actor", "test", "--round", r#"{"worktree":"/wt/b-1","actor":"q"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the cut and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"actor": "q", "worktree": "/wt/b-1"})));
    let before = r.s.contract().head().unwrap();
    r.ok(&[
        "event", "batch", "b-1", "--expect", "OPEN", "--version", "1", "--actor", "test",
        "--kind", r#"{"CiStarted":{"run":"r1"}}"#, "--round", r#"{"worktree":"/wt/b-1","red":"x"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the event and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"red": "x", "worktree": "/wt/b-1"})));
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
    let fields = [("worktree".to_string(), "/wt".to_string())].into();
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
