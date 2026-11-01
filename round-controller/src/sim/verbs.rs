//! Scripted `queue round` verbs. Each call takes the first scripted answer for its verb (and
//! repo, when the script names one) and is recorded with the queue lock hold the script says the
//! verb reported. `certify` is answered by the fake round VM unless a refusal is scripted.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::ports::{
    Child, Done, EjectReq, Ejected, Fields, OpenReq, Opened, Place, RoundVerbs, Status, Store,
    Superseded, Verb, VerbRefusal,
};
use crate::resources::{round_key, Round};
use crate::sim::git::FakeGit;
use crate::sim::vm::FakeVm;
use lifecycle::batch::BatchRow;
use state_store::{Object, Request, Txn};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Refuse {
    #[serde(default)]
    pub code: Option<i32>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbScript {
    pub verb: String,
    #[serde(default)]
    pub repo: Option<String>,
    /// Answers only a call about this batch.
    #[serde(default)]
    pub batch: Option<String>,
    #[serde(default)]
    pub ok: Option<Fields>,
    #[serde(default)]
    pub refuse: Option<Refuse>,
    #[serde(default)]
    pub lock_hold_ms: u64,
}

#[derive(Clone, Debug)]
struct Answer {
    verb: Verb,
    repo: Option<String>,
    batch: Option<String>,
    reply: Result<Fields, Refuse>,
    lock_hold_ms: u64,
}

impl VerbScript {
    fn answer(self) -> Result<Answer, String> {
        let verb = Verb::from_word(&self.verb)
            .ok_or_else(|| format!("verb {:?} is not a queue round verb", self.verb))?;
        let reply = match (self.ok, self.refuse) {
            (Some(_), _) if verb == Verb::Certify => {
                return Err(
                    "verb certify: its answer is the round VM's; script a [[vm]] or a refuse"
                        .into(),
                )
            }
            (Some(f), None) => Ok(f),
            (None, Some(r)) => Err(r),
            _ => {
                return Err(format!(
                    "verb {}: script exactly one of ok and refuse",
                    self.verb
                ))
            }
        };
        Ok(Answer {
            verb,
            repo: self.repo,
            batch: self.batch,
            reply,
            lock_hold_ms: self.lock_hold_ms,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub verb: Verb,
    pub repo: String,
    pub batch: Option<String>,
    /// Where an `open` asked for its round: `on-base` or `stack-on:<k>`.
    pub place: Option<String>,
    pub at: u64,
    pub lock_hold_ms: u64,
}

pub struct FakeVerbs {
    answers: Mutex<Vec<Answer>>,
    calls: Mutex<Vec<Call>>,
    faults: Mutex<Vec<String>>,
    heads: Mutex<BTreeMap<String, String>>,
    vm: Arc<FakeVm>,
    clock: VirtualClock,
    /// The queue's own writes: with a store, `open` writes the round it cut and `supersede`
    /// takes it out of the live set.
    store: Option<Arc<dyn Store>>,
    /// The repos' landing refs, which a `land` that answers ok moves to the round's head.
    git: Option<Arc<FakeGit>>,
}

impl FakeVerbs {
    pub fn new(
        scripts: Vec<VerbScript>,
        vm: Arc<FakeVm>,
        clock: VirtualClock,
    ) -> Result<FakeVerbs, String> {
        let answers = scripts
            .into_iter()
            .map(VerbScript::answer)
            .collect::<Result<_, _>>()?;
        Ok(FakeVerbs {
            answers: Mutex::new(answers),
            calls: Mutex::default(),
            faults: Mutex::default(),
            heads: Mutex::default(),
            vm,
            clock,
            store: None,
            git: None,
        })
    }

    /// Lets the fake queue write the rounds it opens and supersedes into `store`.
    pub fn with_store(mut self, store: Arc<dyn Store>) -> FakeVerbs {
        self.store = Some(store);
        self
    }

    /// Lets the fake queue's `land` move `git`'s landing ref to the round's head.
    pub fn with_git(mut self, git: Arc<FakeGit>) -> FakeVerbs {
        self.git = Some(git);
        self
    }

    /// The round `open` cut, as the queue records it: tested on the head of the round it is
    /// stacked on, or on the lane's landing ref, unless the script names its `tested_base`.
    /// SUPERSEDED is not a batch state yet (queue-stacking adds it), so a superseded round
    /// leaves the live set by its key going.
    fn wrote_open(&self, r: &OpenReq, o: &Opened, scripted: Option<&String>) {
        let Some(st) = &self.store else { return };
        let read = |key: String, field: &str| {
            st.get(&key)
                .ok()
                .flatten()
                .and_then(|i| i.value.data.get(field)?.as_str().map(str::to_string))
        };
        let (parent, base) = match &r.place {
            Place::OnBase => (None, read(crate::stack::lane_key(&r.repo), "landing_ref")),
            Place::StackOn(k) => (Some(k.clone()), read(round_key(&r.repo, k), "head")),
        };
        let tested_base = scripted.cloned().or(base);
        let mut row = BatchRow::cut(
            o.batch.clone(),
            r.repo.clone(),
            o.head.clone(),
            tested_base.clone().unwrap_or_default(),
        );
        row.parent = parent;
        let round = Round {
            row,
            members: r.members.clone(),
            tested_base,
            last_lease_token: None,
        };
        let key = round_key(&r.repo, &o.batch);
        let data = serde_json::to_value(&round).unwrap_or_default();
        put(&**st, &key, Txn::new().put(&key, Object::new(data)));
    }

    pub fn calls(&self) -> Vec<Call> {
        crate::lock(&self.calls).clone()
    }

    /// Calls nobody scripted an answer for.
    pub fn faults(&self) -> Vec<String> {
        crate::lock(&self.faults).clone()
    }

    fn next(
        &self,
        verb: Verb,
        repo: &str,
        batch: Option<&str>,
    ) -> Option<Result<Fields, VerbRefusal>> {
        let mut answers = crate::lock(&self.answers);
        let found = answers.iter().position(|a| {
            a.verb == verb
                && a.repo.as_deref().is_none_or(|r| r == repo)
                && a.batch.as_deref().is_none_or(|b| Some(b) == batch)
        });
        let a = found.map(|i| answers.remove(i));
        let call = Call {
            verb,
            repo: repo.into(),
            batch: batch.map(str::to_string),
            place: None,
            at: self.clock.now_ms(),
            lock_hold_ms: a.as_ref().map_or(0, |a| a.lock_hold_ms),
        };
        crate::lock(&self.calls).push(call);
        a.map(|a| {
            a.reply.map_err(|r| VerbRefusal {
                verb,
                code: r.code,
                text: r.text,
            })
        })
    }

    fn answer(&self, verb: Verb, repo: &str, batch: Option<&str>) -> Result<Fields, VerbRefusal> {
        self.next(verb, repo, batch).unwrap_or_else(|| {
            let what = format!(
                "unscripted call: queue round {} {repo} {}",
                verb.word(),
                batch.unwrap_or("-")
            );
            crate::lock(&self.faults).push(what.clone());
            Err(VerbRefusal {
                verb,
                code: None,
                text: what,
            })
        })
    }

    fn head(&self, batch: &str, head: &str) {
        crate::lock(&self.heads).insert(batch.into(), head.into());
    }

    fn done(&self, verb: Verb, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.answer(verb, repo, Some(batch)).map(|f| Done {
            said: f
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" "),
        })
    }
}

/// One fake-queue write; its request id is the key and the store's head, so each is new.
fn put(st: &dyn Store, key: &str, t: Txn) {
    let head = st
        .list(&[key], &state_store::Selector::Everything)
        .map_or(0, |(_, h)| h);
    let _ = st.txn(
        &t,
        &Request::new(&format!("fake-queue-{key}-{head}"), "queue", "fake"),
    );
}

impl RoundVerbs for FakeVerbs {
    fn open(&self, r: &OpenReq) -> Result<Opened, VerbRefusal> {
        let answered = self.answer(Verb::Open, &r.repo, None);
        if let Some(c) = crate::lock(&self.calls).last_mut() {
            c.place = Some(r.place.word());
        }
        let f = answered?;
        let o = Opened::from_fields(&f)?;
        self.head(&o.batch, &o.head);
        self.wrote_open(r, &o, f.get("tested_base"));
        Ok(o)
    }

    fn status(&self, repo: &str) -> Result<Status, VerbRefusal> {
        self.answer(Verb::Status, repo, None)
            .map(|f| Status::from_fields(&f))
    }

    /// The pass's identity is the batch's head: the merged tree its members and base make.
    fn certify(
        &self,
        repo: &str,
        batch: &str,
        _lease_token: Option<&str>,
    ) -> Result<Box<dyn Child>, VerbRefusal> {
        if let Some(Err(r)) = self.next(Verb::Certify, repo, Some(batch)) {
            return Err(r);
        }
        let identity = crate::lock(&self.heads)
            .get(batch)
            .cloned()
            .unwrap_or_else(|| batch.to_string());
        Ok(self.vm.start(&identity))
    }

    fn eject(&self, r: &EjectReq) -> Result<Ejected, VerbRefusal> {
        let e = Ejected::from_fields(&self.answer(Verb::Eject, &r.repo, Some(&r.batch))?)?;
        self.head(&r.batch, &e.head);
        Ok(e)
    }

    fn land(
        &self,
        repo: &str,
        batch: &str,
        _lease_token: Option<&str>,
    ) -> Result<Done, VerbRefusal> {
        let d = self.done(Verb::Land, repo, batch)?;
        let head = self.store.as_ref().and_then(|st| {
            let i = st.get(&round_key(repo, batch)).ok()??;
            i.value.data.get("head")?.as_str().map(str::to_string)
        });
        if let (Some(git), Some(head)) = (&self.git, head) {
            git.advance(repo, &head);
        }
        Ok(d)
    }

    fn abandon(&self, repo: &str, batch: &str, _reason: &str) -> Result<Done, VerbRefusal> {
        self.done(Verb::Abandon, repo, batch)
    }

    fn pass_start(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.done(Verb::PassStart, repo, batch)
    }

    fn supersede(
        &self,
        repo: &str,
        batch: &str,
        _lease_token: Option<&str>,
    ) -> Result<Superseded, VerbRefusal> {
        let s = Superseded::from_fields(&self.answer(Verb::Supersede, repo, Some(batch))?);
        if let Some(st) = &self.store {
            let key = round_key(repo, batch);
            put(&**st, &key, Txn::new().delete(&key));
            let lock = crate::lease::Resource::round(repo, batch);
            if let Ok(Some(l)) = st.get(&lock.lock_key()) {
                let fence = state_store::Fence {
                    resource: lock.lock,
                    token: l.create_rev,
                };
                let _ = st.release(
                    &fence,
                    &Request::new(&format!("fake-queue-revoke-{key}"), "queue", "fake"),
                );
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Member;
    use crate::sim::scenario::Profile;
    use crate::sim::vm::{VmScript, VmVerdict};

    fn script(text: &str) -> Vec<VerbScript> {
        #[derive(Deserialize)]
        struct S {
            verb: Vec<VerbScript>,
        }
        toml::from_str::<S>(text).unwrap().verb
    }

    fn fake(text: &str) -> Result<(FakeVerbs, Arc<FakeVm>, VirtualClock), String> {
        let clock = VirtualClock::new(5_000);
        let p = Profile {
            name: "p".into(),
            runner_slots: 2,
            vm_vcpus: 2,
            vm_mem_mib: 4096,
            duration_factor: 1.0,
            pass_cap_secs: 600,
            note: String::new(),
        };
        let vm = FakeVm::new(
            vec![VmScript::of("h1", VmVerdict::Green, 0)],
            p,
            clock.clone(),
        );
        Ok((
            FakeVerbs::new(script(text), vm.clone(), clock.clone())?,
            vm,
            clock,
        ))
    }

    fn open_req(repo: &str) -> OpenReq {
        OpenReq {
            repo: repo.into(),
            members: vec![Member {
                id: "sp-a".into(),
                tip: "1".into(),
            }],
            name: None,
            place: Place::OnBase,
        }
    }

    #[test]
    fn a_script_must_name_a_verb_and_exactly_one_answer() {
        let err = |t: &str| fake(t).err().unwrap();
        assert!(err("[[verb]]\nverb = \"preempt\"\nok = {}\n")
            .contains("\"preempt\" is not a queue round verb"));
        assert!(err("[[verb]]\nverb = \"land\"\n").contains("exactly one of ok and refuse"));
        assert!(
            err("[[verb]]\nverb = \"land\"\nok = {}\nrefuse = { text = \"x\" }\n")
                .contains("exactly one")
        );
        assert!(err("[[verb]]\nverb = \"certify\"\nok = {}\n").contains("round VM's"));
    }

    #[test]
    fn answers_are_taken_in_order_per_verb_and_repo_and_every_call_is_recorded_with_its_lock_hold()
    {
        let (v, _, _) = fake(concat!(
            "[[verb]]\nverb = \"open\"\nrepo = \"other\"\nok = { batch = \"b9\", head = \"h9\" }\n",
            "[[verb]]\nverb = \"open\"\nok = { batch = \"b1\", head = \"h1\" }\nlock_hold_ms = 40\n",
            "[[verb]]\nverb = \"land\"\nrefuse = { code = 1, text = \"stacked on b0\" }\n",
        ))
        .unwrap();
        assert_eq!(v.open(&open_req("r")).unwrap().batch, "b1");
        assert_eq!(
            v.land("r", "b1", None).unwrap_err(),
            VerbRefusal {
                verb: Verb::Land,
                code: Some(1),
                text: "stacked on b0".into()
            }
        );
        assert_eq!(v.open(&open_req("other")).unwrap().batch, "b9");
        let calls = v.calls();
        assert_eq!(
            calls[0],
            Call {
                verb: Verb::Open,
                repo: "r".into(),
                batch: None,
                place: Some("on-base".into()),
                at: 5_000,
                lock_hold_ms: 40
            }
        );
        assert_eq!(
            (
                calls[1].verb,
                calls[1].batch.as_deref(),
                calls[2].lock_hold_ms
            ),
            (Verb::Land, Some("b1"), 0)
        );
        assert!(v.faults().is_empty());
    }

    #[test]
    fn an_unscripted_call_is_refused_and_counted_as_a_fault() {
        let (v, _, _) = fake("verb = []\n").unwrap();
        let e = v.status("r").unwrap_err();
        assert_eq!((e.verb, e.code), (Verb::Status, None));
        assert_eq!(v.faults(), ["unscripted call: queue round status r -"]);
        assert_eq!(v.calls().len(), 1);
        assert!(v.open(&open_req("r")).is_err() && v.land("r", "b1", None).is_err());
        assert_eq!(v.faults().len(), 3);
    }

    #[test]
    fn a_refused_eject_and_one_answering_without_a_head_move_no_head() {
        let (v, _, _) = fake(concat!(
            "[[verb]]\nverb = \"eject\"\nrefuse = { code = 1, text = \"no step ci\" }\n",
            "[[verb]]\nverb = \"eject\"\nok = { members = \"sp-b:1\" }\n",
        ))
        .unwrap();
        let req = EjectReq {
            repo: "r".into(),
            batch: "b1".into(),
            id: "sp-a".into(),
            reason: "x".into(),
            suites: vec![],
            red: true,
        };
        assert_eq!(v.eject(&req).unwrap_err().code, Some(1));
        assert!(v.eject(&req).unwrap_err().text.contains("head="));
        assert!(crate::lock(&v.heads).is_empty());
    }

    #[test]
    fn every_verb_answers_typed_from_its_script() {
        let (v, _, _) = fake(concat!(
            "[[verb]]\nverb = \"status\"\nok = { open_batches = \"b1 b2\" }\n",
            "[[verb]]\nverb = \"eject\"\nok = { head = \"h2\", members = \"sp-b:1\" }\n",
            "[[verb]]\nverb = \"abandon\"\nok = { said = \"gone\" }\n",
            "[[verb]]\nverb = \"pass-start\"\nok = {}\n",
            "[[verb]]\nverb = \"open\"\nok = { batch = \"b1\" }\n",
        ))
        .unwrap();
        assert_eq!(v.status("r").unwrap().open, ["b1", "b2"]);
        let req = EjectReq {
            repo: "r".into(),
            batch: "b1".into(),
            id: "sp-a".into(),
            reason: "x".into(),
            suites: vec![],
            red: true,
        };
        assert_eq!(v.eject(&req).unwrap().head, "h2");
        assert_eq!(v.abandon("r", "b1", "why").unwrap().said, "said=gone");
        assert_eq!(v.pass_start("r", "b1").unwrap().said, "");
        assert!(
            v.open(&open_req("r")).unwrap_err().text.contains("head="),
            "a reply missing a key is refused"
        );
    }

    #[test]
    fn certify_runs_the_batch_head_on_the_vm_unless_a_refusal_is_scripted() {
        let (v, vm, _) = fake(concat!(
            "[[verb]]\nverb = \"open\"\nok = { batch = \"b1\", head = \"h1\" }\n",
            "[[verb]]\nverb = \"certify\"\nrepo = \"busy\"\nrefuse = { code = 1, text = \"leased by another holder\" }\n",
        ))
        .unwrap();
        v.open(&open_req("r")).unwrap();
        assert_eq!(
            v.certify("r", "b1", Some("1-0")).unwrap().poll(),
            Some(Ok(crate::ports::CertifyEnd::default()))
        );
        assert_eq!(
            v.certify("busy", "b1", None).err().unwrap().text,
            "leased by another holder"
        );
        assert!(
            v.certify("r", "b7", None).unwrap().poll().unwrap().is_err(),
            "an unknown batch runs under its own id, unscripted"
        );
        assert_eq!(
            vm.runs()
                .iter()
                .map(|r| r.identity.as_str())
                .collect::<Vec<_>>(),
            ["h1", "b7"]
        );
    }

    #[test]
    fn with_a_store_the_fake_queue_records_the_round_it_opened_and_forgets_the_one_it_superseded() {
        use state_store::{Config, Contract, MemBackend};
        let clock = VirtualClock::new(0);
        let store = Arc::new(
            Contract::open(
                MemBackend::new(),
                state_store::Ports::virtual_clock(&clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .unwrap(),
        );
        let (v, _, _) = fake(concat!(
            "[[verb]]\nverb = \"open\"\nok = { batch = \"k1\", head = \"h1\", tested_base = \"t0\" }\n",
            "[[verb]]\nverb = \"open\"\nok = { batch = \"k2\", head = \"h2\" }\n",
            "[[verb]]\nverb = \"supersede\"\nok = {}\n",
        ))
        .unwrap();
        let v = v.with_store(store.clone());
        v.open(&open_req("r")).unwrap();
        let mut on_k1 = open_req("r");
        on_k1.place = Place::StackOn("k1".into());
        v.open(&on_k1).unwrap();
        let read = |b: &str| crate::resources::RoundView::read(&*store, "r", b).unwrap();
        let k1 = read("k1").unwrap().round;
        assert_eq!(
            (k1.tested_base.as_deref(), k1.row.parent),
            (Some("t0"), None)
        );
        let k2 = read("k2").unwrap().round;
        assert_eq!(
            (k2.tested_base.as_deref(), k2.row.parent.as_deref()),
            (Some("h1"), Some("k1"))
        );
        assert_eq!(v.calls()[1].place.as_deref(), Some("stack-on:k1"));
        v.supersede("r", "k2", Some("7")).unwrap();
        assert!(read("k2").is_none());
        assert!(v.supersede("r", "k1", None).is_err(), "unscripted");
        assert!(read("k1").is_some(), "a refused supersede changes nothing");
    }

    #[test]
    fn a_script_naming_a_batch_answers_only_that_batch_and_an_ok_land_moves_the_ref_to_its_head() {
        use crate::sim::git::GitSeed;
        use state_store::{Config, Contract, MemBackend};
        let clock = VirtualClock::new(0);
        let store = Arc::new(
            Contract::open(
                MemBackend::new(),
                state_store::Ports::virtual_clock(&clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .unwrap(),
        );
        let git = Arc::new(FakeGit::default());
        git.seed(&GitSeed {
            repo: "r".into(),
            landing: "base-0".into(),
            history: vec![],
        });
        let (v, _, _) = fake(concat!(
            "[[verb]]\nverb = \"land\"\nbatch = \"c1\"\nrefuse = { code = 1, text = \"stacked on k1\" }\n",
            "[[verb]]\nverb = \"land\"\nbatch = \"k1\"\nok = {}\n",
        ))
        .unwrap();
        let v = v.with_store(store.clone()).with_git(git.clone());
        put(
            &*store,
            "batches/r/k1",
            Txn::new().put(
                "batches/r/k1",
                Object::new(serde_json::json!({ "head": "k1-h" })),
            ),
        );
        assert_eq!(v.land("r", "k1", Some("3-0")).unwrap().said, "");
        use crate::ports::Git;
        assert_eq!(git.landing_sha("r").unwrap(), "k1-h");
        assert_eq!(v.land("r", "c1", None).unwrap_err().text, "stacked on k1");
        assert_eq!(
            git.landing_sha("r").unwrap(),
            "k1-h",
            "a refused land moves nothing"
        );
        assert!(
            v.land("r", "k1", None).is_err(),
            "each answer is taken once"
        );
    }
}
