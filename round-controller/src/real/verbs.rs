//! [`RoundVerbs`] on the real `queue round` verbs, spawned through [`Proc`] with payloads on
//! stdin. A verb takes the queue lock only for its own transition, so nothing here holds one.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};

use crate::ports::{
    fields, Certified, Done, EjectReq, Ejected, Fields, OpenReq, Opened, Pending, Place,
    RoundVerbs, Status, Superseded, Verb, VerbRefusal,
};

pub struct Deadlines {
    /// Every transition verb.
    pub verb: Duration,
    /// `certify`, which runs the round VM's whole pass.
    pub certify: Duration,
}

pub struct RealVerbs {
    proc_: Arc<dyn Proc>,
    prog: String,
    deadlines: Deadlines,
}

struct Call {
    verb: Verb,
    args: Vec<String>,
    stdin: Vec<u8>,
}

impl Call {
    fn new(verb: Verb, args: &[&str]) -> Call {
        let mut all = vec!["round".to_string(), verb.word().to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        Call {
            verb,
            args: all,
            stdin: Vec::new(),
        }
    }

    fn stdin(mut self, flag: &str, payload: String) -> Call {
        self.args.extend([flag.to_string(), "-".to_string()]);
        self.stdin = payload.into_bytes();
        self
    }
}

/// A verb's answer: its fields on exit 0, else a refusal carrying its stderr whole. `red=` on
/// stdout is certify's verdict, not a refusal, whatever the exit.
fn answer(
    proc_: &dyn Proc,
    prog: &str,
    deadline: Duration,
    c: Call,
) -> Result<Fields, VerbRefusal> {
    let spec = ProcSpec {
        prog: prog.to_string(),
        args: c.args,
        stdin: c.stdin,
        deadline,
    };
    let out = proc_.run(&spec).map_err(|text| VerbRefusal {
        verb: c.verb,
        code: None,
        text,
    })?;
    let f = fields(&String::from_utf8_lossy(&out.stdout));
    match out.code {
        Some(0) => Ok(f),
        Some(_) if c.verb == Verb::Certify && f.contains_key("red") => Ok(f),
        code => Err(VerbRefusal {
            verb: c.verb,
            code,
            text: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

impl RealVerbs {
    pub fn new(proc_: Arc<dyn Proc>, prog: &str, deadlines: Deadlines) -> RealVerbs {
        RealVerbs {
            proc_,
            prog: prog.to_string(),
            deadlines,
        }
    }

    fn call(&self, c: Call) -> Result<Fields, VerbRefusal> {
        answer(&*self.proc_, &self.prog, self.deadlines.verb, c)
    }

    fn done(&self, c: Call) -> Result<Done, VerbRefusal> {
        self.call(c).map(|f| Done {
            said: f
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" "),
        })
    }
}

/// A certify running on its own thread; `poll` hands back its answer once it has one.
struct Spawned {
    rx: Mutex<Receiver<Result<Certified, VerbRefusal>>>,
    got: Mutex<Option<Result<Certified, VerbRefusal>>>,
}

fn lost() -> VerbRefusal {
    VerbRefusal {
        verb: Verb::Certify,
        code: None,
        text: "the certify thread ended without an answer".into(),
    }
}

impl Pending<Certified> for Spawned {
    fn poll(&self) -> Option<Result<Certified, VerbRefusal>> {
        let mut got = crate::lock(&self.got);
        if got.is_none() {
            *got = match crate::lock(&self.rx).try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(lost())),
            };
        }
        got.clone()
    }
}

impl RoundVerbs for RealVerbs {
    fn open(&self, r: &OpenReq) -> Result<Opened, VerbRefusal> {
        let mut c = Call::new(Verb::Open, &[&r.repo]);
        if let Some(n) = &r.name {
            c.args.extend(["--name".to_string(), n.clone()]);
        }
        match &r.place {
            Place::OnBase => c.args.push("--on-base".into()),
            Place::StackOn(k) => c.args.extend(["--stack-on".to_string(), k.clone()]),
        }
        let members: String = r
            .members
            .iter()
            .map(|m| format!("{}:{}\n", m.id, m.tip))
            .collect();
        Opened::from_fields(&self.call(c.stdin("--members-file", members))?)
    }

    fn status(&self, repo: &str) -> Result<Status, VerbRefusal> {
        self.call(Call::new(Verb::Status, &[repo]))
            .map(|f| Status::from_fields(&f))
    }

    fn certify(&self, repo: &str, batch: &str) -> Result<Box<dyn Pending<Certified>>, VerbRefusal> {
        let (tx, rx) = mpsc::channel();
        let (proc_, prog, deadline) = (
            self.proc_.clone(),
            self.prog.clone(),
            self.deadlines.certify,
        );
        let c = Call::new(Verb::Certify, &[batch, repo]);
        std::thread::spawn(move || {
            let _ = tx
                .send(answer(&*proc_, &prog, deadline, c).and_then(|f| Certified::from_fields(&f)));
        });
        Ok(Box::new(Spawned {
            rx: Mutex::new(rx),
            got: Mutex::new(None),
        }))
    }

    fn eject(&self, r: &EjectReq) -> Result<Ejected, VerbRefusal> {
        let suites = r.suites.join(",");
        let mut c = Call::new(
            Verb::Eject,
            &[&r.batch, &r.id, &r.repo, "--suites", &suites],
        );
        if r.red {
            c.args.push("--red".into());
        }
        Ejected::from_fields(&self.call(c.stdin("--reason-file", r.reason.clone()))?)
    }

    fn land(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.done(Call::new(Verb::Land, &[batch, repo]))
    }

    fn abandon(&self, repo: &str, batch: &str, reason: &str) -> Result<Done, VerbRefusal> {
        self.done(
            Call::new(Verb::Abandon, &[batch, repo]).stdin("--reason-file", reason.to_string()),
        )
    }

    fn pass_start(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal> {
        self.done(Call::new(Verb::PassStart, &[batch, repo]))
    }

    fn supersede(
        &self,
        repo: &str,
        batch: &str,
        lease_token: Option<&str>,
    ) -> Result<Superseded, VerbRefusal> {
        let mut c = Call::new(Verb::Supersede, &[batch, repo]);
        if let Some(t) = lease_token {
            c.args.extend(["--lease-token".to_string(), t.to_string()]);
        }
        self.call(c).map(|f| Superseded::from_fields(&f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::ProcOutput;

    /// Answers every spawn with one scripted output and records what it was asked to run.
    struct FakeProc {
        answer: Result<ProcOutput, String>,
        seen: Mutex<Vec<ProcSpec>>,
    }

    fn fake(code: Option<i32>, stdout: &str, stderr: &str) -> Arc<FakeProc> {
        Arc::new(FakeProc {
            answer: Ok(ProcOutput {
                code,
                stdout: stdout.into(),
                stderr: stderr.into(),
            }),
            seen: Mutex::default(),
        })
    }

    impl Proc for FakeProc {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            self.seen.lock().unwrap().push(spec.clone());
            self.answer.clone()
        }
    }

    fn verbs(p: &Arc<FakeProc>) -> RealVerbs {
        RealVerbs::new(
            p.clone(),
            "queue",
            Deadlines {
                verb: Duration::from_secs(120),
                certify: Duration::from_secs(1020),
            },
        )
    }

    fn eject_req() -> EjectReq {
        EjectReq {
            repo: "r".into(),
            batch: "b1".into(),
            id: "sp-a".into(),
            reason: "proven by a removal set".into(),
            suites: vec!["test-a.sh".into(), "testenv".into()],
            red: true,
        }
    }

    #[test]
    fn a_non_zero_eject_is_a_refusal_with_stderr_whole() {
        let p = fake(
            Some(1),
            "",
            "queue.sh round eject: no step \"ci\" in this round\nrefused\n",
        );
        let e = verbs(&p).eject(&eject_req()).unwrap_err();
        assert_eq!(
            e,
            VerbRefusal {
                verb: Verb::Eject,
                code: Some(1),
                text: "queue.sh round eject: no step \"ci\" in this round\nrefused\n".into()
            }
        );
        let spec = p.seen.lock().unwrap()[0].clone();
        assert_eq!(spec.prog, "queue");
        assert_eq!(
            spec.args,
            [
                "round",
                "eject",
                "b1",
                "sp-a",
                "r",
                "--suites",
                "test-a.sh,testenv",
                "--red",
                "--reason-file",
                "-"
            ]
        );
        assert_eq!(
            (spec.stdin, spec.deadline),
            (
                b"proven by a removal set".to_vec(),
                Duration::from_secs(120)
            )
        );
    }

    #[test]
    fn open_and_eject_refuse_a_reply_without_its_head_and_open_passes_a_refusal_on() {
        let req = OpenReq {
            repo: "r".into(),
            members: vec![],
            name: None,
            place: Place::OnBase,
        };
        assert!(verbs(&fake(Some(0), "batch=b1\n", ""))
            .open(&req)
            .unwrap_err()
            .text
            .contains("head="));
        assert_eq!(
            verbs(&fake(Some(3), "", "locked"))
                .open(&req)
                .unwrap_err()
                .code,
            Some(3)
        );
        assert!(verbs(&fake(Some(0), "members=a\n", ""))
            .eject(&eject_req())
            .unwrap_err()
            .text
            .contains("head="));
    }

    #[test]
    fn a_zero_eject_is_typed_from_its_fields() {
        let p = fake(
            Some(0),
            "queue.sh round eject: ejected sp-a\nhead=h2\nmembers=sp-b:2\n",
            "",
        );
        let mut r = eject_req();
        r.red = false;
        assert_eq!(
            verbs(&p).eject(&r).unwrap(),
            Ejected {
                head: "h2".into(),
                members: "sp-b:2".into()
            }
        );
        assert!(!p.seen.lock().unwrap()[0]
            .args
            .contains(&"--red".to_string()));
    }

    #[test]
    fn a_verb_that_never_ran_is_a_refusal_with_no_code() {
        let p = Arc::new(FakeProc {
            answer: Err("queue exceeded its 120s deadline".into()),
            seen: Mutex::default(),
        });
        let e = verbs(&p).land("r", "b1").unwrap_err();
        assert_eq!(
            (e.verb, e.code, e.text.as_str()),
            (Verb::Land, None, "queue exceeded its 120s deadline")
        );
    }

    #[test]
    fn a_verb_killed_by_a_signal_is_a_refusal_with_no_code() {
        let e = verbs(&fake(None, "", "killed"))
            .pass_start("r", "b1")
            .unwrap_err();
        assert_eq!((e.code, e.text.as_str()), (None, "killed"));
    }

    #[test]
    fn open_sends_its_members_on_stdin_and_reads_the_batch() {
        let p = fake(Some(0), "batch=b1\nhead=h1\nmembers=sp-a:1\n", "");
        let req = OpenReq {
            repo: "r".into(),
            members: vec![crate::ports::Member {
                id: "sp-a".into(),
                tip: "1".into(),
            }],
            name: Some("n1".into()),
            place: Place::OnBase,
        };
        assert_eq!(verbs(&p).open(&req).unwrap().batch, "b1");
        let spec = p.seen.lock().unwrap()[0].clone();
        assert_eq!(
            spec.args,
            [
                "round",
                "open",
                "r",
                "--name",
                "n1",
                "--on-base",
                "--members-file",
                "-"
            ]
        );
        assert_eq!(spec.stdin, b"sp-a:1\n");
        let stacked = OpenReq {
            name: None,
            place: Place::StackOn("k1".into()),
            ..req
        };
        verbs(&p).open(&stacked).unwrap();
        assert_eq!(
            p.seen.lock().unwrap()[1].args,
            [
                "round",
                "open",
                "r",
                "--stack-on",
                "k1",
                "--members-file",
                "-"
            ]
        );
    }

    #[test]
    fn status_land_abandon_and_pass_start_name_the_batch_then_the_repo() {
        let p = fake(Some(0), "open_batches=b1\nx=1\n", "");
        let v = verbs(&p);
        assert_eq!(v.status("r").unwrap().open, ["b1"]);
        assert_eq!(v.land("r", "b1").unwrap().said, "open_batches=b1 x=1");
        v.abandon("r", "b1", "why").unwrap();
        v.pass_start("r", "b1").unwrap();
        let seen = p.seen.lock().unwrap();
        assert_eq!(seen[0].args, ["round", "status", "r"]);
        assert_eq!(seen[1].args, ["round", "land", "b1", "r"]);
        assert_eq!(
            (seen[2].args.clone(), seen[2].stdin.clone()),
            (
                vec!["round", "abandon", "b1", "r", "--reason-file", "-"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                b"why".to_vec()
            )
        );
        assert_eq!(seen[3].args, ["round", "pass-start", "b1", "r"]);
    }

    #[test]
    fn supersede_names_the_batch_then_the_repo_and_passes_the_lease_token_when_it_has_one() {
        let p = fake(Some(0), "released=sp-a:1 sp-b:2\n", "");
        let v = verbs(&p);
        assert_eq!(
            v.supersede("r", "b2", Some("41")).unwrap().released,
            "sp-a:1 sp-b:2"
        );
        v.supersede("r", "b3", None).unwrap();
        let seen = p.seen.lock().unwrap();
        assert_eq!(
            seen[0].args,
            ["round", "supersede", "b2", "r", "--lease-token", "41"]
        );
        assert_eq!(seen[1].args, ["round", "supersede", "b3", "r"]);
        let e = verbs(&fake(Some(1), "", "leased by another holder"))
            .supersede("r", "b2", None)
            .unwrap_err();
        assert_eq!((e.verb, e.code), (Verb::Supersede, Some(1)));
    }

    /// Polls until the certify thread answers; the fake proc answers at once.
    fn settled(h: &dyn Pending<Certified>) -> Result<Certified, VerbRefusal> {
        for _ in 0..1_000_000 {
            if let Some(r) = h.poll() {
                return r;
            }
            std::thread::yield_now();
        }
        panic!("the certify thread never answered");
    }

    fn certified(
        p: &Arc<FakeProc>,
    ) -> (
        Result<Certified, VerbRefusal>,
        Option<Result<Certified, VerbRefusal>>,
    ) {
        let h = verbs(p).certify("r", "b1").unwrap();
        (settled(&*h), h.poll())
    }

    #[test]
    fn certify_is_a_pollable_handle_that_keeps_its_answer() {
        let p = fake(Some(0), "", "");
        let (r, again) = certified(&p);
        assert_eq!(r, Ok(Certified::Green));
        assert_eq!(again, Some(Ok(Certified::Green)));
        let spec = p.seen.lock().unwrap()[0].clone();
        assert_eq!(
            (spec.args, spec.deadline),
            (
                vec![
                    "round".to_string(),
                    "certify".into(),
                    "b1".into(),
                    "r".into()
                ],
                Duration::from_secs(1020)
            )
        );
    }

    #[test]
    fn a_red_certify_is_a_verdict_whatever_its_exit_and_any_other_failure_a_refusal() {
        let (r, _) = certified(&fake(
            Some(1),
            "queue.sh round certify: round b1 is RED\nred=test-a.sh\n",
            "RED",
        ));
        assert_eq!(
            r,
            Ok(Certified::Red {
                suites: vec!["test-a.sh".into()]
            })
        );
        let (r, _) = certified(&fake(Some(2), "", "fault"));
        assert_eq!(r.unwrap_err().code, Some(2));
        let (r, _) = certified(&fake(None, "red=test-a.sh\n", "killed"));
        assert_eq!(
            r.unwrap_err().code,
            None,
            "a killed certify is no verdict, whatever it printed"
        );
        let (r, _) = certified(&fake(Some(0), "red=\n", ""));
        assert!(r.unwrap_err().text.contains("naming no suite"));
    }

    #[test]
    fn a_certify_whose_thread_dies_answers_once_that_it_was_lost() {
        let (tx, rx) = mpsc::channel::<Result<Certified, VerbRefusal>>();
        let s = Spawned {
            rx: Mutex::new(rx),
            got: Mutex::new(None),
        };
        assert_eq!(s.poll(), None);
        drop(tx);
        assert_eq!(s.poll().unwrap().unwrap_err().text, lost().text);
        assert_eq!(
            s.poll().unwrap().unwrap_err().text,
            lost().text,
            "the loss is kept, not re-read"
        );
    }
}
