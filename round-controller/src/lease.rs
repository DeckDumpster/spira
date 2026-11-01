//! Leases over the contract's `lock`: the lock's `create_rev` is the fencing token, and every
//! write a holder makes is a `txn` that compares it, so a holder whose lease lapsed and was
//! re-taken has every write refused.

use serde_json::{json, Value};
use state_store::{Compare, Error, Fence, Op, Request, Rev, Txn, LOCKS_PREFIX};

use crate::ports::Ports;

/// The batch value's fields a round lease writes.
pub const LAST_TOKEN_FIELD: &str = "last_lease_token";
pub const HOLDER_FIELD: &str = "lease_holder";

/// What a lease locks, and the record its holder is written into, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The lock's resource: its key is `locks/<lock>`.
    pub lock: String,
    pub record: Option<String>,
}

impl Resource {
    /// `locks/round/<repo>/<batch>`, recorded in `batches/<repo>/<batch>`.
    pub fn round(repo: &str, batch: &str) -> Resource {
        Resource {
            lock: format!("round/{repo}/{batch}"),
            record: Some(format!("batches/{repo}/{batch}")),
        }
    }

    pub fn lock_key(&self) -> String {
        format!("{LOCKS_PREFIX}{}", self.lock)
    }
}

/// Another holder's live lease. `holder` is `None` when no record names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub holder: Option<String>,
    pub token: Rev,
    /// Advisory, from the lock.s expiry on read; nothing fences on it.
    pub ttl_left_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseError {
    Held(Held),
    /// The lease lapsed or was taken under another token.
    Lost,
    /// A terminal round is never leased.
    Terminal,
    Store(String),
}

#[derive(Debug)]
pub struct Lease {
    resource: Resource,
    fence: Fence,
}

fn request(p: &Ports, op: &str) -> Request {
    Request::new(
        &format!("rc-lease-{op}-{:016x}", p.next_u64()),
        "round-controller",
        &format!("lease.{op}"),
    )
}

fn store_err(e: Error) -> LeaseError {
    match e {
        Error::LockLost { .. } => LeaseError::Lost,
        e => LeaseError::Store(e.to_string()),
    }
}

pub fn token_json(t: Rev) -> Value {
    json!({ "main": t.main, "sub": t.sub })
}

pub fn token_of(v: &Value) -> Option<Rev> {
    Some(Rev::new(
        v.get("main")?.as_u64()?,
        u32::try_from(v.get("sub")?.as_u64()?).ok()?,
    ))
}

/// The record's holder, if it names `token` as its last lease.
fn recorded_holder(p: &Ports, res: &Resource, token: Rev) -> Option<String> {
    let item = p.store.get(res.record.as_deref()?).ok()??;
    let d = &item.value.data;
    (d.get(LAST_TOKEN_FIELD).and_then(token_of) == Some(token))
        .then(|| d.get(HOLDER_FIELD)?.as_str().map(str::to_string))
        .flatten()
}

const TRIES: usize = 3;

/// Takes `res` for `holder` for the configured TTL. With a record, the lock is taken in one txn
/// with the record's holder, compared on its `mod_rev`, and then the token is written fenced: a
/// txn cannot name the token it is minting. A crash between the two leaves the round with its old
/// token, so it reads as that lease's orphan, or as waiting when it had none.
pub fn acquire(p: &Ports, res: &Resource, holder: &str) -> Result<Lease, LeaseError> {
    let ttl = p.leases.ttl_ms;
    for _ in 0..TRIES {
        let mut t = Txn::new();
        if let Some(key) = &res.record {
            let item = read_record(p, key)?;
            let mut v = item.value.clone();
            object(&mut v.data, key)?.insert(HOLDER_FIELD.into(), json!(holder));
            t = t
                .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
                .put(key, v);
        }
        let r = p
            .store
            .txn(&t.lock_acquire(&res.lock, ttl), &request(p, "acquire"))
            .map_err(store_err)?;
        if let (true, Some(fence)) = (r.succeeded, r.fences.into_iter().next()) {
            let lease = Lease {
                resource: res.clone(),
                fence,
            };
            return match &res.record {
                Some(key) => match lease.record_token(p, key) {
                    Ok(()) => Ok(lease),
                    Err(e) => {
                        let _ = lease.release(p);
                        Err(e)
                    }
                },
                None => Ok(lease),
            };
        }
        if let Some(h) = holder_of(p, res).map_err(LeaseError::Store)? {
            return Err(LeaseError::Held(h));
        }
    }
    Err(LeaseError::Store(format!(
        "{} moved under {TRIES} tries to take the lease",
        res.record.as_deref().unwrap_or(&res.lock)
    )))
}

fn read_record(p: &Ports, key: &str) -> Result<state_store::Item, LeaseError> {
    let item = p
        .store
        .get(key)
        .map_err(store_err)?
        .ok_or_else(|| LeaseError::Store(format!("{key} does not exist")))?;
    let state = item.value.data.get("state").and_then(Value::as_str);
    if state.and_then(crate::orphans::terminal) == Some(true) {
        return Err(LeaseError::Terminal);
    }
    Ok(item)
}

fn object<'a>(
    v: &'a mut Value,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, LeaseError> {
    v.as_object_mut()
        .ok_or_else(|| LeaseError::Store(format!("{key} is not an object")))
}

/// Every write a holder of `lease` makes: `compares`, plus its fence, then `ops`.
pub fn fenced_txn(lease: &Lease, compares: Vec<Compare>, ops: Vec<Op>) -> Txn {
    Txn {
        compares,
        ops,
        else_ops: Vec::new(),
    }
    .lock_check(&lease.fence)
}

/// The live lease on `res`, if any.
pub fn holder_of(p: &Ports, res: &Resource) -> Result<Option<Held>, String> {
    let lock = p.store.get(&res.lock_key()).map_err(|e| e.to_string())?;
    let Some((lock, expires)) = lock.and_then(|l| l.lock_expires_at_ms.map(|e| (l, e))) else {
        return Ok(None);
    };
    Ok(Some(Held {
        holder: recorded_holder(p, res, lock.create_rev),
        token: lock.create_rev,
        ttl_left_ms: expires.saturating_sub(p.clock.now_ms()),
    }))
}

impl Lease {
    pub fn token(&self) -> Rev {
        self.fence.token
    }

    pub fn resource(&self) -> &Resource {
        &self.resource
    }

    pub fn renew(&self, p: &Ports) -> Result<(), LeaseError> {
        p.store
            .renew(&self.fence, &request(p, "renew"))
            .map_err(store_err)
    }

    pub fn release(self, p: &Ports) -> Result<(), LeaseError> {
        p.store
            .release(&self.fence, &request(p, "release"))
            .map_err(store_err)
    }

    /// The record's other fields are written by others, so the token is a CAS on its `mod_rev`,
    /// re-read when another writer won, and fenced, so a lapsed lease records nothing.
    fn record_token(&self, p: &Ports, key: &str) -> Result<(), LeaseError> {
        for _ in 0..TRIES {
            let item = read_record(p, key)?;
            let mut v = item.value.clone();
            object(&mut v.data, key)?.insert(LAST_TOKEN_FIELD.into(), token_json(self.token()));
            let t = fenced_txn(
                self,
                vec![Compare::ModRev(key.into(), Some(item.mod_rev))],
                vec![Op::Put(key.into(), v)],
            );
            if p.store
                .txn(&t, &request(p, "record"))
                .map_err(store_err)?
                .succeeded
            {
                return Ok(());
            }
            if holder_of(p, &self.resource)
                .map_err(LeaseError::Store)?
                .is_none_or(|h| h.token != self.token())
            {
                return Err(LeaseError::Lost);
            }
        }
        Err(LeaseError::Store(format!(
            "{key} moved under {TRIES} tries to record the lease"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::bench::Bench;
    use state_store::Object;

    fn seeded(b: &Bench) {
        b.seed(
            "batches/r/b1",
            json!({ "state": "CI_RUNNING", "members": "a:1" }),
        );
    }

    #[test]
    fn acquiring_a_round_lease_records_its_token_and_holder_in_the_batch() {
        let b = Bench::new();
        seeded(&b);
        let p = b.ports();
        let l = acquire(&p, &Resource::round("r", "b1"), "runner-a").unwrap();
        let v = b.store.get("batches/r/b1").unwrap().unwrap().value.data;
        assert_eq!(v["members"], "a:1", "the batch's other fields are kept");
        assert_eq!(token_of(&v[LAST_TOKEN_FIELD]), Some(l.token()));
        assert_eq!(v[HOLDER_FIELD], "runner-a");
        assert_eq!(
            holder_of(&p, l.resource()).unwrap(),
            Some(Held {
                holder: Some("runner-a".into()),
                token: l.token(),
                ttl_left_ms: p.leases.ttl_ms
            })
        );
    }

    #[test]
    fn a_second_acquire_is_held_naming_the_holder_and_the_token() {
        let b = Bench::new();
        seeded(&b);
        let p = b.ports();
        let res = Resource::round("r", "b1");
        let l = acquire(&p, &res, "runner-a").unwrap();
        assert_eq!(
            acquire(&p, &res, "runner-b").unwrap_err(),
            LeaseError::Held(Held {
                holder: Some("runner-a".into()),
                token: l.token(),
                ttl_left_ms: p.leases.ttl_ms
            })
        );
    }

    #[test]
    fn fencing_refuses_a_write_by_a_holder_whose_lease_expired_and_was_re_taken() {
        let b = Bench::new();
        seeded(&b);
        let p = b.ports();
        let res = Resource::round("r", "b1");
        let old = acquire(&p, &res, "runner-a").unwrap();
        let progress = |n: u64| {
            vec![Op::Put(
                "batches/r/b1/progress".into(),
                Object::new(json!({ "n": n })),
            )]
        };
        assert!(
            p.store
                .txn(&fenced_txn(&old, vec![], progress(1)), &request(&p, "w"))
                .unwrap()
                .succeeded
        );
        b.clock.advance(p.leases.ttl_ms + 1);
        let new = acquire(&p, &res, "runner-b").unwrap();
        assert_ne!(new.token(), old.token());
        let r = p
            .store
            .txn(&fenced_txn(&old, vec![], progress(2)), &request(&p, "w"))
            .unwrap();
        assert!(!r.succeeded, "the expired holder's write is refused");
        assert_eq!(old.renew(&p), Err(LeaseError::Lost));
        assert!(
            p.store
                .txn(&fenced_txn(&new, vec![], progress(3)), &request(&p, "w"))
                .unwrap()
                .succeeded
        );
        let n = b.store.get("batches/r/b1/progress").unwrap().unwrap();
        assert_eq!(n.value.data["n"], 3);
    }

    #[test]
    fn a_renewed_lease_outlives_its_first_ttl_and_a_released_one_is_free() {
        let b = Bench::new();
        seeded(&b);
        let p = b.ports();
        let res = Resource::round("r", "b1");
        let l = acquire(&p, &res, "runner-a").unwrap();
        for _ in 0..4 {
            b.clock.advance(p.leases.renew_every_ms());
            l.renew(&p).unwrap();
        }
        assert!(holder_of(&p, &res).unwrap().is_some());
        l.release(&p).unwrap();
        assert_eq!(holder_of(&p, &res).unwrap(), None);
        acquire(&p, &res, "runner-b").unwrap();
    }

    #[test]
    fn a_lease_on_a_missing_or_malformed_record_is_refused_and_its_lock_given_back() {
        let b = Bench::new();
        let p = b.ports();
        let res = Resource::round("r", "nope");
        assert_eq!(
            acquire(&p, &res, "a").unwrap_err(),
            LeaseError::Store("batches/r/nope does not exist".into())
        );
        assert_eq!(holder_of(&p, &res).unwrap(), None);
        b.seed("batches/r/list", json!([1, 2]));
        let res = Resource::round("r", "list");
        assert_eq!(
            acquire(&p, &res, "a").unwrap_err(),
            LeaseError::Store("batches/r/list is not an object".into())
        );
        assert_eq!(holder_of(&p, &res).unwrap(), None);
    }

    #[test]
    fn a_record_that_keeps_moving_is_refused_after_its_tries() {
        let b = Bench::new();
        seeded(&b);
        b.racing_writes("batches/r/b1");
        let p = b.ports();
        let res = Resource::round("r", "b1");
        assert_eq!(
            acquire(&p, &res, "a").unwrap_err(),
            LeaseError::Store("batches/r/b1 moved under 3 tries to take the lease".into())
        );
        assert_eq!(holder_of(&p, &res).unwrap(), None);
    }

    #[test]
    fn a_token_write_that_keeps_losing_its_cas_gives_the_lock_back() {
        let b = Bench::new();
        seeded(&b);
        b.racing_writes_after("batches/r/b1", 1);
        let p = b.ports();
        let res = Resource::round("r", "b1");
        assert_eq!(
            acquire(&p, &res, "a").unwrap_err(),
            LeaseError::Store("batches/r/b1 moved under 3 tries to record the lease".into())
        );
        assert_eq!(holder_of(&p, &res).unwrap(), None);
    }

    #[test]
    fn a_terminal_round_is_never_leased_and_a_lapsed_lock_reads_as_free_before_any_sweep() {
        let b = Bench::new();
        b.seed("batches/r/done", json!({ "state": "LANDED" }));
        let p = b.ports();
        assert_eq!(
            acquire(&p, &Resource::round("r", "done"), "a").unwrap_err(),
            LeaseError::Terminal
        );
        seeded(&b);
        let res = Resource::round("r", "b1");
        acquire(&p, &res, "a").unwrap();
        b.clock.advance(p.leases.ttl_ms - 1);
        assert_eq!(holder_of(&p, &res).unwrap().unwrap().ttl_left_ms, 1);
        b.clock.advance(1);
        assert_eq!(holder_of(&p, &res).unwrap(), None);
    }

    #[test]
    fn a_lease_without_a_record_names_no_holder_and_store_faults_are_named() {
        let b = Bench::new();
        let p = b.ports();
        let res = Resource {
            lock: "round/lease-probe/x".into(),
            record: None,
        };
        let l = acquire(&p, &res, "probe").unwrap();
        assert_eq!(
            acquire(&p, &res, "probe").unwrap_err(),
            LeaseError::Held(Held {
                holder: None,
                token: l.token(),
                ttl_left_ms: p.leases.ttl_ms
            })
        );
        let short = Bench::with_ttl_ms(10);
        let e = acquire(&short.ports(), &res, "probe").unwrap_err();
        assert!(
            matches!(e, LeaseError::Store(ref s) if s.contains("LockTtlBelowMinimum")),
            "{e:?}"
        );
        let down = Bench::unserved();
        let p = down.ports();
        assert!(matches!(acquire(&p, &res, "a"), Err(LeaseError::Store(_))));
        assert!(holder_of(&p, &res).is_err());
    }

    #[test]
    fn a_token_round_trips_through_json_and_a_malformed_one_is_none() {
        let t = Rev::new(42, 3);
        assert_eq!(token_of(&token_json(t)), Some(t));
        assert_eq!(token_of(&json!({ "main": 1 })), None);
        assert_eq!(token_of(&json!("42")), None);
    }
}
