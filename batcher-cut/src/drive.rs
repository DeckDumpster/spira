//! The attribution loop (DESIGN.md §4): stream the main run's results into the pure
//! `Attributor`, launch the reruns it hands out into spare slots, and — once every red is
//! settled — eject the owners, re-run only their suites on the survivors, and say whether
//! the round lands. Every effect goes through `RoundRunner` (suite runs) or `RoundOps`
//! (git, the bead store, incidents, TSD), so the whole loop runs against fakes in tests.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use batcher::attrib::{Attributor, Budget, Decision, Job, JobResult, Outcome, Purpose, Shape};
use batcher::core::{Id, Member};

/// What one poll of the main run returned: results that became final since the last poll,
/// and whether it has ended.
#[derive(Debug, Default)]
pub struct MainPoll {
    pub results: Vec<(String, bool)>,
    pub done: Option<MainEnd>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainEnd {
    /// The suites ran (green or red).
    Ran,
    /// The tree did not build (testenv rc 4): nothing to attribute suite by suite.
    WorkspaceBuild,
}

/// Which main run to start: the round's full corpus, or the survivors' verification of the
/// owners' suites (whose release build is the one that lands).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainKind {
    Corpus,
    Verify,
}

pub trait RoundRunner {
    /// The members whose merges make up the round tree from here on (round order).
    fn set_members(&mut self, members: &[Member]);
    /// The base the round tree (and every job tree) is built on, after the base moved.
    fn set_base(&mut self, sha: &str);
    fn start_main(&mut self, kind: MainKind, suites: &[String]) -> Result<(), String>;
    /// A harness fault of the main run is an error: the round is not judged.
    fn poll_main(&mut self) -> Result<MainPoll, String>;
    /// Build the job's tree (round tree minus `job.removal`) and submit suite `job.suite` on it.
    fn launch(&mut self, job: &Job) -> Result<(), String>;
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)>;
    /// Seconds.
    fn now(&self) -> u64;
    fn wait(&mut self);
}

/// Everything the round does besides running suites.
pub trait RoundOps {
    /// S's suspect order among `members` (covers-touching first).
    fn suspects(&self, suite: &str, members: &[Id]) -> Vec<Id>;
    /// `owner`: the member's own tip owns the red; a stacked dependent leaves as collateral.
    fn eject(&mut self, member: &Member, suites: &[String], owner: bool);
    /// Reset the round tree to the base and merge `survivors`; the members that actually
    /// merged (a fresh conflict drops one).
    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String>;
    /// Mechanically repair what the merged tree alone broke (a commit on the round tree naming
    /// every member); false when there was nothing to repair.
    fn fix_integration(&mut self, members: &[Member], suites: &[String]) -> bool;
    /// If the base moved since the round was cut: rebuild the round on the new base with
    /// `members` and return that base and the members that merged. `None` when it did not
    /// move — a retry would change no input.
    fn base_moved(&mut self, members: &[Member]) -> Result<Option<(String, Vec<Member>)>, String>;
    /// A red no single member reproduces, still red on a changed input: summon the Judge.
    fn judge(&mut self, suites: &[String], members: &[Id]);
    /// `kind`: `base`, `unattributed`, `integration` or `workspace-build`.
    fn incident(&mut self, kind: &str, suites: &[String]);
    /// The members whose diff touches `suite`.
    fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id>;
    /// Commit the deletion of every proven flip into the round tree and re-run the checks
    /// the deletion can break; an error blocks the round.
    fn delete_flips(&mut self, suites: &[String]) -> Result<(), String>;
    fn record(&mut self, iteration: u32, decision: &Decision);
    /// `member` owns red `suite`; `rerun` is that suite rerun on the full round tree (`None`
    /// when the rerun faulted): green there means it flipped.
    fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>);
}

/// Ids of the settled-owner reruns start here, clear of the attributor's own.
const RERUN_ID_BASE: u64 = 1 << 40;

/// `suite` once more on the round tree with every member present, to the end.
fn rerun_with_members<R: RoundRunner>(runner: &mut R, suite: &str, n: u64) -> Option<JobResult> {
    let job = Job { id: RERUN_ID_BASE + n, suite: suite.to_string(), removal: vec![], purpose: Purpose::Plain };
    if runner.launch(&job).is_err() {
        return None;
    }
    loop {
        if let Some((_, r)) = runner.poll_jobs().into_iter().find(|(id, _)| *id == job.id) {
            return (r != JobResult::Fault).then_some(r);
        }
        runner.wait();
    }
}

pub struct Settled {
    pub decision: Decision,
    pub end: MainEnd,
    /// When the main run ended and when the last red settled (runner clock).
    pub main_end_at: u64,
    pub settled_at: u64,
}

/// One main run with concurrent attribution, to the point where every red is settled.
pub fn drive<R: RoundRunner, O: RoundOps + ?Sized>(
    runner: &mut R,
    ops: &O,
    kind: MainKind,
    suites: &[String],
    members: &[Member],
    budget: Budget,
) -> Result<Settled, String> {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    let prereqs: BTreeMap<Id, Vec<Id>> =
        members.iter().map(|m| (m.id.clone(), m.stack.keys().filter(|p| ids.contains(p)).cloned().collect())).collect();
    let mut a = Attributor::new(budget, Shape::new(&ids, &prereqs), suites.len());
    runner.set_members(members);
    runner.start_main(kind, suites)?;
    let mut seen = std::collections::BTreeSet::new();
    let mut main_end_at = None;
    loop {
        let poll = runner.poll_main()?;
        let now = runner.now();
        for (suite, green) in poll.results {
            if seen.insert(suite.clone()) {
                let order = if green { vec![] } else { ops.suspects(&suite, &ids) };
                a.on_main_result(&suite, green, &order, now);
            }
        }
        if let Some(end) = poll.done {
            if end == MainEnd::WorkspaceBuild {
                return Ok(Settled { decision: Decision::default(), end, main_end_at: now, settled_at: now });
            }
            if main_end_at.is_none() {
                // Absence is never green: a selected suite with no result is red.
                for s in suites {
                    if seen.insert(s.clone()) {
                        a.on_main_result(s, false, &ops.suspects(s, &ids), now);
                    }
                }
                a.on_main_done(now);
                main_end_at = Some(now);
            }
        }
        for (id, r) in runner.poll_jobs() {
            a.on_job_result(id, r, now);
        }
        for job in a.next_jobs() {
            if let Err(e) = runner.launch(&job) {
                eprintln!("batcher: attribution job {} ({} without {:?}) not launched: {e}", job.id, job.suite, job.removal);
                a.on_job_result(job.id, JobResult::Fault, now);
            }
        }
        if a.settled() {
            return Ok(Settled { decision: a.decision(), end: MainEnd::Ran, main_end_at: main_end_at.unwrap_or(now), settled_at: now });
        }
        runner.wait();
    }
}

const FLIP_JOB_BASE: u64 = 1 << 40;

/// A flip is never shipped (law-a-test-that-flips-is-deleted): each flaky suite must run green
/// alone twice on the round tree, alone on the base, and alone on every member that touches
/// it; then it is deleted from the round. Any other result is a real red and blocks the round.
fn remove_flips<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    flaky: &[String],
    members: &[Member],
    budget: Budget,
) -> Result<(), String> {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut next = FLIP_JOB_BASE;
    let mut push = |suite: &str, removal: Vec<Id>, purpose: Purpose| {
        queue.push_back(Job { id: next, suite: suite.to_string(), removal, purpose });
        next += 1;
    };
    for suite in flaky {
        push(suite, vec![], Purpose::Plain);
        push(suite, vec![], Purpose::Plain);
        push(suite, ids.clone(), Purpose::Base);
        for x in ops.touching(suite, &ids) {
            let mut keep = vec![x.clone()];
            while let Some(p) = members.iter().filter(|m| keep.contains(&m.id)).flat_map(|m| m.stack.keys()).find(|p| ids.contains(*p) && !keep.contains(*p)) {
                keep.push(p.clone());
            }
            push(suite, ids.iter().filter(|i| !keep.contains(i)).cloned().collect(), Purpose::Without(x));
        }
    }
    let suite_of: BTreeMap<u64, String> = queue.iter().map(|j| (j.id, j.suite.clone())).collect();
    let mut running = 0usize;
    let mut unproven: BTreeSet<String> = BTreeSet::new();
    while !queue.is_empty() || running > 0 {
        while running < budget.slots as usize {
            let Some(job) = queue.pop_front() else { break };
            if unproven.contains(&job.suite) {
                continue;
            }
            match runner.launch(&job) {
                Ok(()) => running += 1,
                Err(e) => {
                    eprintln!("batcher: flip proof job {} ({}) not launched: {e}", job.id, job.suite);
                    unproven.insert(job.suite);
                }
            }
        }
        for (id, r) in runner.poll_jobs() {
            if let Some(suite) = suite_of.get(&id) {
                running = running.saturating_sub(1);
                if r != JobResult::Green {
                    unproven.insert(suite.clone());
                }
            }
        }
        if running > 0 {
            runner.wait();
        }
    }
    if !unproven.is_empty() {
        let reds: Vec<String> = unproven.into_iter().collect();
        ops.incident("unattributed", &reds);
        return Err(format!("red that is not a flip: {}", reds.join(",")));
    }
    println!("batcher: flip proven, deleting from the round: {}", flaky.join(","));
    ops.delete_flips(flaky)
}

#[derive(Debug, PartialEq, Eq)]
pub enum RoundEnd {
    /// Land these members; `attribution_secs` is the longest per-red attribution wall.
    Land { members: Vec<Member>, attribution_secs: Option<u64> },
    /// Nothing lands; the reason was already reported.
    Blocked(String),
}

/// The whole round: corpus with concurrent attribution; then, while anyone was ejected, the
/// owners' suites (only those) on the survivors' release build, attributed the same way.
pub fn attribute_round<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    corpus: &[String],
    starting: Vec<Member>,
    budget: Budget,
) -> Result<RoundEnd, String> {
    let mut members = starting;
    let mut suites = corpus.to_vec();
    let mut kind = MainKind::Corpus;
    let mut worst: Option<u64> = None;
    let mut fixed = false;
    let bound = members.len() as u32 + 2;
    let mut retried = false;
    let t0 = runner.now();
    for iteration in 0..bound {
        let s = drive(runner, &*ops, kind, &suites, &members, budget)?;
        if s.end == MainEnd::WorkspaceBuild {
            ops.incident("workspace-build", &[crate::io::WORKSPACE_BUILD.to_string()]);
            return Ok(RoundEnd::Blocked("the round's workspace failed to build".into()));
        }
        let d = s.decision;
        if !d.records.is_empty() {
            println!(
                "batcher: iteration {iteration}: main run ended at +{}s, decision at +{}s",
                s.main_end_at.saturating_sub(t0),
                s.settled_at.saturating_sub(t0)
            );
        }
        ops.record(iteration, &d);
        for r in &d.records {
            worst = worst.max(r.attribution_secs());
            println!(
                "batcher: red {} → {}{} after {}s, {} rerun(s){}",
                r.suite,
                r.outcome.as_ref().map(Outcome::word).unwrap_or("unsettled"),
                match &r.outcome {
                    Some(Outcome::Owner(o)) => format!(" {}", o.join(",")),
                    _ => String::new(),
                },
                r.attribution_secs().unwrap_or(0),
                r.reruns,
                if r.settled_before_main_end { ", settled before the main run ended" } else { "" }
            );
        }
        if !d.base.is_empty() {
            ops.incident("base", &d.base);
        }
        let integration = d.integration_suites();
        if !integration.is_empty() && !fixed && ops.fix_integration(&members, &integration) {
            fixed = true;
            suites = integration;
            kind = MainKind::Verify;
            continue;
        }
        if fixed && !integration.is_empty() {
            ops.incident("integration", &integration);
            return Ok(RoundEnd::Blocked(format!("integration red survived the in-round fix: {}", integration.join(","))));
        }
        if !d.unattributed.is_empty() {
            if !retried && d.owners.is_empty() {
                retried = true;
                if let Some((sha, rebuilt)) = ops.base_moved(&members)? {
                    println!("batcher: unattributed red {} — base moved, retrying once on {sha}", d.unattributed.join(","));
                    runner.set_base(&sha);
                    members = rebuilt;
                    suites = d.unattributed.clone();
                    kind = MainKind::Verify;
                    continue;
                }
            }
            let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
            ops.judge(&d.unattributed, &ids);
            return Ok(RoundEnd::Blocked(format!("unattributed red: {}", d.unattributed.join(","))));
        }
        if d.owners.is_empty() {
            if !d.flaky.is_empty() {
                if let Err(why) = remove_flips(runner, ops, &d.flaky, &members, budget) {
                    return Ok(RoundEnd::Blocked(why));
                }
            }
            return Ok(RoundEnd::Land { members, attribution_secs: worst });
        }
        // Every owner leaves with its suites; a member stacked on an owner leaves with it.
        let mut out: BTreeMap<Id, Vec<String>> = d.owners.clone();
        loop {
            let before = out.len();
            for m in &members {
                if let Some(p) = m.stack.keys().find(|p| out.contains_key(*p)) {
                    let suites = out[p].clone();
                    out.entry(m.id.clone()).or_insert(suites);
                }
            }
            if out.len() == before {
                break;
            }
        }
        let mut n = 0;
        for m in &members {
            for suite in d.owners.get(&m.id).into_iter().flatten() {
                n += 1;
                let rerun = rerun_with_members(runner, suite, n);
                ops.escape(m, suite, rerun);
            }
        }
        for m in &members {
            if let Some(s) = out.get(&m.id) {
                ops.eject(m, s, d.owners.contains_key(&m.id));
            }
        }
        let survivors: Vec<Member> = members.iter().filter(|m| !out.contains_key(&m.id)).cloned().collect();
        if survivors.is_empty() {
            return Ok(RoundEnd::Blocked("round emptied by ejection".into()));
        }
        members = ops.rebuild(&survivors)?;
        if members.is_empty() {
            return Ok(RoundEnd::Blocked("round emptied rebuilding the tree after ejection".into()));
        }
        suites = d.owned_suites();
        kind = MainKind::Verify;
    }
    Ok(RoundEnd::Blocked(format!("still ejecting after {bound} iterations")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeSet;

    pub fn member(id: &str) -> Member {
        Member {
            id: id.into(),
            tip: format!("{id}-tip"),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: BTreeMap::new(),
            blocked_by: Vec::new(),
        }
    }

    /// Whether suite S is red on a tree holding these members.
    pub type RedFn = Box<dyn Fn(&str, &BTreeSet<String>) -> bool>;

    /// A round on a simulated clock. Main-run suite i finishes at `start + dur[i]` in a
    /// maxpar-wide pool; a job takes `job_secs`; `red(suite, members in the tree)` decides
    /// every result, `flaky` suites are red exactly once.
    pub struct Fake {
        pub t: u64,
        pub maxpar: usize,
        pub job_secs: u64,
        pub dur: BTreeMap<String, u64>,
        pub red: RedFn,
        pub flaky: BTreeSet<String>,
        pub members: Vec<String>,
        pub main: Vec<(String, u64, bool)>,
        pub main_kind: Vec<(MainKind, Vec<String>)>,
        pub jobs: Vec<(Job, u64, JobResult)>,
        pub reported: BTreeSet<String>,
        pub main_ended: Option<u64>,
        pub first_launch: Option<u64>,
        /// (t, corpus suites running, attribution jobs running)
        pub use_log: Vec<(u64, usize, usize)>,
        pub done_jobs: BTreeSet<u64>,
        pub bases: Vec<String>,
        pub on_base: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    }

    impl Fake {
        pub fn new(red: impl Fn(&str, &BTreeSet<String>) -> bool + 'static) -> Fake {
            Fake {
                t: 0,
                maxpar: 2,
                job_secs: 3,
                dur: BTreeMap::new(),
                red: Box::new(red),
                flaky: BTreeSet::new(),
                members: vec![],
                main: vec![],
                main_kind: vec![],
                jobs: vec![],
                reported: BTreeSet::new(),
                main_ended: None,
                first_launch: None,
                use_log: vec![],
                done_jobs: BTreeSet::new(),
                bases: vec![],
                on_base: None,
            }
        }
        fn verdict(&mut self, suite: &str, tree: &BTreeSet<String>) -> bool {
            if self.flaky.remove(suite) {
                return false;
            }
            !(self.red)(suite, tree)
        }
        fn corpus_running(&self) -> usize {
            self.main.iter().filter(|(_, fin, _)| *fin > self.t).count().min(self.maxpar)
        }
        fn jobs_running(&self) -> usize {
            self.jobs.iter().filter(|(j, fin, _)| *fin > self.t && !self.done_jobs.contains(&j.id)).count()
        }
    }

    impl RoundRunner for Fake {
        fn set_members(&mut self, members: &[Member]) {
            self.members = members.iter().map(|m| m.id.clone()).collect();
        }
        fn set_base(&mut self, sha: &str) {
            self.bases.push(sha.to_string());
            if let Some(c) = &self.on_base {
                c.set(true);
            }
        }
        fn start_main(&mut self, kind: MainKind, suites: &[String]) -> Result<(), String> {
            self.main_kind.push((kind, suites.to_vec()));
            self.main.clear();
            self.done_jobs.clear();
            self.jobs.clear();
            self.reported.clear();
            self.main_ended = None;
            let tree: BTreeSet<String> = self.members.iter().cloned().collect();
            // LPT-free simple list scheduling: maxpar lanes, suites in the given order.
            let mut lanes = vec![self.t; self.maxpar];
            for s in suites {
                let d = *self.dur.get(s).unwrap_or(&5);
                let i = (0..lanes.len()).min_by_key(|&i| lanes[i]).unwrap();
                lanes[i] += d;
                let green = self.verdict(s, &tree);
                self.main.push((s.clone(), lanes[i], green));
            }
            Ok(())
        }
        fn poll_main(&mut self) -> Result<MainPoll, String> {
            let mut p = MainPoll::default();
            for (s, fin, g) in &self.main {
                if *fin <= self.t && self.reported.insert(s.clone()) {
                    p.results.push((s.clone(), *g));
                }
            }
            if self.reported.len() == self.main.len() {
                self.main_ended.get_or_insert(self.t);
                p.done = Some(MainEnd::Ran);
            }
            Ok(p)
        }
        fn launch(&mut self, job: &Job) -> Result<(), String> {
            self.first_launch.get_or_insert(self.t);
            let tree: BTreeSet<String> = self.members.iter().filter(|m| !job.removal.contains(m)).cloned().collect();
            let green = self.verdict(&job.suite, &tree);
            self.jobs.push((job.clone(), self.t + self.job_secs, if green { JobResult::Green } else { JobResult::Red }));
            Ok(())
        }
        fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
            let mut out = vec![];
            for (j, fin, r) in &self.jobs {
                if *fin <= self.t && self.done_jobs.insert(j.id) {
                    out.push((j.id, *r));
                }
            }
            out
        }
        fn now(&self) -> u64 {
            self.t
        }
        fn wait(&mut self) {
            self.use_log.push((self.t, self.corpus_running(), self.jobs_running()));
            self.t += 1;
            assert!(self.t < 100_000, "the loop never settled");
        }
    }

    #[derive(Default)]
    pub struct Ops {
        pub touch: BTreeMap<String, Vec<String>>,
        pub ejected: Vec<(String, Vec<String>)>,
        pub owner_flags: Vec<(String, bool)>,
        pub rebuilt: Vec<Vec<String>>,
        pub incidents: Vec<(String, Vec<String>)>,
        pub can_fix: bool,
        pub fix_calls: u32,
        pub fixed: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        pub recorded: Vec<(u32, Decision)>,
        pub deleted: Vec<Vec<String>>,
        pub escaped: Vec<(String, String, Option<JobResult>)>,
        pub judged: Vec<(Vec<String>, Vec<String>)>,
        /// Base shas the base will move to, one per `base_moved` call; empty means it never moves.
        pub moves: Vec<String>,
    }

    impl RoundOps for Ops {
        fn suspects(&self, suite: &str, members: &[Id]) -> Vec<Id> {
            let t = self.touch.get(suite).cloned().unwrap_or_default();
            let mut v: Vec<Id> = members.iter().filter(|m| t.contains(m)).cloned().collect();
            v.extend(members.iter().filter(|m| !t.contains(m)).cloned());
            v
        }
        fn eject(&mut self, member: &Member, suites: &[String], owner: bool) {
            self.ejected.push((member.id.clone(), suites.to_vec()));
            self.owner_flags.push((member.id.clone(), owner));
        }
        fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
            self.rebuilt.push(survivors.iter().map(|m| m.id.clone()).collect());
            Ok(survivors.to_vec())
        }
        fn fix_integration(&mut self, _: &[Member], _: &[String]) -> bool {
            self.fix_calls += 1;
            if self.can_fix {
                if let Some(f) = &self.fixed {
                    f.set(true);
                }
            }
            self.can_fix
        }
        fn base_moved(&mut self, members: &[Member]) -> Result<Option<(String, Vec<Member>)>, String> {
            Ok(if self.moves.is_empty() { None } else { Some((self.moves.remove(0), members.to_vec())) })
        }
        fn judge(&mut self, suites: &[String], members: &[Id]) {
            self.judged.push((suites.to_vec(), members.to_vec()));
        }
        fn incident(&mut self, kind: &str, suites: &[String]) {
            self.incidents.push((kind.into(), suites.to_vec()));
        }
        fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id> {
            let t = self.touch.get(suite).cloned().unwrap_or_default();
            members.iter().filter(|m| t.contains(m)).cloned().collect()
        }
        fn delete_flips(&mut self, suites: &[String]) -> Result<(), String> {
            self.deleted.push(suites.to_vec());
            Ok(())
        }
        fn record(&mut self, iteration: u32, decision: &Decision) {
            self.recorded.push((iteration, decision.clone()));
        }
        fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>) {
            self.escaped.push((member.id.clone(), suite.to_string(), rerun));
        }
    }

    fn corpus(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("test-{i:02}.sh")).collect()
    }

    fn three() -> Vec<Member> {
        vec![member("m1"), member("m2"), member("m3")]
    }

    fn breaks(culprit: &'static str, suite: &'static str) -> impl Fn(&str, &BTreeSet<String>) -> bool {
        move |s, tree| s == suite && tree.contains(culprit)
    }

    #[test]
    fn attribution_starts_on_the_first_streamed_red_and_settles_before_the_corpus_ends() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
        f.maxpar = 4;
        f.dur.insert("test-00.sh".into(), 2);
        let budget = Budget { slots: 6, maxpar: 4 };
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(40), three(), budget).unwrap();
        let main_end = f.main_ended.unwrap();
        let first = f.first_launch.unwrap();
        assert!(first <= 3, "first rerun at t={first}, the red landed at t=2");
        assert!(first < main_end, "attribution began before the corpus ended ({first} < {main_end})");
        let rec = &ops.recorded[0].1.records[0];
        assert!(rec.settled_before_main_end, "{rec:?} vs corpus end {main_end}");
        assert_eq!(ops.ejected, vec![("m2".to_string(), vec!["test-00.sh".to_string()])]);
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1", "m3"]));
    }

    #[test]
    fn a_settled_owner_s_suite_is_rerun_on_the_full_tree_before_it_is_ejected() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
        f.maxpar = 4;
        let mut ops = Ops::default();
        attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 4 }).unwrap();
        assert_eq!(ops.escaped, vec![("m2".to_string(), "test-00.sh".to_string(), Some(JobResult::Red))]);
    }

    #[test]
    fn the_slot_budget_holds_at_every_tick_and_the_corpus_is_never_starved() {
        let mut f = Fake::new(|s, tree| (s == "test-03.sh" && tree.contains("m1")) || (s == "test-07.sh" && tree.contains("m3")));
        f.maxpar = 3;
        f.job_secs = 7;
        let budget = Budget { slots: 5, maxpar: 3 };
        let mut ops = Ops::default();
        attribute_round(&mut f, &mut ops, &corpus(30), three(), budget).unwrap();
        for (t, c, j) in &f.use_log {
            assert!(c + j <= 5, "t={t}: corpus {c} + attribution {j} > 5 slots");
        }
        // The corpus ran maxpar-wide for as long as it had that many suites left: its schedule
        // was fixed at start and attribution never delayed a corpus result.
        let corpus_first_run = &f.main_kind[0];
        assert_eq!(corpus_first_run.1.len(), 30);
        assert!(f.use_log.iter().any(|(_, _, j)| *j == 2), "the two spare slots were used");
    }

    #[test]
    fn a_flip_is_proven_green_alone_and_deleted_from_the_round_never_landed() {
        let mut f = Fake::new(|_, _| false);
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        ops.touch.insert("test-04.sh".into(), vec!["m2".into()]);
        let end = attribute_round(&mut f, &mut ops, &corpus(6), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
        assert_eq!(ops.deleted, vec![vec!["test-04.sh".to_string()]]);
        let proofs: Vec<_> = f.jobs.iter().filter(|(j, _, _)| j.id >= FLIP_JOB_BASE).map(|(j, _, _)| j.removal.clone()).collect();
        assert_eq!(proofs.len(), 4, "two alone, the base, one touching member: {proofs:?}");
        assert!(proofs.contains(&vec!["m1".to_string(), "m3".to_string()]), "the touching member alone: {proofs:?}");
        assert!(proofs.contains(&vec!["m1".to_string(), "m2".to_string(), "m3".to_string()]), "the base alone: {proofs:?}");
    }

    #[test]
    fn a_red_on_the_base_is_not_a_flip_and_blocks_the_round() {
        let mut f = Fake::new(|s, tree| s == "test-04.sh" && tree.is_empty());
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(6), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert!(ops.deleted.is_empty());
        assert_eq!(ops.incidents, vec![("unattributed".to_string(), vec!["test-04.sh".to_string()])]);
    }

    #[test]
    fn owner_flake_and_base_are_told_apart() {
        let mut f = Fake::new(|s, tree| match s {
            "test-01.sh" => tree.contains("m3"),
            "test-02.sh" => true, // red on every tree, the base included
            _ => false,
        });
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(8), three(), Budget { slots: 4, maxpar: 2 }).unwrap();
        let d = &ops.recorded[0].1;
        assert_eq!(d.owners.get("m3"), Some(&vec!["test-01.sh".to_string()]));
        assert_eq!(d.base, vec!["test-02.sh".to_string()]);
        assert_eq!(d.flaky, vec!["test-04.sh".to_string()]);
        assert_eq!(ops.incidents, vec![("base".to_string(), vec!["test-02.sh".to_string()])]);
        assert!(matches!(end, RoundEnd::Land { .. }));
    }

    #[test]
    fn every_owner_is_ejected_with_its_suites_and_the_survivors_rerun_only_those() {
        let mut f = Fake::new(|s, tree| match s {
            "test-01.sh" | "test-05.sh" => tree.contains("m1"),
            "test-03.sh" => tree.contains("m3"),
            _ => false,
        });
        let mut ops = Ops::default();
        let members = vec![member("m1"), member("m2"), member("m3"), member("m4")];
        let end = attribute_round(&mut f, &mut ops, &corpus(10), members, Budget { slots: 5, maxpar: 2 }).unwrap();
        let mut ej = ops.ejected.clone();
        ej.sort();
        assert_eq!(
            ej,
            vec![
                ("m1".to_string(), vec!["test-01.sh".to_string(), "test-05.sh".to_string()]),
                ("m3".to_string(), vec!["test-03.sh".to_string()]),
            ]
        );
        assert_eq!(ops.rebuilt, vec![vec!["m2".to_string(), "m4".to_string()]]);
        assert_eq!(f.main_kind.len(), 2);
        assert_eq!(f.main_kind[1], (MainKind::Verify, vec!["test-01.sh".into(), "test-03.sh".into(), "test-05.sh".into()]));
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2));
    }

    #[test]
    fn a_stacked_dependent_leaves_with_its_owner() {
        let mut f = Fake::new(breaks("m1", "test-02.sh"));
        let mut dep = member("m2");
        dep.stack.insert("m1".into(), "m1-tip".into());
        let mut ops = Ops::default();
        attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), dep, member("m3")], Budget { slots: 4, maxpar: 2 }).unwrap();
        let ids: Vec<&str> = ops.ejected.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(ids, ["m1", "m2"]);
        assert_eq!(ops.rebuilt, vec![vec!["m3".to_string()]]);
        assert_eq!(ops.owner_flags, [("m1".to_string(), true), ("m2".to_string(), false)], "the owner keeps its charge; the dependent is collateral");
    }

    #[test]
    fn nothing_is_ejected_when_every_red_is_a_flake_or_the_base_s() {
        let mut f = Fake::new(|s, _| s == "test-06.sh");
        f.flaky.insert("test-02.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(8), three(), Budget { slots: 4, maxpar: 2 }).unwrap();
        assert!(ops.ejected.is_empty());
        assert!(ops.rebuilt.is_empty());
        assert_eq!(f.main_kind.len(), 1, "no verification run");
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
    }

    // Two members each fine alone, red together until the round-level fix lands (stale
    // coverage.json shape): both would be named owners today and the round emptied.
    #[test]
    fn an_integration_red_is_fixed_in_the_round_and_every_member_lands() {
        let fixed = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = fixed.clone();
        let mut f = Fake::new(move |s, tree| s == "test-01.sh" && !flag.get() && tree.contains("m1") && tree.contains("m2"));
        let mut ops = Ops { can_fix: true, fixed: Some(fixed), ..Ops::default() };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3), "{end:?}");
        assert_eq!(ops.fix_calls, 1);
        assert!(ops.ejected.is_empty());
        assert!(ops.incidents.is_empty());
    }

    #[test]
    fn an_integration_red_the_fix_does_not_clear_is_held_not_ejected() {
        let mut f = Fake::new(|s, tree| s == "test-01.sh" && tree.contains("m1") && tree.contains("m2"));
        let mut ops = Ops { can_fix: true, ..Ops::default() };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(ops.fix_calls, 1, "the fix is tried once");
        assert_eq!(ops.incidents, vec![("integration".to_string(), vec!["test-01.sh".to_string()])]);
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn an_unattributed_red_with_the_base_unmoved_goes_straight_to_the_judge() {
        let mut f = Fake::new(|s, tree| s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2")));
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)));
        assert_eq!(ops.judged, vec![(vec!["test-01.sh".to_string()], vec!["m1".to_string(), "m2".to_string(), "m3".to_string()])]);
        assert!(ops.incidents.is_empty());
        assert!(ops.ejected.is_empty());
        assert_eq!(f.main_kind.len(), 1, "no retry: the base did not move");
    }

    #[test]
    fn an_unattributed_red_is_retried_once_on_the_moved_base_then_judged() {
        let mut f = Fake::new(|s, tree| s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2")));
        let mut ops = Ops { moves: vec!["base2".into(), "base3".into()], ..Ops::default() };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)));
        assert_eq!(f.bases, vec!["base2".to_string()], "retried once, never twice");
        assert_eq!(f.main_kind.len(), 2);
        assert_eq!(f.main_kind[1], (MainKind::Verify, vec!["test-01.sh".to_string()]));
        assert_eq!(ops.judged.len(), 1);
    }

    #[test]
    fn an_unattributed_red_that_clears_on_the_moved_base_lands() {
        let moved = std::rc::Rc::new(std::cell::Cell::new(false));
        let m2 = moved.clone();
        let mut f = Fake::new(move |s, tree| !m2.get() && s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2")));
        f.on_base = Some(moved);
        let mut ops = Ops { moves: vec!["base2".into()], ..Ops::default() };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3), "{end:?}");
        assert!(ops.judged.is_empty());
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn a_suite_with_no_result_at_the_end_is_red_not_green() {
        struct Silent(Fake);
        impl RoundRunner for Silent {
            fn set_members(&mut self, m: &[Member]) {
                self.0.set_members(m)
            }
            fn set_base(&mut self, sha: &str) {
                self.0.set_base(sha)
            }
            fn start_main(&mut self, k: MainKind, s: &[String]) -> Result<(), String> {
                self.0.start_main(k, s)
            }
            fn poll_main(&mut self) -> Result<MainPoll, String> {
                let mut p = self.0.poll_main()?;
                p.results.retain(|(s, _)| s != "test-01.sh");
                Ok(p)
            }
            fn launch(&mut self, j: &Job) -> Result<(), String> {
                self.0.launch(j)
            }
            fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
                self.0.poll_jobs()
            }
            fn now(&self) -> u64 {
                self.0.now()
            }
            fn wait(&mut self) {
                self.0.wait()
            }
        }
        let mut f = Silent(Fake::new(|_, _| false));
        let s = drive(&mut f, &Ops::default(), MainKind::Corpus, &corpus(3), &three(), Budget { slots: 4, maxpar: 2 }).unwrap();
        assert_eq!(s.decision.records.len(), 1);
        assert_eq!(s.decision.records[0].suite, "test-01.sh");
        assert_eq!(s.decision.flaky, vec!["test-01.sh".to_string()], "its reruns were green");
    }
}
