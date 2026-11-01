//! Scripted `queue round` verbs. Each call takes the first scripted answer for its verb (and
//! repo, when the script names one) and is recorded with the queue lock hold the script says the
//! verb reported. `certify` is answered by the fake round VM unless a refusal is scripted.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::ports::{
    Certified, Done, EjectReq, Ejected, Fields, OpenReq, Opened, Pending, RoundVerbs, Status, Verb,
    VerbRefusal,
};
use crate::sim::vm::FakeVm;

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
        })
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Calls nobody scripted an answer for.
    pub fn faults(&self) -> Vec<String> {
        self.faults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn next(
        &self,
        verb: Verb,
        repo: &str,
        batch: Option<&str>,
    ) -> Option<Result<Fields, VerbRefusal>> {
        let mut answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        let found = answers
            .iter()
            .position(|a| a.verb == verb && a.repo.as_deref().is_none_or(|r| r == repo));
        let a = found.map(|i| answers.remove(i));
        let call = Call {
            verb,
            repo: repo.into(),
            batch: batch.map(str::to_string),
            at: self.clock.now_ms(),
            lock_hold_ms: a.as_ref().map_or(0, |a| a.lock_hold_ms),
        };
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
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
            self.faults
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(what.clone());
            Err(VerbRefusal {
                verb,
                code: None,
                text: what,
            })
        })
    }

    fn head(&self, batch: &str, head: &str) {
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(batch.into(), head.into());
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

impl RoundVerbs for FakeVerbs {
    fn open(&self, r: &OpenReq) -> Result<Opened, VerbRefusal> {
        let o = Opened::from_fields(&self.answer(Verb::Open, &r.repo, None)?)?;
        self.head(&o.batch, &o.head);
        Ok(o)
    }

    fn status(&self, repo: &str) -> Result<Status, VerbRefusal> {
        self.answer(Verb::Status, repo, None)
            .map(|f| Status::from_fields(&f))
    }

    /// The pass's identity is the batch's head: the merged tree its members and base make.
    fn certify(&self, repo: &str, batch: &str) -> Result<Box<dyn Pending<Certified>>, VerbRefusal> {
        if let Some(Err(r)) = self.next(Verb::Certify, repo, Some(batch)) {
            return Err(r);
        }
        let identity = self
            .heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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

    fn land(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.done(Verb::Land, repo, batch)
    }

    fn abandon(&self, repo: &str, batch: &str, _reason: &str) -> Result<Done, VerbRefusal> {
        self.done(Verb::Abandon, repo, batch)
    }

    fn pass_start(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.done(Verb::PassStart, repo, batch)
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
            vec![VmScript {
                identity: "h1".into(),
                verdict: VmVerdict::Green,
                suites: vec![],
                duration_secs: 0,
            }],
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
            v.land("r", "b1").unwrap_err(),
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
            v.certify("r", "b1").unwrap().poll(),
            Some(Ok(Certified::Green))
        );
        assert_eq!(
            v.certify("busy", "b1").err().unwrap().text,
            "leased by another holder"
        );
        assert!(
            v.certify("r", "b7").unwrap().poll().unwrap().is_err(),
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
}
