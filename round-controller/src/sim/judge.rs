//! The Judge in a scenario: a `[judge]` table names one round and its finished pass, scripts
//! each sub-pass's result by the members its tree leaves out, seeds the verdicts the ledger
//! already holds, and says at which harness passes the Judge is driven (a drive may be killed
//! after a number of launches). A fake ledger records what the Judge writes, for counting.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use batcher::attrib::{Budget, Job, JobResult, Purpose};
use batcher::core::Member as RoundMember;
use batcher::drive::{MainKind, MainPoll, RoundRunner};
use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::judge::{self, Blocker, Judged, PassResult, Round};
use crate::ports::{Attempt, Class, InitiatedBy, LaneFault, Ledger, Member, PassIdentity, Ports};
use crate::sim::scenario::Profile;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSpec {
    pub id: String,
    pub tip: String,
    /// The round members this one is stacked on.
    #[serde(default)]
    pub stack: Vec<String>,
    #[serde(default)]
    pub changed: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSpec {
    pub exit: i32,
    #[serde(default)]
    pub red: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub output: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    /// The members the sub-pass's tree leaves out: none for the rerun, all for a base run.
    #[serde(default)]
    pub removal: Vec<String>,
    /// One answer per launch of this removal set, in order: `green`, `red` or `fault`.
    pub results: Vec<String>,
    pub secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldSpec {
    pub removal: Vec<String>,
    pub class: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveSpec {
    /// The harness pass (from 1) after which the Judge runs.
    pub pass: u32,
    /// The runner refuses every launch after this many in this drive: it was killed.
    #[serde(default)]
    pub kill_after: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSpec {
    pub repo: String,
    pub batch: String,
    #[serde(default = "one")]
    pub round_pass: u32,
    pub base: String,
    pub image_digest: String,
    #[serde(default = "harness")]
    pub harness_version: String,
    #[serde(default = "all")]
    pub selection: String,
    #[serde(default)]
    pub green_digests: Vec<String>,
    pub member: Vec<MemberSpec>,
    pub result: ResultSpec,
    #[serde(default)]
    pub job: Vec<JobSpec>,
    #[serde(default)]
    pub held: Vec<HeldSpec>,
    pub drive: Vec<DriveSpec>,
}

fn one() -> u32 {
    1
}
fn harness() -> String {
    "harness".into()
}
fn all() -> String {
    "all".into()
}

fn job_result(w: &str) -> Option<JobResult> {
    match w {
        "green" => Some(JobResult::Green),
        "red" => Some(JobResult::Red),
        "fault" => Some(JobResult::Fault),
        _ => None,
    }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v
}

impl JudgeSpec {
    pub fn check(&self) -> Result<(), String> {
        let ids: Vec<&str> = self.member.iter().map(|m| m.id.as_str()).collect();
        let known = |m: &String| ids.contains(&m.as_str());
        for j in &self.job {
            if let Some(m) = j.removal.iter().find(|m| !known(m)) {
                return Err(format!("judge.job removes {m}, which is not a member"));
            }
            if let Some(w) = j.results.iter().find(|w| job_result(w).is_none()) {
                return Err(format!("judge.job result {w:?} is not green, red or fault"));
            }
        }
        for h in &self.held {
            if !["green", "red"].contains(&h.class.as_str()) {
                return Err(format!(
                    "judge.held class {:?} is not green or red",
                    h.class
                ));
            }
        }
        Ok(())
    }

    pub fn round(&self, profile: &Profile) -> Round {
        let members = self
            .member
            .iter()
            .map(|m| RoundMember {
                id: m.id.clone(),
                tip: m.tip.clone(),
                title: String::new(),
                priority: None,
                express: false,
                base_fix: false,
                certified_at: 0,
                stack: m.stack.iter().map(|p| (p.clone(), String::new())).collect(),
                blocked_by: vec![],
            })
            .collect();
        Round {
            repo: self.repo.clone(),
            batch: self.batch.clone(),
            pass: self.round_pass,
            members,
            base: self.base.clone(),
            image_digest: self.image_digest.clone(),
            harness_version: self.harness_version.clone(),
            selection: self.selection.clone(),
            initiated_by: InitiatedBy::Planner,
            budget: Budget {
                slots: profile.runner_slots as u32,
                maxpar: 1,
            },
            changed: self
                .member
                .iter()
                .map(|m| (m.id.clone(), m.changed.clone()))
                .collect(),
        }
    }

    fn pass_result(&self) -> PassResult {
        let r = &self.result;
        PassResult {
            exit: r.exit,
            red: r.red.clone(),
            reason: r.reason.clone(),
            phase: r.phase.clone(),
            output: r.output.clone(),
        }
    }
}

#[derive(Default)]
struct LedgerState {
    attempts: Vec<(PassIdentity, String, Attempt)>,
    proven: Vec<(PassIdentity, Member)>,
    lane_faults: Vec<(String, String, String, LaneFault)>,
    green_digests: BTreeMap<(String, String), Vec<String>>,
}

/// The ledger as the Judge sees it, in memory.
#[derive(Default)]
pub struct FakeLedger(Mutex<LedgerState>);

impl FakeLedger {
    /// Seeded with the scenario's held verdicts (as planner attempts) and green digests.
    pub fn seeded(spec: Option<&JudgeSpec>, profile: &Profile) -> FakeLedger {
        let l = FakeLedger::default();
        let Some(spec) = spec else { return l };
        let round = spec.round(profile);
        let mut st = crate::lock(&l.0);
        st.green_digests.insert(
            (spec.repo.clone(), spec.base.clone()),
            spec.green_digests.clone(),
        );
        for h in &spec.held {
            let class = match h.class.as_str() {
                "green" => Class::Green,
                _ => Class::Red {
                    step: spec.result.red.join(","),
                },
            };
            st.attempts.push((
                round.identity(&h.removal),
                "seed".into(),
                Attempt {
                    class,
                    initiated_by: InitiatedBy::Planner,
                    sub_passes: vec![],
                    evidence: "seeded".into(),
                    at: 0,
                },
            ));
        }
        drop(st);
        l
    }

    pub fn attempts(&self) -> Vec<(PassIdentity, String, Attempt)> {
        crate::lock(&self.0).attempts.clone()
    }
}

impl Ledger for FakeLedger {
    fn held(&self, _: &Ports, id: &PassIdentity) -> Result<Option<Class>, String> {
        let st = crate::lock(&self.0);
        Ok(st
            .attempts
            .iter()
            .rev()
            .find(|(i, _, _)| i == id)
            .map(|(_, _, a)| a.class.clone()))
    }
    fn record_attempt(
        &self,
        _: &Ports,
        id: &PassIdentity,
        batch: &str,
        a: &Attempt,
    ) -> Result<u32, String> {
        let mut st = crate::lock(&self.0);
        st.attempts.push((id.clone(), batch.into(), a.clone()));
        Ok(st.attempts.iter().filter(|(i, _, _)| i == id).count() as u32)
    }
    fn mark_proven(
        &self,
        _: &Ports,
        id: &PassIdentity,
        proven: &[Member],
        _: u64,
    ) -> Result<(), String> {
        let mut st = crate::lock(&self.0);
        for m in proven {
            st.proven.push((id.clone(), m.clone()));
        }
        Ok(())
    }
    fn record_lane_fault(
        &self,
        _: &Ports,
        repo: &str,
        base: &str,
        image_digest: &str,
        f: &LaneFault,
    ) -> Result<(), String> {
        crate::lock(&self.0).lane_faults.push((
            repo.into(),
            base.into(),
            image_digest.into(),
            f.clone(),
        ));
        Ok(())
    }
    fn green_digests(&self, _: &Ports, repo: &str, base: &str) -> Result<Vec<String>, String> {
        Ok(crate::lock(&self.0)
            .green_digests
            .get(&(repo.to_string(), base.to_string()))
            .cloned()
            .unwrap_or_default())
    }
}

#[derive(Clone, Debug)]
struct Launched {
    id: u64,
    purpose: &'static str,
    ends: u64,
    result: JobResult,
    collected: bool,
}

#[derive(Default)]
struct Book {
    scripts: BTreeMap<Vec<String>, VecDeque<(JobResult, u64)>>,
    launched: Vec<Launched>,
    faults: Vec<String>,
}

/// The sub-pass runner: each launch takes the next scripted answer for its removal set, and
/// answers once its scaled virtual duration has passed. Scripts are shared across drives, so a
/// re-drive that ran a sub-pass again would find it unscripted.
struct FakeRunner<'a> {
    book: &'a Mutex<Book>,
    clock: VirtualClock,
    factor: f64,
    kill_after: Option<usize>,
    launches: usize,
}

fn purpose_word(p: &Purpose) -> &'static str {
    match p {
        Purpose::Plain => "plain",
        Purpose::Base => "base",
        Purpose::Without(_) => "without",
    }
}

impl RoundRunner for FakeRunner<'_> {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, sha: &str) {
        crate::lock(self.book)
            .faults
            .push(format!("the Judge moved the base to {sha}"));
    }
    fn start_main(&mut self, _: MainKind, _: &[String]) -> Result<(), String> {
        crate::lock(self.book)
            .faults
            .push("the Judge started a main run".into());
        Err("the Judge starts no main run".into())
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        Ok(MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, job: &Job) -> Result<(), String> {
        if self.kill_after.is_some_and(|k| self.launches >= k) {
            return Err("the runner was killed".into());
        }
        self.launches += 1;
        let now = self.clock.now_ms();
        let mut b = crate::lock(self.book);
        let key = sorted(&job.removal);
        let (result, secs) = match b.scripts.get_mut(&key).and_then(VecDeque::pop_front) {
            Some(a) => a,
            None => {
                b.faults.push(format!(
                    "unscripted sub-pass without {:?} ({})",
                    key,
                    purpose_word(&job.purpose)
                ));
                (JobResult::Fault, 0)
            }
        };
        let ms = (secs as f64 * self.factor * 1000.0).round() as u64;
        b.launched.push(Launched {
            id: job.id,
            purpose: purpose_word(&job.purpose),
            ends: now + ms,
            result,
            collected: false,
        });
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        let now = self.clock.now_ms();
        let mut b = crate::lock(self.book);
        b.launched
            .iter_mut()
            .filter(|l| !l.collected && l.ends <= now)
            .map(|l| {
                l.collected = true;
                (l.id, l.result)
            })
            .collect()
    }
    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }
    fn wait(&mut self) {
        self.clock.advance(1000);
    }
}

/// One scenario's Judge: its drives' answers and the runner's book, for counting.
pub struct JudgeRun {
    spec: JudgeSpec,
    round: Round,
    factor: f64,
    book: Mutex<Book>,
    pub judged: Mutex<Vec<Judged>>,
}

impl JudgeRun {
    pub fn new(spec: &JudgeSpec, profile: &Profile) -> JudgeRun {
        let mut book = Book::default();
        for j in &spec.job {
            let q = book.scripts.entry(sorted(&j.removal)).or_default();
            q.extend(
                j.results
                    .iter()
                    .filter_map(|w| job_result(w))
                    .map(|r| (r, j.secs)),
            );
        }
        JudgeRun {
            spec: spec.clone(),
            round: spec.round(profile),
            factor: profile.duration_factor,
            book: Mutex::new(book),
            judged: Mutex::default(),
        }
    }

    /// Every drive scheduled after harness pass `n`.
    pub fn drive(&self, p: &Ports, clock: &VirtualClock, n: u32) {
        for d in self.spec.drive.iter().filter(|d| d.pass == n) {
            let mut runner = FakeRunner {
                book: &self.book,
                clock: clock.clone(),
                factor: self.factor,
                kill_after: d.kill_after,
                launches: 0,
            };
            let j = judge::judge(p, &mut runner, &self.round, &self.spec.pass_result());
            crate::lock(&self.judged).push(j);
        }
    }

    /// What the harness itself refuses: an unscripted sub-pass, or one that finished and was
    /// never read.
    pub fn broken(&self, now: u64, at: &str) -> Vec<String> {
        let b = crate::lock(&self.book);
        let mut out: Vec<String> = b
            .faults
            .iter()
            .map(|f| format!("invariant no-unscripted-fault violated under {at}: {f}"))
            .collect();
        for l in b.launched.iter().filter(|l| !l.collected && l.ends <= now) {
            out.push(format!(
                "invariant every-sub-pass-read violated under {at}: {} sub-pass {} finished unread",
                l.purpose, l.id
            ));
        }
        out
    }

    pub fn count(&self, c: &JudgeCount, ledger: &FakeLedger) -> u64 {
        let attempts = ledger.attempts();
        let st = crate::lock(&ledger.0);
        let judged = crate::lock(&self.judged);
        let launched = &crate::lock(&self.book).launched;
        let repeats = |by: Option<InitiatedBy>| {
            let mut n: BTreeMap<&PassIdentity, u64> = BTreeMap::new();
            for (i, _, a) in attempts.iter().filter(|(_, b, _)| b != "seed") {
                if by.is_none_or(|by| a.initiated_by == by) {
                    *n.entry(i).or_default() += 1;
                }
            }
            n.values().map(|v| v - 1).sum::<u64>()
        };
        let live = attempts.iter().filter(|(_, b, _)| b != "seed");
        let n = match c {
            JudgeCount::Attempts(None) => live.count(),
            JudgeCount::Attempts(Some(w)) => live.filter(|(_, _, a)| a.class.word() == *w).count(),
            JudgeCount::JudgePasses => live
                .filter(|(_, _, a)| a.initiated_by == InitiatedBy::Judge)
                .count(),
            JudgeCount::PlannerRepeats => return repeats(Some(InitiatedBy::Planner)),
            JudgeCount::IdentityRepeats => return repeats(None),
            JudgeCount::LaneFaults(None) => st.lane_faults.len(),
            JudgeCount::LaneFaults(Some(w)) => st
                .lane_faults
                .iter()
                .filter(|(_, _, _, f)| f.cause.word() == *w)
                .count(),
            JudgeCount::LaneFaultsNaming(d) => st
                .lane_faults
                .iter()
                .filter(|(_, _, key, f)| {
                    key == d
                        && f.evidence.contains(d.as_str())
                        && f.cause == crate::ports::Cause::Image { digest: d.clone() }
                })
                .count(),
            JudgeCount::Proven => st.proven.len(),
            JudgeCount::Judged(w) => judged.iter().filter(|j| j.word() == *w).count(),
            JudgeCount::EjectRefused => judged
                .iter()
                .filter(|j| {
                    matches!(
                        j,
                        Judged::Held {
                            blocker: Blocker::EjectRefused { .. },
                            ..
                        }
                    )
                })
                .count(),
            JudgeCount::Launches(None) => launched.len(),
            JudgeCount::Launches(Some(w)) => launched.iter().filter(|l| l.purpose == *w).count(),
        };
        n as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JudgeCount {
    /// Attempts the Judge recorded; `attempts:<class>` of one class.
    Attempts(Option<&'static str>),
    /// Removal-set passes, recorded initiated by the Judge.
    JudgePasses,
    /// Planner attempts on an identity beyond its first.
    PlannerRepeats,
    /// Any attempt on an identity beyond its first.
    IdentityRepeats,
    LaneFaults(Option<&'static str>),
    /// `lane-faults-naming:<digest>`: Image faults keyed on that digest, naming it.
    LaneFaultsNaming(String),
    Proven,
    Judged(&'static str),
    EjectRefused,
    /// Sub-passes launched; `launches:<plain|base|without>` of one purpose.
    Launches(Option<&'static str>),
}

impl JudgeCount {
    pub fn parse(s: &str) -> Option<JudgeCount> {
        let (what, of) = s.split_once(':').map_or((s, None), |(w, o)| (w, Some(o)));
        let pick = |set: &[&'static str], w: &str| set.iter().find(|x| **x == w).copied();
        match (what, of) {
            ("attempts", None) => Some(JudgeCount::Attempts(None)),
            ("attempts", Some(w)) => {
                pick(&["green", "red", "fault", "flaky"], w).map(|w| JudgeCount::Attempts(Some(w)))
            }
            ("judge-passes", None) => Some(JudgeCount::JudgePasses),
            ("planner-repeats", None) => Some(JudgeCount::PlannerRepeats),
            ("identity-repeats", None) => Some(JudgeCount::IdentityRepeats),
            ("lane-faults", None) => Some(JudgeCount::LaneFaults(None)),
            ("lane-faults", Some(w)) => {
                pick(&["image", "base", "install"], w).map(|w| JudgeCount::LaneFaults(Some(w)))
            }
            ("lane-faults-naming", Some(d)) if !d.is_empty() => {
                Some(JudgeCount::LaneFaultsNaming(d.into()))
            }
            ("proven", None) => Some(JudgeCount::Proven),
            ("judged", Some(w)) => {
                pick(&["preempted", "recorded", "held", "unfinished"], w).map(JudgeCount::Judged)
            }
            ("held-eject-refused", None) => Some(JudgeCount::EjectRefused),
            ("launches", None) => Some(JudgeCount::Launches(None)),
            ("launches", Some(w)) => {
                pick(&["plain", "base", "without"], w).map(|w| JudgeCount::Launches(Some(w)))
            }
            _ => None,
        }
    }
}
