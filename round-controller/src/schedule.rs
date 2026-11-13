//! The schedule step: rounds cut just in time for a runner slot, and their runners launched at
//! once. For a repo with members ready and its stack below its depth, `pass` takes a slot, cuts
//! on it, takes the round's lease (whose token the lease library records on the round) and
//! launches the runner with both tokens. A launch that fails gives both back and is recorded in
//! the round's `redrive`, so the orphans step retries it after its back-off. Nothing here waits on
//! a VM, sets `SPIRA_QUEUE_LOCK_HELD` or takes the per-repo queue lock: each queue verb locks for
//! its own transition only.

use state_store::LOCKS_PREFIX;

use crate::cut;
use crate::death::unit_name;
use crate::lease::{acquire, Resource};
use crate::orphans::{self, Attempt, HOLDER};
use crate::outcome::Outcome;
use crate::ports::{Launched, Ports};
use crate::refusal::{self, Refusal};
use crate::slots::{self, SlotGrant};
use crate::stack;

/// The step name a schedule's standing refusal is kept under.
pub const STEP: &str = "schedule";

/// What scheduling one round came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scheduled {
    Launched {
        batch: String,
        unit: String,
    },
    /// The launch failed: the round's lease and slot were given back and the attempt recorded.
    LaunchFailed {
        batch: String,
        why: String,
        next_attempt_at_ms: u64,
    },
}

/// Cuts and launches rounds for `repo` while its stack has room and members are ready, each on a
/// slot taken for it. A repo with no lane record is not the controller's yet and is left alone.
/// No slot free, or a cut the ledger or a lane fault refuses, ends the step with that refusal.
pub fn schedule(p: &Ports, repo: &str) -> Result<Vec<Scheduled>, Refusal> {
    let Some(lane) = stack::lane(p, repo)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    // Each round opened fills one level of the stack, so the depth bounds the step.
    for _ in 0..lane.depth() {
        if cut::plan(p, repo)?.is_none() {
            break;
        }
        let slot =
            slots::grant(p, &slots::HAND_PREFERENCE, HOLDER).map_err(|no| Refusal::NoSlot {
                key: format!("{LOCKS_PREFIX}runner-slot/"),
                text: no.to_string(),
            })?;
        let opened = match cut::cut(p, repo, &slot) {
            Ok(Some(o)) => o,
            Ok(None) => {
                let _ = slot.release(p);
                break;
            }
            Err(e) => {
                let _ = slot.release(p);
                return Err(e);
            }
        };
        cut::record(p, repo, &Ok(Some(opened.clone())));
        let s = start(p, repo, &opened.batch, slot)?;
        let failed = matches!(s, Scheduled::LaunchFailed { .. });
        out.push(s);
        // A launcher that fails once fails the next cut too: wait for the re-drive's back-off.
        if failed {
            break;
        }
    }
    Ok(out)
}

/// Takes the round's lease and launches its runner on `slot`, recording the unit under the slot
/// first, so a slot whose holder lapses is granted again only once that unit is shown stopped.
/// A round whose lease cannot be taken carries no token, so no orphan re-drive would ever find
/// it: it is abandoned at once, before any pass ran, and its members go back to the pool.
fn start(p: &Ports, repo: &str, batch: &str, mut slot: SlotGrant) -> Result<Scheduled, Refusal> {
    let res = Resource::round(repo, batch);
    let lease = match acquire(p, &res, HOLDER) {
        Ok(l) => l,
        Err(e) => {
            let _ = slot.release(p);
            let why = format!("{e:?}");
            let abandoned = p.verbs.abandon(
                repo,
                batch,
                &format!("round-controller cut it but could not take its lease: {why}"),
            );
            p.log.write(&Outcome::of(
                p.clock.now_ms(),
                repo,
                Some(batch),
                crate::ports::Verb::Abandon,
                &abandoned,
            ));
            return Err(Refusal::LeaseNotTaken {
                key: res.lock_key(),
                text: match abandoned {
                    Ok(_) => why,
                    Err(r) => format!("{why}; and abandon refused: {}", r.text),
                },
            });
        }
    };
    let token = lease.token();
    let launched = match orphans::record_unit(p, &mut slot, batch, token) {
        Ok(()) => p.launcher.start(p, repo, batch, token, &slot),
        Err(text) => Launched::Failed { text },
    };
    let now = p.clock.now_ms();
    match launched {
        Launched::Started { unit } | Launched::Running { unit } => {
            p.log.write(&Outcome::launch(now, repo, batch, &unit, None));
            Ok(Scheduled::Launched {
                batch: batch.into(),
                unit,
            })
        }
        Launched::Failed { text } => {
            let next = now.saturating_add(orphans::backoff_ms(p, 1));
            let attempt = Attempt {
                expired_token: token,
                lease_token: token,
                launch_result: text.clone(),
                at_ms: now,
                next_attempt_at_ms: Some(next),
            };
            let recorded = orphans::append(p, &orphans::record_key(repo, batch), &attempt);
            let _ = lease.release(p);
            let _ = slot.release(p);
            let why = match recorded {
                Ok(()) => text,
                Err(e) => format!("{text}; and {e}"),
            };
            let unit = unit_name(batch, token).unwrap_or_default();
            p.log
                .write(&Outcome::launch(now, repo, batch, &unit, Some(&why)));
            Ok(Scheduled::LaunchFailed {
                batch: batch.into(),
                why,
                next_attempt_at_ms: next,
            })
        }
    }
}

/// `pass`'s schedule step: [`schedule`], its refusal told once.
pub fn step(p: &Ports, repo: &str) {
    let r = schedule(p, repo);
    refusal::note(p, repo, STEP, r.as_ref().err());
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;
    use state_store::{Fence, Item, Request, Selector, Txn, TxnResult};

    use super::*;
    use crate::lease::holder_of;
    use crate::ports::{Fields, Store};
    use crate::sim::bench::Bench;
    use crate::sim::verbs::VerbScript;
    use crate::sim::{FakeVerbs, FakeVm};

    fn open_ok(batch: &str) -> VerbScript {
        VerbScript {
            verb: "open".into(),
            repo: None,
            batch: None,
            ok: Some(Fields::from([
                ("batch".to_string(), batch.to_string()),
                ("head".to_string(), format!("{batch}-h")),
            ])),
            refuse: None,
            lock_hold_ms: 0,
        }
    }

    /// A bench whose queue answers `opens`, writing each round it opens when `writes` is set.
    fn bench(opens: &[&str], writes: bool) -> Bench {
        let mut b = Bench::new();
        let profile = crate::sim::profiles().unwrap().remove(0);
        let vm = FakeVm::new(vec![], profile, b.clock.clone());
        let verbs = FakeVerbs::new(
            opens.iter().map(|o| open_ok(o)).collect(),
            vm,
            b.clock.clone(),
        )
        .unwrap();
        b.verbs = if writes {
            verbs.with_store(b.store.clone())
        } else {
            verbs
        };
        b.seed(
            "lanes/r",
            json!({ "landing_ref": "base-0", "image_digest": "sha256:i0", "harness_version": "h0" }),
        );
        b
    }

    fn member(b: &Bench, id: &str) {
        b.seed(
            &format!("lifecycle/{id}"),
            json!({ "repo": "r", "state": "SUBMITTED", "tip": format!("{id}-t") }),
        );
    }

    fn kinds(b: &Bench) -> Vec<&'static str> {
        b.log
            .outcomes()
            .into_iter()
            .map(|o| o.kind.word())
            .collect()
    }

    #[test]
    fn a_ready_member_is_cut_on_a_slot_taken_for_it_and_launched_under_its_lease() {
        let b = bench(&["k1"], true);
        member(&b, "sp-a");
        b.launch.script(Ok(()));
        let p = b.ports();
        let got = schedule(&p, "r").unwrap();
        let held = holder_of(&p, &Resource::round("r", "k1")).unwrap().unwrap();
        let unit = unit_name("k1", held.token).unwrap();
        assert_eq!(
            got,
            [Scheduled::Launched {
                batch: "k1".into(),
                unit: unit.clone()
            }]
        );
        let tried = b.launch.tried();
        assert_eq!(tried.len(), 1);
        let slot = p.store.get("slots/proxmox/0").unwrap().unwrap().value.data;
        assert_eq!(
            slot["unit"].as_str(),
            Some(unit.as_str()),
            "the unit is recorded under the slot before it is launched"
        );
        assert_eq!(kinds(&b), ["opened", "launched"]);
        assert_eq!(
            schedule(&p, "r").unwrap(),
            [],
            "the member is in a live round, so nothing more is cut"
        );
        assert_eq!(b.launch.tried().len(), 1);
    }

    #[test]
    fn nothing_to_cut_or_no_lane_takes_no_slot() {
        let b = bench(&[], true);
        let p = b.ports();
        assert_eq!(schedule(&p, "r").unwrap(), []);
        assert_eq!(schedule(&p, "unregistered").unwrap(), []);
        assert!(p.store.get("slots/proxmox/0").unwrap().is_none());
        assert!(b.verbs.calls().is_empty());
    }

    #[test]
    fn with_no_slot_free_nothing_is_cut_and_the_refusal_names_the_holders() {
        let b = bench(&["k1"], true);
        member(&b, "sp-a");
        let p = b.ports();
        let mut held = Vec::new();
        while let Ok(g) = slots::grant(&p, &slots::HAND_PREFERENCE, "hand-driver") {
            held.push(g);
        }
        match schedule(&p, "r") {
            Err(Refusal::NoSlot { key, text }) => {
                assert_eq!(key, "locks/runner-slot/");
                assert!(text.contains("held by hand-driver"), "{text}");
            }
            r => panic!("{r:?}"),
        }
        assert!(b.verbs.calls().is_empty());
        step(&p, "r");
        step(&p, "r");
        assert_eq!(
            kinds(&b),
            ["declined"],
            "a standing no-slot refusal is told once"
        );
    }

    #[test]
    fn a_failed_launch_gives_back_the_lease_and_the_slot_and_records_the_attempt() {
        let b = bench(&["k1"], true);
        member(&b, "sp-a");
        b.launch.script(Err("systemd-run: exit 1".into()));
        let p = b.ports();
        let now = p.clock.now_ms();
        let got = schedule(&p, "r").unwrap();
        assert_eq!(
            got,
            [Scheduled::LaunchFailed {
                batch: "k1".into(),
                why: "systemd-run: exit 1".into(),
                next_attempt_at_ms: now + p.leases.ttl_ms
            }]
        );
        assert_eq!(holder_of(&p, &Resource::round("r", "k1")).unwrap(), None);
        assert_eq!(*crate::lock(&b.slot_releases), [now]);
        let rec = p
            .store
            .get(&orphans::record_key("r", "k1"))
            .unwrap()
            .unwrap()
            .value
            .data;
        assert_eq!(rec["attempts"][0]["launch_result"], "systemd-run: exit 1");
        let o = orphans::orphans(&p, "r").unwrap();
        assert_eq!(o.len(), 1, "the round is the orphans step's now");
        assert_eq!(
            Some(o[0].expired),
            crate::lease::token_of(&rec["attempts"][0]["expired_token"]),
            "its attempt is recorded against the token the round was left with"
        );
        assert_eq!(kinds(&b).last(), Some(&"launch-failed"));
    }

    #[test]
    fn a_failed_launch_whose_attempt_cannot_be_recorded_says_both() {
        let b = bench(&["k1"], true);
        member(&b, "sp-a");
        b.racing_writes_after(&orphans::record_key("r", "k1"), 0);
        b.launch.script(Err("no unit".into()));
        match schedule(&b.ports(), "r").unwrap().as_slice() {
            [Scheduled::LaunchFailed { why, .. }] => {
                assert!(why.starts_with("no unit; and "), "{why}")
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn a_round_whose_lease_cannot_be_taken_is_abandoned_its_slot_given_back_and_nothing_launched() {
        // The queue answers the open but writes no round record, so there is nothing to lease.
        let mut b = bench(&["k1"], false);
        let profile = crate::sim::profiles().unwrap().remove(0);
        let vm = FakeVm::new(vec![], profile, b.clock.clone());
        let abandon = VerbScript {
            verb: "abandon".into(),
            ok: Some(Fields::new()),
            ..open_ok("k1")
        };
        b.verbs = FakeVerbs::new(vec![open_ok("k1"), abandon], vm, b.clock.clone()).unwrap();
        member(&b, "sp-a");
        let p = b.ports();
        match schedule(&p, "r") {
            Err(Refusal::LeaseNotTaken { key, text }) => {
                assert_eq!(key, "locks/round/r/k1");
                assert!(
                    text.contains("does not exist") && !text.contains("refused"),
                    "{text}"
                );
            }
            r => panic!("{r:?}"),
        }
        assert!(b.launch.tried().is_empty());
        assert_eq!(crate::lock(&b.slot_releases).len(), 1);
        let verbs: Vec<_> = b
            .verbs
            .calls()
            .into_iter()
            .map(|c| (c.verb, c.batch))
            .collect();
        assert_eq!(
            verbs,
            [
                (crate::ports::Verb::Open, None),
                (crate::ports::Verb::Abandon, Some("k1".into()))
            ],
            "the round it could not lease goes back to the pool, not into a stack level forever"
        );
        assert_eq!(kinds(&b), ["opened", "abandoned"]);
        // An abandon the queue refuses is named in the refusal.
        let b = bench(&["k1"], false);
        member(&b, "sp-a");
        match schedule(&b.ports(), "r") {
            Err(Refusal::LeaseNotTaken { text, .. }) => {
                assert!(text.contains("abandon refused"), "{text}")
            }
            r => panic!("{r:?}"),
        }
    }

    /// The bench's store, with the lane read `fail_lane` (1-based) refused and the lifecycle list
    /// `empty_pool` answered empty: what another writer does between the plan and the cut.
    struct Racing<'a> {
        inner: &'a Bench,
        lane_reads: Cell<u32>,
        fail_lane: u32,
        pool_lists: Cell<u32>,
        empty_pool: u32,
    }

    impl Store for Racing<'_> {
        fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
            if key == "lanes/r" {
                self.lane_reads.set(self.lane_reads.get() + 1);
                if self.lane_reads.get() == self.fail_lane {
                    return crate::real::Unserved.get(key);
                }
            }
            self.inner.get(key)
        }
        fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
            if prefixes == ["lifecycle/"] {
                self.pool_lists.set(self.pool_lists.get() + 1);
                if self.pool_lists.get() == self.empty_pool {
                    return Ok((Vec::new(), 0));
                }
            }
            self.inner.list(prefixes, sel)
        }
        fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<TxnResult> {
            self.inner.txn(t, req)
        }
        fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> state_store::Result<Fence> {
            self.inner.lock(resource, ttl_ms, req)
        }
        fn renew(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
            self.inner.renew(f, req)
        }
        fn release(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
            self.inner.release(f, req)
        }
    }

    #[test]
    fn a_cut_that_finds_nothing_or_is_refused_after_the_grant_gives_the_slot_back() {
        for (fail_lane, empty_pool) in [(0, 2), (3, 0)] {
            let b = bench(&["k1"], true);
            member(&b, "sp-a");
            let racing = Racing {
                inner: &b,
                lane_reads: Cell::new(0),
                fail_lane,
                pool_lists: Cell::new(0),
                empty_pool,
            };
            let p = Ports {
                store: &racing,
                ..b.ports()
            };
            let r = schedule(&p, "r");
            if fail_lane > 0 {
                assert!(matches!(r, Err(Refusal::Unreadable { .. })), "{r:?}");
            } else {
                assert_eq!(r.unwrap(), []);
            }
            assert!(b.verbs.calls().is_empty(), "nothing was opened");
            assert_eq!(crate::lock(&b.slot_releases).len(), 1);
        }
    }
}
