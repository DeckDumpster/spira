use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState, Stack};
use runtime::ports::{Clock, VirtualClock};
use state_store::gc::Controller;
use state_store::{Config, Contract, Deleted, Error, MemBackend, Ports, Request, Selector};

use super::{Outcome, Store, Verdict};
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

fn audits_at(s: &Store<MemBackend>, rev: u64) -> (bool, Vec<Audit>) {
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

#[test]
fn a_verdict_pass_between_a_read_and_a_claim_does_not_conflict_the_claim() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let before = s.record("sp-a").unwrap().unwrap();
    let pass = [Verdict {
        id: "sp-a".into(),
        verdict: "claimable".into(),
        blocker: None,
        runes: vec!["builder".into()],
    }];
    s.publish_verdicts(&pass, "2026-10-31T12:00:00Z", &req("pass-1"))
        .unwrap();
    assert_eq!(
        s.record("sp-a").unwrap().unwrap().mod_rev,
        before.mod_rev,
        "a verdict pass moved the lifecycle record"
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
    assert_eq!(
        (
            r.verdict.as_deref(),
            r.runes.as_slice(),
            r.judged_at.as_deref()
        ),
        (
            Some("claimable"),
            &["builder".to_string()][..],
            Some("2026-10-31T12:00:00Z")
        )
    );
}

#[test]
fn a_pass_keeps_the_blockers_another_publisher_wrote_and_skips_ids_with_no_record() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    let seeded = super::Readiness {
        blocked_by: vec!["sp-b".into()],
        ..Default::default()
    };
    let t = state_store::Txn::new().put(
        &keys::readiness("sp-a"),
        state_store::Object::new(super::encode(&seeded).unwrap()),
    );
    s.contract().txn(&t, &req("seed")).unwrap();
    let pass = [
        Verdict {
            id: "sp-a".into(),
            verdict: "blocked".into(),
            blocker: Some("sp-b".into()),
            runes: vec![],
        },
        Verdict {
            id: "sp-ghost".into(),
            verdict: "claimable".into(),
            blocker: None,
            runes: vec![],
        },
    ];
    s.publish_verdicts(&pass, "2026-10-31T12:00:00Z", &req("pass-1"))
        .unwrap();
    let r = s.readiness("sp-a").unwrap().unwrap().value;
    assert_eq!(
        (r.blocked_by, r.verdict.as_deref(), r.blocker.as_deref()),
        (vec!["sp-b".to_string()], Some("blocked"), Some("sp-b"))
    );
    assert!(s.readiness("sp-ghost").unwrap().is_none());
}

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
    gc.tick(clock.now_ms()).unwrap();
    assert!(s.record("sp-a").unwrap().is_none());

    created(&s, "sp-b");
    assert!(matches!(
        s.delete("sp-b", false, &req("d-2")).unwrap(),
        Deleted::Removed { .. }
    ));
}

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
    let (listed, _) = s.records().unwrap();
    assert_eq!(listed[0].value.row.lease_until, lease(&s));
    clock.advance(10_000);
    assert_eq!(
        lease(&s),
        Some(102),
        "with its lock lapsed, a record reads its claim's own lease"
    );
}

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

#[test]
fn deleting_a_record_that_is_already_gone_succeeds() {
    let (s, _, _) = fresh();
    assert!(matches!(
        s.delete("sp-none", false, &req("d-1")).unwrap(),
        Deleted::Removed { .. }
    ));
}

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
    s.publish_verdicts(
        &[Verdict {
            id: "sp-a".into(),
            verdict: "claimable".into(),
            blocker: None,
            runes: vec![],
        }],
        "2026-10-31T12:00:00Z",
        &req("pass"),
    )
    .unwrap();
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

#[test]
fn records_are_listed_from_one_snapshot_with_its_rev_and_without_other_kinds() {
    let (s, _, _) = fresh();
    created(&s, "sp-a");
    created(&s, "sp-b");
    s.publish_verdicts(
        &[Verdict {
            id: "sp-a".into(),
            verdict: "claimable".into(),
            blocker: None,
            runes: vec![],
        }],
        "2026-10-31T12:00:00Z",
        &req("p"),
    )
    .unwrap();
    let (records, rev) = s.records().unwrap();
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
    for (k, v) in [("kind_tag", "transition"), ("stream", "lifecycle"), ("machine", "bead"), ("from", "READY"), ("to", "WORKING"), ("bead_id", "sp-a"), ("actor", "dwarf-1")] {
        assert_eq!(r[k], v, "{k}: {r}");
    }
    assert_eq!(r["at"], 100);
}

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
    created(&s, "sp-b");
    s.claim("sp-b", &ev(BeadState::Ready, 0, claim("dwarf-2", 102), "dwarf-2", 102), &req("claim-b")).unwrap();
    let both = logs.wait_records(2, Duration::from_secs(5));
    assert!(both.iter().any(|r| r["bead_id"] == "sp-b"), "the shipper is first in, first out: once sp-b's claim is here, everything before it is: {both:?}");
    let claims = logs.records().into_iter().filter(|r| r["event"] == "Claim" && r["bead_id"] == "sp-a").count();
    assert_eq!(claims, 1, "the retried request shipped once");
    assert!(logs.records().iter().all(|r| r["event"] != "HolderDead"), "a refused event is no transition");
}
