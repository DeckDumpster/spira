//! [`LcStore`]: the control-plane store through `spira-lc rc <verb> --json -`, the only door to it
//! (law-only-spira-lc-talks-to-the-stores). Calls and answers are `state_store::wire`'s.

use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use serde::de::DeserializeOwned;
use serde::Serialize;
use state_store::wire::{self, Verb};
use state_store::{Error, Fence, Item, Request, Selector, Txn, TxnResult};

use crate::ports::Store;

/// A store call answers in milliseconds; spira-lc's own socket client gives up at 5 s.
pub const RC_DEADLINE: Duration = Duration::from_secs(5);

pub struct LcStore {
    pub proc_: Arc<dyn Proc>,
    pub program: String,
}

impl LcStore {
    fn call<T: DeserializeOwned>(&self, verb: Verb, body: &impl Serialize) -> state_store::Result<T> {
        let what = format!("{} rc {}", self.program, verb.word());
        let stdin = serde_json::to_vec(body).map_err(|e| Error::Backend(format!("{what}: {e}")))?;
        let out = self
            .proc_
            .run(&ProcSpec {
                prog: self.program.clone(),
                args: vec!["rc".into(), verb.word().into(), "--json".into(), "-".into()],
                stdin,
                deadline: RC_DEADLINE,
            })
            .map_err(|e| Error::Backend(format!("{what}: {e}")))?;
        let Some(code) = out.code else {
            return Err(Error::Backend(format!(
                "{what} was killed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        };
        wire::decode(code, &String::from_utf8_lossy(&out.stdout)).map_err(|e| match e {
            Error::Backend(why) => Error::Backend(format!("{what}: {why}")),
            refusal => refusal,
        })
    }
}

impl Store for LcStore {
    fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
        self.call(Verb::Get, &wire::Get { key: key.into() })
    }
    fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
        let body = wire::List {
            prefixes: prefixes.iter().map(|p| p.to_string()).collect(),
            selector: sel.clone(),
        };
        self.call(Verb::List, &body)
            .map(|l: wire::Listed| (l.items, l.list_rev))
    }
    fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<TxnResult> {
        let body = wire::TxnCall {
            txn: t.clone(),
            request: req.clone(),
        };
        self.call(Verb::Txn, &body)
    }
    fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> state_store::Result<Fence> {
        let body = wire::Lock {
            resource: resource.into(),
            ttl_ms,
            request: req.clone(),
        };
        self.call(Verb::Lock, &body)
    }
    fn renew(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        let body = wire::Fenced {
            fence: f.clone(),
            request: req.clone(),
        };
        self.call(Verb::Renew, &body)
    }
    fn release(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        let body = wire::Fenced {
            fence: f.clone(),
            request: req.clone(),
        };
        self.call(Verb::Release, &body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use runtime::ports::ProcOutput;
    use state_store::{Contract, MemBackend, Object, Rev};

    use super::*;
    use crate::ledger::{FaultPolicy, StoreLedger};
    use crate::lease::{acquire, Resource};
    use crate::ports::{Attempt, Class, InitiatedBy, Ledger, Member, PassIdentity, Ports};
    use crate::sim::bench::Bench;

    /// spira-lc in-process: the argv and stdin `LcStore` sends, answered by `spira_lc::rc::run` on
    /// the bench's own contract.
    struct Loopback {
        store: Arc<Contract<MemBackend>>,
        seen: Mutex<Vec<ProcSpec>>,
    }

    impl Proc for Loopback {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.seen).push(spec.clone());
            let mut args = spec.args.clone();
            assert_eq!(args.last().map(String::as_str), Some("-"), "{args:?}");
            *args.last_mut().unwrap() = String::from_utf8(spec.stdin.clone()).unwrap();
            let (code, out) = spira_lc::rc::run(self.store.as_ref(), &args);
            Ok(ProcOutput {
                code: Some(code),
                stdout: out.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    fn door(b: &Bench) -> (LcStore, Arc<Loopback>) {
        let lb = Arc::new(Loopback {
            store: b.store.clone(),
            seen: Mutex::default(),
        });
        let lc = LcStore {
            proc_: lb.clone(),
            program: "spira-lc".into(),
        };
        (lc, lb)
    }

    fn identity() -> PassIdentity {
        PassIdentity {
            repo: "r".into(),
            members: vec![Member {
                id: "sp-a".into(),
                tip: "t1".into(),
            }],
            base: "b0".into(),
            image_digest: "d1".into(),
            harness_version: "h".into(),
            selection: "all".into(),
        }
    }

    /// One round lease's life and one verdict, against `p`: what each call answered.
    fn script(p: &Ports) -> Vec<String> {
        let res = Resource {
            lock: "round/r/b1".into(),
            record: None,
        };
        let lease = acquire(p, &res, "probe").unwrap();
        let token = lease.token();
        let renewed = lease.renew(p);
        let ledger = StoreLedger {
            policy: FaultPolicy {
                backoff_secs: 1,
                retries: 1,
            },
        };
        let a = Attempt {
            class: Class::Green,
            initiated_by: InitiatedBy::Planner,
            sub_passes: vec![],
            evidence: "e".into(),
            at: 5,
        };
        let n = ledger.record_attempt(p, &identity(), "b1", &a);
        let released = lease.release(p);
        vec![
            format!("{token:?}"),
            format!("{renewed:?}"),
            format!("{n:?}"),
            format!("{released:?}"),
        ]
    }

    fn everything(b: &Bench) -> Vec<Item> {
        b.store.list(&[""], &Selector::Everything).unwrap().0
    }

    #[test]
    fn a_lease_and_a_verdict_through_the_door_match_the_bare_contract() {
        let bare = Bench::new();
        let (bare_answers, bare_state) = (script(&bare.ports()), everything(&bare));
        let b = Bench::new();
        let (lc, lb) = door(&b);
        let p = Ports {
            store: &lc,
            ..b.ports()
        };
        assert_eq!(script(&p), bare_answers);
        assert_eq!(everything(&b), bare_state);
        let seen = crate::lock(&lb.seen);
        assert!(seen.iter().all(|s| s.prog == "spira-lc" && s.deadline == RC_DEADLINE));
        assert!(seen.iter().any(|s| s.args[..2] == ["rc", "txn"]));
        assert!(seen.iter().any(|s| s.args[..2] == ["rc", "renew"]));
        assert!(seen.iter().any(|s| s.args[..2] == ["rc", "release"]));
    }

    #[test]
    fn a_retried_txn_with_the_same_request_id_answers_already_applied() {
        let b = Bench::new();
        let (lc, _) = door(&b);
        let t = Txn::new().put("batches/r/b1", Object::new(serde_json::json!({ "n": 1 })));
        let req = Request::new("rq-retry", "round-controller", "test");
        let first = lc.txn(&t, &req).unwrap();
        let again = lc.txn(&t, &req).unwrap();
        assert!(first.succeeded && !first.already_applied);
        assert!(again.already_applied && again.succeeded);
        assert_eq!(again.rev, first.rev);
    }

    #[test]
    fn a_locked_refusal_decodes_as_locked() {
        let b = Bench::new();
        let (lc, _) = door(&b);
        let req = |id: &str| Request::new(id, "round-controller", "test");
        let held = lc.lock("round/r/b1", 5000, &req("l1")).unwrap();
        match lc.lock("round/r/b1", 5000, &req("l2")) {
            Err(Error::Locked { resource, token }) => {
                assert_eq!((resource.as_str(), token), ("round/r/b1", held.token))
            }
            other => panic!("{other:?}"),
        }
        let lost = Fence {
            resource: "round/r/b1".into(),
            token: Rev::new(held.token.main + 100, 0),
        };
        assert!(matches!(lc.renew(&lost, &req("n")), Err(Error::LockLost { .. })));
    }

    struct Answers(Mutex<Vec<Result<ProcOutput, String>>>);

    impl Proc for Answers {
        fn run(&self, _: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.0).remove(0)
        }
    }

    #[test]
    fn a_timeout_a_kill_and_cannot_tell_are_backend_carrying_the_text() {
        let out = |code: Option<i32>, stdout: &str, stderr: &str| {
            Ok(ProcOutput {
                code,
                stdout: stdout.into(),
                stderr: stderr.into(),
            })
        };
        let lc = LcStore {
            proc_: Arc::new(Answers(Mutex::new(vec![
                Err("spira-lc exceeded its 5s deadline".into()),
                out(None, "", "killed by signal 9"),
                out(Some(2), "{\"cannot_tell\":\"cannot tell: SPIRA_LC_STORE_PATH is unset\"}", ""),
                out(Some(0), "garbage", ""),
            ]))),
            program: "spira-lc".into(),
        };
        for want in ["exceeded its 5s deadline", "killed by signal 9", "SPIRA_LC_STORE_PATH", "garbage"] {
            match lc.get("batches/r/b1") {
                Err(Error::Backend(why)) => {
                    assert!(why.contains(want) && why.contains("spira-lc rc get"), "{why}")
                }
                other => panic!("{want}: {other:?}"),
            }
        }
    }

    #[test]
    fn every_key_the_controller_writes_is_under_a_prefix_the_door_admits() {
        use crate::slots::{Pool, Slot};
        let admitted = |k: &str| spira_lc::rc::WRITE_PREFIXES.iter().any(|p| k.starts_with(p));
        let id = identity();
        let slot = Slot {
            pool: Pool::Proxmox,
            n: 0,
        };
        let keys = [
            crate::resources::round_key("r", "b1"),
            crate::land::status_key("r", "b1"),
            crate::orphans::record_key("r", "b1"),
            crate::ledger::summary_key(&id),
            crate::ledger::attempt_key(&id, 1),
            crate::ledger::by_member_key("r", &id.members[0]),
            crate::ledger::fault_key("r", "b0", "d1"),
            crate::stack::lane_key("r"),
            slot.record_key(),
            format!("locks/{}", slot.lock()),
            Resource::round("r", "b1").lock_key(),
            crate::refusal::standing_key("r", "land"),
        ];
        for k in keys {
            assert!(admitted(&k), "{k}");
        }
        assert!(!admitted("lifecycle/sp-a") && !admitted("locks/lifecycle/sp-a"));
    }
}
