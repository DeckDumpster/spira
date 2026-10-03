//! Concurrent attribution of a round's reds (sp-hvtgs; batcher-cut DESIGN.md §4).
//!
//! The pure half: which reruns a red suite needs, in what order, how many may run at once,
//! and what their results mean. The driver (batcher-cut `drive`) streams the main run's
//! results in and launches the jobs this hands out; nothing here reads a file, runs git or
//! looks at a clock — `now` is always an argument, so every schedule is a replayable fixture.
//!
//! Per red suite S: a plain rerun with every member first (green → flaky). Red → the base run
//! (every member removed) and one run without each suspect, covering members first. Green
//! without X → X owns S. Base red → base red. Nothing green and the base green →
//! unattributed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::Id;

/// Why a job runs. `Without` names the suspect whose removal set the job drops.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Purpose {
    Plain,
    Base,
    Without(Id),
}

/// One rerun: suite `suite` alone, on the round tree minus `removal` (round order; empty for
/// the plain rerun).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub suite: String,
    pub removal: Vec<Id>,
    pub purpose: Purpose,
}

/// A job's result. `Fault` is the harness failing to run it — never read as green or red.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobResult {
    Green,
    Red,
    Fault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Green without each named member: they own the suite.
    Owner(Vec<Id>),
    /// Green on a plain rerun with every member present: charged to nobody.
    Flaky,
    /// Red with every member removed: the base's, charged to nobody.
    Base,
    /// No single removal turned it green and the base is green (or the reruns faulted).
    Unattributed,
}

impl Outcome {
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Owner(_) => "owner",
            Outcome::Flaky => "flaky",
            Outcome::Base => "base",
            Outcome::Unattributed => "unattributed",
        }
    }
}

/// What the round learned about one red suite — the TSD row item 7 reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedRecord {
    pub suite: String,
    pub outcome: Option<Outcome>,
    pub red_at: u64,
    pub settled_at: Option<u64>,
    pub reruns: u32,
    /// Settled no later than the main run ended (or before it ended at all).
    pub settled_before_main_end: bool,
}

impl RedRecord {
    pub fn attribution_secs(&self) -> Option<u64> {
        self.settled_at.map(|s| s.saturating_sub(self.red_at))
    }

    /// The `round-attribution` TSD row's fields (DESIGN.md "Record"), in the order written.
    /// An unsettled red has no attribution time: the field is empty, never `0`, so a reader
    /// cannot mistake "never settled" for "settled at once" (sp-cln99).
    pub fn tsd_fields(&self, repo: &str, round: &str, iteration: u32) -> Vec<(&'static str, String)> {
        let owner = match &self.outcome {
            Some(Outcome::Owner(o)) => o.join(","),
            _ => String::new(),
        };
        vec![
            ("repo", repo.to_string()),
            ("round", round.to_string()),
            ("iteration", iteration.to_string()),
            ("suite", self.suite.clone()),
            ("outcome", self.outcome.as_ref().map(|o| o.word()).unwrap_or("unsettled").to_string()),
            ("owner", owner),
            ("attribution_secs", self.attribution_secs().map(|s| s.to_string()).unwrap_or_default()),
            ("reruns", self.reruns.to_string()),
            ("settled_before_corpus_end", self.settled_before_main_end.to_string()),
        ]
    }
}

/// The round's membership as attribution sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Round (merge) order.
    pub members: Vec<Id>,
    /// Member → its removal set: itself plus every round member stacked on it, transitively,
    /// in round order. A dependent's tip carries its prerequisite's commits, so the round
    /// tree without X cannot contain X's dependents.
    pub closure: BTreeMap<Id, Vec<Id>>,
}

impl Shape {
    /// `prereqs`: member → the round members it is stacked on.
    pub fn new(members: &[Id], prereqs: &BTreeMap<Id, Vec<Id>>) -> Shape {
        let mut closure = BTreeMap::new();
        for x in members {
            let mut set: BTreeSet<&Id> = BTreeSet::from([x]);
            loop {
                let before = set.len();
                for m in members {
                    if prereqs.get(m).is_some_and(|ps| ps.iter().any(|p| set.contains(p))) {
                        set.insert(m);
                    }
                }
                if set.len() == before {
                    break;
                }
            }
            closure.insert(x.clone(), members.iter().filter(|m| set.contains(m)).cloned().collect());
        }
        Shape { members: members.to_vec(), closure }
    }

    fn removal_of(&self, x: &Id) -> Vec<Id> {
        self.closure.get(x).cloned().unwrap_or_else(|| vec![x.clone()])
    }
}

/// The round's slot budget: `slots` in all, of which the main run always keeps `maxpar`
/// while it has that many suites left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub slots: u32,
    pub maxpar: u32,
}

impl Budget {
    /// `SPIRA_BATCHER_ROUND_SLOTS`, else `maxpar + 4` (DESIGN §4.2).
    pub fn with_default(maxpar: u32, slots: Option<u32>) -> Budget {
        Budget { slots: slots.unwrap_or(maxpar + 4).max(1), maxpar }
    }
}

#[derive(Clone, Debug)]
struct SuiteState {
    red_at: u64,
    suspects: Vec<Id>,
    plain_red: bool,
    settled: Option<(Outcome, u64)>,
    owners: Vec<(Id, Vec<Id>)>,
    reruns: u32,
    faults: BTreeMap<Purpose, u32>,
    /// Jobs of this suite queued or running.
    outstanding: BTreeSet<u64>,
    base_green: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// Owner → the suites named against it.
    pub owners: BTreeMap<Id, Vec<String>>,
    pub flaky: Vec<String>,
    pub base: Vec<String>,
    pub unattributed: Vec<String>,
    pub records: Vec<RedRecord>,
}

impl Decision {
    /// The suites the survivors must pass before they land: every suite an owner turned red.
    pub fn owned_suites(&self) -> Vec<String> {
        let s: BTreeSet<&String> = self.owners.values().flatten().collect();
        s.into_iter().cloned().collect()
    }

    /// Suites red only in the merged tree: nothing removable alone clears them
    /// (unattributed), or no one member does (two or more each clear them).
    pub fn integration_suites(&self) -> Vec<String> {
        let mut count: BTreeMap<&String, usize> = BTreeMap::new();
        for s in self.owners.values().flatten() {
            *count.entry(s).or_default() += 1;
        }
        let mut out: BTreeSet<String> = self.unattributed.iter().cloned().collect();
        out.extend(count.into_iter().filter(|(_, n)| *n >= 2).map(|(s, _)| s.clone()));
        out.into_iter().collect()
    }
}

pub struct Attributor {
    budget: Budget,
    shape: Shape,
    main_total: usize,
    main_seen: BTreeSet<String>,
    main_end_at: Option<u64>,
    suites: BTreeMap<String, SuiteState>,
    reds: Vec<String>,
    plain_q: VecDeque<Job>,
    attr_q: VecDeque<Job>,
    running: BTreeMap<u64, Job>,
    next_id: u64,
    /// Every job ever handed out, in launch order (for tests and telemetry).
    pub launched: Vec<Job>,
}

impl Attributor {
    pub fn new(budget: Budget, shape: Shape, main_total: usize) -> Attributor {
        Attributor {
            budget,
            shape,
            main_total,
            main_seen: BTreeSet::new(),
            main_end_at: None,
            suites: BTreeMap::new(),
            reds: vec![],
            plain_q: VecDeque::new(),
            attr_q: VecDeque::new(),
            running: BTreeMap::new(),
            next_id: 1,
            launched: vec![],
        }
    }

    /// Main-run suites still owed a slot: none once it ended, else at most `maxpar`.
    pub fn main_in_flight(&self) -> u32 {
        if self.main_end_at.is_some() {
            return 0;
        }
        let left = self.main_total.saturating_sub(self.main_seen.len()) as u32;
        left.min(self.budget.maxpar)
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Slots attribution may take right now.
    pub fn spare(&self) -> u32 {
        self.budget.slots.saturating_sub(self.main_in_flight()).saturating_sub(self.running.len() as u32)
    }

    fn enqueue(&mut self, suite: &str, removal: Vec<Id>, purpose: Purpose, front: bool) {
        let id = self.next_id;
        self.next_id += 1;
        let job = Job { id, suite: suite.to_string(), removal, purpose };
        if let Some(st) = self.suites.get_mut(suite) {
            st.outstanding.insert(id);
        }
        let q = if job.purpose == Purpose::Plain { &mut self.plain_q } else { &mut self.attr_q };
        if front {
            q.push_front(job);
        } else {
            q.push_back(job);
        }
    }

    /// One final result of the main run. `suspects` is S's suspect order (only read on red).
    pub fn on_main_result(&mut self, suite: &str, green: bool, suspects: &[Id], now: u64) {
        if !self.main_seen.insert(suite.to_string()) || green {
            return;
        }
        self.reds.push(suite.to_string());
        self.suites.insert(
            suite.to_string(),
            SuiteState {
                red_at: now,
                suspects: suspects.to_vec(),
                plain_red: false,
                settled: None,
                owners: vec![],
                reruns: 0,
                faults: BTreeMap::new(),
                outstanding: BTreeSet::new(),
                base_green: false,
            },
        );
        self.enqueue(suite, vec![], Purpose::Plain, false);
    }

    pub fn on_main_done(&mut self, now: u64) {
        if self.main_end_at.is_none() {
            self.main_end_at = Some(now);
        }
    }

    pub fn main_done(&self) -> bool {
        self.main_end_at.is_some()
    }

    /// Hands out as many queued jobs as there are spare slots — plain reruns before
    /// attribution runs, each in arrival order.
    pub fn next_jobs(&mut self) -> Vec<Job> {
        let mut out = vec![];
        while self.spare() > 0 {
            let Some(job) = self.plain_q.pop_front().or_else(|| self.attr_q.pop_front()) else { break };
            if let Some(st) = self.suites.get_mut(&job.suite) {
                st.reruns += 1;
            }
            self.running.insert(job.id, job.clone());
            self.launched.push(job.clone());
            out.push(job);
        }
        out
    }

    fn cancel_queued(&mut self, suite: &str) {
        let keep = |j: &Job| j.suite != suite;
        let dropped: Vec<u64> = self.attr_q.iter().chain(self.plain_q.iter()).filter(|j| !keep(j)).map(|j| j.id).collect();
        self.attr_q.retain(keep);
        self.plain_q.retain(keep);
        if let Some(st) = self.suites.get_mut(suite) {
            for id in dropped {
                st.outstanding.remove(&id);
            }
        }
    }

    fn settle(&mut self, suite: &str, outcome: Outcome, now: u64) {
        if let Some(st) = self.suites.get_mut(suite) {
            if st.settled.is_none() {
                st.settled = Some((outcome, now));
            }
        }
        self.cancel_queued(suite);
    }

    pub fn on_job_result(&mut self, job_id: u64, result: JobResult, now: u64) {
        let Some(job) = self.running.remove(&job_id) else { return };
        let all = self.shape.members.clone();
        let suite = job.suite.clone();
        let Some(st) = self.suites.get_mut(&suite) else { return };
        st.outstanding.remove(&job_id);

        if result == JobResult::Fault {
            let n = st.faults.entry(job.purpose.clone()).or_insert(0);
            *n += 1;
            if *n < 2 && st.settled.is_none() {
                self.enqueue(&suite, job.removal.clone(), job.purpose.clone(), true);
            } else if job.purpose == Purpose::Plain {
                self.settle(&suite, Outcome::Unattributed, now);
            }
            self.maybe_exhausted(&suite, now);
            return;
        }
        let green = result == JobResult::Green;
        match &job.purpose {
            Purpose::Plain => {
                if green {
                    self.settle(&suite, Outcome::Flaky, now);
                } else {
                    st.plain_red = true;
                    let suspects = st.suspects.clone();
                    self.queue_attribution(&suite, &suspects, &all);
                }
            }
            Purpose::Base => {
                if green {
                    st.base_green = true;
                    self.maybe_exhausted(&suite, now);
                } else {
                    self.settle(&suite, Outcome::Base, now);
                }
            }
            Purpose::Without(x) => {
                let is_everything = job.removal.len() == all.len();
                if green {
                    self.add_owner(&suite, x.clone(), job.removal.clone(), now);
                } else if is_everything {
                    // X's removal set is the whole round: this run was the base run too.
                    self.settle(&suite, Outcome::Base, now);
                } else {
                    self.maybe_exhausted(&suite, now);
                }
            }
        }
    }

    fn queue_attribution(&mut self, suite: &str, suspects: &[Id], all: &[Id]) {
        let mut seen: BTreeSet<Vec<Id>> = BTreeSet::new();
        let mut withouts = vec![];
        for x in suspects.iter().chain(all.iter()) {
            let removal = self.shape.removal_of(x);
            if seen.insert(removal.clone()) {
                withouts.push((x.clone(), removal));
            }
        }
        if !seen.contains(all) {
            self.enqueue(suite, all.to_vec(), Purpose::Base, false);
        }
        for (x, removal) in withouts {
            self.enqueue(suite, removal, Purpose::Without(x), false);
        }
    }

    /// Green without `x` (removal `removal`). A removal set that strictly contains an
    /// owner's explains nothing new (a prerequisite of the owner); one strictly inside an
    /// owner's replaces it.
    fn add_owner(&mut self, suite: &str, x: Id, removal: Vec<Id>, now: u64) {
        let Some(st) = self.suites.get_mut(suite) else { return };
        match &st.settled {
            None => {}
            Some((Outcome::Owner(_), _)) => {}
            Some(_) => return,
        }
        let r: BTreeSet<&Id> = removal.iter().collect();
        let sub = |a: &BTreeSet<&Id>, b: &[Id]| b.iter().all(|m| a.contains(m));
        if st.owners.iter().any(|(_, o)| o.len() < removal.len() && sub(&r, o)) {
            return;
        }
        st.owners.retain(|(_, o)| {
            let os: BTreeSet<&Id> = o.iter().collect();
            !(removal.len() < o.len() && sub(&os, &removal))
        });
        st.owners.push((x, removal));
        let owners: Vec<Id> = st.owners.iter().map(|(x, _)| x.clone()).collect();
        let at = st.settled.as_ref().map(|(_, t)| *t).unwrap_or(now);
        st.settled = Some((Outcome::Owner(owners), at));
        self.cancel_queued(suite);
    }

    /// Nothing left to run for `suite` and still unsettled: unattributed.
    fn maybe_exhausted(&mut self, suite: &str, now: u64) {
        let Some(st) = self.suites.get(suite) else { return };
        if st.settled.is_none() && st.outstanding.is_empty() && st.plain_red {
            self.settle(suite, Outcome::Unattributed, now);
        }
    }

    /// Every red settled and the main run over: the round has its decision.
    pub fn settled(&self) -> bool {
        self.main_done() && self.suites.values().all(|s| s.settled.is_some())
    }

    pub fn decision(&self) -> Decision {
        let mut d = Decision::default();
        for suite in &self.reds {
            let st = &self.suites[suite];
            let outcome = st.settled.as_ref().map(|(o, _)| o.clone());
            match &outcome {
                Some(Outcome::Owner(ids)) => {
                    for id in ids {
                        d.owners.entry(id.clone()).or_default().push(suite.clone());
                    }
                }
                Some(Outcome::Flaky) => d.flaky.push(suite.clone()),
                Some(Outcome::Base) => d.base.push(suite.clone()),
                Some(Outcome::Unattributed) | None => d.unattributed.push(suite.clone()),
            }
            let settled_at = st.settled.as_ref().map(|(_, t)| *t);
            d.records.push(RedRecord {
                suite: suite.clone(),
                outcome,
                red_at: st.red_at,
                settled_at,
                reruns: st.reruns,
                settled_before_main_end: match (settled_at, self.main_end_at) {
                    (Some(s), Some(e)) => s <= e,
                    (Some(_), None) => true,
                    _ => false,
                },
            });
        }
        d
    }
}

// ---------------------------------------------------------------------------------------
// Suspect order: the members whose diff touches S's `# covers:` paths first.
// ---------------------------------------------------------------------------------------

/// Does `path` match `pat` the way the selector's `case` matching does (`*` crosses `/`):
/// the selector crate's matcher, so attribution and selection cannot disagree (sp-wx2tw).
pub use suite_select::glob::case_match as case_glob;

/// Whether a member that changed `paths` touches suite `suite` (`spira/<suite>`), whose
/// `# covers:` globs are `covers` (`None`: no declaration — it covers everything, as in
/// the selector). A `file#function` glob matches on its file.
pub fn touches(suite: &str, covers: Option<&[String]>, paths: &[String]) -> bool {
    let Some(globs) = covers else { return !paths.is_empty() };
    paths.iter().any(|p| {
        p.rsplit('/').next() == Some(suite)
            || globs.iter().any(|g| {
                let file = g.split('#').next().unwrap_or(g);
                !file.is_empty() && case_glob(file, p)
            })
    })
}

/// S's suspects: members that touch it first, then the rest; each group in round order.
pub fn suspect_order(suite: &str, covers: Option<&[String]>, members: &[Id], changed: &BTreeMap<Id, Vec<String>>) -> Vec<Id> {
    let empty = vec![];
    let (mut hit, mut rest): (Vec<Id>, Vec<Id>) = (vec![], vec![]);
    for m in members {
        if touches(suite, covers, changed.get(m).unwrap_or(&empty)) {
            hit.push(m.clone());
        } else {
            rest.push(m.clone());
        }
    }
    hit.extend(rest);
    hit
}

/// Who a pre-suite install fault belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallFault {
    Owner(Id),
    /// Install fails with every member removed: the base's, charged to nobody.
    Base,
    /// No single member's removal restores install, or a probe kept faulting.
    Unattributed,
}

/// Attribution with no red suite to follow: the same base / bisect / single ladder, with
/// "does the container install succeed on this tree" as the predicate. `probe` gets the removal
/// set and answers `Green` (installs), `Red` (install fails) or `Fault` (the probe itself
/// failed — retried once, then never read as either).
pub fn attribute_install_fault(shape: &Shape, mut probe: impl FnMut(&[Id]) -> JobResult) -> InstallFault {
    let mut ask = |removal: &[Id]| match probe(removal) {
        JobResult::Fault => probe(removal),
        r => r,
    };
    let all = &shape.members;
    match ask(all) {
        JobResult::Green => {}
        JobResult::Red => return InstallFault::Base,
        JobResult::Fault => return InstallFault::Unattributed,
    }
    // Invariant: the first `lo` members install, the first `hi` do not.
    let (mut lo, mut hi) = (0, all.len());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        match ask(&all[mid..]) {
            JobResult::Green => lo = mid,
            JobResult::Red => hi = mid,
            JobResult::Fault => return InstallFault::Unattributed,
        }
    }
    let Some(culprit) = all.get(hi.wrapping_sub(1)) else { return InstallFault::Unattributed };
    match ask(&shape.removal_of(culprit)) {
        JobResult::Green => InstallFault::Owner(culprit.clone()),
        _ => InstallFault::Unattributed,
    }
}

#[cfg(test)]
mod tests;
