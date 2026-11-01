//! [`RoundVerbs`] on the real `queue round` verbs, spawned through [`Proc`] with payloads on
//! stdin. A verb takes the queue lock only for its own transition, so nothing here holds one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};

use crate::ports::{
    fields, Child, Done, EjectReq, Ejected, Fields, OpenReq, Opened, Place, RoundVerbs, Status,
    Superseded, Verb, VerbRefusal,
};
use crate::real::child::{Spawn, SpawnSpec};

pub struct Deadlines {
    /// Every transition verb. `certify` has none of its own: the runner's phase deadlines and
    /// pass wall end it.
    pub verb: Duration,
}

pub struct RealVerbs {
    proc_: Arc<dyn Proc>,
    spawn: Arc<dyn Spawn>,
    prog: String,
    deadlines: Deadlines,
    /// round-vm's run directory, where its progress file is, when the process can name one.
    run: Option<PathBuf>,
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

/// A verb's answer: its fields on exit 0, else a refusal carrying its stderr whole.
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
        code => Err(VerbRefusal {
            verb: c.verb,
            code,
            text: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

impl RealVerbs {
    pub fn new(
        proc_: Arc<dyn Proc>,
        spawn: Arc<dyn Spawn>,
        prog: &str,
        deadlines: Deadlines,
        run: Option<PathBuf>,
    ) -> RealVerbs {
        RealVerbs {
            proc_,
            spawn,
            prog: prog.to_string(),
            deadlines,
            run,
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

    /// Spawned in a process group of its own, so the runner's signal reaches round-vm.
    fn certify(
        &self,
        repo: &str,
        batch: &str,
        lease_token: Option<&str>,
    ) -> Result<Box<dyn Child>, VerbRefusal> {
        let mut c = Call::new(Verb::Certify, &[batch, repo]);
        if let Some(t) = lease_token {
            c.args.extend(["--lease-token".to_string(), t.to_string()]);
        }
        self.spawn
            .spawn(&SpawnSpec {
                verb: Some(Verb::Certify),
                prog: self.prog.clone(),
                args: c.args,
                progress: self.run.clone(),
                scope: Some(format!(
                    "round-controller-certify-{batch}-{}",
                    lease_token.unwrap_or("0-0")
                )),
                ..SpawnSpec::default()
            })
            .map_err(|text| VerbRefusal {
                verb: Verb::Certify,
                code: None,
                text,
            })
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

    fn land(
        &self,
        repo: &str,
        batch: &str,
        lease_token: Option<&str>,
    ) -> Result<Done, VerbRefusal> {
        let mut c = Call::new(Verb::Land, &[batch, repo]);
        if let Some(t) = lease_token {
            c.args.extend(["--lease-token".to_string(), t.to_string()]);
        }
        self.done(c)
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
    use std::sync::Mutex;

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
            Arc::new(Recorded(Mutex::default(), None)),
            "queue",
            Deadlines {
                verb: Duration::from_secs(120),
            },
            Some(PathBuf::from("/run/x")),
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
        let e = verbs(&p).land("r", "b1", None).unwrap_err();
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
        assert_eq!(v.land("r", "b1", None).unwrap().said, "open_batches=b1 x=1");
        v.abandon("r", "b1", "why").unwrap();
        v.pass_start("r", "b1").unwrap();
        v.land("r", "b1", Some("9-0")).unwrap();
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
        assert_eq!(
            seen[4].args,
            ["round", "land", "b1", "r", "--lease-token", "9-0"]
        );
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

    struct Recorded(Mutex<Vec<SpawnSpec>>, Option<String>);

    impl Spawn for Recorded {
        fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn Child>, String> {
            self.0.lock().unwrap().push(spec.clone());
            match &self.1 {
                Some(e) => Err(e.clone()),
                None => Err("recorded only".into()),
            }
        }
    }

    #[test]
    fn certify_spawns_the_batch_then_the_repo_with_the_lease_token_and_reads_progress_for_its_batch(
    ) {
        let rec = Arc::new(Recorded(Mutex::default(), None));
        let v = RealVerbs::new(
            fake(Some(0), "", ""),
            rec.clone(),
            "queue",
            Deadlines {
                verb: Duration::from_secs(120),
            },
            Some(PathBuf::from("/run/x")),
        );
        let _ = v.certify("r", "b1", Some("7-0"));
        let _ = v.certify("r", "b2", None);
        let seen = rec.0.lock().unwrap().clone();
        assert_eq!(seen[0].prog, "queue");
        assert_eq!(
            seen[0].args,
            ["round", "certify", "b1", "r", "--lease-token", "7-0"]
        );
        assert_eq!(seen[0].progress, Some(PathBuf::from("/run/x")));
        assert_eq!(
            seen[0].scope.as_deref(),
            Some("round-controller-certify-b1-7-0"),
            "its own scope, so a signal reaches round-vm whatever group it is in"
        );
        assert_eq!(seen[1].args, ["round", "certify", "b2", "r"]);
        let refused = RealVerbs::new(
            fake(Some(0), "", ""),
            Arc::new(Recorded(Mutex::default(), Some("no such program".into()))),
            "queue",
            Deadlines {
                verb: Duration::from_secs(120),
            },
            Some(PathBuf::from("/run/x")),
        )
        .certify("r", "b1", None)
        .err()
        .unwrap();
        assert_eq!(
            (refused.verb, refused.code, refused.text.as_str()),
            (Verb::Certify, None, "no such program")
        );
    }
}
