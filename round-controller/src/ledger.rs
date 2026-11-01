//! The verdict ledger: per-attempt verdicts under a CAS summary, by-member reds for proven
//! members only, lane faults keyed on (repo, base, image digest), and [`refuses`], which says
//! whether the Planner may open an identity.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use state_store::{Compare, Item, Object, Request, Txn};

use crate::ports::{Member, Ports};
use crate::resources::{round_key, Attempt, Backoff, Cause, Class, PassIdentity, Summary};

const ACTOR: &str = "round-controller";

pub fn summary_key(id: &PassIdentity) -> String {
    format!("verdicts/{}/{}", id.repo, id.hash())
}

pub fn attempt_key(id: &PassIdentity, n: u32) -> String {
    format!("{}/attempt/{n}", summary_key(id))
}

pub fn by_member_key(repo: &str, m: &Member) -> String {
    format!("verdicts/{repo}/by-member/{}/{}", m.id, m.tip)
}

pub fn fault_key(repo: &str, base: &str, image_digest: &str) -> String {
    format!("faults/{repo}/{base}/{image_digest}")
}

fn lifecycle_key(id: &str) -> String {
    format!("lifecycle/{id}")
}

/// How transient faults back an identity off; both values are knobs, not policy
/// (law-tuning-is-not-policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultPolicy {
    /// `SPIRA_ROUND_FAULT_BACKOFF_SECS`: the first attempt's back-off, doubled per attempt.
    pub backoff_secs: u64,
    /// `SPIRA_ROUND_FAULT_RETRIES`: attempts after the first; a fault on the last one stands.
    pub retries: u32,
}

impl FaultPolicy {
    pub fn from_lookup(get: impl Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        fn parse<T: std::str::FromStr>(
            get: &impl Fn(&str) -> Result<String, String>,
            key: &str,
        ) -> Result<T, String>
        where
            T::Err: std::fmt::Display,
        {
            let v = get(key)?;
            v.trim()
                .parse()
                .map_err(|e| format!("{key} = {v:?} does not parse: {e}"))
        }
        Ok(FaultPolicy {
            backoff_secs: parse(&get, "SPIRA_ROUND_FAULT_BACKOFF_SECS")?,
            retries: parse(&get, "SPIRA_ROUND_FAULT_RETRIES")?,
        })
    }

    pub fn from_config() -> Result<Self, String> {
        Self::from_lookup(spira_config::process::cfg)
    }

    /// The back-off after the `n`th attempt, a transient fault, recorded at `at`.
    pub fn backoff(&self, n: u32, at: u64) -> Backoff {
        if n > self.retries {
            return Backoff::Standing;
        }
        let factor = 1u64.checked_shl(n.saturating_sub(1)).unwrap_or(u64::MAX);
        Backoff::Until(
            at.saturating_add(
                self.backoff_secs
                    .saturating_mul(1000)
                    .saturating_mul(factor),
            ),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LedgerError {
    Store(state_store::Error),
    /// The summary moved under the write twice running.
    Contended {
        key: String,
    },
    Undecodable {
        key: String,
        text: String,
    },
    /// `mark_proven` names a member the identity does not hold.
    NotAMember {
        id: String,
    },
    /// `mark_proven` on an identity whose summary is not red.
    NotRed {
        hash: String,
    },
    /// `record_lane_fault` with a cause that backs off an identity instead.
    NotALaneCause {
        cause: Cause,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl From<state_store::Error> for LedgerError {
    fn from(e: state_store::Error) -> Self {
        LedgerError::Store(e)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(i: &Item) -> Result<T, LedgerError> {
    serde_json::from_value(i.value.data.clone()).map_err(|e| LedgerError::Undecodable {
        key: i.key.clone(),
        text: e.to_string(),
    })
}

fn encode<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or_default()
}

fn digest(v: &impl Serialize) -> String {
    Sha256::digest(serde_json::to_vec(v).unwrap_or_default())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Red and fault verdicts are kept while any member's lifecycle record lives; green and flaky
/// ones while their round does. The v2 GC controller collects them.
fn owners(id: &PassIdentity, batch: &str, class: &Class) -> Vec<String> {
    if class.retained_by_members() {
        id.members.iter().map(|m| lifecycle_key(&m.id)).collect()
    } else {
        vec![round_key(&id.repo, batch)]
    }
}

fn owned(data: serde_json::Value, owners: &[String]) -> Object {
    let refs: Vec<&str> = owners.iter().map(String::as_str).collect();
    Object::new(data).with_owners(&refs)
}

/// The verdict write: attempt `n` created only if absent, and the summary CAS'd on the
/// `mod_rev` it was read at (`None`: absent). Two user ops; the contract adds two more. The
/// summary keeps every owner it had, so a green after a red cannot let the round's collection
/// take the summary and strand the red's attempts.
pub fn attempt_txn(
    id: &PassIdentity,
    batch: &str,
    n: u32,
    attempt: &Attempt,
    summary: &Summary,
    read: Option<&Item>,
) -> Txn {
    let own = owners(id, batch, &attempt.class);
    let mut kept: Vec<String> =
        read.map_or_else(Vec::new, |i| i.value.metadata.owner_references.clone());
    for o in &own {
        if !kept.contains(o) {
            kept.push(o.clone());
        }
    }
    Txn::new()
        .when(Compare::ModRev(attempt_key(id, n), None))
        .when(Compare::ModRev(summary_key(id), read.map(|i| i.mod_rev)))
        .put(&attempt_key(id, n), owned(encode(attempt), &own))
        .put(&summary_key(id), owned(encode(summary), &kept))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub n: u32,
    pub summary: Summary,
}

/// Records one attempt on `id`, run by round `batch`. A second fault on one identity is
/// attempt 2, never a lost or conflicting verdict: a summary that moved under the write is
/// re-read once, and a retried call whose attempt is already the latest is answered with it.
pub fn record_attempt(
    p: &Ports,
    id: &PassIdentity,
    batch: &str,
    attempt: &Attempt,
    policy: &FaultPolicy,
) -> Result<Recorded, LedgerError> {
    let key = summary_key(id);
    for _ in 0..2 {
        let cur = p.store.get(&key)?;
        let prev: Option<Summary> = cur.as_ref().map(decode).transpose()?;
        if let Some(s) = &prev {
            if let Some(last) = p.store.get(&attempt_key(id, s.attempts))? {
                if decode::<Attempt>(&last)? == *attempt {
                    return Ok(Recorded {
                        n: s.attempts,
                        summary: s.clone(),
                    });
                }
            }
        }
        let n = prev.as_ref().map_or(0, |s| s.attempts) + 1;
        let transient = matches!(&attempt.class, Class::Fault { cause } if !cause.is_lane());
        let transient_faults = match (&prev, transient) {
            (_, false) => 0,
            (Some(s), true) => s.transient_faults + 1,
            (None, true) => 1,
        };
        let summary = Summary {
            class: attempt.class.clone(),
            attempts: n,
            transient_faults,
            backoff_until: transient.then(|| policy.backoff(transient_faults, attempt.at)),
            last_attempt: attempt.at,
        };
        let t = attempt_txn(id, batch, n, attempt, &summary, cur.as_ref());
        let req = Request::new(
            &format!(
                "ledger-attempt-{}-{n}-{}",
                id.hash(),
                digest(&(batch, attempt))
            ),
            ACTOR,
            "ledger-attempt",
        );
        if p.store.txn(&t, &req)?.succeeded {
            return Ok(Recorded { n, summary });
        }
    }
    Err(LedgerError::Contended { key })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenRed {
    pub hash: String,
    pub base: String,
    pub image_digest: String,
    pub harness_version: String,
    pub at: u64,
}

/// Marks each of `proven` red at its tip under `id`'s hash. Only the Judge calls it, after the
/// Attributor's Decision proved those members; an unproven member of a red round gets no key.
pub fn mark_proven(
    p: &Ports,
    id: &PassIdentity,
    proven: &[Member],
    at: u64,
) -> Result<(), LedgerError> {
    let hash = id.hash();
    if let Some(m) = proven.iter().find(|m| !id.members.contains(m)) {
        return Err(LedgerError::NotAMember { id: m.id.clone() });
    }
    if proven.is_empty() {
        return Ok(());
    }
    let red = match p.store.get(&summary_key(id))? {
        Some(i) => matches!(decode::<Summary>(&i)?.class, Class::Red { .. }),
        None => false,
    };
    if !red {
        return Err(LedgerError::NotRed { hash });
    }
    let value = encode(&ProvenRed {
        hash: hash.clone(),
        base: id.base.clone(),
        image_digest: id.image_digest.clone(),
        harness_version: id.harness_version.clone(),
        at,
    });
    let t = proven.iter().fold(Txn::new(), |t, m| {
        t.put(
            &by_member_key(&id.repo, m),
            owned(value.clone(), &[lifecycle_key(&m.id)]),
        )
    });
    let req = Request::new(
        &format!("ledger-proven-{hash}-{}", digest(&proven)),
        ACTOR,
        "ledger-proven",
    );
    p.store.txn(&t, &req)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneFault {
    pub cause: Cause,
    #[serde(default)]
    pub evidence: String,
    pub since: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneFaultRecorded {
    New,
    /// One already stood on that key; its `since` is kept.
    Standing,
}

/// Stops `repo`'s lane on (base, image digest) until an input changes or it is cleared.
pub fn record_lane_fault(
    p: &Ports,
    repo: &str,
    base: &str,
    image_digest: &str,
    fault: &LaneFault,
) -> Result<LaneFaultRecorded, LedgerError> {
    if !fault.cause.is_lane() {
        return Err(LedgerError::NotALaneCause {
            cause: fault.cause.clone(),
        });
    }
    let key = fault_key(repo, base, image_digest);
    let t = Txn::new()
        .when(Compare::ModRev(key.clone(), None))
        .put(&key, Object::new(encode(fault)));
    let req = Request::new(
        &format!("ledger-fault-{}-{}", digest(&key), p.next_u64()),
        ACTOR,
        "ledger-lane-fault",
    );
    Ok(if p.store.txn(&t, &req)?.succeeded {
        LaneFaultRecorded::New
    } else {
        LaneFaultRecorded::Standing
    })
}

/// Ends a lane fault's standing; the key goes with it.
pub fn clear_lane_fault(
    p: &Ports,
    repo: &str,
    base: &str,
    image_digest: &str,
) -> Result<(), LedgerError> {
    let key = fault_key(repo, base, image_digest);
    let req = Request::new(
        &format!("ledger-fault-clear-{}-{}", digest(&key), p.next_u64()),
        ACTOR,
        "ledger-lane-fault-clear",
    );
    p.store.txn(&Txn::new().delete(&key), &req)?;
    Ok(())
}

/// Why the Planner may not open an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerRefusal {
    RedIdentity {
        hash: String,
        step: String,
    },
    RedMember {
        id: String,
        tip: String,
        hash: String,
    },
    LaneFault {
        key: String,
        cause: Cause,
    },
    /// `until` is `None` once the fault stands.
    BackingOff {
        hash: String,
        until: Option<u64>,
    },
}

impl LedgerRefusal {
    pub fn word(&self) -> &'static str {
        match self {
            LedgerRefusal::RedIdentity { .. } => "red-identity",
            LedgerRefusal::RedMember { .. } => "red-member",
            LedgerRefusal::LaneFault { .. } => "lane-fault",
            LedgerRefusal::BackingOff { .. } => "backing-off",
        }
    }
}

/// Whether the Planner may open `id`: `None` admits it. A lane fault on the identity's (base,
/// digest) first, then the identity's own verdict, then any member proven red on the same base,
/// image and harness that the lifecycle has not absolved.
pub fn refuses(p: &Ports, id: &PassIdentity) -> Result<Option<LedgerRefusal>, LedgerError> {
    let fkey = fault_key(&id.repo, &id.base, &id.image_digest);
    if let Some(i) = p.store.get(&fkey)? {
        let f: LaneFault = decode(&i)?;
        return Ok(Some(LedgerRefusal::LaneFault {
            key: fkey,
            cause: f.cause,
        }));
    }
    let hash = id.hash();
    if let Some(i) = p.store.get(&summary_key(id))? {
        let s: Summary = decode(&i)?;
        match (s.class, s.backoff_until) {
            (Class::Red { step }, _) => return Ok(Some(LedgerRefusal::RedIdentity { hash, step })),
            (Class::Fault { .. }, Some(Backoff::Standing)) => {
                return Ok(Some(LedgerRefusal::BackingOff { hash, until: None }))
            }
            (Class::Fault { .. }, Some(Backoff::Until(t))) if p.clock.now_ms() < t => {
                return Ok(Some(LedgerRefusal::BackingOff {
                    hash,
                    until: Some(t),
                }))
            }
            _ => {}
        }
    }
    for m in &id.members {
        let Some(i) = p.store.get(&by_member_key(&id.repo, m))? else {
            continue;
        };
        let r: ProvenRed = decode(&i)?;
        let same = (&r.base, &r.image_digest, &r.harness_version)
            == (&id.base, &id.image_digest, &id.harness_version);
        if same && !absolved(p, m)? {
            return Ok(Some(LedgerRefusal::RedMember {
                id: m.id.clone(),
                tip: m.tip.clone(),
                hash: r.hash,
            }));
        }
    }
    Ok(None)
}

/// The lifecycle withdrew the red at this tip: its record states an ejected red tip, and it is
/// not this one. No record, or one that does not carry the field, is not an absolve.
fn absolved(p: &Ports, m: &Member) -> Result<bool, LedgerError> {
    Ok(p.store.get(&lifecycle_key(&m.id))?.is_some_and(|i| {
        i.value
            .data
            .get("ejected_red_tip")
            .is_some_and(|t| t.as_str() != Some(m.tip.as_str()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::MemLog;
    use crate::ports::RoundVerbs;
    use crate::resources::{InitiatedBy, SubPass, SubPassKind, TXN_BOOKKEEPING};
    use runtime::ports::{Clock, SeededRng, SysProc, VirtualClock};
    use state_store::{Config, Contract, MemBackend, Selector};
    use std::sync::Mutex;

    use crate::liveness::LeaseSettings;
    use crate::sim::bench::{bench_settings, FakeLaunch, FakeUnits, FakeVms};

    struct World {
        clock: VirtualClock,
        store: Contract<MemBackend>,
        rng: Mutex<SeededRng>,
        log: MemLog,
        units: FakeUnits,
        vms: FakeVms,
        launch: FakeLaunch,
        leases: LeaseSettings,
    }

    impl World {
        fn new() -> Self {
            let clock = VirtualClock::new(1_800_000_000_000);
            let store = Contract::open(
                MemBackend::new(),
                state_store::Ports::virtual_clock(&clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .unwrap();
            World {
                clock,
                store,
                rng: Mutex::new(SeededRng::new(1)),
                log: MemLog::default(),
                units: FakeUnits::default(),
                vms: FakeVms::default(),
                launch: FakeLaunch::default(),
                leases: bench_settings(),
            }
        }

        fn ports<'a>(&'a self, verbs: &'a dyn RoundVerbs) -> Ports<'a> {
            Ports {
                clock: &self.clock,
                proc_: &SysProc,
                rng: &self.rng,
                store: &self.store,
                verbs,
                log: &self.log,
                units: &self.units,
                vms: &self.vms,
                launcher: &self.launch,
                leases: &self.leases,
            }
        }

        fn put(&self, key: &str, data: serde_json::Value) {
            self.store
                .txn(
                    &Txn::new().put(key, Object::new(data)),
                    &Request::new(
                        &format!("put-{key}-{}", self.store.head().unwrap()),
                        "t",
                        "t",
                    ),
                )
                .unwrap();
        }

        fn keys(&self, prefix: &str) -> Vec<String> {
            self.store
                .list(&[prefix], &Selector::Everything)
                .unwrap()
                .0
                .into_iter()
                .map(|i| i.key)
                .collect()
        }
    }

    /// No script: any verb the ledger called would be named an unscripted fault.
    fn verbs() -> crate::sim::FakeVerbs {
        let clock = VirtualClock::new(0);
        let profile = crate::sim::profiles().unwrap().remove(0);
        let vm = crate::sim::FakeVm::new(vec![], profile, clock.clone());
        crate::sim::FakeVerbs::new(vec![], vm, clock).unwrap()
    }

    fn m(id: &str, tip: &str) -> Member {
        Member {
            id: id.into(),
            tip: tip.into(),
        }
    }

    fn ident(members: &[(&str, &str)]) -> PassIdentity {
        PassIdentity::new(
            "lane",
            members.iter().map(|(i, t)| m(i, t)).collect(),
            "b0",
            "sha256:i1",
            "h1",
            "all",
        )
        .unwrap()
    }

    fn attempt(class: Class, at: u64) -> Attempt {
        Attempt {
            class,
            initiated_by: InitiatedBy::Planner,
            sub_passes: vec![],
            evidence: String::new(),
            at,
        }
    }

    fn vm_fault(at: u64) -> Attempt {
        attempt(Class::Fault { cause: Cause::Vm }, at)
    }

    fn red(step: &str, at: u64) -> Attempt {
        attempt(Class::Red { step: step.into() }, at)
    }

    const POLICY: FaultPolicy = FaultPolicy {
        backoff_secs: 70,
        retries: 2,
    };

    #[test]
    fn a_verdict_write_is_two_user_ops_and_fits_the_limits() {
        let id = ident(&[("sp-a", "1")]);
        let s = Summary {
            class: Class::Green,
            attempts: 1,
            transient_faults: 0,
            backoff_until: None,
            last_attempt: 0,
        };
        let t = attempt_txn(&id, "b1", 1, &attempt(Class::Green, 0), &s, None);
        assert_eq!((t.ops.len(), t.compares.len(), t.else_ops.len()), (2, 2, 0));
        assert_eq!(state_store::limits::check(&t, TXN_BOOKKEEPING), Ok(()));
    }

    #[test]
    fn two_faults_on_one_identity_are_attempts_one_and_two_under_a_summary_of_two() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-b", "2"), ("sp-a", "1")]);
        let now = w.clock.now_ms();
        let one = record_attempt(&p, &id, "b1", &vm_fault(now), &POLICY).unwrap();
        let two = record_attempt(&p, &id, "b2", &vm_fault(now + 1), &POLICY).unwrap();
        assert_eq!((one.n, two.n, two.summary.attempts), (1, 2, 2));
        assert_eq!(
            one.summary.backoff_until,
            Some(Backoff::Until(now + 70_000))
        );
        assert_eq!(
            two.summary.backoff_until,
            Some(Backoff::Until(now + 1 + 140_000))
        );
        let h = id.hash();
        assert_eq!(
            w.keys(&format!("verdicts/lane/{h}")),
            [
                format!("verdicts/lane/{h}"),
                format!("verdicts/lane/{h}/attempt/1"),
                format!("verdicts/lane/{h}/attempt/2"),
            ]
        );
        let a2 = w.store.get(&attempt_key(&id, 2)).unwrap().unwrap();
        assert_eq!(decode::<Attempt>(&a2).unwrap(), vm_fault(now + 1));
        assert_eq!(
            a2.value.metadata.owner_references,
            ["lifecycle/sp-a", "lifecycle/sp-b"]
        );
    }

    #[test]
    fn a_green_attempt_and_its_summary_are_owned_by_the_round() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-a", "1")]);
        let mut g = attempt(Class::Green, 5);
        g.sub_passes = vec![SubPass {
            kind: SubPassKind::Rerun,
            green: true,
        }];
        let r = record_attempt(&p, &id, "b7", &g, &POLICY).unwrap();
        assert_eq!(r.summary.backoff_until, None);
        for k in [summary_key(&id), attempt_key(&id, 1)] {
            let i = w.store.get(&k).unwrap().unwrap();
            assert_eq!(
                i.value.metadata.owner_references,
                ["batches/lane/b7"],
                "{k}"
            );
        }
        assert_eq!(
            decode::<Attempt>(&w.store.get(&attempt_key(&id, 1)).unwrap().unwrap()).unwrap(),
            g
        );
    }

    /// A store whose summary moves under every write until it has moved `moves` times.
    struct Racing<'a> {
        inner: &'a Contract<MemBackend>,
        moves: Mutex<u32>,
    }

    impl crate::ports::Store for Racing<'_> {
        fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
            self.inner.get(key)
        }
        fn list(&self, p: &[&str], s: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
            self.inner.list(p, s)
        }
        fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<state_store::TxnResult> {
            let mut left = crate::lock(&self.moves);
            if *left > 0 {
                *left -= 1;
                let Some(state_store::Op::Put(k, v)) = t.ops.last() else {
                    unreachable!("a verdict write ends with its summary")
                };
                let mut v = v.clone();
                if let Some(a) = v.data.get_mut("attempts") {
                    *a = serde_json::json!(a.as_u64().unwrap_or(0) + 10);
                }
                self.inner.txn(
                    &Txn::new().put(k, v),
                    &Request::new(&format!("{}-rival", req.id), "rival", "t"),
                )?;
            }
            self.inner.txn(t, req)
        }
        fn lock(
            &self,
            r: &str,
            ttl: u64,
            req: &Request,
        ) -> state_store::Result<state_store::Fence> {
            self.inner.lock(r, ttl, req)
        }
        fn renew(&self, f: &state_store::Fence, req: &Request) -> state_store::Result<()> {
            self.inner.renew(f, req)
        }
        fn release(&self, f: &state_store::Fence, req: &Request) -> state_store::Result<()> {
            self.inner.release(f, req)
        }
    }

    #[test]
    fn a_summary_moved_once_is_re_read_and_twice_is_contended() {
        let w = World::new();
        let v = verbs();
        let id = ident(&[("sp-a", "1")]);
        let racing = Racing {
            inner: &w.store,
            moves: Mutex::new(1),
        };
        let mut p = w.ports(&v);
        p.store = &racing;
        let r = record_attempt(&p, &id, "b1", &vm_fault(1), &POLICY).unwrap();
        assert_eq!(
            (r.n, r.summary.attempts),
            (12, 12),
            "the rival's 11 then ours"
        );
        *crate::lock(&racing.moves) = 2;
        assert_eq!(
            record_attempt(&p, &id, "b1", &vm_fault(2), &POLICY),
            Err(LedgerError::Contended {
                key: summary_key(&id)
            })
        );
    }

    #[test]
    fn an_undecodable_summary_and_an_unserved_store_are_errors_not_verdicts() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-a", "1")]);
        w.put(&summary_key(&id), serde_json::json!({ "class": "purple" }));
        assert!(matches!(
            record_attempt(&p, &id, "b1", &vm_fault(1), &POLICY),
            Err(LedgerError::Undecodable { .. })
        ));
        assert!(matches!(
            refuses(&p, &id),
            Err(LedgerError::Undecodable { .. })
        ));
        let mut p = w.ports(&v);
        p.store = &crate::real::Unserved;
        assert!(matches!(
            record_attempt(&p, &id, "b1", &vm_fault(1), &POLICY),
            Err(LedgerError::Store(_))
        ));
        assert!(matches!(refuses(&p, &id), Err(LedgerError::Store(_))));
    }

    #[test]
    fn the_back_off_doubles_per_attempt_and_stands_after_the_retries() {
        let p = FaultPolicy {
            backoff_secs: 10,
            retries: 2,
        };
        assert_eq!(p.backoff(1, 0), Backoff::Until(10_000));
        assert_eq!(p.backoff(2, 0), Backoff::Until(20_000));
        assert_eq!(p.backoff(3, 5), Backoff::Standing);
        let none = FaultPolicy {
            backoff_secs: 10,
            retries: 0,
        };
        assert_eq!(none.backoff(1, 0), Backoff::Standing);
        let huge = FaultPolicy {
            backoff_secs: u64::MAX,
            retries: u32::MAX,
        };
        assert_eq!(huge.backoff(200, 1), Backoff::Until(u64::MAX));
    }

    #[test]
    fn the_policy_reads_both_keys_and_names_one_that_does_not_parse() {
        let get = |bad: &'static str| {
            move |k: &str| -> Result<String, String> {
                match k {
                    k if k == bad => Ok("ten".into()),
                    "SPIRA_ROUND_FAULT_BACKOFF_SECS" => Ok(" 45 ".into()),
                    "SPIRA_ROUND_FAULT_RETRIES" => Ok("3".into()),
                    _ => Err(format!("{k} is not declared")),
                }
            }
        };
        assert_eq!(
            FaultPolicy::from_lookup(get("none")),
            Ok(FaultPolicy {
                backoff_secs: 45,
                retries: 3
            })
        );
        let e = FaultPolicy::from_lookup(get("SPIRA_ROUND_FAULT_RETRIES")).unwrap_err();
        assert!(e.starts_with("SPIRA_ROUND_FAULT_RETRIES = \"ten\""), "{e}");
        assert!(FaultPolicy::from_lookup(|k| Err(format!("{k} is not declared"))).is_err());
    }

    #[test]
    fn each_refusal_arm_and_none() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let now = w.clock.now_ms();
        let id = ident(&[("sp-a", "1")]);
        assert_eq!(refuses(&p, &id), Ok(None));

        record_attempt(&p, &id, "b1", &vm_fault(now), &POLICY).unwrap();
        assert_eq!(
            refuses(&p, &id),
            Ok(Some(LedgerRefusal::BackingOff {
                hash: id.hash(),
                until: Some(now + 70_000)
            }))
        );
        w.clock.advance(70_000);
        assert_eq!(refuses(&p, &id), Ok(None), "the back-off ran out");
        for i in 0..2 {
            record_attempt(&p, &id, "b1", &vm_fault(now + 1 + i), &POLICY).unwrap();
        }
        let standing = refuses(&p, &id).unwrap().unwrap();
        assert_eq!(
            standing,
            LedgerRefusal::BackingOff {
                hash: id.hash(),
                until: None
            }
        );
        assert_eq!(standing.word(), "backing-off");

        let r = ident(&[("sp-r", "1")]);
        record_attempt(&p, &r, "b2", &red("spira-lint", now), &POLICY).unwrap();
        let got = refuses(&p, &r).unwrap().unwrap();
        assert_eq!(
            got,
            LedgerRefusal::RedIdentity {
                hash: r.hash(),
                step: "spira-lint".into()
            }
        );
        assert_eq!(got.word(), "red-identity");

        let digest = ident(&[("sp-z", "9")]);
        let fault = LaneFault {
            cause: Cause::Image {
                digest: "sha256:i1".into(),
            },
            evidence: "python3.14-venv missing".into(),
            since: now,
        };
        assert_eq!(
            record_lane_fault(&p, "lane", "b0", "sha256:i1", &fault),
            Ok(LaneFaultRecorded::New)
        );
        let got = refuses(&p, &digest).unwrap().unwrap();
        assert_eq!(
            got,
            LedgerRefusal::LaneFault {
                key: "faults/lane/b0/sha256:i1".into(),
                cause: fault.cause.clone()
            }
        );
        assert_eq!(got.word(), "lane-fault");
    }

    #[test]
    fn a_member_is_refused_only_when_proven_and_only_on_the_same_base_image_and_harness() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let ab = ident(&[("sp-a", "1"), ("sp-b", "2")]);
        record_attempt(&p, &ab, "b1", &red("suite-x", 1), &POLICY).unwrap();
        mark_proven(&p, &ab, &[m("sp-a", "1")], 2).unwrap();
        assert_eq!(
            w.keys("verdicts/lane/by-member/"),
            ["verdicts/lane/by-member/sp-a/1"]
        );
        let by = w
            .store
            .get("verdicts/lane/by-member/sp-a/1")
            .unwrap()
            .unwrap();
        assert_eq!(by.value.metadata.owner_references, ["lifecycle/sp-a"]);

        assert_eq!(
            refuses(&p, &ident(&[("sp-b", "2")])),
            Ok(None),
            "B is innocent"
        );
        let ac = ident(&[("sp-a", "1"), ("sp-c", "3")]);
        let got = refuses(&p, &ac).unwrap().unwrap();
        assert_eq!(
            got,
            LedgerRefusal::RedMember {
                id: "sp-a".into(),
                tip: "1".into(),
                hash: ab.hash()
            }
        );
        assert_eq!(got.word(), "red-member");
        let mut moved = ac.clone();
        moved.base = "b1".into();
        assert_eq!(refuses(&p, &moved), Ok(None), "the base moved");
        let mut harness = ac.clone();
        harness.harness_version = "h2".into();
        assert_eq!(refuses(&p, &harness), Ok(None), "the harness changed");
        let mut image = ac.clone();
        image.image_digest = "sha256:i2".into();
        assert_eq!(refuses(&p, &image), Ok(None), "the image changed");
        assert_eq!(refuses(&p, &ident(&[("sp-a", "4")])), Ok(None), "a new tip");
    }

    #[test]
    fn a_lifecycle_absolve_of_the_tip_lifts_red_member() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let a = ident(&[("sp-a", "1")]);
        record_attempt(&p, &a, "b1", &red("s", 1), &POLICY).unwrap();
        mark_proven(&p, &a, &[m("sp-a", "1")], 2).unwrap();
        let ac = ident(&[("sp-a", "1"), ("sp-c", "3")]);
        for row in [
            serde_json::json!({ "state": "Rework" }),
            serde_json::json!({ "ejected_red_tip": "1" }),
        ] {
            w.put("lifecycle/sp-a", row);
            assert!(matches!(
                refuses(&p, &ac),
                Ok(Some(LedgerRefusal::RedMember { .. }))
            ));
        }
        assert!(matches!(
            refuses(&p, &ac),
            Ok(Some(LedgerRefusal::RedMember { .. }))
        ));
        w.put(
            "lifecycle/sp-a",
            serde_json::json!({ "ejected_red_tip": null }),
        );
        assert_eq!(refuses(&p, &ac), Ok(None));
    }

    #[test]
    fn flaky_and_green_never_refuse_and_a_lane_cause_backs_nothing_off() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        for (i, class) in [
            Class::Flaky,
            Class::Green,
            Class::Fault { cause: Cause::Base },
        ]
        .into_iter()
        .enumerate()
        {
            let id = ident(&[(&format!("sp-{i}"), "1")]);
            let r = record_attempt(&p, &id, "b1", &attempt(class.clone(), 1), &POLICY).unwrap();
            assert_eq!(r.summary.backoff_until, None, "{class:?}");
            assert_eq!(refuses(&p, &id), Ok(None), "{class:?}");
        }
    }

    #[test]
    fn mark_proven_refuses_a_stranger_an_identity_not_red_and_a_bad_summary() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let ab = ident(&[("sp-a", "1"), ("sp-b", "2")]);
        assert_eq!(
            mark_proven(&p, &ab, &[m("sp-c", "3")], 1),
            Err(LedgerError::NotAMember { id: "sp-c".into() })
        );
        assert_eq!(
            mark_proven(&p, &ab, &[m("sp-a", "9")], 1),
            Err(LedgerError::NotAMember { id: "sp-a".into() }),
            "a member at another tip"
        );
        assert_eq!(
            mark_proven(&p, &ab, &[m("sp-a", "1")], 1),
            Err(LedgerError::NotRed { hash: ab.hash() }),
            "no verdict"
        );
        record_attempt(&p, &ab, "b1", &vm_fault(1), &POLICY).unwrap();
        assert_eq!(
            mark_proven(&p, &ab, &[m("sp-a", "1")], 1),
            Err(LedgerError::NotRed { hash: ab.hash() }),
            "a fault proves no one"
        );
        w.put(&summary_key(&ab), serde_json::json!(7));
        assert!(matches!(
            mark_proven(&p, &ab, &[m("sp-a", "1")], 1),
            Err(LedgerError::Undecodable { .. })
        ));
        assert!(w.keys("verdicts/lane/by-member/").is_empty());
    }

    #[test]
    fn a_lane_fault_stands_once_keeps_its_since_and_clears() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let f = |since| LaneFault {
            cause: Cause::Install,
            evidence: String::new(),
            since,
        };
        assert_eq!(
            record_lane_fault(&p, "lane", "b0", "d1", &f(1)),
            Ok(LaneFaultRecorded::New)
        );
        assert_eq!(
            record_lane_fault(&p, "lane", "b0", "d1", &f(2)),
            Ok(LaneFaultRecorded::Standing)
        );
        let i = w.store.get("faults/lane/b0/d1").unwrap().unwrap();
        assert_eq!(decode::<LaneFault>(&i).unwrap().since, 1);
        assert_eq!(
            record_lane_fault(
                &p,
                "lane",
                "b0",
                "d1",
                &LaneFault {
                    cause: Cause::OverCap,
                    evidence: String::new(),
                    since: 3
                }
            ),
            Err(LedgerError::NotALaneCause {
                cause: Cause::OverCap
            })
        );
        clear_lane_fault(&p, "lane", "b0", "d1").unwrap();
        clear_lane_fault(&p, "lane", "b0", "d1").unwrap();
        assert!(w.keys("faults/").is_empty());
        let id = PassIdentity::new("lane", vec![m("sp-a", "1")], "b0", "d1", "h1", "all").unwrap();
        assert_eq!(refuses(&p, &id), Ok(None));
        w.put("faults/lane/b0/d1", serde_json::json!("garbled"));
        assert!(matches!(
            refuses(&p, &id),
            Err(LedgerError::Undecodable { .. })
        ));
    }

    #[test]
    fn a_garbled_by_member_key_is_an_error() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        w.put("verdicts/lane/by-member/sp-a/1", serde_json::json!([]));
        assert!(matches!(
            refuses(&p, &ident(&[("sp-a", "1")])),
            Err(LedgerError::Undecodable { .. })
        ));
        assert_eq!(
            LedgerError::NotRed { hash: "h".into() }.to_string(),
            "NotRed { hash: \"h\" }"
        );
    }

    #[test]
    fn a_retried_call_is_answered_with_its_attempt_not_a_second_one() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-a", "1")]);
        let one = record_attempt(&p, &id, "b1", &vm_fault(5), &POLICY).unwrap();
        assert_eq!(
            record_attempt(&p, &id, "b1", &vm_fault(5), &POLICY),
            Ok(one)
        );
        assert_eq!(w.keys(&format!("{}/attempt/", summary_key(&id))).len(), 1);
        assert_eq!(
            record_attempt(&p, &id, "b1", &vm_fault(6), &POLICY).map(|r| r.n),
            Ok(2)
        );
    }

    #[test]
    fn a_green_after_a_red_keeps_the_members_owning_the_summary() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-a", "1")]);
        record_attempt(&p, &id, "b1", &red("s", 1), &POLICY).unwrap();
        record_attempt(&p, &id, "b2", &attempt(Class::Green, 2), &POLICY).unwrap();
        let s = w.store.get(&summary_key(&id)).unwrap().unwrap();
        assert_eq!(
            s.value.metadata.owner_references,
            ["lifecycle/sp-a", "batches/lane/b2"]
        );
        let a2 = w.store.get(&attempt_key(&id, 2)).unwrap().unwrap();
        assert_eq!(a2.value.metadata.owner_references, ["batches/lane/b2"]);
    }

    #[test]
    fn the_back_off_counts_transient_faults_in_a_row_only() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let id = ident(&[("sp-a", "1")]);
        let one_retry = FaultPolicy {
            backoff_secs: 10,
            retries: 1,
        };
        let base = attempt(Class::Fault { cause: Cause::Base }, 1);
        record_attempt(&p, &id, "b1", &base, &one_retry).unwrap();
        let r = record_attempt(&p, &id, "b1", &vm_fault(2), &one_retry).unwrap();
        assert_eq!(
            (r.n, r.summary.transient_faults, r.summary.backoff_until),
            (2, 1, Some(Backoff::Until(10_002)))
        );
        let r = record_attempt(&p, &id, "b1", &vm_fault(3), &one_retry).unwrap();
        assert_eq!(
            (r.summary.transient_faults, r.summary.backoff_until),
            (2, Some(Backoff::Standing))
        );
    }

    #[test]
    fn a_lane_fault_recorded_cleared_and_recorded_again_at_one_instant_stands_again() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let f = LaneFault {
            cause: Cause::Base,
            evidence: String::new(),
            since: 1,
        };
        for _ in 0..2 {
            assert_eq!(
                record_lane_fault(&p, "lane", "b0", "d1", &f),
                Ok(LaneFaultRecorded::New)
            );
            assert_eq!(w.keys("faults/").len(), 1);
            clear_lane_fault(&p, "lane", "b0", "d1").unwrap();
            assert!(w.keys("faults/").is_empty());
        }
    }

    #[test]
    fn proving_no_one_writes_nothing() {
        let w = World::new();
        let v = verbs();
        let p = w.ports(&v);
        let head = w.store.head().unwrap();
        mark_proven(&p, &ident(&[("sp-a", "1")]), &[], 1).unwrap();
        assert_eq!(w.store.head().unwrap(), head);
    }
}
