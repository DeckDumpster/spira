use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState, Stack};
use runtime::ports::{Clock, VirtualClock};
use state_store::gc::Controller;
use state_store::{
    Backend, Config, Contract, Deleted, Error, MemBackend, Ports, Request, Selector,
};

use super::{Judged, Outcome, Pass, Store};
use crate::control::keys;
use crate::control::records::{encode, Audit};

const FLOOR_MS: u64 = 1_000;
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

fn open(b: &MemBackend, clock: &VirtualClock) -> Store<MemBackend> {
    let c = Contract::open(
        b.clone(),
        Ports::virtual_clock(clock),
        Config {
            min_lock_ttl_ms: FLOOR_MS,
        },
    )
    .unwrap();
    Store::new(c, FLOOR_MS)
}

fn fresh() -> (Store<MemBackend>, MemBackend, VirtualClock) {
    let (b, clock) = (MemBackend::new(), VirtualClock::new(0));
    (open(&b, &clock), b, clock)
}

fn req(id: &str) -> Request {
    Request::new(id, "test", "t")
}

fn ev(expect: BeadState, version: u64, kind: BeadEventKind, actor: &str, at: i64) -> BeadEvent {
    BeadEvent {
        expect,
        version,
        kind,
        actor: actor.into(),
        at: Some(at),
    }
}

fn claim(holder: &str, lease_until: i64) -> BeadEventKind {
    BeadEventKind::Claim {
        holder: holder.into(),
        lease_until,
        stack: Stack::new(),
        stack_depth: 0,
        stack_max_depth: 4,
        persona: None,
        directed: None,
    }
}

fn created(s: &Store<MemBackend>, id: &str) {
    assert!(!s
        .create(
            id,
            Some("harness"),
            None,
            100,
            &req(&format!("create-{id}"))
        )
        .unwrap());
}

fn audits_at<B: Backend>(s: &Store<B>, rev: u64) -> (bool, Vec<Audit>) {
    let e = s
        .contract()
        .get(&state_store::events_key(rev))
        .unwrap()
        .expect("an events key per txn");
    let refused = e.value.data["refused"].as_bool().unwrap();
    let branch = if refused { "else" } else { "then" };
    (
        refused,
        serde_json::from_value(e.value.data["evidence"][branch].clone()).unwrap(),
    )
}

fn seed_judged(s: &Store<MemBackend>, id: &str, request: &str) {
    let judged = [Judged { id: id.into(), claimable: true, ..Default::default() }];
    let pass = Pass { judged_at: "2026-10-31T12:00:00Z".into(), ..Default::default() };
    s.publish_readiness(&judged, &pass, &req(request)).unwrap();
}

#[test_id(t0jeypg)]
#[test]
fn a_readiness_pass_between_a_read_and_a_claim_does_not_conflict_the_claim() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let before = s.record("sp-a").unwrap().unwrap();
    seed_judged(&s, "sp-a", "pass-1");
    assert_eq!(
        s.record("sp-a").unwrap().unwrap().mod_rev,
        before.mod_rev,
        "a readiness pass moved the lifecycle record"
    );
    let (out, fence) = s
        .claim(
            "sp-a",
            &ev(
                BeadState::Ready,
                before.value.row.version,
                claim("dwarf-1", 160),
                "dwarf-1",
                100,
            ),
            &req("claim-1"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    assert!(fence.is_some());
    let r = s.readiness("sp-a").unwrap().unwrap().value;
    assert_eq!(r.judged_at.as_deref(), Some("2026-10-31T12:00:00Z"));
}

fn publish_at(s: &Store<MemBackend>, judged: &[Judged], at: &str, request: &str) -> u64 {
    let pass = Pass { judged_at: at.into(), planning_watermark: Some(at.into()), ..Default::default() };
    s.publish_readiness(judged, &pass, &req(request)).unwrap()
}

fn two_judged(s: &Store<MemBackend>) -> Vec<Judged> {
    ["sp-a", "sp-b"]
        .map(|id| {
            created(s, id);
            Judged { id: id.into(), claimable: true, ..Default::default() }
        })
        .into()
}

fn readiness_mod_revs(s: &Store<MemBackend>) -> Vec<u64> {
    ["sp-a", "sp-b"].map(|id| s.readiness(id).unwrap().unwrap().mod_rev.main).into()
}

#[test_id(tp61hwe)]
#[test]
fn a_pass_that_changes_no_judgement_leaves_the_head_and_every_readiness_key_alone() {
    let (s, _, _) = fresh();
    let judged = two_judged(&s);
    publish_at(&s, &judged, "2026-10-31T12:00:00Z", "pass-1");
    let (head, keys) = (s.contract().head().unwrap(), readiness_mod_revs(&s));
    let rev = publish_at(&s, &judged, "2026-10-31T12:00:09Z", "pass-2");
    assert_eq!((rev, s.contract().head().unwrap()), (head, head));
    assert_eq!(readiness_mod_revs(&s), keys);
}

#[test_id(t97denn)]
#[test]
fn a_pass_puts_exactly_the_record_whose_claim_fields_changed() {
    type Change = fn(&mut Judged);
    let changes: [(&str, Change); 4] = [
        ("claimable", |j| j.claimable = false),
        ("blocked_by", |j| j.blocked_by = vec!["sp-z".into()]),
        ("verdict", |j| j.verdict = Some("held".into())),
        ("gates", |j| j.gates = [("sp-z".to_string(), Some(spira_config::edges::gate(BeadState::Working, None, false)))].into()),
    ];
    for (field, change) in changes {
        let (s, _, _) = fresh();
        let mut judged = two_judged(&s);
        publish_at(&s, &judged, "2026-10-31T12:00:00Z", "pass-1");
        let before = readiness_mod_revs(&s);
        change(&mut judged[1]);
        let rev = publish_at(&s, &judged, "2026-10-31T12:00:09Z", "pass-2");
        assert_eq!(readiness_mod_revs(&s), vec![before[0], rev], "{field}");
        let put = s.readiness("sp-b").unwrap().unwrap().value;
        assert_eq!(put.judged_at.as_deref(), Some("2026-10-31T12:00:09Z"), "{field}");
    }
}

#[test_id(tmjrgus)]
#[test]
fn the_first_claim_judgement_of_a_record_is_written_even_when_it_matches_the_defaults() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let judged = [Judged { id: "sp-a".into(), ..Default::default() }];
    publish_at(&s, &judged, "2026-10-31T12:00:00Z", "pass-1");
    assert!(s.readiness("sp-a").unwrap().unwrap().value.judged_for_claim);
}

#[test_id(tjo77m8)]
#[test]
fn a_claim_on_a_judgement_an_earlier_pass_wrote_applies_and_records_that_writes_rev() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let gates = [("sp-gone".to_string(), None)].into();
    let judged = [Judged { id: "sp-a".into(), claimable: true, gates, ..Default::default() }];
    publish_at(&s, &judged, "2026-10-31T12:00:00Z", "pass-1");
    let written_at = s.readiness("sp-a").unwrap().unwrap().value.judged_at_rev;
    created(&s, "sp-b");
    publish_at(&s, &judged, "2026-10-31T12:00:09Z", "pass-2");
    let (out, _) = s
        .claim("sp-a", &ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100), &req("claim-1"))
        .unwrap();
    let Outcome::Applied { rev, .. } = out else {
        panic!("{out:?}")
    };
    let (_, audits) = audits_at(&s, rev);
    assert_eq!(audits[0].evidence["Claim"]["judged"]["judged_at_rev"], written_at);
}

#[test_id(tc1w5hb)]
#[test]
fn a_transition_is_one_txn_on_the_records_mod_rev_audited_in_its_events_key() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let hold = BeadEventKind::Hold {
        kind: lifecycle::bead::HoldKind::Wait,
        cause: lifecycle::reason::HoldCause::NoProgressBackoff,
        detail: None,
    };
    let Outcome::Applied { rev, record } = s
        .apply(
            "sp-a",
            &ev(BeadState::Ready, 0, hold, "ops", 200),
            &req("hold-1"),
        )
        .unwrap()
    else {
        panic!("the hold did not apply")
    };
    assert_eq!(s.record("sp-a").unwrap().unwrap().mod_rev.main, rev);
    assert_eq!(record.updated_at, 200);
    let (refused, audits) = audits_at(&s, rev);
    assert!(!refused);
    assert_eq!(audits.len(), 1);
    assert_eq!(
        (
            audits[0].event.as_str(),
            audits[0].from_state.as_str(),
            audits[0].applied,
            audits[0].actor.as_str()
        ),
        ("Hold", "READY", true, "ops")
    );
}

#[test_id(tde5sgc)]
#[test]
fn a_write_from_a_stale_read_loses_the_race_and_the_loss_is_audited() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let stale = s.record("sp-a").unwrap().unwrap();
    s.apply(
        "sp-a",
        &ev(BeadState::Ready, 0, BeadEventKind::Express, "ops", 200),
        &req("x-1"),
    )
    .unwrap();
    let item = s.contract().get(&keys::lifecycle("sp-a")).unwrap().unwrap();
    let mut row = stale.value.row.clone();
    row.version += 1;
    let out = s
        .write(
            "sp-a",
            &ev(BeadState::Ready, 0, BeadEventKind::Unexpress, "ops", 201),
            &stale,
            &item,
            row,
            None,
            None,
            &req("x-2"),
        )
        .unwrap();
    let Outcome::LostRace { rev } = out else {
        panic!("{out:?}")
    };
    let (refused, audits) = audits_at(&s, rev);
    assert!(
        refused,
        "the lost write's events key does not say it was refused"
    );
    assert_eq!(
        (
            audits[0].applied,
            audits[0].refusal.as_deref(),
            audits[0].to_state.as_str()
        ),
        (false, Some("StaleVersion"), "READY")
    );
}

#[test_id(ttc4tih)]
#[test]
fn a_machine_refusal_writes_no_record_and_is_audited_with_its_reason() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let before = s.record("sp-a").unwrap().unwrap();
    let out = s
        .apply(
            "sp-a",
            &ev(BeadState::Working, 0, BeadEventKind::Release, "ops", 200),
            &req("r-1"),
        )
        .unwrap();
    let Outcome::Refused { rev, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(s.record("sp-a").unwrap().unwrap().mod_rev, before.mod_rev);
    let (_, audits) = audits_at(&s, rev);
    assert_eq!(
        (
            audits[0].applied,
            audits[0].refusal.as_deref(),
            audits[0].expect.as_str()
        ),
        (false, Some("ExpectMismatch"), "WORKING")
    );
}

#[test_id(tth1w2y)]
#[test]
fn create_twice_with_an_equal_value_succeeds_and_with_a_different_one_conflicts() {
    let (s, _, _) = fresh();
    assert!(!s
        .create("sp-a", Some("harness"), None, 100, &req("c-1"))
        .unwrap());
    assert!(
        s.create("sp-a", Some("harness"), None, 900, &req("c-2"))
            .unwrap(),
        "a re-create a moment later is not equal"
    );
    let err = s
        .create("sp-a", Some("elsewhere"), None, 100, &req("c-3"))
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    s.apply(
        "sp-a",
        &ev(BeadState::Ready, 0, BeadEventKind::Express, "ops", 200),
        &req("x-1"),
    )
    .unwrap();
    assert!(matches!(
        s.create("sp-a", Some("harness"), None, 100, &req("c-4"))
            .unwrap_err(),
        Error::Conflict { .. }
    ));
}

#[test_id(thvwntu)]
#[test]
fn a_tip_promotes_an_untouched_birth_and_a_born_submitted_record_audits_its_submit() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    assert!(!s
        .create("sp-a", Some("harness"), Some(TIP), 150, &req("c-2"))
        .unwrap());
    let r = s.record("sp-a").unwrap().unwrap().value;
    assert_eq!(
        (r.row.state, r.row.tip.as_deref(), r.row.version),
        (BeadState::Submitted, Some(TIP), 1)
    );
    assert!(!s.create("sp-b", None, Some(TIP), 150, &req("c-3")).unwrap());
    let rev = s.record("sp-b").unwrap().unwrap().mod_rev.main;
    let (_, audits) = audits_at(&s, rev);
    assert_eq!(
        (audits[0].event.as_str(), audits[0].to_state.as_str()),
        ("Submit", "SUBMITTED")
    );
}

#[test_id(t3dwaoz)]
#[test]
fn deleting_a_record_whose_bead_is_open_marks_it_and_it_goes_only_once_planning_closes_it() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let gc = Controller::new(s.contract());
    assert!(matches!(
        s.delete("sp-a", true, &req("d-1")).unwrap(),
        Deleted::Pending { .. }
    ));
    let marked = s.contract().get(&keys::lifecycle("sp-a")).unwrap().unwrap();
    assert!(marked.value.metadata.deletion_timestamp.is_some());
    gc.tick(clock.now_ms()).unwrap();
    assert!(
        s.record("sp-a").unwrap().is_some(),
        "GC removed a record its bead still has open"
    );
    assert!(s.planning_closed("sp-a", &req("p-1")).unwrap());
    assert!(
        s.record("sp-a").unwrap().is_none(),
        "the write that closed planning removed the record"
    );
    assert!(s.contract().get(&keys::gate("sp-a")).unwrap().is_some());
    gc.tick(clock.now_ms()).unwrap();
    assert!(s.contract().get(&keys::gate("sp-a")).unwrap().is_none());

    created(&s, "sp-b");
    assert!(matches!(
        s.delete("sp-b", false, &req("d-2")).unwrap(),
        Deleted::Removed { .. }
    ));
}

#[test_id(t6rmpmn)]
#[test]
fn a_holders_submit_after_its_lock_expired_and_another_holder_retook_it_is_refused() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let (_, first) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let first = first.unwrap();
    clock.advance(5_000);
    let reaped = s
        .apply(
            "sp-a",
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::HolderDead,
                "reaper",
                105,
            ),
            &req("dead-1"),
        )
        .unwrap();
    assert!(matches!(reaped, Outcome::Applied { .. }), "{reaped:?}");
    let v = s.record("sp-a").unwrap().unwrap().value.row.version;
    let (out, second) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, v, claim("dwarf-2", 200), "dwarf-2", 106),
            &req("claim-2"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    assert_ne!(second.unwrap().token, first.token);
    let v = s.record("sp-a").unwrap().unwrap().value.row.version;
    let late = s
        .holder_apply(
            "sp-a",
            &first,
            &ev(
                BeadState::Working,
                v,
                BeadEventKind::Submit { tip: TIP.into() },
                "dwarf-1",
                107,
            ),
            &req("submit-1"),
        )
        .unwrap();
    assert!(matches!(late, Outcome::LockLost { .. }), "{late:?}");
    assert_eq!(
        s.record("sp-a")
            .unwrap()
            .unwrap()
            .value
            .row
            .holder
            .as_deref(),
        Some("dwarf-2")
    );
}

#[test_id(ty4ss07)]
#[test]
fn a_holders_write_after_its_lock_expired_is_refused_even_before_anyone_retakes_it() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let (_, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    clock.advance(5_000);
    let out = s
        .holder_apply(
            "sp-a",
            &f.unwrap(),
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::Submit { tip: TIP.into() },
                "dwarf-1",
                107,
            ),
            &req("submit-1"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::LockLost { .. }), "{out:?}");
    assert_eq!(
        s.record("sp-a").unwrap().unwrap().value.row.state,
        BeadState::Working
    );
}

#[test_id(tblzy4w)]
#[test]
fn a_claim_is_one_txn_and_the_holders_token_is_its_locks_create_rev() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let head = s.contract().head().unwrap();
    let (out, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let Outcome::Applied { rev, record } = out else {
        panic!("{out:?}")
    };
    let f = f.unwrap();
    assert_eq!(rev, head + 1, "a claim took more than one commit");
    let lock = s
        .contract()
        .get(&keys::claim_lock("sp-a"))
        .unwrap()
        .unwrap();
    let stored = s.record("sp-a").unwrap().unwrap();
    assert_eq!(
        (f.token, f.token.main, stored.mod_rev.main),
        (lock.create_rev, rev, rev),
        "the record and its lock were not written by one txn"
    );
    assert_eq!(record.row.holder.as_deref(), Some("dwarf-1"));
    assert_eq!(s.holder_fence("sp-a").unwrap(), Some(f.clone()));
    let v = encode(&stored.value).unwrap();
    assert!(v.get("claim_token").is_none(), "a second lease in {v}");
    let out = s
        .holder_apply(
            "sp-a",
            &f,
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::Submit { tip: TIP.into() },
                "dwarf-1",
                110,
            ),
            &req("submit-1"),
        )
        .unwrap();
    let Outcome::Applied { record, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(
        (record.row.state, record.row.holder),
        (BeadState::Submitted, None)
    );
    assert!(
        s.contract()
            .get(&keys::claim_lock("sp-a"))
            .unwrap()
            .is_none(),
        "submit kept the claim lock"
    );
    assert_eq!(s.holder_fence("sp-a").unwrap(), None);
}

#[test_id(t5wl7lj)]
#[test]
fn a_claim_while_another_claims_lock_is_live_writes_nothing_and_answers_locked() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let other = s
        .contract()
        .lock(&keys::claim_resource("sp-a"), 60_000, &req("stray"))
        .unwrap();
    let before = s.record("sp-a").unwrap().unwrap();
    let (out, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    assert_eq!(out, Outcome::Locked { token: other.token });
    assert!(f.is_none());
    let after = s.record("sp-a").unwrap().unwrap();
    assert_eq!(
        (after.mod_rev, after.value.row.state),
        (before.mod_rev, BeadState::Ready)
    );
}

#[test_id(t1dhl8q)]
#[test]
fn a_working_records_lease_until_is_its_locks_expiry_and_follows_a_renew() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let (_, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let f = f.unwrap();
    let lease = |s: &Store<MemBackend>| s.record("sp-a").unwrap().unwrap().value.row.lease_until;
    let expiry = |s: &Store<MemBackend>| {
        s.contract()
            .get(&keys::claim_lock("sp-a"))
            .unwrap()
            .unwrap()
            .lock_expires_at_ms
            .unwrap()
    };
    assert_eq!(lease(&s), Some(expiry(&s).div_ceil(1_000) as i64));
    let first = lease(&s);
    clock.advance(1_500);
    s.renew(&f, &req("renew-1")).unwrap();
    assert!(
        lease(&s) > first,
        "a renew did not move the lease a read shows"
    );
    assert_eq!(lease(&s), Some(expiry(&s).div_ceil(1_000) as i64));
    let listed = s.in_state(BeadState::Working).unwrap();
    assert_eq!(listed[0].value.row.lease_until, lease(&s));
    clock.advance(10_000);
    assert_eq!(
        lease(&s),
        Some(102),
        "with its lock lapsed, a record reads its claim's own lease"
    );
}

#[test_id(tvrxmox)]
#[test]
fn a_refused_claim_takes_no_lock() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let (out, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 9, claim("dwarf-1", 160), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    assert!(
        matches!(
            out,
            Outcome::Refused {
                refusal: lifecycle::Refusal::StaleVersion { .. },
                ..
            }
        ),
        "{out:?}"
    );
    assert!(f.is_none());
    let lock = format!(
        "{}{}",
        state_store::LOCKS_PREFIX,
        keys::claim_resource("sp-a")
    );
    assert!(s.contract().get(&lock).unwrap().is_none());
}

#[test_id(tx62kmz)]
#[test]
fn a_claims_lock_lasts_its_lease_but_never_less_than_the_floor() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    created(&s, "sp-b");
    let (_, long) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100),
            &req("claim-a"),
        )
        .unwrap();
    let (_, short) = s
        .claim(
            "sp-b",
            &ev(BeadState::Ready, 0, claim("dwarf-2", 90), "dwarf-2", 100),
            &req("claim-b"),
        )
        .unwrap();
    let (long, short) = (long.unwrap(), short.unwrap());
    clock.advance(FLOOR_MS - 1);
    assert!(
        s.renew(&short, &req("renew-b1")).is_ok(),
        "a lease in the past took a lock under the floor"
    );
    clock.advance(FLOOR_MS + 1);
    assert!(matches!(
        s.renew(&short, &req("renew-b2")),
        Err(Error::LockLost { .. })
    ));
    clock.advance_to(59_999);
    assert!(s.renew(&long, &req("renew-a1")).is_ok());
    clock.advance_to(59_999 + 60_001);
    assert!(
        matches!(
            s.renew(&long, &req("renew-a2")),
            Err(Error::LockLost { .. })
        ),
        "a 60 s lease outlived 60 s"
    );
}

#[test_id(t3bsxd5)]
#[test]
fn renew_extends_the_lock_and_writes_nothing_to_the_record() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let (_, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let f = f.unwrap();
    let before = s.record("sp-a").unwrap().unwrap().mod_rev;
    for n in 0..5 {
        clock.advance(1_500);
        s.renew(&f, &req(&format!("renew-{n}"))).unwrap();
    }
    assert_eq!(s.record("sp-a").unwrap().unwrap().mod_rev, before);
    let out = s
        .holder_apply(
            "sp-a",
            &f,
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::Submit { tip: TIP.into() },
                "dwarf-1",
                110,
            ),
            &req("submit-1"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
}

#[test_id(t2mju2v)]
#[test]
fn restarting_the_store_leaves_every_live_lock_valid() {
    let (s, b, clock) = fresh();
    created(&s, "sp-a");
    let (_, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let f = f.unwrap();
    clock.advance(1_500);
    drop(s);
    let s = open(&b, &clock);
    clock.advance(1_500);
    let out = s
        .holder_apply(
            "sp-a",
            &f,
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::Submit { tip: TIP.into() },
                "dwarf-1",
                110,
            ),
            &req("submit-1"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
}

#[test_id(tgaou3n)]
#[test]
fn a_retried_claim_is_answered_from_its_request_and_writes_nothing() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let e = ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100);
    let (first, f1) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    let head = s.contract().head().unwrap();
    let (again, f2) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    assert_eq!(again, first);
    assert_eq!(f2, f1);
    assert_eq!(s.contract().head().unwrap(), head, "a retry wrote again");
}

#[test_id(t3vfdlw)]
#[test]
fn a_retried_claim_hands_back_no_fence_once_its_lock_has_lapsed() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let e = ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100);
    let (first, f1) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    assert!(matches!(first, Outcome::Applied { .. }), "{first:?}");
    assert!(f1.is_some());
    clock.advance(5_000);
    let (again, f2) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    assert!(matches!(again, Outcome::Applied { .. }), "{again:?}");
    assert_eq!(f2, None, "a retry handed back a lapsed fence");
}

#[test_id(t2kfmtc)]
#[test]
fn a_claim_request_that_lost_stays_lost_on_retry_and_a_new_request_claims() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let stray = s
        .contract()
        .lock(&keys::claim_resource("sp-a"), 60_000, &req("stray"))
        .unwrap();
    let e = ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100);
    let (out, _) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    assert!(matches!(out, Outcome::Locked { .. }), "{out:?}");
    s.contract().release(&stray, &req("stray-off")).unwrap();
    let (again, f) = s.claim("sp-a", &e, &req("claim-1")).unwrap();
    assert!(matches!(again, Outcome::LostRace { .. }), "{again:?}");
    assert!(f.is_none());
    assert_eq!(
        s.record("sp-a").unwrap().unwrap().value.row.state,
        BeadState::Ready,
        "a spent request claimed on retry"
    );
    let (fresh_claim, f) = s.claim("sp-a", &e, &req("claim-2")).unwrap();
    assert!(
        matches!(fresh_claim, Outcome::Applied { .. }),
        "{fresh_claim:?}"
    );
    assert!(f.is_some());
}

#[test_id(t7kniba)]
#[test]
fn a_record_read_at_an_old_rev_does_not_borrow_a_later_claims_lease() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let (first, _) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    let Outcome::Applied { rev: then, .. } = first else {
        panic!("{first:?}")
    };
    s.apply(
        "sp-a",
        &ev(
            BeadState::Working,
            1,
            BeadEventKind::HolderDead,
            "reaper",
            103,
        ),
        &req("dead-1"),
    )
    .unwrap();
    clock.advance(50_000);
    let v = s.record("sp-a").unwrap().unwrap().value.row.version;
    s.claim(
        "sp-a",
        &ev(BeadState::Ready, v, claim("dwarf-2", 900), "dwarf-2", 150),
        &req("claim-2"),
    )
    .unwrap();
    assert_eq!(
        s.record_at("sp-a", then)
            .unwrap()
            .unwrap()
            .value
            .row
            .lease_until,
        Some(102)
    );
}

#[test_id(tyxzjl2)]
#[test]
fn a_retried_event_is_answered_from_its_request_and_writes_nothing() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let e = ev(BeadState::Ready, 0, BeadEventKind::Express, "ops", 200);
    let first = s.apply("sp-a", &e, &req("x-1")).unwrap();
    s.apply(
        "sp-a",
        &ev(BeadState::Ready, 1, BeadEventKind::Unexpress, "ops", 201),
        &req("x-2"),
    )
    .unwrap();
    let head = s.contract().head().unwrap();
    assert_eq!(s.apply("sp-a", &e, &req("x-1")).unwrap(), first);
    assert_eq!(s.contract().head().unwrap(), head, "a retry wrote again");
}

#[test_id(tyqzzfs)]
#[test]
fn a_claim_without_its_at_or_outside_claim_is_refused_before_any_write() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let head = s.contract().head().unwrap();
    let mut e = ev(BeadState::Ready, 0, claim("dwarf-1", 160), "dwarf-1", 100);
    assert!(s.apply("sp-a", &e, &req("a-1")).is_err());
    e.at = None;
    assert!(s.claim("sp-a", &e, &req("c-1")).is_err());
    assert_eq!(s.contract().head().unwrap(), head);
}

#[test_id(t2h35ub)]
#[test]
fn a_reaper_moving_a_record_out_of_working_releases_the_claim_lock_at_once() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    s.claim(
        "sp-a",
        &ev(BeadState::Ready, 0, claim("dwarf-1", 1_000), "dwarf-1", 100),
        &req("claim-1"),
    )
    .unwrap();
    let dead = s
        .apply(
            "sp-a",
            &ev(
                BeadState::Working,
                1,
                BeadEventKind::HolderDead,
                "reaper",
                101,
            ),
            &req("dead-1"),
        )
        .unwrap();
    assert!(matches!(dead, Outcome::Applied { .. }), "{dead:?}");
    let v = s.record("sp-a").unwrap().unwrap().value.row.version;
    let (out, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, v, claim("dwarf-2", 1_000), "dwarf-2", 102),
            &req("claim-2"),
        )
        .unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    assert!(f.is_some());
}

#[test_id(tg8speu)]
#[test]
fn a_record_marked_for_deletion_is_not_recreated_as_equal() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    s.delete("sp-a", true, &req("d-1")).unwrap();
    assert!(matches!(
        s.create("sp-a", Some("harness"), None, 100, &req("c-2"))
            .unwrap_err(),
        Error::Conflict { .. }
    ));
}

#[test_id(tosvml1)]
#[test]
fn deleting_a_record_that_is_already_gone_succeeds() {
    let (s, _, _) = fresh();
    assert!(matches!(
        s.delete("sp-none", false, &req("d-1")).unwrap(),
        Deleted::Removed { .. }
    ));
}

#[test_id(tozmrts)]
#[test]
fn readiness_and_progress_are_owned_and_go_with_their_record_and_batch() {
    let (s, _, clock) = fresh();
    created(&s, "sp-a");
    let batch = keys::batch("harness", "b-1");
    s.contract()
        .create(
            &batch,
            state_store::Object::new(serde_json::json!({"state": "OPEN"})),
            &req("b"),
        )
        .unwrap();
    s.set_progress(
        "harness",
        "b-1",
        serde_json::json!({"done": 1}),
        &req("p-1"),
    )
    .unwrap();
    seed_judged(&s, "sp-a", "pass");
    s.delete("sp-a", false, &req("d-1")).unwrap();
    s.contract().delete(&batch, None, &req("bd")).unwrap();
    Controller::new(s.contract()).tick(clock.now_ms()).unwrap();
    assert!(s.readiness("sp-a").unwrap().is_none());
    assert!(s
        .contract()
        .get(&keys::batch_progress("harness", "b-1"))
        .unwrap()
        .is_none());
}

#[test_id(tjb15z3)]
#[test]
fn a_progress_that_is_not_one_object_is_refused() {
    let (s, _, _) = fresh();
    let batch = keys::batch("harness", "b-1");
    s.contract()
        .create(
            &batch,
            state_store::Object::new(serde_json::json!({})),
            &req("b"),
        )
        .unwrap();
    assert!(s
        .set_progress("harness", "b-1", serde_json::json!([1, 2]), &req("p-1"))
        .is_err());
}

#[test_id(tkm4myw)]
#[test]
fn batch_progress_goes_under_its_own_key_and_never_moves_the_batch() {
    let (s, _, _) = fresh();
    let batch = keys::batch("harness", "b-1");
    s.contract()
        .create(
            &batch,
            state_store::Object::new(serde_json::json!({"state": "OPEN"})),
            &req("b"),
        )
        .unwrap();
    let before = s.contract().get(&batch).unwrap().unwrap().mod_rev;
    s.set_progress(
        "harness",
        "b-1",
        serde_json::json!({"done": 3, "of": 9}),
        &req("p-1"),
    )
    .unwrap();
    assert_eq!(s.contract().get(&batch).unwrap().unwrap().mod_rev, before);
    let p = s
        .contract()
        .get(&keys::batch_progress("harness", "b-1"))
        .unwrap()
        .unwrap();
    assert_eq!(p.value.data["done"], 3);
    assert!(matches!(
        s.set_progress("harness", "b-9", serde_json::json!({}), &req("p-2"))
            .unwrap_err(),
        Error::NotFound { .. }
    ));
}

#[test_id(ttuezpx)]
#[test]
fn records_are_listed_from_one_snapshot_with_its_rev_and_without_other_kinds() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    created(&s, "sp-b");
    seed_judged(&s, "sp-a", "p");
    let rev = s.contract().head().unwrap();
    let records = s.records_page(rev, None, None, None, None, |_| true).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|r| r.value.row.bead_id.as_str())
            .collect::<Vec<_>>(),
        ["sp-a", "sp-b"]
    );
    assert_eq!(rev, s.contract().head().unwrap());
    let (all, _) = s
        .contract()
        .list(&[keys::READINESS], &Selector::Everything)
        .unwrap();
    assert_eq!(all.len(), 1);
    let first = s
        .record_at("sp-b", records[1].mod_rev.main)
        .unwrap()
        .unwrap();
    assert_eq!(first.value, records[1].value);
}

#[test_id(tf0lfyh)]
#[test]
fn a_transition_the_store_commits_is_shipped_with_its_machine_states_bead_and_actor() {
    use spira_otel::vlogs::fake::FakeLogs;
    use std::time::Duration;
    let logs = FakeLogs::start();
    let (b, clock) = (MemBackend::new(), VirtualClock::new(0));
    let s = open(&b, &clock).with_shipper(spira_otel::vlogs::Shipper::start(&logs.url()));
    created(&s, "sp-a");
    s.claim("sp-a", &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100), &req("claim-1")).unwrap();
    let got: Vec<serde_json::Value> = logs.wait_records(1, Duration::from_secs(5)).into_iter().filter(|r| r["event"] == "Claim").collect();
    assert_eq!(got.len(), 1, "{got:?}");
    let r = &got[0];
    for (k, v) in [("kind_tag", "transition"), ("stream", "lifecycle.bead"), ("machine", "bead"), ("from", "READY"), ("to", "WORKING"), ("bead_id", "sp-a"), ("actor", "dwarf-1")] {
        assert_eq!(r[k], v, "{k}: {r}");
    }
    assert_eq!(r["at"], 100);
}

#[test_id(tl2tcxm)]
#[test]
fn a_refused_transition_and_a_retried_request_ship_nothing() {
    use spira_otel::vlogs::fake::FakeLogs;
    use std::time::Duration;
    let logs = FakeLogs::start();
    let (b, clock) = (MemBackend::new(), VirtualClock::new(0));
    let s = open(&b, &clock).with_shipper(spira_otel::vlogs::Shipper::start(&logs.url()));
    created(&s, "sp-a");
    let claimed = |id: &str| s.claim("sp-a", &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100), &req(id)).unwrap();
    claimed("claim-1");
    claimed("claim-1");
    let _ = s.apply("sp-a", &ev(BeadState::Ready, 0, BeadEventKind::HolderDead, "reaper", 101), &req("refused-1"));
    let renewed = s.apply("sp-a", &ev(BeadState::Working, 1, BeadEventKind::Renew { lease_until: 200 }, "dwarf-1", 103), &req("renew-1")).unwrap();
    assert!(matches!(renewed, Outcome::Applied { .. }), "the renew is a legal event that leaves the state where it was");
    created(&s, "sp-b");
    s.claim("sp-b", &ev(BeadState::Ready, 0, claim("dwarf-2", 102), "dwarf-2", 102), &req("claim-b")).unwrap();
    let both = logs.wait_records(2, Duration::from_secs(5));
    assert!(both.iter().any(|r| r["bead_id"] == "sp-b"), "the shipper is first in, first out: once sp-b's claim is here, everything before it is: {both:?}");
    let claims = logs.records().into_iter().filter(|r| r["event"] == "Claim" && r["bead_id"] == "sp-a").count();
    assert_eq!(claims, 1, "the retried request shipped once");
    assert!(logs.records().iter().all(|r| r["event"] != "HolderDead"), "a refused event is no transition");
    assert!(logs.records().iter().all(|r| r["event"] != "Renew"), "an event that leaves the state where it was is no move, as the gantt reads it");
}

#[derive(Clone, Default)]
struct Tally {
    commits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    request_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Tally {
    fn take(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (
            self.commits.swap(0, SeqCst),
            self.request_reads.swap(0, SeqCst),
        )
    }
}

#[derive(Clone)]
struct Counted {
    inner: MemBackend,
    tally: Tally,
}

impl Counted {
    fn read(&self, key: &str) {
        if key.starts_with(state_store::REQUESTS_PREFIX) {
            self.tally
                .request_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl Backend for Counted {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.due(now, limit, at)
    }
    fn owned_by(
        &self,
        owner: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> state_store::Result<Vec<String>> {
        self.inner.owned_by(owner, after, limit, at)
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
        self.read(key);
        self.inner.get(key, at)
    }
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.read(prefix);
        self.inner.range(prefix, after, limit, at)
    }
    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.inner.range_in_state(prefix, state, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[state_store::Write]) -> state_store::Result<u64> {
        self.tally
            .commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<state_store::LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> state_store::Result<()> {
        self.inner.compact(rev)
    }
    fn compacted(&self) -> state_store::Result<u64> {
        self.inner.compacted()
    }
}

fn counted_claim() -> (Store<Counted>, Tally, VirtualClock, state_store::Fence) {
    let (inner, clock, tally) = (MemBackend::new(), VirtualClock::new(0), Tally::default());
    let c = Contract::open(
        Counted {
            inner,
            tally: tally.clone(),
        },
        Ports::virtual_clock(&clock),
        Config {
            min_lock_ttl_ms: FLOOR_MS,
        },
    )
    .unwrap();
    let s = Store::new(c, FLOOR_MS);
    s.create("sp-a", Some("harness"), None, 100, &req("create-a"))
        .unwrap();
    let (_, f) = s
        .claim(
            "sp-a",
            &ev(BeadState::Ready, 0, claim("dwarf-1", 102), "dwarf-1", 100),
            &req("claim-1"),
        )
        .unwrap();
    (s, tally, clock, f.unwrap())
}

fn renewal(lease_until: i64, at: i64) -> BeadEvent {
    ev(
        BeadState::Working,
        1,
        BeadEventKind::Renew { lease_until },
        "dwarf-1",
        at,
    )
}

fn lock_expiry<B: Backend>(s: &Store<B>) -> Option<u64> {
    s.contract()
        .get(&keys::claim_lock("sp-a"))
        .unwrap()
        .and_then(|l| l.lock_expires_at_ms)
}

#[test_id(tzaf8vg)]
#[test]
fn a_renewal_is_one_commit_that_restarts_the_lock_and_writes_the_record() {
    let (s, tally, clock, f) = counted_claim();
    clock.advance(500);
    let before = lock_expiry(&s).unwrap();
    tally.take();
    let out = s
        .renew_lease("sp-a", &f, &renewal(160, 101), &req("renew-1"))
        .unwrap();
    let Outcome::Applied { rev, record } = out else {
        panic!("{out:?}")
    };
    let (commits, request_reads) = tally.take();
    assert_eq!(commits, 1, "a renewal is one store commit");
    assert_eq!(request_reads, 2, "reads of requests/ per renewal");
    assert_eq!(
        s.holder_fence("sp-a").unwrap(),
        Some(f),
        "the token is kept"
    );
    let expiry = lock_expiry(&s).unwrap();
    assert!(expiry > before, "the lock moved to the new lease");
    assert_eq!(record.row.lease_until, Some(expiry.div_ceil(1_000) as i64));
    let (refused, audits) = audits_at(&s, rev);
    assert!(!refused);
    assert_eq!(audits[0].event, "Renew");
    let history = s
        .contract()
        .list(&[state_store::HISTORY_PREFIX], &Selector::Everything)
        .unwrap()
        .0;
    assert!(
        !history.iter().any(|h| h.key.ends_with("/renew-1/apply")), // not a path: a store-key suffix
        "an applied renewal writes no history entry: {history:?}"
    );
}

#[test_id(te0wzyw)]
#[test]
fn a_beads_history_grows_with_its_moves_not_its_renewals() {
    let (s, _, clock, f) = counted_claim();
    for i in 1..=3u64 {
        clock.advance(500);
        let mut renew = renewal(150 + 10 * i as i64, 100 + i as i64);
        renew.version = i;
        let out = s
            .renew_lease("sp-a", &f, &renew, &req(&format!("renew-{i}")))
            .unwrap();
        assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    }
    let phase = ev(
        BeadState::Working,
        4,
        BeadEventKind::Phase { phase: lifecycle::bead::DwarfPhase::Building },
        "dwarf-1",
        104,
    );
    let out = s.apply("sp-a", &phase, &req("phase-1")).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    let stranger = ev(
        BeadState::Ready,
        5,
        BeadEventKind::Renew { lease_until: 160 },
        "dwarf-2",
        105,
    );
    let out = s.renew_lease("sp-a", &f, &stranger, &req("renew-4")).unwrap();
    assert!(matches!(out, Outcome::Refused { .. }), "{out:?}");

    let h = s.history("sp-a", Some("bead")).unwrap();
    let seen: Vec<(&str, bool)> = h
        .iter()
        .map(|c| (c.audit.event.as_str(), c.audit.applied))
        .collect();
    assert!(
        !seen.contains(&("Renew", true)),
        "an applied renewal is in the history: {seen:?}"
    );
    assert!(seen.contains(&("Claim", true)), "{seen:?}");
    assert!(seen.contains(&("Phase", true)), "{seen:?}");
    assert!(seen.contains(&("Renew", false)), "the refusal is kept: {seen:?}");
}

#[test_id(tkf8x6c)]
#[test]
fn a_renewal_after_the_lock_lapsed_is_lock_lost_and_leaves_the_record() {
    let (s, _, clock, f) = counted_claim();
    let before = s.record("sp-a").unwrap().unwrap().mod_rev;
    clock.advance(10_000);
    let out = s
        .renew_lease("sp-a", &f, &renewal(160, 110), &req("renew-1"))
        .unwrap();
    assert!(matches!(out, Outcome::LockLost { .. }), "{out:?}");
    assert_eq!(s.record("sp-a").unwrap().unwrap().mod_rev, before);
}

#[test_id(tlvu896)]
#[test]
fn a_non_holders_renewal_is_refused_and_the_lock_expiry_is_unchanged() {
    let (s, _, _, f) = counted_claim();
    let before = lock_expiry(&s);
    let stranger = state_store::Fence {
        resource: f.resource.clone(),
        token: state_store::Rev::new(f.token.main + 1, 0),
    };
    let out = s
        .renew_lease("sp-a", &stranger, &renewal(160, 101), &req("renew-1"))
        .unwrap();
    assert!(matches!(out, Outcome::LockLost { .. }), "{out:?}");
    let refused = s
        .renew_lease(
            "sp-a",
            &f,
            &ev(
                BeadState::Ready,
                1,
                BeadEventKind::Renew { lease_until: 160 },
                "dwarf-1",
                101,
            ),
            &req("renew-2"),
        )
        .unwrap();
    assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");
    assert_eq!(lock_expiry(&s), before);
}

fn refs_of(pairs: &[(&str, &str)]) -> super::ExternalRefs {
    let held: std::collections::BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    std::sync::Arc::new(move |id: &str| Ok(held.get(id).cloned()))
}

fn dropped(s: &Store<MemBackend>, id: &str, request: &str) -> Outcome {
    let cur = s.record(id).unwrap().unwrap();
    let kind = BeadEventKind::Drop { reason: lifecycle::reason::DropReason::Unwanted };
    s.apply(id, &ev(cur.value.row.state, cur.value.row.version, kind, "operator", 120), &req(request)).unwrap()
}

#[test_id(tanzft1)]
#[test]
fn a_github_bead_entering_a_terminal_state_enqueues_exactly_one_closeout() {
    let (s, _, _) = fresh();
    let s = s.with_refs(refs_of(&[("sp-gh", "github:o/r#7")]));
    created(&s, "sp-gh");
    assert!(s.gh_closeouts().unwrap().is_empty(), "nothing is owed before the bead is terminal");
    assert!(matches!(dropped(&s, "sp-gh", "drop-1"), Outcome::Applied { .. }));
    let rows = s.gh_closeouts().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "sp-gh");
    assert_eq!(rows[0].1["external_ref"], "github:o/r#7");
    assert_eq!(rows[0].1["outcome_state"], "DROPPED");
    assert_eq!(rows[0].1["attempts"], 0);
    assert!(matches!(dropped(&s, "sp-gh", "drop-2"), Outcome::Refused { .. }), "a repeated terminal event is refused");
    assert_eq!(s.gh_closeouts().unwrap().len(), 1, "a repeated event enqueues none");
}

#[test_id(tobsi19)]
#[test]
fn a_bead_with_no_or_a_non_github_reference_enqueues_nothing() {
    let (s, _, _) = fresh();
    let s = s.with_refs(refs_of(&[("sp-jira", "jira:X-1"), ("sp-untrusted", "github-untrusted:o/r#3")]));
    for id in ["sp-none", "sp-jira", "sp-untrusted"] {
        created(&s, id);
        assert!(matches!(dropped(&s, id, &format!("drop-{id}")), Outcome::Applied { .. }));
    }
    assert!(s.gh_closeouts().unwrap().is_empty());
}

#[test_id(tb7d82t)]
#[test]
fn a_reference_the_content_store_cannot_read_fails_the_transition_closed() {
    let (s, _, _) = fresh();
    let s = s.with_refs(std::sync::Arc::new(|_: &str| Err("content store down".to_string())));
    created(&s, "sp-gh");
    let cur = s.record("sp-gh").unwrap().unwrap();
    let kind = BeadEventKind::Drop { reason: lifecycle::reason::DropReason::Unwanted };
    let err = s.apply("sp-gh", &ev(cur.value.row.state, cur.value.row.version, kind, "operator", 120), &req("drop-1")).unwrap_err();
    assert!(err.to_string().contains("content store down"), "{err}");
    assert_eq!(s.record("sp-gh").unwrap().unwrap().value.row.state, BeadState::Ready, "the bead did not move");
}

#[test_id(tde9xud)]
#[test]
fn a_closeout_is_asked_failed_and_done_idempotently() {
    let (s, _, _) = fresh();
    let s = s.with_refs(refs_of(&[("sp-gh", "github:o/r#7")]));
    created(&s, "sp-gh");
    dropped(&s, "sp-gh", "drop-1");
    assert!(s.gh_closeout_failed("sp-gh", &req("f1")).unwrap());
    assert!(s.gh_closeout_failed("sp-gh", &req("f2")).unwrap());
    assert!(s.gh_closeout_asked("sp-gh", "sp-ask1", &req("a1")).unwrap());
    let row = s.gh_closeout("sp-gh").unwrap().unwrap();
    assert_eq!((row["attempts"].as_u64(), row["ask_id"].as_str()), (Some(2), Some("sp-ask1")));
    s.gh_closeout_done("sp-gh", &req("d1")).unwrap();
    s.gh_closeout_done("sp-gh", &req("d2")).unwrap();
    assert!(s.gh_closeouts().unwrap().is_empty());
    assert!(!s.gh_closeout_failed("sp-gh", &req("f3")).unwrap(), "no row is no failure to count");
}

#[test_id(tfrkin8)]
#[test]
fn the_pending_verb_names_every_column_the_drain_reads() {
    let (s, _, _) = fresh();
    let s = s.with_refs(refs_of(&[("sp-gh", "github:o/r#7")]));
    created(&s, "sp-gh");
    dropped(&s, "sp-gh", "drop-1");
    let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let (code, out) = crate::control::gh_closeout::run(&s, &args(&["gh-closeout", "pending"]), "req-1");
    assert_eq!(code, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = &rows[0];
    for k in ["bead_id", "external_ref", "outcome_state", "sha", "ask_id", "enqueued_at", "attempts"] {
        assert!(row.get(k).is_some(), "{k} missing from {row}");
    }
    let (code, _) = crate::control::gh_closeout::run(&s, &args(&["gh-closeout", "done", "sp-gh"]), "req-2");
    assert_eq!(code, 0);
    assert_eq!(crate::control::gh_closeout::run(&s, &args(&["gh-closeout", "pending"]), "req-3").1, "[]");
}

mod on_sqlite {
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, Ports};
    use state_store_sqlite::{Options, SqliteBackend};

    use super::super::Store;
    use crate::control::state::{self, Call};

    const AT: i64 = 1_767_225_600;

    fn sqlite() -> (testkit::TempDir, Store<SqliteBackend>) {
        let dir = testkit::TempDir::new("lc-sqlite-store");
        let backend = SqliteBackend::open(&dir.join("control.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let clock = VirtualClock::new(AT as u64 * 1_000);
        let contract = Contract::open(backend, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: super::FLOOR_MS }).unwrap();
        (dir, Store::new(contract, super::FLOOR_MS))
    }

    fn ok(s: &Store<SqliteBackend>, argv: &[&str]) -> String {
        let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let request = crate::control::served::request_id();
        let (code, out) = state::run(s, &args, &Call { now: AT, request: &request, inputs: &crate::control::claimable::FIXED });
        assert_eq!(code, 0, "{argv:?}: {out}");
        out
    }

    fn claim(s: &Store<SqliteBackend>, id: &str) {
        ok(s, &["event", "bead", id, "--expect", "READY", "--version", "0", "--actor", "t", "--kind", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#]);
    }

    #[test_id(t5frjsb)]
    #[test]
    fn a_show_and_a_list_in_the_same_thread_reflect_the_create_just_made_with_no_wait() {
        let (_dir, s) = sqlite();
        ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
        let shown: serde_json::Value = serde_json::from_str(&ok(&s, &["show", "sp-a"])).unwrap();
        assert_eq!(shown["bead"]["state"], "READY", "{shown}");
        let listed: Vec<serde_json::Value> = serde_json::from_str(&ok(&s, &["list", "--ids", "sp-a"])).unwrap();
        assert_eq!(listed.iter().map(|r| r["bead_id"].as_str().unwrap()).collect::<Vec<_>>(), ["sp-a"]);
        claim(&s, "sp-a");
        let shown: serde_json::Value = serde_json::from_str(&ok(&s, &["show", "sp-a"])).unwrap();
        assert_eq!(shown["bead"]["state"], "WORKING", "{shown}");
    }

    #[test_id(t5nqbx4)]
    #[test]
    fn a_bare_list_returns_the_lifecycle_rows_without_consulting_the_claim_view_inputs() {
        let (_dir, s) = sqlite();
        ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
        let args: Vec<String> = ["list", "--live", "--bare"].map(String::from).to_vec();
        let request = crate::control::served::request_id();
        let (code, out) = state::run(&s, &args, &Call { now: AT, request: &request, inputs: &crate::control::claimable::UNREADABLE });
        assert_eq!(code, 0, "{out}");
        let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(rows.len(), 1, "{out}");
        assert_eq!((rows[0]["bead_id"].as_str(), rows[0]["state"].as_str()), (Some("sp-a"), Some("READY")), "{out}");
        assert!(rows[0]["claim_view"].is_null(), "{out}");
    }

    #[test_id(tjk8zkr)]
    #[test]
    fn a_list_without_bare_still_carries_the_claim_view() {
        let (_dir, s) = sqlite();
        ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&ok(&s, &["list", "--live"])).unwrap();
        assert!(!rows[0]["claim_view"].is_null(), "{rows:?}");
    }

    #[test_id(tot6s3x)]
    #[test]
    fn a_list_row_and_a_show_carry_the_beads_builder_time_and_session_count() {
        let (_dir, s) = sqlite();
        ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
        claim(&s, "sp-a");
        let listed: Vec<serde_json::Value> = serde_json::from_str(&ok(&s, &["list", "--live"])).unwrap();
        assert_eq!((listed[0]["worked_s"].as_str(), listed[0]["sessions"].as_str()), (Some("0"), Some("1")));
        let shown: serde_json::Value = serde_json::from_str(&ok(&s, &["show", "sp-a"])).unwrap();
        assert_eq!(shown["bead"]["sessions"].as_str(), Some("1"), "{shown}");
        assert_eq!((listed[0]["builder_claims"].as_str(), shown["bead"]["builder_claims"].as_str()), (Some("0"), Some("0")));
        assert_eq!(shown["bead"]["session_ends"], serde_json::json!([]), "{shown}");
    }
}

#[test_id(tzq1kkg)]
#[test]
fn a_txn_whose_audits_on_one_key_do_not_chain_is_refused_and_writes_nothing() {
    let (s, _b, _c) = fresh();
    let audit = |from: &str, to: &str| Audit {
        machine: "delivery".into(),
        lc_key: "sp-x".into(),
        event: "Cut".into(),
        expect: from.into(),
        from_state: from.into(),
        to_state: to.into(),
        applied: true,
        refusal: None,
        evidence: serde_json::Value::Null,
        actor: "test".into(),
        at: 0,
    };
    let head = s.contract().head().unwrap();
    let err = s
        .audited_txn(
            state_store::Txn::new(),
            &[audit("A", "B"), audit("C", "D")],
            &[],
            &req("r-1"),
            "t",
        )
        .unwrap_err();
    let text = err.to_string();
    for want in ["delivery", "sp-x", "C", "B"] {
        assert!(text.contains(want), "{want} missing from {text}");
    }
    assert_eq!(s.contract().head().unwrap(), head);
}
