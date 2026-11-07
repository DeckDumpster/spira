//! The Judge: every finished pass gets exactly one recorded attempt, written once after typing.
//! A red's sub-passes (a plain rerun, then base runs) belong to that attempt; the Attributor's
//! removal sets are their own identities. A member is ejected only on evidence, and a refused
//! eject holds the round.

use std::collections::{BTreeMap, BTreeSet};

use batcher::attrib::{Attributor, Budget, Job, JobResult, Purpose, Shape};
use batcher::core::{Id, Member as RoundMember};
use batcher::drive::RoundRunner;
use serde::{Deserialize, Serialize};
use state_store::{Object, Request, Txn};

use crate::outcome::Outcome;
use crate::ports::{
    Attempt, Cause, Class, EjectReq, InitiatedBy, LaneFault, Member, PassIdentity, Ports, SubPass,
    SubPassKind, Verb,
};

/// certify's exits (`queue round certify`): the corpus could not be judged; `round preempt`
/// stopped the pass; the round VM died under it.
pub const FAULT: i32 = 4;
pub const PREEMPTED: i32 = 5;
pub const VM_LOST: i32 = 75;

/// The fault reasons certify reports for a pass cut off by a cap.
pub const OVER_CAP: &str = "over-cap";
pub const FENCES_CAP: &str = "fences-cap";
/// The setup phases a red can stop in that name the lane rather than the tree.
pub const IMAGE_SETUP: &str = "image-setup";
pub const INSTALL: &str = "install";
/// The fence that runs the image setup on the round's tree; certify attributes its red itself.
pub const PREFLIGHT_RED: &str = "fence-round-image-preflight";

const ACTOR: &str = "round-controller";
/// The Judge's own sub-pass job ids start here, clear of the Attributor's.
const JUDGE_JOB_BASE: u64 = 1 << 41;

/// What certify reported for one finished pass.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PassResult {
    pub exit: i32,
    /// The steps certify reported red, as it named them: `testenv`, `spira-lint`, a suite.
    pub red: Vec<String>,
    /// A fault's reason word.
    pub reason: Option<String>,
    /// The setup phase a red stopped in.
    pub phase: Option<String>,
    /// The red's output.
    pub output: String,
    /// The members certify itself ejected for the red: the recipe changers of a red image
    /// preflight, already out of the round.
    pub ejected: Vec<Id>,
}

/// The round being judged, and what its pass tested.
#[derive(Clone, Debug)]
pub struct Round {
    pub repo: String,
    pub batch: String,
    pub pass: u32,
    /// Round order, with their stacks.
    pub members: Vec<RoundMember>,
    pub base: String,
    pub image_digest: String,
    pub harness_version: String,
    pub selection: String,
    pub initiated_by: InitiatedBy,
    pub budget: Budget,
    /// Each member's changed paths against the base.
    pub changed: BTreeMap<Id, Vec<String>>,
}

impl Round {
    pub fn ids(&self) -> Vec<Id> {
        self.members.iter().map(|m| m.id.clone()).collect()
    }

    /// The identity of this round's tree without `removal`.
    pub fn identity(&self, removal: &[Id]) -> PassIdentity {
        let mut members: Vec<Member> = self
            .members
            .iter()
            .filter(|m| !removal.contains(&m.id))
            .map(|m| Member {
                id: m.id.clone(),
                tip: m.tip.clone(),
            })
            .collect();
        members.sort();
        PassIdentity {
            repo: self.repo.clone(),
            members,
            base: self.base.clone(),
            image_digest: self.image_digest.clone(),
            harness_version: self.harness_version.clone(),
            selection: self.selection.clone(),
        }
    }

    pub fn shape(&self) -> Shape {
        let ids = self.ids();
        let prereqs: BTreeMap<Id, Vec<Id>> = self
            .members
            .iter()
            .map(|m| {
                let on = m
                    .stack
                    .keys()
                    .filter(|p| ids.contains(p))
                    .cloned()
                    .collect();
                (m.id.clone(), on)
            })
            .collect();
        Shape::new(&ids, &prereqs)
    }

    pub fn progress_key(&self) -> String {
        format!("batches/{}/{}/progress", self.repo, self.batch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Typed {
    /// The verb that stopped the pass records it.
    Preempted,
    Green,
    Red {
        steps: Vec<String>,
    },
    Fault(Cause),
}

/// A pass's verdict from certify's report alone: a fault holds no verdict whatever it names
/// red, and a non-zero exit naming no red step is no verdict either.
pub fn type_pass(pass: &PassResult) -> Typed {
    match pass.exit {
        PREEMPTED => Typed::Preempted,
        VM_LOST => Typed::Fault(Cause::Vm),
        FAULT => Typed::Fault(match pass.reason.as_deref() {
            Some(OVER_CAP) => Cause::OverCap,
            Some(FENCES_CAP) => Cause::FencesCap,
            _ => Cause::Vm,
        }),
        _ if !pass.red.is_empty() => Typed::Red {
            steps: pass.red.clone(),
        },
        0 => Typed::Green,
        _ => Typed::Fault(Cause::Vm),
    }
}

/// A lane fault's cause, once two base runs were red. Image when the red stopped in image
/// setup or the base was green on another digest; Install when it stopped in install, which is
/// where the install bisection's first probe, the base, already answered; else Base.
pub fn type_lane_fault(round: &Round, pass: &PassResult, green_digests: &[String]) -> Cause {
    match pass.phase.as_deref() {
        Some(IMAGE_SETUP) => Cause::Image {
            digest: round.image_digest.clone(),
        },
        _ if green_digests.iter().any(|d| *d != round.image_digest) => Cause::Image {
            digest: round.image_digest.clone(),
        },
        Some(INSTALL) => Cause::Install,
        _ => Cause::Base,
    }
}

/// The members whose changed paths the red's output names, in round order.
pub fn reaching(output: &str, round: &Round) -> Vec<Id> {
    round
        .ids()
        .into_iter()
        .filter(|m| {
            round.changed.get(m).is_some_and(|paths| {
                paths
                    .iter()
                    .any(|p| !p.is_empty() && output.contains(p.as_str()))
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    EjectRefused {
        member: Id,
        suites: Vec<String>,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Judged {
    /// No attempt: the pass was preempted.
    Preempted,
    /// Attempt `n` holds `class`; `ejected` are the members whose eject returned `Ok`.
    Recorded {
        n: u32,
        class: Class,
        ejected: Vec<Id>,
    },
    /// Attempt `n` is recorded and the round waits on `blocker`; nothing else proceeds.
    /// `ejected` left before the refusal. Progress stays, so a re-drive once the blocker lifts
    /// records nothing again and retries only the ejects still owed.
    Held {
        n: u32,
        ejected: Vec<Id>,
        blocker: Blocker,
    },
    /// A sub-pass would not launch or the store refused: no attempt yet, and the progress so
    /// far stands for the re-drive.
    Unfinished { text: String },
}

impl Judged {
    pub fn word(&self) -> &'static str {
        match self {
            Judged::Preempted => "preempted",
            Judged::Recorded { .. } => "recorded",
            Judged::Held { .. } => "held",
            Judged::Unfinished { .. } => "unfinished",
        }
    }
}

/// The sub-pass results so far, at `batches/<repo>/<id>/progress`, in the order they ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub pass: u32,
    pub runs: Vec<Ran>,
    /// The attempt this judgement recorded, so a re-drive never records it twice.
    #[serde(default)]
    pub recorded: Option<u32>,
    /// The members whose eject returned `Ok`, so a re-drive never ejects them twice.
    #[serde(default)]
    pub ejected: Vec<Id>,
    /// The finished pass being judged, so a re-drive judges it without running it again.
    #[serde(default)]
    pub main: Option<PassResult>,
    /// The eject the verb refused and the judgement still waits on; gone once the progress is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<EjectHeld>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EjectHeld {
    pub member: Id,
    pub suites: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ran {
    /// The members the run's tree left out: none for the rerun, all of them for a base run.
    pub removal: Vec<Id>,
    pub result: String,
    /// The steps a red run found red, when the runner named them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<String>,
}

/// One sub-pass's answer: its result, and the steps it found red.
type Answer = (JobResult, Vec<String>);

pub fn result_word(r: JobResult) -> &'static str {
    match r {
        JobResult::Green => "green",
        JobResult::Red => "red",
        JobResult::Fault => "fault",
    }
}

fn result_of(w: &str) -> JobResult {
    match w {
        "green" => JobResult::Green,
        "red" => JobResult::Red,
        _ => JobResult::Fault,
    }
}

pub fn judge(p: &Ports, runner: &mut impl RoundRunner, round: &Round, pass: &PassResult) -> Judged {
    let typed = type_pass(pass);
    let done = match typed {
        Typed::Preempted => return Judged::Preempted,
        Typed::Green => record(p, round, Class::Green, vec![], String::new()),
        Typed::Fault(cause) => record(p, round, Class::Fault { cause }, vec![], String::new()),
        Typed::Red { steps } => judge_red(p, runner, round, pass, &steps),
    };
    done.unwrap_or_else(|text| Judged::Unfinished { text })
}

fn record(
    p: &Ports,
    round: &Round,
    class: Class,
    sub_passes: Vec<SubPass>,
    evidence: String,
) -> Result<Judged, String> {
    let n = attempt(p, round, &class, sub_passes, evidence)?;
    Ok(Judged::Recorded {
        n,
        class,
        ejected: vec![],
    })
}

fn attempt(
    p: &Ports,
    round: &Round,
    class: &Class,
    sub_passes: Vec<SubPass>,
    evidence: String,
) -> Result<u32, String> {
    let a = Attempt {
        class: class.clone(),
        initiated_by: round.initiated_by,
        sub_passes,
        evidence,
        at: p.clock.now_ms(),
    };
    p.ledger
        .record_attempt(p, &round.identity(&[]), &round.batch, &a)
}

/// One judgement's sub-passes: each result is saved to progress as it arrives, and a re-drive
/// answers each run from progress, in the order it ran, before it launches anything.
struct Subs<'a, 'p, R: RoundRunner> {
    p: &'a Ports<'p>,
    runner: &'a mut R,
    round: &'a Round,
    /// Every sub-pass runs the red steps, comma-separated, as `testenv --suites` takes a list.
    suite: String,
    progress: Progress,
    /// Per removal set, how many of its progress entries this judgement has used.
    used: BTreeMap<Vec<Id>, usize>,
    saved: bool,
    next_id: u64,
}

impl<'a, 'p, R: RoundRunner> Subs<'a, 'p, R> {
    fn open(
        p: &'a Ports<'p>,
        runner: &'a mut R,
        round: &'a Round,
        steps: &[String],
    ) -> Result<Self, String> {
        let stored = p
            .store
            .get(&round.progress_key())
            .map_err(|e| format!("progress: {e}"))?;
        let found: Option<Progress> = stored
            .map(|i| serde_json::from_value(i.value.data).map_err(|e| format!("progress: {e}")))
            .transpose()?;
        let saved = found.is_some();
        let progress = found.filter(|g| g.pass == round.pass).unwrap_or(Progress {
            pass: round.pass,
            ..Progress::default()
        });
        runner.set_members(&round.members);
        Ok(Subs {
            p,
            runner,
            round,
            suite: steps.join(","),
            progress,
            used: BTreeMap::new(),
            saved,
            next_id: JUDGE_JOB_BASE,
        })
    }

    fn recall(&mut self, removal: &[Id]) -> Option<Answer> {
        let k = self.used.get(removal).copied().unwrap_or(0);
        let r = self
            .progress
            .runs
            .iter()
            .filter(|r| r.removal == removal)
            .nth(k)
            .map(|r| (result_of(&r.result), r.red.clone()))?;
        self.used.insert(removal.to_vec(), k + 1);
        Some(r)
    }

    fn remember(&mut self, removal: &[Id], (r, red): &Answer) -> Result<(), String> {
        self.progress.runs.push(Ran {
            removal: removal.to_vec(),
            result: result_word(*r).into(),
            red: red.clone(),
        });
        *self.used.entry(removal.to_vec()).or_default() += 1;
        self.save()
    }

    fn save(&mut self) -> Result<(), String> {
        let data = serde_json::to_value(&self.progress).map_err(|e| format!("progress: {e}"))?;
        let rq = self.request(&format!(
            "{}-{}-{}-{}",
            self.progress.runs.len(),
            self.progress.recorded.unwrap_or(0),
            self.progress.ejected.len(),
            u8::from(self.progress.held.is_some())
        ));
        self.p
            .store
            .txn(
                &Txn::new().put(&self.round.progress_key(), Object::new(data)),
                &rq,
            )
            .map_err(|e| format!("progress: {e}"))?;
        self.saved = true;
        Ok(())
    }

    fn request(&self, what: &str) -> Request {
        Request::new(
            &format!(
                "judge-{}-{}-{}-progress-{what}",
                self.round.repo, self.round.batch, self.round.pass
            ),
            ACTOR,
            "judge-progress",
        )
    }

    fn clear(&mut self) -> Result<(), String> {
        if !self.saved {
            return Ok(());
        }
        self.p
            .store
            .txn(
                &Txn::new().delete(&self.round.progress_key()),
                &self.request("clear"),
            )
            .map_err(|e| format!("progress: {e}"))?;
        self.saved = false;
        Ok(())
    }

    fn launch(&mut self, removal: &[Id], purpose: Purpose) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let job = Job {
            id,
            suite: self.suite.clone(),
            removal: removal.to_vec(),
            purpose,
        };
        self.runner
            .launch(&job)
            .map_err(|e| format!("sub-pass without {:?} did not launch: {e}", job.removal))?;
        Ok(id)
    }

    /// One run on the round tree without `removal`, retried once if it faulted.
    fn sub(&mut self, removal: &[Id], purpose: Purpose) -> Result<Answer, String> {
        let a = self.once(removal, purpose.clone())?;
        if a.0 == JobResult::Fault {
            return self.once(removal, purpose);
        }
        Ok(a)
    }

    fn once(&mut self, removal: &[Id], purpose: Purpose) -> Result<Answer, String> {
        if let Some(a) = self.recall(removal) {
            return Ok(a);
        }
        let id = self.launch(removal, purpose)?;
        let r = loop {
            if let Some((_, r)) = self.runner.poll_jobs().into_iter().find(|(i, _)| *i == id) {
                break r;
            }
            self.runner.wait();
        };
        let a = (r, self.red_steps(id, r));
        self.remember(removal, &a)?;
        Ok(a)
    }

    /// The steps job `id` found red, when it answered red and the runner names them.
    fn red_steps(&mut self, id: u64, r: JobResult) -> Vec<String> {
        if r == JobResult::Red {
            self.runner.red_steps(id)
        } else {
            vec![]
        }
    }
}

/// `what: red (steps)`, naming the steps when the runner named them.
fn said(what: &str, (r, red): &Answer) -> String {
    if red.is_empty() {
        format!("{what}: {}", result_word(*r))
    } else {
        format!("{what}: {} ({})", result_word(*r), red.join(","))
    }
}

/// The red verdict, naming the steps the plain rerun found red when the runner named them,
/// else the steps certify reported.
fn red_class(steps: &[String], rerun: &Answer) -> Class {
    let named = if rerun.1.is_empty() { steps } else { &rerun.1 };
    Class::Red {
        step: named.join(","),
    }
}

fn note(subs: &mut Vec<SubPass>, evidence: &mut Vec<String>, kind: SubPassKind, a: &Answer) {
    let what = match kind {
        SubPassKind::Rerun => "plain rerun on the full tree",
        SubPassKind::Base => "base run",
    };
    evidence.push(said(what, a));
    let r = a.0;
    if r != JobResult::Fault {
        subs.push(SubPass {
            kind,
            green: r == JobResult::Green,
        });
    }
}

/// One member to eject: `owner` when the evidence names it, not when it leaves stacked on one.
struct Eject {
    member: Id,
    owner: bool,
    why: String,
}

fn judge_red<R: RoundRunner>(
    p: &Ports,
    runner: &mut R,
    round: &Round,
    pass: &PassResult,
    steps: &[String],
) -> Result<Judged, String> {
    if steps.iter().any(|st| st == PREFLIGHT_RED) && !pass.ejected.is_empty() {
        return certify_ejected(p, round, pass, steps);
    }
    let mut s = Subs::open(p, runner, round, steps)?;
    let (mut subs, mut evidence) = (vec![], vec![]);
    let rerun = s.sub(&[], Purpose::Plain)?;
    note(&mut subs, &mut evidence, SubPassKind::Rerun, &rerun);
    let red = red_class(steps, &rerun);
    match rerun.0 {
        JobResult::Green => return finish(&mut s, pass, Class::Flaky, subs, evidence, vec![]),
        JobResult::Fault => return finish(&mut s, pass, red, subs, evidence, vec![]),
        JobResult::Red => {}
    }
    let all = round.ids();
    let base = s.sub(&all, Purpose::Base)?;
    note(&mut subs, &mut evidence, SubPassKind::Base, &base);
    match base.0 {
        JobResult::Fault => return finish(&mut s, pass, red, subs, evidence, vec![]),
        JobResult::Red => {
            let again = s.sub(&all, Purpose::Base)?;
            note(&mut subs, &mut evidence, SubPassKind::Base, &again);
            return match again.0 {
                JobResult::Green => finish(&mut s, pass, Class::Flaky, subs, evidence, vec![]),
                JobResult::Fault => finish(&mut s, pass, red, subs, evidence, vec![]),
                JobResult::Red => {
                    let digests = p.ledger.green_digests(p, &round.repo, &round.base)?;
                    let cause = type_lane_fault(round, pass, &digests);
                    evidence.push(format!(
                        "two base runs red on {} at image {}: a {} lane fault",
                        round.base,
                        round.image_digest,
                        cause.word()
                    ));
                    let f = LaneFault {
                        cause: cause.clone(),
                        evidence: evidence.join("\n"),
                        since: p.clock.now_ms(),
                    };
                    p.ledger.record_lane_fault(
                        p,
                        &round.repo,
                        &round.base,
                        &round.image_digest,
                        &f,
                    )?;
                    finish(&mut s, pass, Class::Fault { cause }, subs, evidence, vec![])
                }
            };
        }
        JobResult::Green => {}
    }
    let ejects = removal_sets(&mut s, pass, &red, &mut evidence)?;
    finish(&mut s, pass, red, subs, evidence, ejects)
}

/// certify already ejected the recipe changers a red image preflight names: the red is theirs,
/// recorded once with no sub-pass, and each is proven.
fn certify_ejected(p: &Ports, round: &Round, pass: &PassResult, steps: &[String]) -> Result<Judged, String> {
    let class = Class::Red { step: steps.join(",") };
    let evidence = format!(
        "certify ejected {}: each changes a round-recipe input and {PREFLIGHT_RED} went red\n{}",
        pass.ejected.join(","),
        pass.output
    );
    let n = attempt(p, round, &class, vec![], evidence)?;
    let identity = round.identity(&[]);
    let proven: Vec<Member> = identity.members.iter().filter(|m| pass.ejected.contains(&m.id)).cloned().collect();
    p.ledger.mark_proven(p, &identity, &proven, p.clock.now_ms())?;
    Ok(Judged::Recorded { n, class, ejected: pass.ejected.clone() })
}

/// The Attributor's removal sets, the base run green: each set is its own identity, reused
/// when that identity already holds a green or red verdict, and recorded as a Judge pass when
/// it runs. The members to eject, each with its evidence.
fn removal_sets<R: RoundRunner>(
    s: &mut Subs<R>,
    pass: &PassResult,
    red: &Class,
    evidence: &mut Vec<String>,
) -> Result<Vec<Eject>, String> {
    let round = s.round;
    let all = round.ids();
    let shape = round.shape();
    let reach = reaching(&pass.output, round);
    let mut suspects = reach.clone();
    suspects.extend(all.iter().filter(|m| !reach.contains(m)).cloned());
    let mut a = Attributor::new(round.budget, shape.clone(), 1);
    let now = s.runner.now();
    a.on_main_result(&s.suite, false, &suspects, now);
    a.on_main_done(now);
    let mut running: BTreeMap<u64, Job> = BTreeMap::new();
    loop {
        let jobs = a.next_jobs();
        if jobs.is_empty() && running.is_empty() {
            break;
        }
        for job in jobs {
            let now = s.runner.now();
            if job.purpose == Purpose::Plain {
                a.on_job_result(job.id, JobResult::Red, now);
            } else if job.removal.len() == all.len() {
                a.on_job_result(job.id, JobResult::Green, now);
            } else if let Some((r, _)) = s.recall(&job.removal) {
                a.on_job_result(job.id, r, now);
            } else if let Some(r) = reused(s.p, &round.identity(&job.removal))? {
                evidence.push(format!(
                    "without {}: {} (its identity's verdict, reused)",
                    job.removal.join(","),
                    result_word(r)
                ));
                a.on_job_result(job.id, r, now);
            } else {
                let rid = s.launch(&job.removal, job.purpose.clone())?;
                running.insert(rid, job);
            }
        }
        let polled = if running.is_empty() {
            vec![]
        } else {
            s.runner.poll_jobs()
        };
        let mut arrived = false;
        for (rid, r) in polled {
            let Some(job) = running.remove(&rid) else {
                continue;
            };
            arrived = true;
            let answer = (r, s.red_steps(rid, r));
            let class = match r {
                JobResult::Green => Class::Green,
                JobResult::Red if !answer.1.is_empty() => Class::Red {
                    step: answer.1.join(","),
                },
                JobResult::Red => red.clone(),
                JobResult::Fault => Class::Fault { cause: Cause::Vm },
            };
            let a_ = Attempt {
                class,
                initiated_by: InitiatedBy::Judge,
                sub_passes: vec![],
                evidence: format!(
                    "removal set of round {} pass {}: without {}",
                    round.batch,
                    round.pass,
                    job.removal.join(",")
                ),
                at: s.p.clock.now_ms(),
            };
            s.p.ledger
                .record_attempt(s.p, &round.identity(&job.removal), &round.batch, &a_)?;
            s.remember(&job.removal, &answer)?;
            evidence.push(said(&format!("without {}", job.removal.join(",")), &answer));
            a.on_job_result(job.id, r, s.runner.now());
        }
        if !arrived && !running.is_empty() {
            s.runner.wait();
        }
    }
    let d = a.decision();
    let mut owners: Vec<(Id, String)> = all
        .iter()
        .filter(|m| d.owners.contains_key(*m))
        .map(|m| (m.clone(), format!("{m}: a run without it was green")))
        .collect();
    if owners.is_empty() {
        if let [one] = reach.as_slice() {
            owners.push((
                one.clone(),
                format!("{one}: the base run was green and only its diff reaches the failing code"),
            ));
        }
    }
    let mut out: Vec<Eject> = vec![];
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    for (owner, why) in owners {
        evidence.push(why.clone());
        let leaving = shape.closure.get(&owner).cloned().unwrap_or_default();
        for m in leaving {
            if seen.insert(m.clone()) {
                let is_owner = m == owner;
                let why = if is_owner {
                    why.clone()
                } else {
                    format!("{m}: stacked on {owner}, which leaves")
                };
                out.push(Eject {
                    member: m,
                    owner: is_owner,
                    why,
                });
            }
        }
    }
    Ok(out)
}

fn reused(p: &Ports, id: &PassIdentity) -> Result<Option<JobResult>, String> {
    Ok(match p.ledger.held(p, id)? {
        Some(Class::Green) => Some(JobResult::Green),
        Some(Class::Red { .. }) => Some(JobResult::Red),
        _ => None,
    })
}

/// Writes the one attempt, then ejects each member the evidence proves, marking it proven only
/// once its eject returned `Ok`. A refused eject holds the round and nothing after it runs.
/// Each step is saved to progress before the next, so a re-drive repeats none of them.
fn finish<R: RoundRunner>(
    s: &mut Subs<R>,
    pass: &PassResult,
    class: Class,
    sub_passes: Vec<SubPass>,
    evidence: Vec<String>,
    ejects: Vec<Eject>,
) -> Result<Judged, String> {
    let (p, round) = (s.p, s.round);
    let n = match s.progress.recorded {
        Some(n) => n,
        None => {
            let n = attempt(p, round, &class, sub_passes, evidence.join("\n"))?;
            if !ejects.is_empty() {
                s.progress.recorded = Some(n);
                s.save()?;
            }
            n
        }
    };
    let identity = round.identity(&[]);
    for e in ejects {
        if s.progress.ejected.contains(&e.member) {
            continue;
        }
        let req = EjectReq {
            repo: round.repo.clone(),
            batch: round.batch.clone(),
            id: e.member.clone(),
            reason: e.why,
            suites: pass.red.clone(),
            red: e.owner,
        };
        let r = p.verbs.eject(&req);
        p.log.write(&Outcome::of(
            p.clock.now_ms(),
            &round.repo,
            Some(&round.batch),
            Verb::Eject,
            &r,
        ));
        if let Err(refused) = r {
            s.progress.held = Some(EjectHeld {
                member: e.member.clone(),
                suites: pass.red.clone(),
                text: refused.text.clone(),
            });
            s.save()?;
            return Ok(Judged::Held {
                n,
                ejected: s.progress.ejected.clone(),
                blocker: Blocker::EjectRefused {
                    member: e.member,
                    suites: pass.red.clone(),
                    text: refused.text,
                },
            });
        }
        if e.owner {
            let m = identity
                .members
                .iter()
                .filter(|m| m.id == e.member)
                .cloned()
                .collect::<Vec<_>>();
            p.ledger.mark_proven(p, &identity, &m, p.clock.now_ms())?;
        }
        s.progress.ejected.push(e.member);
        s.progress.held = None;
        s.save()?;
    }
    let ejected = std::mem::take(&mut s.progress.ejected);
    s.clear()?;
    Ok(Judged::Recorded { n, class, ejected })
}

#[cfg(test)]
mod tests;
