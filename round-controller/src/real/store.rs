//! [`LcStore`]: the control-plane store through `spira-lc rc <verb> --json <body>` over the one
//! door, [`LcClient`] (law-only-spira-lc-talks-to-the-stores). Calls and answers are
//! `state_store::wire`'s. The body rides on argv: the socket takes no stdin.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use spira_store::lc_call::LcClient;
use state_store::wire::{self, Verb};
use state_store::{Error, Fence, Item, Request, Selector, Txn, TxnResult};

use crate::ports::Store;

/// A store call answers in milliseconds; spira-lc's own socket client gives up at 5 s.
pub const RC_DEADLINE: Duration = Duration::from_secs(5);

pub struct LcStore {
    pub client: LcClient,
}

impl LcStore {
    fn call<T: DeserializeOwned>(
        &self,
        verb: Verb,
        body: &impl Serialize,
    ) -> state_store::Result<T> {
        let what = format!("spira-lc rc {}", verb.word());
        let body =
            serde_json::to_string(body).map_err(|e| Error::Backend(format!("{what}: {e}")))?;
        let out = self
            .client
            .within(RC_DEADLINE)
            .call(&["rc", verb.word(), "--json", &body])
            .map_err(|e| Error::Backend(format!("{what}: {e}")))?;
        if out.code < 0 {
            return Err(Error::Backend(format!(
                "{what} was killed: {}",
                out.err.trim()
            )));
        }
        wire::decode(out.code, &out.out).map_err(|e| match e {
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
            states: Vec::new(),
            limit: None,
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
    use std::sync::{Arc, Mutex};

    use spira_store::lc_call::{Reply, Transport};
    use state_store::{Contract, MemBackend, Object, Rev};

    use super::*;
    use crate::lease::{acquire, Resource};
    use crate::ledger::{FaultPolicy, StoreLedger};
    use crate::ports::{Attempt, Class, InitiatedBy, Ledger, Member, PassIdentity, Ports};
    use crate::sim::bench::Bench;

    /// spira-lc in-process: the argv `LcStore` sends, answered by `spira_lc::rc::run` on the
    /// bench's own contract.
    struct Loopback {
        store: Arc<Contract<MemBackend>>,
        seen: Mutex<Vec<(Vec<String>, Option<String>)>>,
    }

    impl Transport for Loopback {
        fn exchange(
            &self,
            argv: &[String],
            stdin: Option<&str>,
            _d: Duration,
        ) -> Result<Reply, String> {
            crate::lock(&self.seen).push((argv.to_vec(), stdin.map(str::to_string)));
            let (code, out) = spira_lc::rc::run(self.store.as_ref(), argv);
            Ok(Reply {
                code,
                out,
                err: String::new(),
            })
        }
    }

    fn door(b: &Bench) -> (LcStore, Arc<Loopback>) {
        let lb = Arc::new(Loopback {
            store: b.store.clone(),
            seen: Mutex::default(),
        });
        let lc = LcStore {
            client: LcClient::over(lb.clone(), true),
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
        assert!(
            seen.iter()
                .all(|(a, stdin)| stdin.is_none() && a[2] == "--json" && a[3].starts_with('{')),
            "the body rides on argv: {seen:?}"
        );
        assert!(seen.iter().any(|(a, _)| a[..2] == ["rc", "txn"]));
        assert!(seen.iter().any(|(a, _)| a[..2] == ["rc", "renew"]));
        assert!(seen.iter().any(|(a, _)| a[..2] == ["rc", "release"]));
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
        assert!(matches!(
            lc.renew(&lost, &req("n")),
            Err(Error::LockLost { .. })
        ));
    }

    struct Answers(Mutex<Vec<Result<Reply, String>>>);

    impl Transport for Answers {
        fn exchange(&self, _: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            crate::lock(&self.0).remove(0)
        }
    }

    #[test]
    fn a_timeout_a_kill_and_cannot_tell_are_backend_carrying_the_text() {
        let out = |code: i32, stdout: &str, stderr: &str| {
            Ok(Reply {
                code,
                out: stdout.into(),
                err: stderr.into(),
            })
        };
        let lc = LcStore {
            client: LcClient::over(
                Arc::new(Answers(Mutex::new(vec![
                    Err("spira-lc exceeded its 5s deadline".into()),
                    out(-1, "", "killed by signal 9"),
                    out(
                        2,
                        "{\"cannot_tell\":\"cannot tell: SPIRA_LC_STORE_PATH is unset\"}",
                        "",
                    ),
                    out(0, "garbage", ""),
                ]))),
                true,
            ),
        };
        for want in [
            "exceeded its 5s deadline",
            "killed by signal 9",
            "SPIRA_LC_STORE_PATH",
            "garbage",
        ] {
            match lc.get("batches/r/b1") {
                Err(Error::Backend(why)) => {
                    assert!(
                        why.contains(want) && why.contains("spira-lc rc get"),
                        "{why}"
                    )
                }
                other => panic!("{want}: {other:?}"),
            }
        }
    }

    #[test]
    fn every_key_the_controller_writes_is_under_a_prefix_the_door_admits() {
        use crate::slots::{Pool, Slot};
        let admitted = |k: &str| {
            spira_lc::rc::WRITE_PREFIXES
                .iter()
                .any(|p| k.starts_with(p))
        };
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

    /// The ledger through `spira-lc rc` on a store file, as the daemon serves it.
    mod on_sqlite {
        use super::*;
        use crate::ledger::{refuses, LedgerRefusal};
        use crate::resources::Cause;
        use runtime::ports::Clock;
        use state_store::{Config, Ports as StorePorts};
        use state_store_sqlite::{Options, SqliteBackend};

        struct File {
            _dir: testkit::TempDir,
            store: Arc<Contract<SqliteBackend>>,
        }

        impl Transport for File {
            fn exchange(
                &self,
                argv: &[String],
                _: Option<&str>,
                _: Duration,
            ) -> Result<Reply, String> {
                let (code, out) = spira_lc::rc::run(self.store.as_ref(), argv);
                Ok(Reply {
                    code,
                    out,
                    err: String::new(),
                })
            }
        }

        fn file(b: &Bench) -> (Arc<File>, LcStore) {
            let dir = testkit::TempDir::new("rc-ledger-sqlite");
            let backend = SqliteBackend::open(&dir.join("store.db"), Options::default()).unwrap();
            let store = Contract::open(
                backend,
                StorePorts::virtual_clock(&b.clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .unwrap();
            let f = Arc::new(File {
                _dir: dir,
                store: Arc::new(store),
            });
            let lc = LcStore {
                client: LcClient::over(f.clone(), true),
            };
            (f, lc)
        }

        fn attempt(class: Class, at: u64) -> Attempt {
            Attempt {
                class,
                initiated_by: InitiatedBy::Judge,
                sub_passes: vec![],
                evidence: String::new(),
                at,
            }
        }

        fn ledger() -> StoreLedger {
            StoreLedger {
                policy: FaultPolicy {
                    backoff_secs: 10,
                    retries: 2,
                },
            }
        }

        #[test]
        fn a_red_verdict_refuses_its_identity_and_survives_a_retry_of_the_same_call() {
            let b = Bench::new();
            let (_f, lc) = file(&b);
            let p = Ports {
                store: &lc,
                ..b.ports()
            };
            let a = attempt(
                Class::Red {
                    step: "build".into(),
                },
                7,
            );
            assert_eq!(ledger().record_attempt(&p, &identity(), "b1", &a), Ok(1));
            assert_eq!(ledger().record_attempt(&p, &identity(), "b1", &a), Ok(1));
            assert_eq!(
                refuses(&p, &identity()),
                Ok(Some(LedgerRefusal::RedIdentity {
                    hash: identity().hash(),
                    step: "build".into()
                }))
            );
            assert_eq!(
                ledger().held(&p, &identity()),
                Ok(Some(Class::Red {
                    step: "build".into()
                }))
            );
        }

        #[test]
        fn a_transient_fault_backs_the_identity_off_until_the_clock_passes_it() {
            let b = Bench::new();
            let (_f, lc) = file(&b);
            let p = Ports {
                store: &lc,
                ..b.ports()
            };
            let at = b.clock.now_ms();
            let a = attempt(Class::Fault { cause: Cause::Vm }, at);
            assert_eq!(ledger().record_attempt(&p, &identity(), "b1", &a), Ok(1));
            assert_eq!(
                refuses(&p, &identity()),
                Ok(Some(LedgerRefusal::BackingOff {
                    hash: identity().hash(),
                    until: Some(at + 10_000)
                }))
            );
            b.clock.advance(10_000);
            assert_eq!(refuses(&p, &identity()), Ok(None));
        }

        #[test]
        fn a_member_proven_red_refuses_a_new_identity_until_the_lifecycle_absolves_it() {
            let b = Bench::new();
            let (f, lc) = file(&b);
            let p = Ports {
                store: &lc,
                ..b.ports()
            };
            let red = identity();
            let a = attempt(
                Class::Red {
                    step: "test".into(),
                },
                7,
            );
            ledger().record_attempt(&p, &red, "b1", &a).unwrap();
            ledger().mark_proven(&p, &red, &red.members, 8).unwrap();
            let mut other = red.clone();
            other.selection = "some".into();
            assert!(matches!(
                refuses(&p, &other),
                Ok(Some(LedgerRefusal::RedMember { .. }))
            ));
            f.store
                .txn(
                    &Txn::new().put(
                        "lifecycle/sp-a",
                        Object::new(serde_json::json!({ "ejected_red_tip": "t2" })),
                    ),
                    &Request::new("absolve", "test", "seed"),
                )
                .unwrap();
            assert_eq!(refuses(&p, &other), Ok(None));
        }

        #[test]
        fn the_debug_verbs_print_the_ledgers_answer_for_a_fixture_round() {
            use crate::ledger_verbs::{parse_record, parse_refuses, record, refuses};
            let b = Bench::new();
            let (_f, lc) = file(&b);
            let p = Ports {
                store: &lc,
                ..b.ports()
            };
            let policy = FaultPolicy {
                backoff_secs: 10,
                retries: 2,
            };
            let id = [
                "--repo",
                "r",
                "--members",
                "sp-a@t1",
                "--base",
                "b0",
                "--image-digest",
                "d1",
                "--harness-version",
                "h",
                "--selection",
                "all",
            ];
            let hash = identity().hash();
            let with = |extra: &[&'static str]| [id.as_slice(), extra].concat();
            assert_eq!(
                refuses(&p, &parse_refuses(&id).unwrap()),
                Ok(format!("hash={hash}\nrefuses=none"))
            );
            let red = parse_record(&with(&["--batch", "b1", "--class", "red:build"])).unwrap();
            assert_eq!(
                record(&p, &policy, &red),
                Ok(format!("hash={hash}\nattempt=1\nclass=red\nbackoff=none"))
            );
            assert_eq!(
                refuses(&p, &parse_refuses(&id).unwrap()),
                Ok(format!("hash={hash}\nrefuses=red-identity step=build"))
            );
            let mut vm = id;
            vm[11] = "some";
            let at = b.clock.now_ms();
            let fault =
                parse_record(&[vm.as_slice(), &["--batch", "b2", "--class", "fault:vm"]].concat())
                    .unwrap();
            let want = format!(
                "hash={}\nattempt=1\nclass=fault\nbackoff=until={}",
                fault.identity.hash(),
                at + 10_000
            );
            assert_eq!(record(&p, &policy, &fault), Ok(want));
        }

        #[test]
        fn a_lane_fault_stands_on_the_file_and_clearing_it_admits_the_lane() {
            let b = Bench::new();
            let (_f, lc) = file(&b);
            let p = Ports {
                store: &lc,
                ..b.ports()
            };
            let fault = crate::ledger::LaneFault {
                cause: Cause::Base,
                evidence: "red".into(),
                since: 3,
            };
            ledger()
                .record_lane_fault(&p, "r", "b0", "d1", &fault)
                .unwrap();
            assert!(matches!(
                refuses(&p, &identity()),
                Ok(Some(LedgerRefusal::LaneFault { .. }))
            ));
            crate::ledger::clear_lane_fault(&p, "r", "b0", "d1").unwrap();
            assert_eq!(refuses(&p, &identity()), Ok(None));
        }
    }
}
