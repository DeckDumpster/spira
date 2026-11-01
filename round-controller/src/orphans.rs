//! Orphans and their re-drive. An orphan is a non-terminal round with a `last_lease_token` and
//! no live lease; a round never leased is waiting for a slot, not an orphan. A re-drive proves
//! the old runner dead first, then takes a runner slot and a new lease and launches under both;
//! every attempt is recorded in `batches/<repo>/<id>/redrive`, and a failed launch gives both
//! back and is retried after a doubling back-off, never skipped, until
//! `SPIRA_ROUND_REDRIVE_RETRIES` failures exhaust it.

use serde::{Deserialize, Serialize};
use serde_json::json;
use state_store::{Compare, Object, Request, Rev, Selector, Txn};

use lifecycle::batch::BatchState;

use crate::death::{prove_dead, unit_name, Holder, Proof};
use crate::lease::{self, acquire, holder_of, Held, LeaseError, Resource};
use crate::outcome::Outcome;
use crate::ports::{Launch, Launched, Ports};
use crate::slots::{self, SlotGrant};

/// A batch state as either writer spells it: the lifecycle's word (`CI_RUNNING`) or the round
/// record's own serialisation (`CiRunning`).
fn batch_state(state: &str) -> Option<BatchState> {
    BatchState::from_str(state).or_else(|| serde_json::from_value(json!(state)).ok())
}

/// The states no reader re-drives. SUPERSEDED is the controller's own terminal state.
pub fn terminal(state: &str) -> Option<bool> {
    if state == "SUPERSEDED" {
        return Some(true);
    }
    batch_state(state).map(|s| s.is_terminal())
}

/// A round a runner drives: non-terminal, and not yet certified. A GREEN round is the Lander's,
/// and LANDING and DEPLOYING are inside the land verb, so re-driving any of them would run a
/// pass nothing asked for.
fn runner_drives(state: &str) -> Option<bool> {
    let driven = terminal(state).map(|t| !t)?;
    Some(
        driven
            && !matches!(
                batch_state(state),
                Some(BatchState::Green | BatchState::Landing | BatchState::Deploying)
            ),
    )
}

/// The name the controller's own leases are held under.
pub const HOLDER: &str = "round-controller";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub repo: String,
    pub batch: String,
    pub expired: Rev,
}

pub fn orphans(p: &Ports, repo: &str) -> Result<Vec<Orphan>, String> {
    let prefix = format!("batches/{repo}/");
    let (items, _) = p
        .store
        .list(&[&prefix], &Selector::Everything)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in items {
        let Some(batch) = i.key.strip_prefix(&prefix).filter(|b| !b.contains('/')) else {
            continue;
        };
        let d = &i.value.data;
        // A state no reader can name is not re-driven: that might re-drive a terminal round.
        if d.get("state").and_then(|s| s.as_str()).and_then(runner_drives) != Some(true) {
            continue;
        }
        let Some(expired) = d.get(lease::LAST_TOKEN_FIELD).and_then(lease::token_of) else {
            continue;
        };
        if holder_of(p, &Resource::round(repo, batch))?.is_some() {
            continue;
        }
        out.push(Orphan {
            repo: repo.into(),
            batch: batch.into(),
            expired,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub expired_token: Rev,
    /// The lease this attempt took and launched under.
    pub lease_token: Rev,
    /// `ok`, `running` when the unit was already up under this lease's name, or the launch's
    /// refusal.
    pub launch_result: String,
    pub at_ms: u64,
    /// When a failed launch may be tried again; `None` after a launch that started.
    pub next_attempt_at_ms: Option<u64>,
}

impl Attempt {
    fn failed(&self) -> bool {
        self.next_attempt_at_ms.is_some()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub attempts: Vec<Attempt>,
}

impl Record {
    /// Failed attempts since the last launch that started.
    pub fn failures(&self) -> u32 {
        self.attempts
            .iter()
            .rev()
            .take_while(|a| a.failed())
            .count() as u32
    }
}

pub fn record_key(repo: &str, batch: &str) -> String {
    format!("batches/{repo}/{batch}/redrive")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redrive {
    Launched {
        unit: String,
    },
    PriorRunnerAlive {
        why: String,
    },
    /// No runner slot is free: nothing is leased, launched or recorded.
    NoSlot {
        why: String,
    },
    Waiting {
        until_ms: u64,
    },
    Failed {
        why: String,
        next_attempt_at_ms: u64,
    },
    Exhausted {
        failures: u32,
    },
    /// Another holder took the lease since the orphan was found.
    Leased(Held),
    /// The round turned terminal since the orphan was found.
    Terminal,
    Store(String),
}

fn read(p: &Ports, key: &str) -> Result<(Record, Option<Rev>), String> {
    match p.store.get(key).map_err(|e| e.to_string())? {
        None => Ok((Record::default(), None)),
        Some(i) => Ok((
            serde_json::from_value(i.value.data)
                .map_err(|e| format!("{key} does not parse: {e}"))?,
            Some(i.mod_rev),
        )),
    }
}

/// Appends `a` by a CAS on the record, re-read on conflict. Unfenced: `pass` writes it, and a
/// failed launch must be counted even if the lease it took has lapsed.
fn append(p: &Ports, key: &str, a: &Attempt) -> Result<(), String> {
    const TRIES: usize = 3;
    for _ in 0..TRIES {
        let (mut rec, rev) = read(p, key)?;
        rec.attempts.push(a.clone());
        let t = Txn::new()
            .when(Compare::ModRev(key.into(), rev))
            .put(key, Object::new(json!(rec)));
        let req = Request::new(
            &format!("rc-redrive-{:016x}", p.next_u64()),
            HOLDER,
            "redrive.attempt",
        );
        if p.store.txn(&t, &req).map_err(|e| e.to_string())?.succeeded {
            return Ok(());
        }
    }
    Err(format!(
        "{key} moved under {TRIES} tries to record the attempt"
    ))
}

/// A failed launch waits the lease TTL, doubled for each failure before it.
fn backoff_ms(p: &Ports, failures: u32) -> u64 {
    p.leases.ttl_ms.saturating_mul(
        1u64.checked_shl(failures.saturating_sub(1))
            .unwrap_or(u64::MAX),
    )
}

pub fn redrive(p: &Ports, o: &Orphan, launch: &dyn Launch) -> Redrive {
    let key = record_key(&o.repo, &o.batch);
    let rec = match read(p, &key) {
        Ok((r, _)) => r,
        Err(e) => return Redrive::Store(e),
    };
    let failures = rec.failures();
    let now = p.clock.now_ms();
    if let Some(until) = rec
        .attempts
        .last()
        .and_then(|a| a.next_attempt_at_ms)
        .filter(|u| *u > now)
    {
        return Redrive::Waiting { until_ms: until };
    }
    if failures >= p.leases.redrive_retries {
        return Redrive::Exhausted { failures };
    }
    let old = Holder {
        repo: o.repo.clone(),
        batch: o.batch.clone(),
        token: o.expired,
    };
    if let Proof::Unknown { why } = prove_dead(p, &old) {
        return Redrive::PriorRunnerAlive { why };
    }
    let mut slot = match slots::grant(p, &slots::HAND_PREFERENCE, HOLDER) {
        Ok(s) => s,
        Err(no) => {
            return Redrive::NoSlot {
                why: no.to_string(),
            }
        }
    };
    let l = match acquire(p, &Resource::round(&o.repo, &o.batch), HOLDER) {
        Ok(l) => l,
        Err(e) => {
            let _ = slot.release(p);
            return match e {
                LeaseError::Held(h) => Redrive::Leased(h),
                LeaseError::Terminal => Redrive::Terminal,
                e => Redrive::Store(format!("{e:?}")),
            };
        }
    };
    let mut a = Attempt {
        expired_token: o.expired,
        lease_token: l.token(),
        launch_result: String::new(),
        at_ms: now,
        next_attempt_at_ms: None,
    };
    let launched = match record_unit(p, &mut slot, &o.batch, l.token()) {
        Ok(()) => launch.start(p, &o.repo, &o.batch, l.token(), &slot),
        Err(text) => Launched::Failed { text },
    };
    let (unit, result) = match launched {
        Launched::Started { unit } => (unit, "ok"),
        Launched::Running { unit } => (unit, "running"),
        Launched::Failed { text: why } => {
            let next = now.saturating_add(backoff_ms(p, failures + 1));
            a.launch_result = why.clone();
            a.next_attempt_at_ms = Some(next);
            let recorded = append(p, &key, &a);
            let _ = l.release(p);
            let _ = slot.release(p);
            return match recorded {
                Ok(()) => Redrive::Failed {
                    why,
                    next_attempt_at_ms: next,
                },
                Err(e) => Redrive::Store(format!("launch failed ({why}) and {e}")),
            };
        }
    };
    a.launch_result = result.into();
    // A lost record undercounts the next episode; it never skips a launch.
    let _ = append(p, &key, &a);
    Redrive::Launched { unit }
}

/// Records the runner's unit under its slot before launching it, so a slot whose holder lapses
/// is granted again only once that unit is shown stopped.
fn record_unit(p: &Ports, slot: &mut SlotGrant, batch: &str, token: Rev) -> Result<(), String> {
    let unit = unit_name(batch, token)?;
    slot.record_holder(p, &unit, None)
        .map_err(|e| format!("slot {} did not record {unit}: {e:?}", slot.slot()))
}

/// `pass`'s orphans step: every orphan of `repo` re-driven, or held, each with one line.
pub fn step(p: &Ports, repo: &str) {
    match orphans(p, repo) {
        Err(e) => p
            .log
            .write(&Outcome::store_refused(p.clock.now_ms(), repo, &e)),
        Ok(found) => {
            for o in found {
                let r = redrive(p, &o, p.launcher);
                if let Some(line) = Outcome::redrive(p.clock.now_ms(), &o, &r) {
                    p.log.write(&line);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::death::{unit_name, UnitState, VmFact};
    use crate::outcome::Kind;
    use crate::sim::bench::Bench;

    fn batch(b: &Bench, id: &str, state: &str, token: Option<Rev>) {
        let mut v = json!({ "state": state });
        if let Some(t) = token {
            v[lease::LAST_TOKEN_FIELD] = lease::token_json(t);
        }
        b.seed(&format!("batches/r/{id}"), v);
    }

    fn orphan(token: Rev) -> Orphan {
        Orphan {
            repo: "r".into(),
            batch: "b1".into(),
            expired: token,
        }
    }

    #[test]
    fn only_a_non_terminal_once_leased_round_with_no_live_lease_is_an_orphan() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        batch(&b, "waiting", "OPEN", None);
        batch(&b, "landed", "LANDED", Some(t));
        batch(&b, "superseded", "SUPERSEDED", Some(t));
        batch(&b, "garbled", "WHAT", Some(t));
        b.seed(
            "batches/r/nostate",
            json!({ lease::LAST_TOKEN_FIELD: lease::token_json(t) }),
        );
        batch(&b, "leased", "ATTRIBUTING", None);
        batch(&b, "green", "GREEN", Some(t));
        batch(&b, "green-record", "Green", Some(t));
        batch(&b, "landing", "LANDING", Some(t));
        batch(&b, "deploying", "Deploying", Some(t));
        batch(&b, "landed-record", "Landed", Some(t));
        batch(&b, "b2", "Attributing", Some(Rev::new(4, 0)));
        b.seed(
            "batches/r/b1/progress",
            json!({ "state": "OPEN", lease::LAST_TOKEN_FIELD: lease::token_json(t) }),
        );
        b.seed(
            "batches/other/x",
            json!({ "state": "OPEN", lease::LAST_TOKEN_FIELD: lease::token_json(t) }),
        );
        let p = b.ports();
        acquire(&p, &Resource::round("r", "leased"), "runner").unwrap();
        let mut b2 = orphan(Rev::new(4, 0));
        b2.batch = "b2".into();
        assert_eq!(orphans(&p, "r").unwrap(), vec![orphan(t), b2]);
        assert!(orphans(&Bench::unserved().ports(), "r").is_err());
    }

    #[test]
    fn a_redrive_with_proof_takes_a_new_lease_launches_under_it_and_records_the_attempt() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        b.units
            .set(&unit_name("b1", t).unwrap(), UnitState::Stopped("inactive".into()));
        b.launch.script(Ok(()));
        let p = b.ports();
        let r = redrive(&p, &orphan(t), &b.launch);
        let held = holder_of(&p, &Resource::round("r", "b1")).unwrap().unwrap();
        assert_eq!(
            r,
            Redrive::Launched {
                unit: unit_name("b1", held.token).unwrap()
            }
        );
        assert_eq!(held.holder.as_deref(), Some(HOLDER));
        let (rec, _) = read(&p, &record_key("r", "b1")).unwrap();
        assert_eq!(
            rec.attempts,
            vec![Attempt {
                expired_token: t,
                lease_token: held.token,
                launch_result: "ok".into(),
                at_ms: p.clock.now_ms(),
                next_attempt_at_ms: None
            }]
        );
        assert!(
            orphans(&p, "r").unwrap().is_empty(),
            "the re-driven round is leased"
        );
        let tried = b.launch.tried();
        assert_eq!(tried.len(), 1);
        let slot = crate::slots::Slot {
            pool: crate::slots::Pool::Proxmox,
            n: 0,
        };
        assert_eq!(tried[0].slot, slot.to_string());
        let lock = p.store.get(&format!("locks/{}", slot.lock())).unwrap().unwrap();
        assert_eq!(
            (lock.create_rev, lock.lock_expires_at_ms.is_some()),
            (tried[0].slot_token, true),
            "the runner is handed the live slot it was launched on"
        );
        let rec = p.store.get(&slot.record_key()).unwrap().unwrap().value.data;
        assert_eq!(
            (rec["unit"].as_str(), rec["released"].as_bool()),
            (Some(tried[0].unit.as_str()), Some(false)),
            "the unit is recorded under the slot before it is launched"
        );
    }

    #[test]
    fn with_no_slot_free_a_proven_orphan_is_neither_leased_nor_launched() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        let p = b.ports();
        let mut held = Vec::new();
        while let Ok(g) = crate::slots::grant(&p, &crate::slots::HAND_PREFERENCE, "other") {
            held.push(g);
        }
        assert_eq!(held.len(), 4, "proxmox 1 and ec2 3 under the bench");
        match redrive(&p, &orphan(t), &b.launch) {
            Redrive::NoSlot { why } => assert!(why.contains("no runner slot free"), "{why}"),
            r => panic!("{r:?}"),
        }
        assert!(b.launch.launched().is_empty());
        assert_eq!(holder_of(&p, &Resource::round("r", "b1")).unwrap(), None);
        assert_eq!(p.store.get(&record_key("r", "b1")).unwrap(), None);
    }

    #[test]
    fn a_failed_launch_gives_its_slot_back() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        b.launch.script(Err("systemd-run: exit 1".into()));
        let p = b.ports();
        assert!(matches!(
            redrive(&p, &orphan(t), &b.launch),
            Redrive::Failed { .. }
        ));
        assert_eq!(*crate::lock(&b.slot_releases), [p.clock.now_ms()]);
        assert!(crate::slots::grant(&p, &[crate::slots::Pool::Proxmox], "next").is_ok());
    }

    #[test]
    fn without_proof_the_round_is_held_and_nothing_launches_or_is_recorded() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        b.units
            .set(&unit_name("b1", t).unwrap(), UnitState::Running("active".into()));
        let p = b.ports();
        assert_eq!(
            redrive(&p, &orphan(t), &b.launch),
            Redrive::PriorRunnerAlive {
                why: "unit round-controller-run-b1-3-0 is active".into()
            }
        );
        assert!(b.launch.launched().is_empty());
        assert_eq!(holder_of(&p, &Resource::round("r", "b1")).unwrap(), None);
        assert_eq!(p.store.get(&record_key("r", "b1")).unwrap(), None);
    }

    #[test]
    fn a_failed_launch_backs_off_doubling_and_exhausts_after_the_retries() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        let p = b.ports();
        let ttl = p.leases.ttl_ms;
        assert_eq!(p.leases.redrive_retries, 3);
        let mut waits = Vec::new();
        for _ in 0..3 {
            b.launch.script(Err("systemd-run: exit 1".into()));
            let o = orphans(&p, "r").unwrap().remove(0);
            let now = p.clock.now_ms();
            match redrive(&p, &o, &b.launch) {
                Redrive::Failed {
                    next_attempt_at_ms,
                    why,
                } => {
                    assert_eq!(why, "systemd-run: exit 1");
                    waits.push(next_attempt_at_ms - now);
                    assert_eq!(
                        redrive(&p, &o, &b.launch),
                        Redrive::Waiting {
                            until_ms: next_attempt_at_ms
                        },
                        "never retried inside its back-off"
                    );
                    b.clock.advance_to(next_attempt_at_ms);
                }
                r => panic!("{r:?}"),
            }
        }
        assert_eq!(waits, [ttl, 2 * ttl, 4 * ttl]);
        let o = orphans(&p, "r").unwrap().remove(0);
        assert_eq!(
            redrive(&p, &o, &b.launch),
            Redrive::Exhausted { failures: 3 }
        );
        assert_eq!(b.launch.launched().len(), 3);
        let (rec, _) = read(&p, &record_key("r", "b1")).unwrap();
        assert_eq!(rec.attempts.len(), 3);
        assert_eq!(rec.attempts[0].expired_token, t);
        assert!(rec
            .attempts
            .iter()
            .all(|a| a.launch_result == "systemd-run: exit 1"));
        assert_eq!(
            holder_of(&p, &Resource::round("r", "b1")).unwrap(),
            None,
            "a failed launch gives its lease back"
        );
    }

    #[test]
    fn a_started_launch_resets_the_failure_count() {
        let a = |ok: bool| Attempt {
            expired_token: Rev::new(1, 0),
            lease_token: Rev::new(2, 0),
            launch_result: String::new(),
            at_ms: 0,
            next_attempt_at_ms: (!ok).then_some(5),
        };
        let r = Record {
            attempts: vec![a(false), a(false), a(true), a(false)],
        };
        assert_eq!(r.failures(), 1);
        assert_eq!(Record::default().failures(), 0);
    }

    #[test]
    fn a_lease_taken_meanwhile_or_a_broken_store_is_reported_not_launched() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        let p = b.ports();
        let other = acquire(&p, &Resource::round("r", "b1"), "hand-driver").unwrap();
        match redrive(&p, &orphan(t), &b.launch) {
            Redrive::Leased(h) => assert_eq!(
                (h.token, h.holder.as_deref()),
                (other.token(), Some("hand-driver"))
            ),
            r => panic!("{r:?}"),
        }
        b.seed(&record_key("r", "b2"), json!({ "attempts": "nope" }));
        let o2 = Orphan {
            batch: "b2".into(),
            ..orphan(t)
        };
        assert!(
            matches!(redrive(&p, &o2, &b.launch), Redrive::Store(e) if e.contains("does not parse"))
        );
        let o3 = Orphan {
            batch: "gone".into(),
            ..orphan(t)
        };
        assert!(
            matches!(redrive(&p, &o3, &b.launch), Redrive::Store(e) if e.contains("does not exist"))
        );
        assert!(matches!(
            redrive(&Bench::unserved().ports(), &o3, &b.launch),
            Redrive::Store(_)
        ));
        assert!(b.launch.launched().is_empty());
    }

    #[test]
    fn a_failed_launch_whose_record_keeps_moving_is_reported_with_both_causes() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        b.racing_writes(&record_key("r", "b1"));
        b.launch.script(Err("no unit".into()));
        let r = redrive(&b.ports(), &orphan(t), &b.launch);
        assert_eq!(
            r,
            Redrive::Store("launch failed (no unit) and batches/r/b1/redrive moved under 3 tries to record the attempt".into())
        );
    }

    #[test]
    fn the_step_writes_one_line_per_orphan_and_names_a_store_refusal() {
        let b = Bench::new();
        let t = Rev::new(3, 0);
        batch(&b, "b1", "CI_RUNNING", Some(t));
        b.units
            .set(&unit_name("b1", t).unwrap(), UnitState::Stopped("inactive".into()));
        b.vms.set(&unit_name("b1", t).unwrap(), VmFact::NoVm);
        b.launch.script(Ok(()));
        step(&b.ports(), "r");
        let down = Bench::unserved();
        step(&down.ports(), "r");
        let kinds: Vec<Kind> = b
            .log
            .outcomes()
            .into_iter()
            .chain(down.log.outcomes())
            .map(|o| o.kind)
            .collect();
        assert_eq!(kinds, [Kind::Redriven, Kind::StoreRefused]);
    }

    #[test]
    fn backoff_saturates_rather_than_wrapping() {
        let b = Bench::new();
        assert_eq!(backoff_ms(&b.ports(), 200), u64::MAX);
        assert_eq!(backoff_ms(&b.ports(), 0), b.ports().leases.ttl_ms);
    }
}
