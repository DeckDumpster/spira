//! The attribution loop (batcher-cut DESIGN.md §4): stream the main run's results into the pure
//! `Attributor`, launch the reruns it hands out into spare slots, and — once every red is
//! settled — eject the owners, re-run only their suites on the survivors, and say whether
//! the round lands. Every effect goes through `RoundRunner` (suite runs) or `RoundOps`
//! (git, the bead store, incidents, TSD), so the whole loop runs against fakes in tests.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::attrib::{Attributor, Budget, Decision, Job, JobResult, Outcome, Purpose, Shape};
use crate::buildattr::{self, Action, MemberDiff};
use crate::core::{Id, Member};
use crate::unitattr::{self, PackageGraph};

/// What one poll of the main run returned: results that became final since the last poll,
/// and whether it has ended.
#[derive(Debug, Default)]
pub struct MainPoll {
    pub results: Vec<(String, bool)>,
    pub done: Option<MainEnd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MainEnd {
    /// The suites ran (green or red).
    Ran,
    /// The round was abandoned while it ran; no verdict is owed.
    Abandoned,
    /// The tree did not build (testenv rc 4): nothing to attribute suite by suite.
    WorkspaceBuild,
    /// The merged tree's own config delta does not parse or apply (the check `config-delta`):
    /// no suite ran, and the member whose merge breaks it is found without one.
    ConfigDelta,
    /// No result, job result or runner progress for the stall bound; the main run was stopped.
    Stalled {
        waiting: Vec<String>,
        idle_secs: u64,
    },
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
    /// Abandon the main run that is still going and release what it holds, so `start_main` can
    /// run again.
    fn stop_main(&mut self) -> Result<(), String>;
    /// Build the job's tree (round tree minus `job.removal`) and submit suite `job.suite` on it.
    fn launch(&mut self, job: &Job) -> Result<(), String>;
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)>;
    /// The steps of `job.suite` (a comma-separated list) that job `id` found red, once
    /// `poll_jobs` answered it red; empty when the runner cannot say which.
    fn red_steps(&mut self, _id: u64) -> Vec<String> {
        vec![]
    }
    /// Seconds.
    fn now(&self) -> u64;
    fn wait(&mut self);
    /// Seconds without progress after which a polling wait gives up; `None` waits for ever.
    fn stall_secs(&self) -> Option<u64> {
        None
    }
    /// A mark that changes whenever the runner sees progress beyond the results it answers.
    fn progress(&mut self) -> u64 {
        0
    }
    /// The round was abandoned: nothing drives it any more, and nothing more may be launched.
    fn abandoned(&mut self) -> bool {
        false
    }
    /// The low-pass filter (lint, a fence) that ended the corpus pass before any suite ran, if one did.
    fn filter_red(&mut self) -> Option<FilterRed> {
        None
    }
    /// The low-pass filter red of the corpus pass this attribution follows, when its results are
    /// at hand: the red is attributed from it without running the corpus again.
    fn prior_red(&mut self) -> Option<FilterRed> {
        None
    }
    /// The suites the pass this attribution follows ran red, when its results are at hand.
    fn prior_red_suites(&mut self) -> Vec<String> {
        vec![]
    }
    /// The red this attribution follows survived one attribution already: the members are named
    /// as suspects by what the red suites cover, not mended.
    fn confirmed(&self) -> bool {
        false
    }
    /// The compiler's output of the main run that ended `WorkspaceBuild`.
    fn build_output(&mut self) -> String {
        String::new()
    }
    /// Whether the workspace builds with exactly `members` merged onto the base; `None` when the
    /// runner cannot build a subset or the build faulted.
    fn builds(&mut self, _members: &[Member]) -> Option<bool> {
        None
    }
    /// Whether the config delta of the base plus exactly `members` parses; `None` when it cannot
    /// be told.
    fn config_applies(&mut self, _members: &[Member]) -> Option<bool> {
        None
    }
    /// Whether filter `name` is green with exactly `members` merged onto the base; `None` when the
    /// runner cannot run that filter on a subset or the run faulted.
    fn filter_green(&mut self, _name: &str, _members: &[Member]) -> Option<bool> {
        None
    }
    /// Whether testenv suite `suite` is green with exactly `members` merged onto the base; `None`
    /// when the runner cannot run a suite on a subset or the run faulted.
    fn suite_green(&mut self, _suite: &str, _members: &[Member]) -> Option<bool> {
        None
    }
    /// Whether testenv runs to a verdict, rather than dying on a signal, with exactly `members`
    /// merged onto the base; `None` when the runner cannot probe a subset or the probe faulted.
    fn runner_survives(&mut self, _members: &[Member]) -> Option<bool> {
        None
    }
    /// What the filter probe that last answered red printed; empty when it printed nothing or the
    /// runner keeps no output.
    fn probe_output(&mut self) -> String {
        String::new()
    }
    /// Like `filter_green`, judging only `scope` (`unitattr::rerun_scope`): the tests that failed.
    fn filter_green_in(
        &mut self,
        name: &str,
        _scope: &[String],
        members: &[Member],
    ) -> Option<bool> {
        self.filter_green(name, members)
    }
}

/// A filter's red. `named` is each member the filter's own output blames, with the line that does;
/// empty when the red names no member of this round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterRed {
    pub name: String,
    pub named: Vec<(Id, String)>,
    /// The failing tests the filter's output lists, as `<STATUS> <binary> <test>` lines.
    pub failed: Vec<String>,
}

/// Everything the round does besides running suites.
pub trait RoundOps {
    /// S's suspect order among `members` (covers-touching first).
    fn suspects(&self, suite: &str, members: &[Id]) -> Vec<Id>;
    /// `owner`: the member's own tip owns the red; a stacked dependent leaves as collateral.
    /// `Err` is the refusal, whole: the round ends on it rather than report the member gone.
    fn eject(&mut self, member: &Member, suites: &[String], owner: bool) -> Result<(), String>;
    /// Ejects a member a mender's return named, carrying the mender's diagnosis as the reason.
    fn eject_returned(
        &mut self,
        member: &Member,
        suites: &[String],
        _why: &str,
    ) -> Result<(), String> {
        self.eject(member, suites, true)
    }
    /// Ejects `member` on a filter's red, carrying the line that names it as the evidence.
    fn eject_on(
        &mut self,
        member: &Member,
        filter: &str,
        _line: &str,
        owner: bool,
    ) -> Result<(), String> {
        self.eject(member, &[filter.to_string()], owner)
    }
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
    /// A red no single member reproduces, still red on a changed input: call the Judge.
    fn judge(&mut self, suites: &[String], members: &[Id]);
    /// The workspace's packages and their dependencies on the merged tree; `None` when it cannot be read.
    fn package_graph(&self) -> Option<PackageGraph> {
        None
    }
    /// Each member's changed paths and added lines, for attributing a workspace-build red.
    fn diffs(&self, _members: &[Id]) -> Vec<MemberDiff> {
        vec![]
    }
    /// Per member whose diff against the round base adds or retires a config key, what it
    /// changes (`adds a, b; retires c`).
    fn config_key_changes(&self, _members: &[Id]) -> Vec<(Id, String)> {
        vec![]
    }
    /// Holds `member` behind `behind` — it leaves this round and is admitted again only once
    /// they have landed — carrying the compiler error as the reason.
    fn sequence(&mut self, member: &Member, behind: &[Id], line: &str) -> Result<(), String> {
        self.eject_on(
            member,
            WORKSPACE_BUILD,
            &format!("held behind {}: {line}", behind.join(",")),
            false,
        )
    }
    /// The base commit the round was cut on.
    fn base_sha(&self) -> String {
        String::new()
    }
    /// The last commit on the base (`<sha> <subject>`) that touched any of `paths`.
    fn last_touching(&self, _paths: &[String]) -> Option<String> {
        None
    }
    /// `kind`: `base`, `unattributed`, `integration` or `workspace-build`.
    fn incident(&mut self, kind: &str, suites: &[String]);
    /// The members whose diff adds the file `suite`, which the base lacks.
    fn adding(&self, _suite: &str, _members: &[Id]) -> Vec<Id> {
        vec![]
    }
    /// The members whose diff touches `suite`.
    fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id>;
    /// Disables every proven flip (registry ids) for the registry's hold, naming a follow-on
    /// bead for each, and puts the rows on the round's deny list; an error blocks the round.
    fn disable_flips(&mut self, tests: &[String]) -> Result<(), String>;
    fn record(&mut self, iteration: u32, decision: &Decision);
    /// `member` owns red `suite`; `rerun` is that suite rerun on the full round tree (`None`
    /// when the rerun faulted): green there means it flipped.
    fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>);
    /// Waits for every mender of this red pass and decides the next pass once. `None` when no
    /// mender owns the pass, which leaves the owners to be ejected as before.
    fn settle_mending(&mut self, _: &Decision, _: &[Member]) -> Option<Result<Mended, String>> {
        None
    }
    /// The current pass's settled RETURNED failures that name a member, read while the pass
    /// still runs.
    fn mender_returns(&self) -> Vec<crate::mender::Failure> {
        vec![]
    }
}

/// The round-level pseudo-suite name a --with-bins build failure (exit 4) reports under —
/// never a real `test-*.sh` file, so it can't collide with one a suite selection would pick.
pub const WORKSPACE_BUILD: &str = "workspace-build";

/// The check a merged tree's unappliable config delta is red under.
pub const CONFIG_DELTA: &str = "config-delta";

/// The incident kind of an attribution leg that made no progress for its stall bound.
pub const ATTRIBUTION_STALLED: &str = "attribution-stalled";

/// What a main run's error starts with when the pass built a tree the round was not cut for
/// because the base moved under it: the round is rebuilt once on the new base, never sent to
/// attribution as a harness fault.
pub const BASE_MOVED: &str = "round-vm: base moved from ";
pub const RUNNER_ABORT: &str = "runner-abort";
pub const CONFIG_FAULT: &str = "config-fault";

/// Ids of the settled-owner reruns start here, clear of the attributor's own.
const RERUN_ID_BASE: u64 = 1 << 40;

/// The time of the last progress of one polling wait, against the runner's stall bound.
struct Idle {
    limit: Option<u64>,
    last: u64,
    mark: u64,
}

impl Idle {
    fn new<R: RoundRunner>(runner: &mut R) -> Idle {
        Idle {
            limit: runner.stall_secs(),
            last: runner.now(),
            mark: runner.progress(),
        }
    }

    fn advanced<R: RoundRunner>(&mut self, runner: &R) {
        self.last = runner.now();
    }

    /// The seconds idle once they reach the bound.
    fn stalled<R: RoundRunner>(&mut self, runner: &mut R) -> Option<u64> {
        let mark = runner.progress();
        if mark != self.mark {
            self.mark = mark;
            self.last = runner.now();
        }
        let idle = runner.now().saturating_sub(self.last);
        self.limit.filter(|l| idle >= *l).map(|_| idle)
    }
}

/// `suite` once more on the round tree with every member present, to the end.
fn rerun_with_members<R: RoundRunner>(runner: &mut R, suite: &str, n: u64) -> Option<JobResult> {
    let job = Job {
        id: RERUN_ID_BASE + n,
        suite: suite.to_string(),
        removal: vec![],
        purpose: Purpose::Plain,
    };
    if runner.launch(&job).is_err() {
        return None;
    }
    let mut idle = Idle::new(runner);
    loop {
        let results = runner.poll_jobs();
        if !results.is_empty() {
            idle.advanced(runner);
        }
        if let Some((_, r)) = results.into_iter().find(|(id, _)| *id == job.id) {
            return (r != JobResult::Fault).then_some(r);
        }
        if idle.stalled(runner).is_some() {
            return None;
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
    pub filter: Option<FilterRed>,
    /// Returns that named a round member and stopped the pass before it settled.
    pub returned: Vec<crate::mender::Failure>,
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
    let prereqs: BTreeMap<Id, Vec<Id>> = members
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.stack
                    .keys()
                    .filter(|p| ids.contains(p))
                    .cloned()
                    .collect(),
            )
        })
        .collect();
    let mut a = Attributor::new(budget, Shape::new(&ids, &prereqs), suites.len());
    runner.set_members(members);
    runner.start_main(kind, suites)?;
    let mut seen = std::collections::BTreeSet::new();
    let mut main_end_at = None;
    let mut idle = Idle::new(runner);
    let mut flying: BTreeMap<u64, String> = BTreeMap::new();
    loop {
        if runner.abandoned() {
            runner.stop_main()?;
            let now = runner.now();
            return Ok(Settled {
                decision: Decision::default(),
                end: MainEnd::Abandoned,
                main_end_at: now,
                settled_at: now,
                filter: None,
                returned: vec![],
            });
        }
        let returned: Vec<_> = ops
            .mender_returns()
            .into_iter()
            .filter(|f| f.members.iter().any(|m| ids.contains(m)))
            .collect();
        if !returned.is_empty() {
            runner.stop_main()?;
            let now = runner.now();
            return Ok(Settled {
                decision: a.decision(),
                end: MainEnd::Ran,
                main_end_at: now,
                settled_at: now,
                filter: None,
                returned,
            });
        }
        let poll = runner.poll_main()?;
        let now = runner.now();
        for (suite, green) in poll.results {
            if seen.insert(suite.clone()) {
                idle.advanced(runner);
                if !green {
                    red_main(&mut a, ops, &suite, &ids, now);
                } else {
                    a.on_main_result(&suite, green, &[], now);
                }
            }
        }
        if let Some(end) = poll.done {
            if main_end_at.is_none() {
                idle.advanced(runner);
            }
            if matches!(end, MainEnd::WorkspaceBuild | MainEnd::ConfigDelta) {
                return Ok(Settled {
                    decision: Decision::default(),
                    end,
                    main_end_at: now,
                    settled_at: now,
                    filter: None,
                    returned: vec![],
                });
            }
            if kind == MainKind::Corpus && main_end_at.is_none() {
                if let Some(filter) = runner.filter_red() {
                    return Ok(Settled {
                        decision: Decision::default(),
                        end,
                        main_end_at: now,
                        settled_at: now,
                        filter: Some(filter),
                        returned: vec![],
                    });
                }
            }
            if main_end_at.is_none() {
                // Absence is never green: a selected suite with no result is red.
                for s in suites {
                    if seen.insert(s.clone()) {
                        red_main(&mut a, ops, s, &ids, now);
                    }
                }
                a.on_main_done(now);
                main_end_at = Some(now);
            }
        }
        for (id, r) in runner.poll_jobs() {
            idle.advanced(runner);
            flying.remove(&id);
            a.on_job_result(id, r, now);
        }
        for job in a.next_jobs() {
            if runner.abandoned() {
                break;
            }
            if let Err(e) = runner.launch(&job) {
                eprintln!(
                    "batcher: attribution job {} ({} without {:?}) not launched: {e}",
                    job.id, job.suite, job.removal
                );
                a.on_job_result(job.id, JobResult::Fault, now);
            } else {
                flying.insert(
                    job.id,
                    format!("{} without {}", job.suite, job.removal.join(",")),
                );
            }
        }
        if a.settled() {
            return Ok(Settled {
                decision: a.decision(),
                end: MainEnd::Ran,
                main_end_at: main_end_at.unwrap_or(now),
                settled_at: now,
                filter: None,
                returned: vec![],
            });
        }
        if let Some(idle_secs) = idle.stalled(runner) {
            runner.stop_main()?;
            let mut waiting: Vec<String> = suites
                .iter()
                .filter(|s| !seen.contains(*s))
                .cloned()
                .collect();
            waiting.extend(flying.into_values());
            return Ok(Settled {
                decision: a.decision(),
                end: MainEnd::Stalled { waiting, idle_secs },
                main_end_at: main_end_at.unwrap_or(now),
                settled_at: now,
                filter: None,
                returned: vec![],
            });
        }
        runner.wait();
    }
}

fn red_main<O: RoundOps + ?Sized>(a: &mut Attributor, ops: &O, suite: &str, ids: &[Id], now: u64) {
    match ops.adding(suite, ids) {
        adders if !adders.is_empty() => a.on_main_added(suite, &adders, now),
        _ => a.on_main_result(suite, false, &ops.suspects(suite, ids), now),
    }
}

/// Drops the suites that exist only through a member that has left the tree.
fn present_on<O: RoundOps>(
    ops: &O,
    suites: Vec<String>,
    start: &[Id],
    now: &[Member],
) -> Vec<String> {
    let now: Vec<Id> = now.iter().map(|m| m.id.clone()).collect();
    suites
        .into_iter()
        .filter(|s| ops.adding(s, start).is_empty() || !ops.adding(s, &now).is_empty())
        .collect()
}

const FLIP_JOB_BASE: u64 = 1 << 40;

/// A red naming more tests than this is not a flip: a flip is one or two flaky tests, not a broken package.
const MAX_FLIP_TESTS: usize = 5;

/// A flip is never shipped (law-a-test-that-flips-is-deleted): each flaky suite must run green
/// alone twice on the round tree, alone on the base, and alone on every member that touches
/// it; then it is deleted from the round. A plain run that is red while its twin is green is
/// the suite flipping again, which the law deletes; any other red blocks the round.
fn remove_flips<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    flaky: &[String],
    members: &[Member],
    budget: Budget,
) -> Result<(), NotAFlip> {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut next = FLIP_JOB_BASE;
    let mut push = |suite: &str, removal: Vec<Id>, purpose: Purpose| {
        queue.push_back(Job {
            id: next,
            suite: suite.to_string(),
            removal,
            purpose,
        });
        next += 1;
    };
    for suite in flaky {
        push(suite, vec![], Purpose::Plain);
        push(suite, vec![], Purpose::Plain);
        push(suite, ids.clone(), Purpose::Base);
        for x in ops.touching(suite, &ids) {
            let mut keep = vec![x.clone()];
            while let Some(p) = members
                .iter()
                .filter(|m| keep.contains(&m.id))
                .flat_map(|m| m.stack.keys())
                .find(|p| ids.contains(*p) && !keep.contains(*p))
            {
                keep.push(p.clone());
            }
            push(
                suite,
                ids.iter().filter(|i| !keep.contains(i)).cloned().collect(),
                Purpose::Without(x),
            );
        }
    }
    let suite_of: BTreeMap<u64, String> = queue.iter().map(|j| (j.id, j.suite.clone())).collect();
    let base_jobs: BTreeSet<u64> = queue
        .iter()
        .filter(|j| j.purpose == Purpose::Base)
        .map(|j| j.id)
        .collect();
    let plain_jobs: BTreeSet<u64> = queue
        .iter()
        .filter(|j| j.purpose == Purpose::Plain)
        .map(|j| j.id)
        .collect();
    let mut plain_green: BTreeSet<String> = BTreeSet::new();
    let mut plain_red: BTreeSet<String> = BTreeSet::new();
    let lone = members.len() == 1;
    let mut base_green = false;
    let mut faulted = false;
    let mut running = 0usize;
    let mut unproven: BTreeSet<String> = BTreeSet::new();
    let mut idle = Idle::new(runner);
    let mut flying: BTreeMap<u64, String> = BTreeMap::new();
    while !queue.is_empty() || running > 0 {
        while running < budget.slots as usize {
            let Some(job) = queue.pop_front() else { break };
            if unproven.contains(&job.suite) && !(lone && base_jobs.contains(&job.id)) {
                continue;
            }
            match runner.launch(&job) {
                Ok(()) => {
                    running += 1;
                    flying.insert(job.id, job.suite.clone());
                }
                Err(e) => {
                    eprintln!(
                        "batcher: flip proof job {} ({}) not launched: {e}",
                        job.id, job.suite
                    );
                    faulted = true;
                    unproven.insert(job.suite);
                }
            }
        }
        for (id, r) in runner.poll_jobs() {
            idle.advanced(runner);
            flying.remove(&id);
            if let Some(suite) = suite_of.get(&id) {
                running = running.saturating_sub(1);
                let plain = plain_jobs.contains(&id);
                match r {
                    JobResult::Green => {
                        base_green |= base_jobs.contains(&id);
                        if plain {
                            plain_green.insert(suite.clone());
                        }
                    }
                    JobResult::Red if plain => {
                        plain_red.insert(suite.clone());
                    }
                    _ => {
                        unproven.insert(suite.clone());
                        faulted |= r == JobResult::Fault;
                    }
                }
            }
        }
        if running > 0 {
            if idle.stalled(runner).is_some() {
                unproven.extend(flying.into_values());
                unproven.extend(queue.drain(..).map(|j| j.suite));
                faulted = true;
                break;
            }
            runner.wait();
        }
    }
    unproven.extend(plain_red.into_iter().filter(|s| !plain_green.contains(s)));
    if !unproven.is_empty() {
        let reds: Vec<String> = unproven.into_iter().collect();
        return Err(NotAFlip {
            reds,
            member_owns: base_green && !faulted,
        });
    }
    println!(
        "batcher: flip proven, disabling for the round: {}",
        flaky.join(",")
    );
    let ids: Vec<String> = flaky.iter().map(|s| format!("suite:{s}")).collect();
    ops.disable_flips(&ids).map_err(NotAFlip::fault)
}

/// A flaky suite that did not prove green. `member_owns`: the base ran green and no job faulted,
/// so with one member the red can belong to no one else.
struct NotAFlip {
    reds: Vec<String>,
    member_owns: bool,
}

impl NotAFlip {
    fn fault(why: String) -> Self {
        NotAFlip {
            reds: vec![why],
            member_owns: false,
        }
    }

    fn reason(&self) -> String {
        format!("red that is not a flip: {}", self.reds.join(","))
    }
}

pub struct Mended {
    pub next: crate::mender::Next,
    pub unattributed: Vec<String>,
    /// Every suite the pass's menders handled: what the next pass must prove green.
    pub suites: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RoundEnd {
    /// Land these members; `attribution_secs` is the longest per-red attribution wall.
    Land {
        members: Vec<Member>,
        attribution_secs: Option<u64>,
    },
    /// Nothing lands; the reason was already reported.
    Blocked(String),
    /// The attribution itself failed to run: nothing is judged, no member is charged, and the
    /// round stays open.
    Faulted(String),
    /// The attribution made no progress for the stall bound: stopped, the round stays open.
    Stalled(String),
    /// Nothing lands, but every culprit was ejected: the pool holds only survivors to cut again.
    Emptied(String),
    /// Someone else abandoned the round: it is over, and this driver writes no verdict.
    Abandoned,
    /// The base alone reproduces the red: no member is at fault and none is ejected.
    BaseRed { base: String, why: String },
}

/// A member stacked on one that leaves leaves with it.
fn with_dependents(members: &[Member], out: &mut BTreeMap<Id, Vec<String>>) {
    loop {
        let before = out.len();
        for m in members {
            if let Some(p) = m.stack.keys().find(|p| out.contains_key(*p)) {
                let suites = out[p].clone();
                out.entry(m.id.clone()).or_insert(suites);
            }
        }
        if out.len() == before {
            break;
        }
    }
}

/// Finds the members a filter red belongs to by running `scope` on member subsets: bisection,
/// the lone member, then each member alone.
fn probe_filter_owners<R: RoundRunner>(
    runner: &mut R,
    f: &FilterRed,
    tests: &str,
    scope: &[String],
    members: &[Member],
) -> (Vec<Action>, &'static str, String) {
    let red = format!("{} red{tests}", f.name);
    let mut said: Vec<(Vec<Id>, String)> = vec![];
    let mut green_in = |runner: &mut R, m: &[Member]| {
        let v = runner.filter_green_in(&f.name, scope, m);
        if v == Some(false) {
            said.push((
                m.iter().map(|x| x.id.clone()).collect(),
                runner.probe_output(),
            ));
        }
        v
    };
    let mut actions =
        bisect_members(&red, &mut |m| green_in(runner, m), members).unwrap_or_default();
    if actions.is_empty() {
        actions = sole_member_culprit(&red, members);
    }
    if actions.is_empty() && members.len() > 1 {
        match red_alone(&red, &mut |m| green_in(runner, m), members) {
            Alone::Owners(found) => {
                return (
                    with_probe_output(found, &said),
                    "unattributed",
                    String::new(),
                )
            }
            Alone::Base(said) => return (actions, "base", said),
            Alone::Cleared(said) => return (actions, "unattributed", said),
            Alone::Flipped(said) => {
                let kind = if runner.filter_green_in(&f.name, scope, members) == Some(true) {
                    "flipped"
                } else {
                    "unattributed"
                };
                return (actions, kind, said);
            }
        }
    }
    (
        with_probe_output(actions, &said),
        "unattributed",
        String::new(),
    )
}

/// How much of a red probe's output an eject note carries: the tail, where a failing check says why.
const MAX_PROBE_OUTPUT: usize = 1500;

/// Appends to each ejection the output of the red probe that convicted it: the probe of that
/// member alone when there was one, else the last red probe.
fn with_probe_output(actions: Vec<Action>, said: &[(Vec<Id>, String)]) -> Vec<Action> {
    actions
        .into_iter()
        .map(|a| {
            let id = action_id(&a).clone();
            let out = said
                .iter()
                .find(|(subset, _)| subset.as_slice() == [id.clone()])
                .or(said.last())
                .map(|(_, o)| o.trim())
                .filter(|o| !o.is_empty());
            let Some(out) = out else { return a };
            let from = (out.len().saturating_sub(MAX_PROBE_OUTPUT)..=out.len())
                .find(|i| out.is_char_boundary(*i))
                .unwrap_or(0);
            let tail = &out[from..];
            match a {
                Action::Eject { id, line } => Action::Eject {
                    id,
                    line: format!("{line}\n{tail}"),
                },
                Action::Sequence { id, behind, line } => Action::Sequence {
                    id,
                    behind,
                    line: format!("{line}\n{tail}"),
                },
            }
        })
        .collect()
}

/// A filter's red names its owners: they leave with the red as their evidence and the survivors
/// run the corpus again. A red naming no member cannot be attributed: it is filed (which holds
/// this membership) instead of re-cut — unless the runner can run the filter on member subsets,
/// which bisects the members.
fn eject_filter_owners<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    f: &FilterRed,
    members: &[Member],
) -> Result<FilterEject, String> {
    let named: Vec<&(Id, String)> = f
        .named
        .iter()
        .filter(|(id, _)| members.iter().any(|m| &m.id == id))
        .collect();
    if named.is_empty() {
        let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
        let graph = ops.package_graph();
        let mut packages = unitattr::failing_packages(&f.failed);
        if let Some(g) = &graph {
            packages = g.resolve(&packages);
        }
        let tests = noted_tests(&f.failed);
        let scope = unitattr::rerun_scope(&f.failed, graph.as_ref());
        let mapped = graph
            .as_ref()
            .map(|g| unitattr::reaching(g, &packages, &ops.diffs(&ids)))
            .unwrap_or_default();
        let mut actions: Vec<Action> = mapped
            .into_iter()
            .map(|(id, via)| Action::Eject {
                line: format!(
                    "{} red{tests} (mapped by package: this member's diff touches {via})",
                    f.name
                ),
                id,
            })
            .collect();
        let mut kind = "unattributed";
        let mut cleared = String::new();
        if actions.len() > 1 {
            let suspects: Vec<Member> = members
                .iter()
                .filter(|m| actions.iter().any(|a| action_id(a) == &m.id))
                .cloned()
                .collect();
            let (mut found, _, _) = probe_filter_owners(runner, f, &tests, &scope, &suspects);
            if found.is_empty() && scope != packages {
                (found, _, _) = probe_filter_owners(runner, f, &tests, &packages, &suspects);
            }
            if !found.is_empty() {
                actions = found;
            }
        }
        if actions.is_empty() {
            let narrow = probe_filter_owners(runner, f, &tests, &scope, members);
            (actions, kind, cleared) = narrow;
            if actions.is_empty() && matches!(kind, "unattributed" | "flipped") && scope != packages
            {
                (actions, kind, cleared) =
                    probe_filter_owners(runner, f, &tests, &packages, members);
            }
        }
        if actions.is_empty() && kind == "flipped" {
            let ids = flip_ids(&f.failed, graph.as_ref());
            if let Some(ids) = ids.filter(|ids| ids.len() <= MAX_FLIP_TESTS) {
                println!(
                    "batcher: {} red{tests} is red on the round tree, green on rerun, on the base and on every member alone — a flip",
                    f.name
                );
                return Ok(FilterEject::Flipped(ids));
            }
            kind = "unattributed";
        }
        if actions.is_empty() {
            let mut page = vec![f.name.clone()];
            page.extend(f.failed.iter().take(MAX_NOTED_TESTS).cloned());
            if !cleared.is_empty() {
                page.push(format!("run alone: {cleared}"));
            }
            ops.incident(kind, &page);
            if kind == "base" {
                let mut paths: Vec<String> = packages
                    .iter()
                    .filter_map(|p| graph.as_ref().and_then(|g| g.dirs.get(p)).cloned())
                    .collect();
                paths.sort();
                paths.dedup();
                return Ok(FilterEject::Base(base_why(
                    ops,
                    &paths,
                    &format!("{} red{tests} (run alone: {cleared})", f.name),
                )));
            }
            return Ok(FilterEject::Unnamed(format!(
                "{tests}{}",
                if cleared.is_empty() {
                    String::new()
                } else {
                    format!(" (run alone: {cleared})")
                }
            )));
        }
        let left = who_left(members, &leaving(members, &actions, &f.name));
        return Ok(
            match apply_build_actions(ops, members, &actions, &f.name)? {
                Some(next) => FilterEject::Survivors(next),
                None => FilterEject::Emptied(left),
            },
        );
    }
    let mut out: BTreeMap<Id, Vec<String>> = named
        .iter()
        .map(|(id, _)| (id.clone(), vec![f.name.clone()]))
        .collect();
    with_dependents(members, &mut out);
    for m in members {
        if out.contains_key(&m.id) {
            match named.iter().find(|(id, _)| *id == m.id) {
                Some((_, line)) => ops.eject_on(m, &f.name, line, true).map_err(refused)?,
                None => ops
                    .eject(m, std::slice::from_ref(&f.name), false)
                    .map_err(refused)?,
            }
        }
    }
    let survivors: Vec<Member> = members
        .iter()
        .filter(|m| !out.contains_key(&m.id))
        .cloned()
        .collect();
    let left = who_left(members, &out);
    if survivors.is_empty() {
        return Ok(FilterEject::Emptied(left));
    }
    let rebuilt = ops.rebuild(&survivors)?;
    Ok(if rebuilt.is_empty() {
        FilterEject::Emptied(left)
    } else {
        FilterEject::Survivors(rebuilt)
    })
}

/// The registry id of each failing test line, `None` when one cannot be named: a `dir:` word
/// no package owns, or a line that is not `<STATUS> <binary> <test>`.
pub fn flip_ids(failed: &[String], graph: Option<&PackageGraph>) -> Option<Vec<String>> {
    let mut ids: Vec<String> = vec![];
    for line in failed {
        let mut w = line.split_whitespace();
        let (Some(_), Some(binary), Some(test), None) = (w.next(), w.next(), w.next(), w.next())
        else {
            return None;
        };
        let id = if binary.starts_with("dir:") {
            let package = graph?.resolve(&[binary.to_string()]).remove(0);
            if package.starts_with("dir:") {
                return None;
            }
            format!("integration:{package} {test}")
        } else {
            format!("unit:{binary} {test}")
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    (!ids.is_empty()).then_some(ids)
}

/// How many failing tests an eject note lists before it counts the rest.
const MAX_NOTED_TESTS: usize = 20;

fn noted_tests(failed: &[String]) -> String {
    match failed.len() {
        0 => String::new(),
        n if n <= MAX_NOTED_TESTS => format!(": {}", failed.join("; ")),
        n => format!(
            ": {}; and {} more",
            failed[..MAX_NOTED_TESTS].join("; "),
            n - MAX_NOTED_TESTS
        ),
    }
}

/// A round of one member has no one else to blame: a red nothing else names is that member's.
fn sole_member_culprit(red: &str, members: &[Member]) -> Vec<Action> {
    match members {
        [only] => vec![Action::Eject {
            id: only.id.clone(),
            line: format!("{red} in a round whose only member is {}", only.id),
        }],
        _ => vec![],
    }
}

/// A one-member round red in the corpus pass ends by ejecting that member: no rerun, no probe.
fn eject_sole_member<O: RoundOps>(
    ops: &mut O,
    members: &[Member],
    red: &str,
    filter: &str,
) -> Result<RoundEnd, String> {
    let actions = sole_member_culprit(red, members);
    let left = who_left(members, &leaving(members, &actions, filter));
    apply_build_actions(ops, members, &actions, filter)?;
    Ok(RoundEnd::Emptied(format!("{filter} red {left}")))
}

/// A one-member round red on several suites ejects that member on the suites themselves, so
/// the record reads each suite's own failing lines.
fn eject_sole_member_on_suites<O: RoundOps>(
    ops: &mut O,
    members: &[Member],
    red: &str,
    suites: &[String],
) -> Result<RoundEnd, String> {
    let [only] = members else {
        return Err("a sole-member eject needs exactly one member".to_string());
    };
    let named = suites.join(",");
    let left = who_left(
        members,
        &leaving(members, &sole_member_culprit(red, members), &named),
    );
    ops.eject(only, suites, true).map_err(refused)?;
    Ok(RoundEnd::Emptied(format!("{named} red {left}")))
}

/// A red that survived its attribution belongs to the members whose diffs the red suites cover:
/// one suspect leaves; several are each run alone; none, or none red alone, runs every member alone.
fn confirmed_attribution<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    members: &[Member],
    reds: &[String],
) -> Result<RoundEnd, String> {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    let touched: Vec<(&String, Vec<Id>)> = reds.iter().map(|s| (s, ops.touching(s, &ids))).collect();
    let suspects: Vec<Member> = members
        .iter()
        .filter(|m| touched.iter().any(|(_, t)| t.contains(&m.id)))
        .cloned()
        .collect();
    let named = reds.join(",");
    let mut found: Vec<Action> = vec![];
    let mut base_red: Vec<String> = vec![];
    let mut said: Vec<String> = vec![];
    if let [only] = suspects.as_slice() {
        let its: Vec<String> = touched.iter().filter(|(_, t)| t.contains(&only.id)).map(|(s, _)| (*s).clone()).collect();
        println!("batcher: {named} red on two certify passes; {} is the only member whose diff they cover", only.id);
        ops.eject(only, &its, false).map_err(refused)?;
        let out = leaving(members, &[Action::Eject { id: only.id.clone(), line: String::new() }], &named);
        let left = who_left(members, &out);
        let survivors: Vec<Member> = members.iter().filter(|m| !out.contains_key(&m.id)).cloned().collect();
        let rebuilt = if survivors.is_empty() { vec![] } else { ops.rebuild(&survivors)? };
        return Ok(if rebuilt.is_empty() {
            RoundEnd::Emptied(format!("{named} red {left}"))
        } else {
            RoundEnd::Land { members: rebuilt, attribution_secs: None }
        });
    }
    let mut pools: Vec<&[Member]> = vec![];
    if suspects.len() > 1 {
        pools.push(&suspects);
    }
    pools.push(members);
    for pool in pools {
        said.clear();
        for suite in reds {
            match red_alone(&format!("{suite} red"), &mut |m| runner.suite_green(suite, m), pool) {
                Alone::Owners(a) => {
                    for act in a {
                        if !found.iter().any(|f| action_id(f) == action_id(&act)) {
                            found.push(act);
                        }
                    }
                }
                Alone::Base(w) => {
                    base_red.push(suite.clone());
                    said.push(format!("{suite}: {w}"));
                }
                Alone::Cleared(w) => said.push(format!("{suite}: {w}")),
            }
        }
        if !found.is_empty() || !base_red.is_empty() {
            break;
        }
    }
    if !found.is_empty() {
        return Ok(match apply_build_actions(ops, members, &found, &named)? {
            Some(next) => RoundEnd::Land { members: next, attribution_secs: None },
            None => RoundEnd::Emptied(format!("{named} red ejected every member")),
        });
    }
    if !base_red.is_empty() {
        ops.incident("base", &base_red);
        let paths: Vec<String> = base_red.iter().map(|s| format!(":(glob)**/{s}")).collect();
        return Ok(RoundEnd::BaseRed {
            base: ops.base_sha(),
            why: base_why(ops, &paths, &format!("{} (run alone: {})", base_red.join(","), said.join("; "))),
        });
    }
    ops.judge(reds, &ids);
    Ok(RoundEnd::Blocked(format!("unattributed red: {named} (run alone: {})", said.join("; "))))
}

/// The base-red reason: what failed, then the last commit on the base that touched it.
fn base_why<O: RoundOps>(ops: &O, paths: &[String], red: &str) -> String {
    match ops.last_touching(paths) {
        Some(line) => format!("{red}; last change touching it: {line}"),
        None => red.to_string(),
    }
}

enum FilterEject {
    Base(String),
    Unnamed(String),
    /// Nothing is left to land; who left and who stayed.
    Emptied(String),
    Survivors(Vec<Member>),
    /// The registry ids of tests proven to flip: they are disabled and the same members certify again.
    Flipped(Vec<String>),
}

/// The smallest `k` for which the first `k` of `members` fail to build, given that all of them do.
fn first_failing_prefix(
    green: &mut dyn FnMut(&[Member]) -> Option<bool>,
    members: &[Member],
) -> Option<usize> {
    let (mut lo, mut hi) = (0, members.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if green(&members[..mid])? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(hi)
}

/// Finds by prefix search what turns `red` on when no output names a member: one member alone,
/// or a pair, whose later member is held behind the earlier. `None` for three or more, or when
/// the runner cannot probe subsets.
fn bisect_members(
    red: &str,
    green: &mut dyn FnMut(&[Member]) -> Option<bool>,
    members: &[Member],
) -> Option<Vec<Action>> {
    let k = first_failing_prefix(green, members)?;
    if k == 1 && members.len() > 1 && !green(&[])? {
        return None;
    }
    let last = &members[k - 1];
    let line = |what: &str| format!("{red} {what} (bisected by running member subsets)");
    if !green(std::slice::from_ref(last))? {
        return Some(vec![Action::Eject {
            id: last.id.clone(),
            line: line(&format!("with only {} merged", last.id)),
        }]);
    }
    let with_last = |n: usize| -> Vec<Member> {
        members[..n]
            .iter()
            .chain(std::iter::once(last))
            .cloned()
            .collect()
    };
    let (mut lo, mut hi) = (0, k - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if green(&with_last(mid))? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let partner = members.get(hi.checked_sub(1)?)?;
    if green(&[partner.clone(), last.clone()])? {
        return None;
    }
    Some(vec![Action::Sequence {
        id: last.id.clone(),
        behind: vec![partner.id.clone()],
        line: line(&format!("only with {} merged", partner.id)),
    }])
}

/// A workspace-build red whose compiler errors map to members: owners leave with the error as
/// their evidence, a member lacking what another adds is held behind it, the rest re-cut. An
/// error that maps to no file is bisected by building member subsets. `None` when no member can
/// be named, which leaves the red to be filed.
fn eject_build_culprits<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    members: &[Member],
) -> Result<Option<Vec<Member>>, String> {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    let mut actions = buildattr::attribute(
        &buildattr::compile_errors(&runner.build_output()),
        &ops.diffs(&ids),
    );
    if actions.is_empty() {
        actions = bisect_members(
            "the workspace fails to build",
            &mut |m| runner.builds(m),
            members,
        )
        .unwrap_or_default();
    }
    apply_build_actions(ops, members, &actions, WORKSPACE_BUILD)
}

/// Ejects or sequences the members `actions` name, and the members stacked on them, and
/// re-cuts the rest; `None` when there is nothing to name or nobody left.
fn apply_build_actions<O: RoundOps>(
    ops: &mut O,
    members: &[Member],
    actions: &[Action],
    filter: &str,
) -> Result<Option<Vec<Member>>, String> {
    if actions.is_empty() {
        return Ok(None);
    }
    let out = leaving(members, actions, filter);
    for m in members {
        match actions.iter().find(
            |a| matches!(a, Action::Eject { id, .. } | Action::Sequence { id, .. } if *id == m.id),
        ) {
            Some(Action::Eject { line, .. }) => {
                ops.eject_on(m, filter, line, true).map_err(refused)?
            }
            Some(Action::Sequence { behind, line, .. }) => {
                ops.sequence(m, behind, line).map_err(refused)?
            }
            None if out.contains_key(&m.id) => ops
                .eject(m, &[filter.to_string()], false)
                .map_err(refused)?,
            None => {}
        }
    }
    let survivors: Vec<Member> = members
        .iter()
        .filter(|m| !out.contains_key(&m.id))
        .cloned()
        .collect();
    if survivors.is_empty() {
        return Ok(None);
    }
    let rebuilt = ops.rebuild(&survivors)?;
    Ok((!rebuilt.is_empty()).then_some(rebuilt))
}

/// The members the actions take out of the round, with the members stacked on them.
fn leaving(members: &[Member], actions: &[Action], filter: &str) -> BTreeMap<Id, Vec<String>> {
    let mut out: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    for a in actions {
        let (Action::Eject { id, .. } | Action::Sequence { id, .. }) = a;
        out.insert(id.clone(), vec![filter.to_string()]);
    }
    with_dependents(members, &mut out);
    out
}

/// Who left a round and who stayed, for the reason a round that ended carries.
fn who_left(members: &[Member], out: &BTreeMap<Id, Vec<String>>) -> String {
    let ids = |left: bool| {
        members
            .iter()
            .filter(|m| out.contains_key(&m.id) == left)
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    match ids(false).as_str() {
        "" => format!("ejected every member ({})", ids(true)),
        kept => format!("ejected {}, kept {kept}", ids(true)),
    }
}

fn action_id(a: &Action) -> &Id {
    let (Action::Eject { id, .. } | Action::Sequence { id, .. }) = a;
    id
}

/// What running a red check on the base and on the base plus each member alone found.
enum Alone {
    /// Members whose tree alone is red, each with the per-member verdicts as its evidence.
    Owners(Vec<Action>),
    /// The base alone is red: no member is at fault.
    Base(String),
    /// No member's tree alone is red, or a run faulted before one could be named.
    Cleared(String),
    /// The base and every member alone are green: red only together, or a flip.
    Flipped(String),
}

fn verdict_word(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "green",
        Some(false) => "red",
        None => "unknown",
    }
}

/// Runs a check no output ties to a member on the base and then on the base plus each member
/// alone, and names every member whose tree is red. The verdicts are printed to the round log.
fn red_alone(
    red: &str,
    green: &mut dyn FnMut(&[Member]) -> Option<bool>,
    members: &[Member],
) -> Alone {
    let base = green(&[]);
    match base {
        None => return Alone::Cleared(String::new()),
        Some(false) => {
            println!("batcher: {red}: run alone: base red");
            return Alone::Base("base red".into());
        }
        Some(true) => {}
    }
    let verdicts: Vec<(&Member, Option<bool>)> = members
        .iter()
        .map(|m| (m, green(std::slice::from_ref(m))))
        .collect();
    let said = std::iter::once("base green".to_string())
        .chain(
            verdicts
                .iter()
                .map(|(m, v)| format!("{} {}", m.id, verdict_word(*v))),
        )
        .collect::<Vec<_>>()
        .join(", ");
    println!("batcher: {red}: run alone: {said}");
    let owners: Vec<Action> = verdicts
        .iter()
        .filter(|(_, v)| *v == Some(false))
        .map(|(m, _)| Action::Eject {
            id: m.id.clone(),
            line: format!(
                "{red} with only {} merged onto the base (run alone: {said})",
                m.id
            ),
        })
        .collect();
    if owners.is_empty() {
        if verdicts.iter().all(|(_, v)| *v == Some(true)) {
            Alone::Flipped(said)
        } else {
            Alone::Cleared(said)
        }
    } else {
        Alone::Owners(owners)
    }
}

/// A main run that died on a signal belongs to one member: the only one that changes the code that
/// runs the suites, else the one whose tree alone kills the runner, found by running member subsets.
/// It leaves with the failure as its evidence; `None` when no single member can be named, an empty
/// survivor list when it was the last.
fn eject_runner_abort<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    members: &[Member],
    err: &str,
) -> Result<Option<Vec<Member>>, String> {
    if let Some(next) = eject_config_fault(ops, members, err)? {
        return Ok(Some(next));
    }
    if !buildattr::is_signal_death(err) {
        return Ok(None);
    }
    let diffs = ops.diffs(&members.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    let tail = buildattr::error_tail(err);
    let action = match buildattr::runner_touchers(&diffs)[..] {
        [owner] => Some(Action::Eject { id: owner.id.clone(), line: format!("testenv was killed by a signal in a round where this member changes round-vm/ or testenv/: {tail}") }),
        _ => {
            let red = format!("testenv was killed by a signal ({tail})");
            let mut found = sole_member_culprit(&red, members);
            if found.is_empty() {
                found = bisect_members(&red, &mut |m| runner.runner_survives(m), members).unwrap_or_default();
            }
            found.into_iter().find(|a| matches!(a, Action::Eject { .. }))
        }
    };
    let Some(action) = action else {
        return Ok(None);
    };
    Ok(Some(
        apply_build_actions(ops, members, &[action], RUNNER_ABORT)?.unwrap_or_default(),
    ))
}

/// An image or config-resolution fault names the members whose diff adds or retires a config
/// key and ejects them with the fault's own lines; with no such member it stays a harness fault.
fn eject_config_fault<O: RoundOps>(
    ops: &mut O,
    members: &[Member],
    err: &str,
) -> Result<Option<Vec<Member>>, String> {
    if !buildattr::is_config_fault(err) {
        return Ok(None);
    }
    let tail = buildattr::error_tail(err);
    let actions: Vec<Action> = ops
        .config_key_changes(&members.iter().map(|m| m.id.clone()).collect::<Vec<_>>())
        .into_iter()
        .map(|(id, change)| Action::Eject {
            id,
            line: format!("this member's diff changes config keys ({change}) and the merged tree cannot resolve its config: {tail}"),
        })
        .collect();
    if actions.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        apply_build_actions(ops, members, &actions, CONFIG_FAULT)?.unwrap_or_default(),
    ))
}

/// A red the reruns could not attribute belongs to the member when exactly one member's diff
/// touches the suite; one that touches none or several stays unattributed.
fn own_by_touch<O: RoundOps>(ops: &O, d: &mut Decision, members: &[Member]) {
    let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
    for suite in std::mem::take(&mut d.unattributed) {
        match ops.touching(&suite, &ids)[..] {
            [ref owner] => {
                println!("batcher: red {suite} could not be attributed by rerun; only {owner}'s diff touches it");
                d.owners.entry(owner.clone()).or_default().push(suite);
            }
            _ => d.unattributed.push(suite),
        }
    }
}

const EJECT_REFUSED: &str = "\u{1}eject-refused\u{1}";

fn refused(reason: String) -> String {
    format!("{EJECT_REFUSED}{reason}")
}

/// The whole round: corpus with concurrent attribution; then, while anyone was ejected, the
/// owners' suites (only those) on the survivors' release build, attributed the same way. A
/// refused eject ends it `Blocked` with the refusal as the reason.
pub fn attribute_round<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    corpus: &[String],
    starting: Vec<Member>,
    budget: Budget,
) -> Result<RoundEnd, String> {
    match attribute_members(runner, ops, corpus, starting, budget) {
        Err(e) => match e.strip_prefix(EJECT_REFUSED) {
            Some(reason) => Ok(RoundEnd::Blocked(reason.to_string())),
            None => Err(e),
        },
        ok => ok,
    }
}

fn attribute_members<R: RoundRunner, O: RoundOps>(
    runner: &mut R,
    ops: &mut O,
    corpus: &[String],
    starting: Vec<Member>,
    budget: Budget,
) -> Result<RoundEnd, String> {
    let start: Vec<Id> = starting.iter().map(|m| m.id.clone()).collect();
    let mut members = starting;
    let mut suites = corpus.to_vec();
    let mut kind = MainKind::Corpus;
    let mut worst: Option<u64> = None;
    let mut fixed = false;
    let bound = members.len() as u32 + 2;
    let mut retried = false;
    let mut image_retried = false;
    let mut base_retried = false;
    let t0 = runner.now();
    runner.set_members(&members);
    if runner.abandoned() {
        return Ok(RoundEnd::Abandoned);
    }
    if let Some(f) = runner.prior_red() {
        println!(
            "batcher: {} red in the corpus pass — attributing it by its failing tests, without running the corpus again",
            f.name
        );
        if let [_] = members[..] {
            let red = format!("{} red{}", f.name, noted_tests(&f.failed));
            return eject_sole_member(ops, &members, &red, &f.name);
        }
        match eject_filter_owners(runner, ops, &f, &members)? {
            FilterEject::Flipped(ids) => {
                if let Err(e) = ops.disable_flips(&ids) {
                    return Ok(RoundEnd::Blocked(format!(
                        "cannot disable the flip {}: {e}",
                        ids.join(", ")
                    )));
                }
            }
            FilterEject::Survivors(members) => {
                return Ok(RoundEnd::Land {
                    members,
                    attribution_secs: None,
                })
            }
            FilterEject::Emptied(left) => {
                return Ok(RoundEnd::Emptied(format!("{} red {left}", f.name)))
            }
            FilterEject::Base(why) => {
                return Ok(RoundEnd::BaseRed {
                    base: ops.base_sha(),
                    why,
                })
            }
            FilterEject::Unnamed(tests) => {
                return Ok(RoundEnd::Blocked(format!(
                    "{} red{tests} names no member that can be ejected",
                    f.name
                )))
            }
        }
    }
    let prior_reds: Vec<String> = runner
        .prior_red_suites()
        .into_iter()
        .filter(|s| corpus.contains(s))
        .collect();
    if !prior_reds.is_empty() {
        println!(
            "batcher: {} suite(s) red in the corpus pass — attributing them alone, without the corpus again",
            prior_reds.len()
        );
        if let [_] = members[..] {
            let red = format!("{} red in the corpus pass", prior_reds.join(","));
            return eject_sole_member_on_suites(ops, &members, &red, &prior_reds);
        }
        if runner.confirmed() {
            return confirmed_attribution(runner, ops, &members, &prior_reds);
        }
        suites = prior_reds;
        kind = MainKind::Corpus;
    }
    for iteration in 0..bound {
        if runner.abandoned() {
            return Ok(RoundEnd::Abandoned);
        }
        suites = present_on(&*ops, suites, &start, &members);
        if suites.is_empty() {
            return Ok(RoundEnd::Land {
                members,
                attribution_secs: worst,
            });
        }
        let s = match drive(runner, &*ops, kind, &suites, &members, budget) {
            Ok(s) => s,
            Err(_) if runner.abandoned() => return Ok(RoundEnd::Abandoned),
            Err(e) if e.starts_with(BASE_MOVED) && !base_retried => {
                base_retried = true;
                println!("batcher: {e} — rebuilding the round once on the new base");
                let _ = runner.stop_main();
                match ops.base_moved(&members)? {
                    Some((sha, rebuilt)) => {
                        runner.set_base(&sha);
                        members = rebuilt;
                    }
                    None => members = ops.rebuild(&members)?,
                }
                continue;
            }
            Err(e) => match eject_runner_abort(runner, ops, &members, &e)? {
                Some(next) if next.is_empty() => {
                    return Ok(RoundEnd::Emptied(
                        "the runner failed and every member left".into(),
                    ))
                }
                Some(next) => {
                    println!("batcher: the runner failed — the member whose diff owns the failure left, re-cutting {} member(s)", next.len());
                    let _ = runner.stop_main();
                    members = next;
                    continue;
                }
                None => return Err(e),
            },
        };
        if s.end == MainEnd::Abandoned {
            return Ok(RoundEnd::Abandoned);
        }
        if let MainEnd::Stalled { waiting, idle_secs } = &s.end {
            ops.incident(ATTRIBUTION_STALLED, waiting);
            return Ok(RoundEnd::Stalled(format!(
                "attribution stalled: no progress for {idle_secs}s, waiting on {}",
                waiting.join(", ")
            )));
        }
        if s.end == MainEnd::ConfigDelta {
            let actions = bisect_members(
                "the merged config delta does not apply",
                &mut |m| runner.config_applies(m),
                &members,
            )
            .unwrap_or_default();
            if let Some(next) = apply_build_actions(ops, &members, &actions, CONFIG_DELTA)? {
                println!("batcher: the merged config delta does not apply — culprits left, re-cutting {} member(s)", next.len());
                runner.stop_main()?;
                members = next;
                continue;
            }
            ops.incident(CONFIG_DELTA, &[CONFIG_DELTA.to_string()]);
            return Ok(RoundEnd::Blocked(
                "the merged config delta does not apply and no member's merge is to blame".into(),
            ));
        }
        if s.end == MainEnd::WorkspaceBuild {
            let output = runner.build_output();
            if buildattr::is_image_build_red(&output) {
                let diffs = ops.diffs(&members.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
                match buildattr::recipe_touchers(&diffs)[..] {
                    [owner] => {
                        let line = format!(
                            "the round image build failed and this member changes its recipe: {}",
                            buildattr::error_tail(&output)
                        );
                        if let Some(next) = apply_build_actions(
                            ops,
                            &members,
                            &[Action::Eject {
                                id: owner.id.clone(),
                                line,
                            }],
                            WORKSPACE_BUILD,
                        )? {
                            println!("batcher: the round image build failed — {} left, re-cutting {} member(s)", owner.id, next.len());
                            runner.stop_main()?;
                            members = next;
                            continue;
                        }
                    }
                    [] if !image_retried => {
                        image_retried = true;
                        println!("batcher: the round image build failed and no member changes its recipe — a harness fault, retrying once");
                        runner.stop_main()?;
                        continue;
                    }
                    _ => {}
                }
                ops.incident("workspace-build", &[WORKSPACE_BUILD.to_string()]);
                return Ok(RoundEnd::Blocked("the round's image build failed".into()));
            }
            if let Some(next) = eject_build_culprits(runner, ops, &members)? {
                println!("batcher: the round's workspace failed to build — culprits left, re-cutting {} member(s)", next.len());
                runner.stop_main()?;
                members = next;
                continue;
            }
            ops.incident("workspace-build", &[WORKSPACE_BUILD.to_string()]);
            return Ok(RoundEnd::Blocked(
                "the round's workspace failed to build".into(),
            ));
        }
        if let Some(f) = &s.filter {
            println!(
                "batcher: {} red before any suite ran, naming {} member(s)",
                f.name,
                f.named.len()
            );
            let next = match eject_filter_owners(runner, ops, f, &members)? {
                FilterEject::Survivors(next) => next,
                FilterEject::Flipped(ids) => {
                    if let Err(e) = ops.disable_flips(&ids) {
                        return Ok(RoundEnd::Blocked(format!(
                            "cannot disable the flip {}: {e}",
                            ids.join(", ")
                        )));
                    }
                    members.clone()
                }
                FilterEject::Emptied(left) => {
                    return Ok(RoundEnd::Emptied(format!("{} red {left}", f.name)))
                }
                FilterEject::Base(why) => {
                    return Ok(RoundEnd::BaseRed {
                        base: ops.base_sha(),
                        why,
                    })
                }
                FilterEject::Unnamed(tests) => {
                    return Ok(RoundEnd::Blocked(format!(
                        "{} red{tests} names no member that can be ejected",
                        f.name
                    )))
                }
            };
            runner.stop_main()?;
            members = next;
            continue;
        }
        if !s.returned.is_empty() {
            let mut out: BTreeMap<Id, Vec<String>> = BTreeMap::new();
            let mut why: BTreeMap<Id, String> = BTreeMap::new();
            for f in &s.returned {
                for id in f
                    .members
                    .iter()
                    .filter(|id| members.iter().any(|m| &m.id == *id))
                {
                    let suites = out.entry(id.clone()).or_default();
                    if !suites.contains(&f.suite) {
                        suites.push(f.suite.clone());
                    }
                    why.entry(id.clone()).or_insert_with(|| f.diagnosis.clone());
                }
            }
            let named: Vec<Id> = out.keys().cloned().collect();
            with_dependents(&members, &mut out);
            println!(
                "batcher: iteration {iteration}: a mender returned {} mid-pass — the pass stops and they leave",
                named.join(",")
            );
            for m in members.iter().filter(|m| out.contains_key(&m.id)) {
                match why.get(&m.id) {
                    Some(why) => ops.eject_returned(m, &out[&m.id], why),
                    None => ops.eject(m, &out[&m.id], false),
                }
                .map_err(refused)?;
            }
            let mut next_suites: Vec<String> = s.returned.iter().map(|f| f.suite.clone()).collect();
            next_suites.extend(s.decision.records.iter().map(|r| r.suite.clone()));
            next_suites.sort();
            next_suites.dedup();
            let survivors: Vec<Member> = members
                .iter()
                .filter(|m| !out.contains_key(&m.id))
                .cloned()
                .collect();
            if survivors.is_empty() {
                return Ok(RoundEnd::Emptied("round emptied by ejection".into()));
            }
            members = ops.rebuild(&survivors)?;
            if members.is_empty() {
                return Ok(RoundEnd::Emptied(
                    "round emptied rebuilding the tree after the menders".into(),
                ));
            }
            suites = next_suites;
            kind = MainKind::Verify;
            continue;
        }
        let mut d = s.decision;
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
                if r.settled_before_main_end {
                    ", settled before the main run ended"
                } else {
                    ""
                }
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
            return Ok(RoundEnd::Blocked(format!(
                "integration red survived the in-round fix: {}",
                integration.join(",")
            )));
        }
        own_by_touch(&*ops, &mut d, &members);
        let faulted: Vec<&String> = d
            .unattributed
            .iter()
            .filter(|s| d.faulted.contains(s))
            .collect();
        if !faulted.is_empty() {
            let why = format!(
                "attribution faulted: its reruns of {} could not run",
                faulted
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            println!("batcher: {why}");
            return Ok(RoundEnd::Faulted(why));
        }
        if !d.unattributed.is_empty() {
            if !retried && d.owners.is_empty() {
                retried = true;
                if let Some((sha, rebuilt)) = ops.base_moved(&members)? {
                    println!(
                        "batcher: unattributed red {} — base moved, retrying once on {sha}",
                        d.unattributed.join(",")
                    );
                    runner.set_base(&sha);
                    members = rebuilt;
                    suites = d.unattributed.clone();
                    kind = MainKind::Verify;
                    continue;
                }
            }
            let red = format!("unattributed red {}", d.unattributed.join(","));
            let alone = sole_member_culprit(&red, &members);
            if !alone.is_empty() {
                let named = d.unattributed.join(",");
                println!(
                    "batcher: {named} red names no member; the round's only member is the culprit"
                );
                let unattributed = d.unattributed.clone();
                eject_sole_member_on_suites(ops, &members, &red, &unattributed)?;
                return Ok(RoundEnd::Emptied(format!(
                    "{named} red ejected the only member"
                )));
            }
            let mut found: Vec<Action> = vec![];
            let mut base_red: Vec<String> = vec![];
            let mut said: Vec<String> = vec![];
            if members.len() > 1 {
                for suite in &d.unattributed {
                    match red_alone(
                        &format!("{suite} red"),
                        &mut |m| runner.suite_green(suite, m),
                        &members,
                    ) {
                        Alone::Owners(a) => {
                            for act in a {
                                if !found.iter().any(|f| action_id(f) == action_id(&act)) {
                                    found.push(act);
                                }
                            }
                        }
                        Alone::Base(w) => {
                            base_red.push(suite.clone());
                            said.push(format!("{suite}: {w}"));
                        }
                        Alone::Cleared(w) | Alone::Flipped(w) => said.push(format!("{suite}: {w}")),
                    }
                }
            }
            if !found.is_empty() {
                let named = d.unattributed.join(",");
                match apply_build_actions(ops, &members, &found, &named)? {
                    Some(next) => {
                        println!("batcher: {named} red names no member; run alone it was red for {} — re-cutting {} member(s)", found.iter().map(|a| action_id(a).as_str()).collect::<Vec<_>>().join(","), next.len());
                        runner.stop_main()?;
                        members = next;
                        suites = d.unattributed.clone();
                        kind = MainKind::Verify;
                        continue;
                    }
                    None => {
                        return Ok(RoundEnd::Emptied(format!(
                            "{named} red ejected every member"
                        )))
                    }
                }
            }
            if !base_red.is_empty() {
                ops.incident("base", &base_red);
                let paths: Vec<String> =
                    base_red.iter().map(|s| format!(":(glob)**/{s}")).collect();
                return Ok(RoundEnd::BaseRed {
                    base: ops.base_sha(),
                    why: base_why(
                        ops,
                        &paths,
                        &format!("{} (run alone: {})", base_red.join(","), said.join("; ")),
                    ),
                });
            }
            let ids: Vec<Id> = members.iter().map(|m| m.id.clone()).collect();
            ops.judge(&d.unattributed, &ids);
            let alone = if said.is_empty() {
                String::new()
            } else {
                format!(" (run alone: {})", said.join("; "))
            };
            return Ok(RoundEnd::Blocked(format!(
                "unattributed red: {}{alone}",
                d.unattributed.join(",")
            )));
        }
        if d.owners.is_empty() {
            if !d.flaky.is_empty() {
                if let Err(red) = remove_flips(runner, ops, &d.flaky, &members, budget) {
                    if let ([only], true) = (members.as_slice(), red.member_owns) {
                        ops.eject(only, &red.reds, true).map_err(refused)?;
                        return Ok(RoundEnd::Emptied("round emptied by ejection".into()));
                    }
                    ops.incident("unattributed", &red.reds);
                    return Ok(RoundEnd::Blocked(red.reason()));
                }
            }
            return Ok(RoundEnd::Land {
                members,
                attribution_secs: worst,
            });
        }
        let mended = ops.settle_mending(&d, &members);
        if let Some(settled) = mended {
            let m = settled?;
            if !m.unattributed.is_empty() {
                println!(
                    "batcher: ALARM mender returns named no member: {}",
                    m.unattributed.join(",")
                );
                ops.incident("unattributed", &m.unattributed);
            }
            match m.next {
                crate::mender::Next::Stop { page } => {
                    ops.incident("mender-page", &m.suites);
                    return Ok(RoundEnd::Blocked(page));
                }
                crate::mender::Next::Pass {
                    members: next,
                    left,
                    retry,
                } => {
                    for l in &left {
                        if let Some(member) = members.iter().find(|x| x.id == l.id) {
                            let own = d
                                .owners
                                .get(&l.id)
                                .cloned()
                                .unwrap_or_else(|| m.suites.clone());
                            if l.is_stacked_dependent(&members) {
                                ops.eject(member, &own, false)
                            } else {
                                ops.eject_returned(member, &own, &l.why)
                            }
                            .map_err(refused)?;
                        }
                    }
                    if next.is_empty() {
                        return Ok(RoundEnd::Emptied("round emptied by ejection".into()));
                    }
                    members = if retry && left.is_empty() {
                        next
                    } else {
                        ops.rebuild(&next)?
                    };
                    if members.is_empty() {
                        return Ok(RoundEnd::Emptied(
                            "round emptied rebuilding the tree after the menders".into(),
                        ));
                    }
                    suites = m.suites;
                    kind = MainKind::Verify;
                    continue;
                }
            }
        }
        // Every owner leaves with its suites; a member stacked on an owner leaves with it.
        let mut out: BTreeMap<Id, Vec<String>> = d.owners.clone();
        with_dependents(&members, &mut out);
        let mut n = 0;
        for m in members.iter() {
            for suite in d.owners.get(&m.id).into_iter().flatten() {
                n += 1;
                let rerun = rerun_with_members(runner, suite, n);
                ops.escape(m, suite, rerun);
            }
        }
        for m in &members {
            if let Some(s) = out.get(&m.id) {
                ops.eject(m, s, d.owners.contains_key(&m.id))
                    .map_err(refused)?;
            }
        }
        let survivors: Vec<Member> = members
            .iter()
            .filter(|m| !out.contains_key(&m.id))
            .cloned()
            .collect();
        if survivors.is_empty() {
            return Ok(RoundEnd::Emptied("round emptied by ejection".into()));
        }
        members = ops.rebuild(&survivors)?;
        if members.is_empty() {
            return Ok(RoundEnd::Emptied(
                "round emptied rebuilding the tree after ejection".into(),
            ));
        }
        suites = d.owned_suites();
        kind = MainKind::Verify;
    }
    Ok(RoundEnd::Blocked(format!(
        "still ejecting after {bound} iterations"
    )))
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
        /// Suites whose first flip-proof run on the whole round tree is red, once.
        pub flips_again: BTreeSet<String>,
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
        pub stopped: u32,
        pub on_base: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        /// A lint-like filter: red for a tree holding these members, naming the given ones.
        pub filter: Option<FilterFn>,
        pub filtered: Option<FilterRed>,
        /// What a red filter probe prints.
        pub probe_said: String,
        /// A compiler: the error text for a tree holding these members, if it does not build.
        pub compiler: Option<BuildFn>,
        pub build_failed: Option<String>,
        /// A runner that dies with this text while the member is in the tree.
        pub aborts_with: Option<(String, String)>,
        /// The next main run's poll fails with this, once.
        pub fails_once: Option<String>,
        /// Whether the fake can build a member subset, and each subset it was asked to build.
        pub bisects: bool,
        pub probes: Vec<Vec<String>>,
        /// Whether the fake can run a suite on a member subset, and each (suite, subset) it was asked for.
        pub runs_alone: bool,
        pub alone: Vec<(String, Vec<String>)>,
        /// The packages each scoped filter probe was asked to judge.
        pub scoped: Vec<Vec<String>>,
        /// The filter red of the corpus pass this attribution follows.
        pub prior: Option<FilterRed>,
        /// The suites the corpus pass this attribution follows ran red.
        pub prior_suites: Vec<String>,
        pub confirmed: bool,
        /// Like the real runner: nothing answers a verify main or a job until a corpus main started.
        pub needs_runner: bool,
        pub runner_started: bool,
        /// A probe scoped to named tests finds nothing red: the red needs its whole package.
        pub narrow_clears: bool,
        /// The failing scope rerun on every member together comes back green.
        pub rerun_green: bool,
        /// A filter probe of exactly this member alone has no verdict.
        pub unknown_alone: Option<String>,
        /// A runner that cannot build a tree with members taken out: every such job fails to launch.
        pub refuses_removals: bool,
        /// Suite → the members whose diff adds it: a run naming it on a tree holding none of them dies as round-vm does.
        pub added_by: BTreeMap<String, Vec<String>>,
        /// The round is abandoned from this many main runs started, or from this time.
        pub abandoned_after_starts: Option<usize>,
        pub abandoned_at: Option<u64>,
        /// Mirrors the clock for an `Ops` that answers by time.
        pub clock: Option<std::rc::Rc<std::cell::Cell<u64>>>,
        /// From this time on no main result, job result or end is ever answered.
        pub stalls_at: Option<u64>,
        pub stall: Option<u64>,
    }

    pub type BuildFn = Box<dyn Fn(&BTreeSet<String>) -> Option<String>>;

    pub type FilterFn = Box<dyn Fn(&BTreeSet<String>) -> Option<FilterRed>>;

    impl Fake {
        pub fn new(red: impl Fn(&str, &BTreeSet<String>) -> bool + 'static) -> Fake {
            Fake {
                t: 0,
                maxpar: 2,
                job_secs: 3,
                dur: BTreeMap::new(),
                red: Box::new(red),
                flaky: BTreeSet::new(),
                flips_again: BTreeSet::new(),
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
                stopped: 0,
                on_base: None,
                filter: None,
                filtered: None,
                probe_said: String::new(),
                compiler: None,
                build_failed: None,
                aborts_with: None,
                fails_once: None,
                bisects: false,
                probes: vec![],
                runs_alone: false,
                alone: vec![],
                scoped: vec![],
                prior: None,
                prior_suites: vec![],
                confirmed: false,
                needs_runner: false,
                runner_started: false,
                narrow_clears: false,
                rerun_green: false,
                unknown_alone: None,
                refuses_removals: false,
                added_by: BTreeMap::new(),
                abandoned_after_starts: None,
                abandoned_at: None,
                clock: None,
                stalls_at: None,
                stall: None,
            }
        }
        fn verdict(&mut self, suite: &str, tree: &BTreeSet<String>) -> bool {
            if self.flaky.remove(suite) {
                return false;
            }
            !(self.red)(suite, tree)
        }
        fn corpus_running(&self) -> usize {
            self.main
                .iter()
                .filter(|(_, fin, _)| *fin > self.t)
                .count()
                .min(self.maxpar)
        }
        fn jobs_running(&self) -> usize {
            self.jobs
                .iter()
                .filter(|(j, fin, _)| *fin > self.t && !self.done_jobs.contains(&j.id))
                .count()
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
            if kind == MainKind::Corpus {
                self.runner_started = true;
            } else if self.needs_runner && !self.runner_started {
                return Err("no round-vm is running to answer the verify request".into());
            }
            self.main.clear();
            self.done_jobs.clear();
            self.jobs.clear();
            self.reported.clear();
            self.main_ended = None;
            let tree: BTreeSet<String> = self.members.iter().cloned().collect();
            if let Some(s) = suites.iter().find(|s| {
                self.added_by
                    .get(*s)
                    .is_some_and(|a| !a.iter().any(|m| tree.contains(m)))
            }) {
                return Err(format!("round-vm: suite list mismatch, not a harness fault — batch: unknown suite: {s}"));
            }
            self.filtered = self.filter.as_ref().and_then(|f| f(&tree));
            if self.filtered.is_some() {
                return Ok(());
            }
            self.build_failed = self.compiler.as_ref().and_then(|c| c(&tree));
            if self.build_failed.is_some() {
                return Ok(());
            }
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
            if self.stalls_at.is_some_and(|at| self.t >= at) {
                return Ok(p);
            }
            if let Some(text) = self.fails_once.take() {
                return Err(text);
            }
            if let Some((m, text)) = &self.aborts_with {
                if self.members.contains(m) {
                    return Err(text.clone());
                }
            }
            if let Some(out) = &self.build_failed {
                p.done = Some(if buildattr::is_config_delta_red(out) {
                    MainEnd::ConfigDelta
                } else {
                    MainEnd::WorkspaceBuild
                });
                return Ok(p);
            }
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
        fn probe_output(&mut self) -> String {
            self.probe_said.clone()
        }
        fn filter_red(&mut self) -> Option<FilterRed> {
            self.filtered.take()
        }
        fn prior_red(&mut self) -> Option<FilterRed> {
            self.prior.take()
        }
        fn confirmed(&self) -> bool {
            self.confirmed
        }
        fn prior_red_suites(&mut self) -> Vec<String> {
            std::mem::take(&mut self.prior_suites)
        }
        fn build_output(&mut self) -> String {
            self.build_failed.clone().unwrap_or_default()
        }
        fn builds(&mut self, members: &[Member]) -> Option<bool> {
            let tree: BTreeSet<String> = members.iter().map(|m| m.id.clone()).collect();
            self.probes.push(tree.iter().cloned().collect());
            self.bisects
                .then(|| self.compiler.as_ref().and_then(|c| c(&tree)).is_none())
        }
        fn config_applies(&mut self, members: &[Member]) -> Option<bool> {
            self.builds(members)
        }
        fn suite_green(&mut self, suite: &str, members: &[Member]) -> Option<bool> {
            let tree: BTreeSet<String> = members.iter().map(|m| m.id.clone()).collect();
            self.alone
                .push((suite.to_string(), tree.iter().cloned().collect()));
            self.runs_alone.then(|| !(self.red)(suite, &tree))
        }
        fn filter_green(&mut self, _name: &str, members: &[Member]) -> Option<bool> {
            let tree: BTreeSet<String> = members.iter().map(|m| m.id.clone()).collect();
            self.probes.push(tree.iter().cloned().collect());
            (self.bisects || self.runs_alone)
                .then(|| self.filter.as_ref().is_none_or(|f| f(&tree).is_none()))
        }
        fn filter_green_in(
            &mut self,
            name: &str,
            packages: &[String],
            members: &[Member],
        ) -> Option<bool> {
            self.scoped.push(packages.to_vec());
            if self.rerun_green && members.len() == self.members.len() {
                self.probes
                    .push(members.iter().map(|m| m.id.clone()).collect());
                return Some(true);
            }
            if let [only] = members {
                if self.unknown_alone.as_deref() == Some(only.id.as_str()) {
                    return None;
                }
            }
            if self.narrow_clears && packages.iter().any(|p| p.contains('=')) {
                self.probes.push(vec![]);
                return Some(true);
            }
            self.filter_green(name, members)
        }
        fn runner_survives(&mut self, members: &[Member]) -> Option<bool> {
            self.probes
                .push(members.iter().map(|m| m.id.clone()).collect());
            self.bisects.then(|| {
                self.aborts_with
                    .as_ref()
                    .is_none_or(|(m, _)| !members.iter().any(|x| &x.id == m))
            })
        }
        fn stop_main(&mut self) -> Result<(), String> {
            self.stopped += 1;
            self.main.clear();
            self.jobs.clear();
            Ok(())
        }
        fn launch(&mut self, job: &Job) -> Result<(), String> {
            if self.needs_runner && !self.runner_started {
                return Err("no round-vm is running to answer the job request".into());
            }
            if self.refuses_removals && !job.removal.is_empty() {
                return Err("no tree without those members".into());
            }
            self.first_launch.get_or_insert(self.t);
            let tree: BTreeSet<String> = self
                .members
                .iter()
                .filter(|m| !job.removal.contains(m))
                .cloned()
                .collect();
            let flips_again = job.id >= FLIP_JOB_BASE
                && job.purpose == Purpose::Plain
                && self.flips_again.remove(&job.suite);
            let green = !flips_again && self.verdict(&job.suite, &tree);
            self.jobs.push((
                job.clone(),
                self.t + self.job_secs,
                if green {
                    JobResult::Green
                } else {
                    JobResult::Red
                },
            ));
            Ok(())
        }
        fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
            let mut out = vec![];
            if self.stalls_at.is_some_and(|at| self.t >= at) {
                return out;
            }
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
        fn abandoned(&mut self) -> bool {
            self.abandoned_after_starts
                .is_some_and(|n| self.main_kind.len() >= n)
                || self.abandoned_at.is_some_and(|t| self.t >= t)
        }
        fn wait(&mut self) {
            self.use_log
                .push((self.t, self.corpus_running(), self.jobs_running()));
            self.t += 1;
            if let Some(c) = &self.clock {
                c.set(self.t);
            }
            assert!(self.t < 100_000, "the loop never settled");
            if let Some(at) = self.stalls_at {
                assert!(self.t < at + 10_000, "the loop never ended");
            }
        }
        fn stall_secs(&self) -> Option<u64> {
            self.stall
        }
    }

    #[derive(Default)]
    pub struct Ops {
        pub touch: BTreeMap<String, Vec<String>>,
        pub ejected: Vec<(String, Vec<String>)>,
        pub eject_evidence: Vec<(String, String)>,
        pub returned: Vec<(String, String)>,
        pub owner_flags: Vec<(String, bool)>,
        pub rebuilt: Vec<Vec<String>>,
        pub incidents: Vec<(String, Vec<String>)>,
        pub can_fix: bool,
        pub fix_calls: u32,
        pub fixed: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        pub recorded: Vec<(u32, Decision)>,
        pub disabled: Vec<Vec<String>>,
        pub escaped: Vec<(String, String, Option<JobResult>)>,
        pub judged: Vec<(Vec<String>, Vec<String>)>,
        /// Base shas the base will move to, one per `base_moved` call; empty means it never moves.
        pub moves: Vec<String>,
        /// Suite → the members whose diff adds it.
        pub adds: BTreeMap<String, Vec<String>>,
        /// The next pass the menders decide, once per red pass; empty means no mender owns it.
        pub menders: Vec<crate::mender::Next>,
        pub diffs: Vec<MemberDiff>,
        pub config_changes: Vec<(Id, String)>,
        pub sequenced: Vec<(String, Vec<String>)>,
        pub graph: Option<PackageGraph>,
        /// The re-cut of the survivors merges none of them.
        pub rebuilt_empty: bool,
        /// Every eject is refused with this line.
        pub refuse_eject: Option<String>,
        /// Every disable is refused with this line.
        pub refuse_disable: Option<String>,
        /// (time it settles, failure): what `mender_returns` answers once the fake clock reaches it.
        pub returns: Vec<(u64, crate::mender::Failure)>,
        pub clock: Option<std::rc::Rc<std::cell::Cell<u64>>>,
        pub returned_at: Vec<u64>,
    }

    impl RoundOps for Ops {
        fn suspects(&self, suite: &str, members: &[Id]) -> Vec<Id> {
            let t = self.touch.get(suite).cloned().unwrap_or_default();
            let mut v: Vec<Id> = members.iter().filter(|m| t.contains(m)).cloned().collect();
            v.extend(members.iter().filter(|m| !t.contains(m)).cloned());
            v
        }
        fn eject(&mut self, member: &Member, suites: &[String], owner: bool) -> Result<(), String> {
            if let Some(line) = &self.refuse_eject {
                return Err(line.clone());
            }
            self.ejected.push((member.id.clone(), suites.to_vec()));
            self.owner_flags.push((member.id.clone(), owner));
            Ok(())
        }
        fn eject_returned(
            &mut self,
            member: &Member,
            suites: &[String],
            why: &str,
        ) -> Result<(), String> {
            self.returned.push((member.id.clone(), why.to_string()));
            self.returned_at
                .push(self.clock.as_ref().map_or(0, |c| c.get()));
            self.eject(member, suites, true)
        }
        fn eject_on(
            &mut self,
            member: &Member,
            filter: &str,
            line: &str,
            owner: bool,
        ) -> Result<(), String> {
            self.eject_evidence
                .push((member.id.clone(), line.to_string()));
            self.eject(member, &[filter.to_string()], owner)
        }
        fn diffs(&self, _: &[Id]) -> Vec<MemberDiff> {
            self.diffs.clone()
        }
        fn config_key_changes(&self, _: &[Id]) -> Vec<(Id, String)> {
            self.config_changes.clone()
        }
        fn package_graph(&self) -> Option<PackageGraph> {
            self.graph.clone()
        }
        fn sequence(&mut self, member: &Member, behind: &[Id], line: &str) -> Result<(), String> {
            self.sequenced.push((member.id.clone(), behind.to_vec()));
            self.eject_evidence
                .push((member.id.clone(), line.to_string()));
            Ok(())
        }
        fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
            self.rebuilt
                .push(survivors.iter().map(|m| m.id.clone()).collect());
            Ok(if self.rebuilt_empty {
                vec![]
            } else {
                survivors.to_vec()
            })
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
        fn base_moved(
            &mut self,
            members: &[Member],
        ) -> Result<Option<(String, Vec<Member>)>, String> {
            Ok(if self.moves.is_empty() {
                None
            } else {
                Some((self.moves.remove(0), members.to_vec()))
            })
        }
        fn judge(&mut self, suites: &[String], members: &[Id]) {
            self.judged.push((suites.to_vec(), members.to_vec()));
        }
        fn base_sha(&self) -> String {
            "b4se".into()
        }
        fn last_touching(&self, paths: &[String]) -> Option<String> {
            Some(format!("c0de fixes {}", paths.join(",")))
        }
        fn incident(&mut self, kind: &str, suites: &[String]) {
            self.incidents.push((kind.into(), suites.to_vec()));
        }
        fn adding(&self, suite: &str, members: &[Id]) -> Vec<Id> {
            let a = self.adds.get(suite).cloned().unwrap_or_default();
            members.iter().filter(|m| a.contains(m)).cloned().collect()
        }
        fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id> {
            let t = self.touch.get(suite).cloned().unwrap_or_default();
            members.iter().filter(|m| t.contains(m)).cloned().collect()
        }
        fn disable_flips(&mut self, tests: &[String]) -> Result<(), String> {
            if let Some(line) = &self.refuse_disable {
                return Err(line.clone());
            }
            self.disabled.push(tests.to_vec());
            Ok(())
        }
        fn record(&mut self, iteration: u32, decision: &Decision) {
            self.recorded.push((iteration, decision.clone()));
        }
        fn settle_mending(&mut self, d: &Decision, _: &[Member]) -> Option<Result<Mended, String>> {
            if self.menders.is_empty() {
                return None;
            }
            Some(Ok(Mended {
                next: self.menders.remove(0),
                unattributed: vec![],
                suites: d.owned_suites(),
            }))
        }
        fn mender_returns(&self) -> Vec<crate::mender::Failure> {
            let now = self.clock.as_ref().map_or(0, |c| c.get());
            self.returns
                .iter()
                .filter(|(at, _)| *at <= now)
                .map(|(_, f)| f.clone())
                .collect()
        }
        fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>) {
            self.escaped
                .push((member.id.clone(), suite.to_string(), rerun));
        }
    }

    fn corpus(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("test-{i:02}.sh")).collect()
    }

    fn three() -> Vec<Member> {
        vec![member("m1"), member("m2"), member("m3")]
    }

    fn breaks(
        culprit: &'static str,
        suite: &'static str,
    ) -> impl Fn(&str, &BTreeSet<String>) -> bool {
        move |s, tree| s == suite && tree.contains(culprit)
    }

    fn stalling_fake(stall: Option<u64>) -> Fake {
        let mut f = Fake::new(|_, _| false);
        f.dur.insert("test-02.sh".into(), 100);
        f.dur.insert("test-03.sh".into(), 100);
        f.stalls_at = Some(20);
        f.stall = stall;
        f
    }

    #[test]
    fn an_attribution_leg_with_no_progress_for_the_stall_bound_stops_and_names_what_it_waited_on() {
        let mut f = stalling_fake(Some(300));
        let mut ops = Ops::default();
        let budget = Budget {
            slots: 6,
            maxpar: 2,
        };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), budget).unwrap();
        let RoundEnd::Stalled(why) = end else {
            panic!("{end:?}")
        };
        assert!(
            why.contains("test-02.sh") && why.contains("test-03.sh"),
            "{why}"
        );
        assert!(!why.contains("test-00.sh"), "{why}");
        assert!(f.t <= 20 + 300 + 1, "ended at t={}", f.t);
        assert_eq!(f.stopped, 1);
        assert_eq!(
            ops.incidents,
            vec![(
                "attribution-stalled".to_string(),
                vec!["test-02.sh".to_string(), "test-03.sh".to_string()]
            )]
        );
    }

    #[test]
    fn a_run_whose_results_keep_coming_inside_the_stall_bound_is_not_stalled() {
        let mut f = Fake::new(|_, _| false);
        f.maxpar = 1;
        for i in 0..5 {
            f.dur.insert(format!("test-{i:02}.sh"), 100);
        }
        f.stall = Some(150);
        let mut ops = Ops::default();
        let budget = Budget {
            slots: 6,
            maxpar: 1,
        };
        let end = attribute_round(&mut f, &mut ops, &corpus(5), three(), budget).unwrap();
        assert!(matches!(end, RoundEnd::Land { .. }), "{end:?}");
        assert!(f.t > 150, "the run outlasted the bound: t={}", f.t);
        assert!(ops.incidents.is_empty());
    }

    #[test]
    fn a_rerun_against_a_stalled_job_gives_up_within_the_stall_bound() {
        let mut f = stalling_fake(Some(300));
        f.stalls_at = Some(0);
        assert_eq!(rerun_with_members(&mut f, "test-00.sh", 0), None);
        assert!(f.t <= 301, "gave up at t={}", f.t);
    }

    #[test]
    fn flip_proof_jobs_that_stall_are_unproven_within_the_stall_bound() {
        let mut f = stalling_fake(Some(300));
        f.stalls_at = Some(0);
        let mut ops = Ops::default();
        let members = three();
        let budget = Budget {
            slots: 6,
            maxpar: 2,
        };
        let flaky = vec!["test-00.sh".to_string()];
        let Err(e) = remove_flips(&mut f, &mut ops, &flaky, &members, budget) else {
            panic!("proved")
        };
        assert_eq!(e.reds, vec!["test-00.sh".to_string()]);
        assert!(!e.member_owns);
        assert!(f.t <= 301, "gave up at t={}", f.t);
    }

    #[test]
    fn an_unbounded_runner_waits_as_before() {
        let mut f = stalling_fake(None);
        f.stalls_at = Some(20_000);
        let mut ops = Ops::default();
        let budget = Budget {
            slots: 6,
            maxpar: 2,
        };
        let end = attribute_round(&mut f, &mut ops, &corpus(4), three(), budget).unwrap();
        assert!(matches!(end, RoundEnd::Land { .. }), "{end:?}");
    }

    #[test]
    fn attribution_starts_on_the_first_streamed_red_and_settles_before_the_corpus_ends() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
        f.maxpar = 4;
        f.dur.insert("test-00.sh".into(), 2);
        let budget = Budget {
            slots: 6,
            maxpar: 4,
        };
        let mut ops = Ops::default();
        let end = attribute_round(&mut f, &mut ops, &corpus(40), three(), budget).unwrap();
        let main_end = f.main_ended.unwrap();
        let first = f.first_launch.unwrap();
        assert!(
            first <= 3,
            "first rerun at t={first}, the red landed at t=2"
        );
        assert!(
            first < main_end,
            "attribution began before the corpus ended ({first} < {main_end})"
        );
        let rec = &ops.recorded[0].1.records[0];
        assert!(
            rec.settled_before_main_end,
            "{rec:?} vs corpus end {main_end}"
        );
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-00.sh".to_string()])]
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1", "m3"])
        );
    }

    #[test]
    fn a_red_that_also_fails_on_the_base_ejects_no_one() {
        let mut f = Fake::new(|s, _| s == "test-00.sh");
        f.maxpar = 4;
        f.dur.insert("test-00.sh".into(), 2);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(40),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(f.stopped, 0);
        assert!(ops.ejected.is_empty());
        assert_eq!(
            ops.incidents,
            vec![("base".to_string(), vec!["test-00.sh".to_string()])]
        );
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
    }

    fn adds(suite: &str, who: &[&str]) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([(
            suite.to_string(),
            who.iter().map(|w| w.to_string()).collect(),
        )])
    }

    // covers: UC-landing-merge-queue-48
    #[test]
    fn a_red_suite_only_one_member_adds_ejects_that_member_and_is_never_run_without_it() {
        let mut f = Fake::new(breaks("m1", "test-01.sh"));
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m1"]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["test-01.sh".to_string()])]
        );
        assert!(
            f.jobs
                .iter()
                .all(|(j, _, _)| j.suite != "test-01.sh" || j.removal.is_empty()),
            "a rerun lacked the adder: {:?}",
            f.jobs.iter().map(|(j, _, _)| j).collect::<Vec<_>>()
        );
        assert!(
            f.main_kind
                .iter()
                .skip(1)
                .all(|(_, s)| !s.contains(&"test-01.sh".to_string())),
            "{:?}",
            f.main_kind
        );
        assert!(ops.incidents.is_empty() && ops.judged.is_empty());
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m2"]),
            "{end:?}"
        );
    }

    #[test]
    fn a_three_member_round_whose_only_red_suite_member_b_adds_ejects_b_with_no_subset_run_and_no_pause(
    ) {
        let mut f = Fake::new(breaks("m2", "test-01.sh"));
        f.added_by = adds("test-01.sh", &["m2"]);
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m2"]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-01.sh".to_string()])]
        );
        assert!(
            f.jobs
                .iter()
                .map(|(j, _, _)| (j.suite.as_str(), j.removal.len()))
                .collect::<Vec<_>>()
                == [("test-01.sh", 0)],
            "only the escape rerun on the full round tree runs: {:?}",
            f.jobs.iter().map(|(j, _, _)| j).collect::<Vec<_>>()
        );
        assert!(ops.incidents.is_empty() && ops.judged.is_empty());
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2),
            "{end:?}"
        );
    }

    #[test]
    fn a_member_that_leaves_for_a_lint_red_takes_the_suite_it_adds_out_of_the_next_corpus() {
        let mut f = Fake::new(|_, _| false);
        f.added_by = adds("test-01.sh", &["m2"]);
        f.filter = Some(lint_naming("m2"));
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m2"]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2),
            "{end:?}"
        );
        assert!(
            f.main_kind[1].1.iter().all(|s| s != "test-01.sh"),
            "{:?}",
            f.main_kind
        );
    }

    #[test]
    fn a_member_that_leaves_for_a_build_red_takes_the_suite_it_adds_out_of_the_next_corpus() {
        let mut f = Fake::new(|_, _| false);
        f.added_by = adds("test-01.sh", &["m2"]);
        f.compiler = Some(broken_by(&["m2"]));
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m2"]),
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", "lc/src/callers.rs", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2),
            "{end:?}"
        );
    }

    #[test]
    fn a_red_suite_no_member_adds_is_still_attributed_by_rerun_after_an_adder_is_ejected() {
        let mut f = Fake::new(|s, tree| {
            (s == "test-01.sh" && tree.iter().any(|m| m == "m1"))
                || (s == "test-02.sh" && tree.iter().any(|m| m == "m2"))
        });
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m1"]),
            ..Ops::default()
        };
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        let ejected: Vec<&str> = ops.ejected.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(ejected, ["m1", "m2"], "{:?}", ops.ejected);
    }

    #[test]
    fn a_stacked_dependent_carrying_the_added_suite_does_not_share_the_blame_with_its_prerequisite()
    {
        let mut f = Fake::new(breaks("m1", "test-01.sh"));
        let mut dep = member("m2");
        dep.stack.insert("m1".into(), "m1-tip".into());
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m1", "m2"]),
            ..Ops::default()
        };
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), dep],
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected.first(),
            Some(&("m1".to_string(), vec!["test-01.sh".to_string()])),
            "{:?}",
            ops.ejected
        );
        assert!(
            f.jobs
                .iter()
                .all(|(j, _, _)| j.suite != "test-01.sh" || j.removal.is_empty()),
            "a rerun lacked the adder"
        );
    }

    #[test]
    fn a_red_suite_two_members_add_is_bisected_between_those_two_only() {
        let mut f = Fake::new(breaks("m2", "test-01.sh"));
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m1", "m2"]),
            ..Ops::default()
        };
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-01.sh".to_string()])]
        );
        let removals: Vec<Vec<String>> = f
            .jobs
            .iter()
            .filter(|(j, _, _)| j.suite == "test-01.sh")
            .map(|(j, _, _)| j.removal.clone())
            .collect();
        assert!(
            removals
                .iter()
                .all(|r| r.len() < 2 && !r.contains(&"m3".to_string())),
            "{removals:?}"
        );
    }

    #[test]
    fn a_red_attribution_could_not_rerun_is_owned_by_the_one_member_whose_diff_the_suite_covers() {
        let mut f = Fake::new(breaks("m2", "test-01.sh"));
        f.refuses_removals = true;
        let mut ops = Ops {
            touch: BTreeMap::from([("test-01.sh".to_string(), vec!["m2".to_string()])]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-01.sh".to_string()])]
        );
        assert!(ops.judged.is_empty() && ops.incidents.is_empty());
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1"]),
            "{end:?}"
        );
    }

    #[test]
    fn a_two_member_round_with_one_red_member_ejects_it_and_certifies_the_other() {
        let mut f = Fake::new(breaks("m2", "test-01.sh"));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-01.sh".to_string()])]
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1"]),
            "{end:?}"
        );
    }

    #[test]
    fn a_named_two_suite_list_out_of_a_43_suite_tree_ejects_the_member_red_on_both_and_lands_the_other(
    ) {
        let two: Vec<String> = corpus(43).into_iter().take(2).collect();
        let named = two.clone();
        let mut f = Fake::new(move |s, tree| named.iter().any(|n| n == s) && tree.contains("m2"));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &two,
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(ops.ejected.len(), 1, "{:?}", ops.ejected);
        assert_eq!(ops.ejected[0].0, "m2");
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1"]),
            "{end:?}"
        );
    }

    #[test]
    fn a_red_whose_attribution_reruns_faulted_is_a_fault_when_two_members_touch_the_suite() {
        let mut f = Fake::new(breaks("m2", "test-01.sh"));
        f.refuses_removals = true;
        let touch = vec!["m1".to_string(), "m2".to_string()];
        let mut ops = Ops {
            touch: BTreeMap::from([("test-01.sh".to_string(), touch)]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(&end, RoundEnd::Faulted(why) if why.starts_with("attribution faulted") && why.contains("test-01.sh")),
            "{end:?}"
        );
        assert!(ops.ejected.is_empty() && ops.judged.is_empty());
    }

    #[test]
    fn a_one_member_round_whose_attribution_reruns_faulted_charges_nobody() {
        let mut f = Fake::new(breaks("m1", "test-01.sh"));
        f.refuses_removals = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Faulted(_)), "{end:?}");
        assert!(ops.ejected.is_empty() && ops.judged.is_empty());
    }

    #[test]
    fn a_red_no_single_member_owns_ejects_no_one() {
        let mut f = Fake::new(|s, tree| s == "test-00.sh" && tree.len() >= 2);
        f.maxpar = 4;
        f.dur.insert("test-00.sh".into(), 2);
        let mut ops = Ops::default();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(40),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(f.stopped, 0);
        assert!(ops.ejected.is_empty());
        assert_eq!(
            ops.judged.len(),
            1,
            "an unattributed red goes to the judge, not to an eject"
        );
    }

    #[test]
    fn a_red_no_rerun_attributes_is_run_on_the_base_and_each_member_alone_and_every_red_member_is_ejected(
    ) {
        let mut f =
            Fake::new(|s, tree| s == "test-00.sh" && (tree.contains("m2") || tree.contains("m3")));
        f.maxpar = 4;
        f.runs_alone = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        let ejected: Vec<&str> = ops.ejected.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(ejected, ["m2", "m3"], "{:?}", ops.eject_evidence);
        assert!(
            ops.eject_evidence
                .iter()
                .all(|(_, l)| l.contains("base green, m1 green, m2 red, m3 red")),
            "{:?}",
            ops.eject_evidence
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 1 && members[0].id == "m1"),
            "{end:?}"
        );
        assert!(
            f.alone.contains(&("test-00.sh".to_string(), vec![])),
            "the base was run: {:?}",
            f.alone
        );
        assert!(ops.judged.is_empty());
    }

    #[test]
    fn a_red_every_member_is_green_alone_for_is_judged_with_the_per_member_verdicts_in_the_reason()
    {
        let mut f = Fake::new(|s, tree| s == "test-00.sh" && tree.len() >= 2);
        f.maxpar = 4;
        f.runs_alone = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert!(
            ops.ejected.is_empty() && ops.judged.len() == 1,
            "{:?}",
            ops.ejected
        );
        assert!(
            matches!(&end, RoundEnd::Blocked(why) if why.contains("run alone: test-00.sh: base green, m1 green, m2 green, m3 green")),
            "{end:?}"
        );
    }

    #[test]
    fn a_filter_red_naming_no_member_whose_base_is_red_is_filed_as_the_base() {
        let mut f = Fake::new(|_, _| false);
        f.runs_alone = true;
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "spira-lint".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.incidents.first().map(|(k, _)| k.as_str()),
            Some("base"),
            "{:?}",
            ops.incidents
        );
        assert!(ops.ejected.is_empty());
        assert!(matches!(end, RoundEnd::BaseRed { .. }), "{end:?}");
    }

    #[test]
    fn a_sole_owner_found_after_the_corpus_ended_is_ejected_and_only_its_suites_are_verified() {
        let mut f = Fake::new(breaks("m2", "test-03.sh"));
        f.maxpar = 4;
        f.dur.insert("test-03.sh".into(), 1);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(f.stopped, 0);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-03.sh".to_string()])]
        );
        assert_eq!(
            f.main_kind[1],
            (MainKind::Verify, vec!["test-03.sh".into()])
        );
        assert!(matches!(end, RoundEnd::Land { .. }));
    }

    #[test]
    fn a_pass_red_only_on_suites_is_attributed_by_verifying_those_suites_alone() {
        let mut f = Fake::new(breaks("m2", "test-03.sh"));
        f.maxpar = 4;
        f.needs_runner = true;
        f.prior_suites = vec!["test-03.sh".into(), "not-in-the-corpus.sh".into()];
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            f.main_kind[0],
            (MainKind::Corpus, vec!["test-03.sh".into()])
        );
        assert!(
            f.main_kind
                .iter()
                .all(|(_, s)| !s.contains(&"not-in-the-corpus.sh".to_string())),
            "{:?}",
            f.main_kind
        );
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-03.sh".to_string()])]
        );
        assert!(matches!(end, RoundEnd::Land { .. }));
    }

    #[test]
    fn a_settled_owner_s_suite_is_rerun_on_the_full_tree_before_it_is_ejected() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
        f.maxpar = 4;
        let mut ops = Ops::default();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(
            ops.escaped,
            vec![(
                "m2".to_string(),
                "test-00.sh".to_string(),
                Some(JobResult::Red)
            )]
        );
    }

    #[test]
    fn the_slot_budget_holds_at_every_tick_and_the_corpus_is_never_starved() {
        let mut f = Fake::new(|s, tree| {
            (s == "test-03.sh" && tree.contains("m1")) || (s == "test-07.sh" && tree.contains("m3"))
        });
        f.maxpar = 3;
        f.job_secs = 7;
        let budget = Budget {
            slots: 5,
            maxpar: 3,
        };
        let mut ops = Ops::default();
        attribute_round(&mut f, &mut ops, &corpus(30), three(), budget).unwrap();
        for (t, c, j) in &f.use_log {
            assert!(c + j <= 5, "t={t}: corpus {c} + attribution {j} > 5 slots");
        }
        // The corpus ran maxpar-wide for as long as it had that many suites left: its schedule
        // was fixed at start and attribution never delayed a corpus result.
        let corpus_first_run = &f.main_kind[0];
        assert_eq!(corpus_first_run.1.len(), 30);
        assert!(
            f.use_log.iter().any(|(_, _, j)| *j == 2),
            "the two spare slots were used"
        );
    }

    #[test]
    fn a_flip_is_proven_green_alone_and_deleted_from_the_round_never_landed() {
        let mut f = Fake::new(|_, _| false);
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        ops.touch.insert("test-04.sh".into(), vec!["m2".into()]);
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(6),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
        assert_eq!(ops.disabled, vec![vec!["suite:test-04.sh".to_string()]]);
        let proofs: Vec<_> = f
            .jobs
            .iter()
            .filter(|(j, _, _)| j.id >= FLIP_JOB_BASE)
            .map(|(j, _, _)| j.removal.clone())
            .collect();
        assert_eq!(
            proofs.len(),
            4,
            "two alone, the base, one touching member: {proofs:?}"
        );
        assert!(
            proofs.contains(&vec!["m1".to_string(), "m3".to_string()]),
            "the touching member alone: {proofs:?}"
        );
        assert!(
            proofs.contains(&vec!["m1".to_string(), "m2".to_string(), "m3".to_string()]),
            "the base alone: {proofs:?}"
        );
    }

    #[test]
    fn a_flip_that_flips_again_in_one_proof_run_is_still_a_flip_and_does_not_block_the_round() {
        let mut f = Fake::new(|_, _| false);
        f.flaky.insert("test-04.sh".into());
        f.flips_again.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(6),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3),
            "{end:?}"
        );
        assert_eq!(ops.disabled, vec![vec!["suite:test-04.sh".to_string()]]);
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_round_whose_every_member_is_ejected_ends_emptied_not_blocked() {
        let mut f = Fake::new(breaks("m1", "test-01.sh"));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(end, RoundEnd::Emptied("round emptied by ejection".into()));
        assert_eq!(ops.ejected.len(), 1);
    }

    const RERUNS_GREEN: i32 = 6;

    #[test]
    fn a_one_member_round_red_on_a_suite_that_reproduces_ejects_the_member_and_never_blocks() {
        let calls = std::cell::Cell::new(0);
        let mut f = Fake::new(move |s, tree| {
            calls.set(calls.get() + 1);
            s == "test-04.sh" && tree.contains("m1") && calls.get() > RERUNS_GREEN
        });
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(6),
            vec![member("m1")],
            Budget {
                slots: 1,
                maxpar: 1,
            },
        )
        .unwrap();
        assert_eq!(
            end,
            RoundEnd::Emptied("round emptied by ejection".into()),
            "{end:?}"
        );
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["test-04.sh".to_string()])]
        );
        assert!(ops.disabled.is_empty());
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_one_member_round_red_in_a_prior_filter_ejects_its_member_without_a_probe() {
        let red = FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv t".into()],
        };
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.prior = Some(red);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["integration-tests".to_string()])]
        );
        assert!(
            f.scoped.is_empty() && f.main_kind.is_empty(),
            "{:?} {:?}",
            f.scoped,
            f.main_kind
        );
        assert!(
            ops.eject_evidence
                .iter()
                .any(|(id, line)| id == "m1" && line.contains("FAIL dir:testenv t")),
            "{:?}",
            ops.eject_evidence
        );
    }

    fn fence_only_round(refusal: Option<&str>) -> (Fake, Ops) {
        let mut f = Fake::new(|_, _| false);
        f.prior = Some(FilterRed {
            name: "fence-round-image-preflight".into(),
            named: vec![],
            failed: vec![],
        });
        let ops = Ops {
            refuse_eject: refusal.map(String::from),
            ..Ops::default()
        };
        (f, ops)
    }

    #[test]
    fn a_fence_red_ejects_the_sole_member_under_the_fence_name_without_a_pass() {
        let (mut f, mut ops) = fence_only_round(None);
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![(
                "m1".to_string(),
                vec!["fence-round-image-preflight".to_string()]
            )]
        );
        assert!(f.main_kind.is_empty(), "{:?}", f.main_kind);
    }

    #[test]
    fn a_refused_eject_blocks_the_round_with_the_refusal_and_starts_no_pass() {
        let (mut f, mut ops) =
            fence_only_round(Some("eject of m1 from b refused: --suites names gate"));
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        match end {
            RoundEnd::Blocked(why) => {
                assert!(why.contains("refused: --suites names gate"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        assert!(f.main_kind.is_empty(), "{:?}", f.main_kind);
    }

    fn reds3() -> Vec<String> {
        vec!["test-00.sh".into(), "test-01.sh".into(), "test-02.sh".into()]
    }

    fn touching_ops(pairs: &[(&str, &[&str])]) -> Ops {
        let mut ops = Ops::default();
        for (suite, ids) in pairs {
            ops.touch.insert(suite.to_string(), ids.iter().map(|i| i.to_string()).collect());
        }
        ops
    }

    #[test]
    fn a_confirmed_red_whose_suites_only_one_member_touches_ejects_it_without_a_probe_or_a_mender() {
        let mut f = Fake::new(|_, _| false);
        f.confirmed = true;
        f.runs_alone = true;
        f.prior_suites = reds3();
        let mut ops = touching_ops(&[("test-00.sh", &["m1"]), ("test-01.sh", &["m1"]), ("test-02.sh", &["m1"])]);
        ops.menders = vec![crate::mender::Next::Stop { page: String::new() }];
        let end = attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), member("m2")], budget6()).unwrap();
        assert_eq!(ops.ejected, vec![("m1".to_string(), reds3())]);
        assert!(matches!(&end, RoundEnd::Land { members, .. } if members.len() == 1 && members[0].id == "m2"), "{end:?}");
        assert!(f.alone.is_empty(), "no probe runs: {:?}", f.alone);
        assert!(f.main_kind.is_empty(), "no corpus or rerun pass: {:?}", f.main_kind);
        assert_eq!(ops.menders.len(), 1, "no mender settled");
    }

    #[test]
    fn a_confirmed_red_with_two_suspects_ejects_only_the_one_red_alone() {
        let mut f = Fake::new(|s, tree| s.starts_with("test-0") && tree.contains("m2"));
        f.confirmed = true;
        f.runs_alone = true;
        f.prior_suites = reds3();
        let t: &[&str] = &["m1", "m2"];
        let mut ops = touching_ops(&[("test-00.sh", t), ("test-01.sh", t), ("test-02.sh", t)]);
        let end = attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), member("m2"), member("m3")], budget6()).unwrap();
        let gone: Vec<&str> = ops.ejected.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(gone, ["m2"]);
        assert!(matches!(&end, RoundEnd::Land { members, .. } if members.len() == 2), "{end:?}");
        assert!(f.alone.iter().all(|(_, t)| !t.contains(&"m3".to_string())), "m3 is no suspect: {:?}", f.alone);
    }

    #[test]
    fn a_confirmed_red_no_member_reproduces_alone_is_blocked() {
        let mut f = Fake::new(|s, tree| s.starts_with("test-0") && tree.len() >= 2);
        f.confirmed = true;
        f.runs_alone = true;
        f.prior_suites = reds3();
        let mut ops = touching_ops(&[]);
        let end = attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), member("m2")], budget6()).unwrap();
        assert!(ops.ejected.is_empty(), "{:?}", ops.ejected);
        assert!(matches!(&end, RoundEnd::Blocked(why) if why.starts_with("unattributed red: test-00.sh,test-01.sh,test-02.sh")), "{end:?}");
    }

    #[test]
    fn a_confirmed_red_the_base_reproduces_is_base_red() {
        let mut f = Fake::new(|s, _| s.starts_with("test-0"));
        f.confirmed = true;
        f.runs_alone = true;
        f.prior_suites = reds3();
        let mut ops = touching_ops(&[]);
        let end = attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), member("m2")], budget6()).unwrap();
        assert!(matches!(end, RoundEnd::BaseRed { .. }), "{end:?}");
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn an_unconfirmed_red_in_prior_suites_still_runs_the_attribution_not_the_suspect_path() {
        let mut f = Fake::new(|_, tree| tree.contains("m1"));
        f.needs_runner = true;
        f.prior_suites = reds3();
        let mut ops = touching_ops(&[("test-00.sh", &["m1"])]);
        attribute_round(&mut f, &mut ops, &corpus(4), vec![member("m1"), member("m2")], budget6()).unwrap();
        assert!(!f.main_kind.is_empty(), "the corpus attribution ran");
    }

    #[test]
    fn a_one_member_round_red_in_prior_suites_ejects_its_member_without_a_run() {
        let mut f = Fake::new(|_, _| false);
        f.prior_suites = vec!["test-03.sh".into()];
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["test-03.sh".to_string()])]
        );
        assert!(f.main_kind.is_empty(), "{:?}", f.main_kind);
    }

    #[test]
    fn a_one_member_round_red_in_two_prior_suites_ejects_its_member_on_each_suite() {
        let mut f = Fake::new(|_, _| false);
        f.prior_suites = vec!["test-01.sh".into(), "test-03.sh".into()];
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![(
                "m1".to_string(),
                vec!["test-01.sh".to_string(), "test-03.sh".to_string()]
            )]
        );
    }

    #[test]
    fn a_one_member_round_red_on_the_base_too_blocks_instead_of_ejecting() {
        let mut f = Fake::new(|s, tree| s == "test-04.sh" && tree.is_empty());
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(6),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn an_unattributed_suite_red_the_base_reproduces_ends_base_red() {
        let bases = std::cell::Cell::new(0u32);
        let mut f = Fake::new(move |s, tree| {
            if s != "test-00.sh" {
                return false;
            }
            if tree.is_empty() {
                bases.set(bases.get() + 1);
                return bases.get() > 1;
            }
            tree.len() >= 2
        });
        f.maxpar = 4;
        f.runs_alone = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        let RoundEnd::BaseRed { base, why } = end else {
            panic!("{end:?}")
        };
        assert_eq!(base, "b4se");
        assert!(why.contains("test-00.sh"), "{why}");
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn a_red_on_the_base_is_not_a_flip_and_blocks_the_round() {
        let mut f = Fake::new(|s, tree| s == "test-04.sh" && tree.is_empty());
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(6),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert!(ops.disabled.is_empty());
        assert_eq!(
            ops.incidents,
            vec![("unattributed".to_string(), vec!["test-04.sh".to_string()])]
        );
    }

    #[test]
    fn owner_and_base_are_told_apart() {
        let mut f = Fake::new(|s, tree| match s {
            "test-01.sh" => tree.contains("m3"),
            "test-02.sh" => true, // red on every tree, the base included
            _ => false,
        });
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(8),
            three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        let all = |pick: fn(&Decision) -> Vec<String>| -> Vec<String> {
            ops.recorded.iter().flat_map(|(_, d)| pick(d)).collect()
        };
        assert_eq!(
            ops.recorded[0].1.owners.get("m3"),
            Some(&vec!["test-01.sh".to_string()])
        );
        assert_eq!(all(|d| d.base.clone()), vec!["test-02.sh".to_string()]);
        assert_eq!(
            ops.incidents,
            vec![("base".to_string(), vec!["test-02.sh".to_string()])]
        );
        assert!(matches!(end, RoundEnd::Land { .. }));
    }

    // covers: UC-landing-merge-queue-48
    #[test]
    fn a_flaky_red_ejects_no_one() {
        let mut f = Fake::new(|_, _| false);
        f.flaky.insert("test-04.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(8),
            three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(f.stopped, 0);
        assert!(ops.ejected.is_empty());
        assert_eq!(ops.recorded[0].1.flaky, vec!["test-04.sh".to_string()]);
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
    }

    #[test]
    fn every_owner_is_ejected_with_its_suites() {
        let mut f = Fake::new(|s, tree| match s {
            "test-01.sh" | "test-05.sh" => tree.contains("m1"),
            "test-03.sh" => tree.contains("m3"),
            _ => false,
        });
        let mut ops = Ops::default();
        let members = vec![member("m1"), member("m2"), member("m3"), member("m4")];
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(10),
            members,
            Budget {
                slots: 5,
                maxpar: 2,
            },
        )
        .unwrap();
        let mut ej: Vec<String> = ops.ejected.iter().map(|(id, _)| id.clone()).collect();
        ej.sort();
        assert_eq!(ej, vec!["m1".to_string(), "m3".to_string()]);
        assert_eq!(
            ops.rebuilt.last(),
            Some(&vec!["m2".to_string(), "m4".to_string()])
        );
        assert_eq!(f.stopped, 0, "the pass runs out; no owner stops it");
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2));
    }

    #[test]
    fn a_red_pass_with_menders_takes_its_next_pass_from_their_outcomes_and_ejects_only_the_returned(
    ) {
        use crate::mender::{Left, Next};
        let mut f = Fake::new(|s, tree| match s {
            "test-03.sh" => tree.contains("m1") && tree.contains("m3"),
            _ => false,
        });
        let mut ops = Ops::default();
        let mended = Member {
            tip: "m1-mended".into(),
            ..member("m1")
        };
        ops.menders = vec![Next::Pass {
            members: vec![mended, member("m2")],
            left: vec![Left {
                id: "m3".into(),
                why: "returned".into(),
            }],
            retry: false,
        }];
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(10),
            three(),
            Budget {
                slots: 8,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected[0],
            ("m3".to_string(), vec!["test-03.sh".to_string()])
        );
        assert_eq!(ops.rebuilt[0], vec!["m1".to_string(), "m2".to_string()]);
        assert_eq!(
            f.main_kind[1],
            (MainKind::Verify, vec!["test-03.sh".into()])
        );
        assert!(matches!(end, RoundEnd::Land { .. } | RoundEnd::Blocked(_)));
    }

    fn returned_failure(suite: &str, member: &str) -> crate::mender::Failure {
        crate::mender::Failure {
            suite: suite.into(),
            outcome: crate::mender::Outcome::Returned,
            diagnosis: "bad at x.rs:9".into(),
            members: vec![member.into()],
        }
    }

    fn slow_a_and_a_return_at_30(
        member_of: &str,
        round: Vec<Member>,
    ) -> (Fake, Ops, Result<RoundEnd, String>) {
        let clock = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut f = Fake::new(|_, _| false);
        f.dur.insert("test-a.sh".into(), 600);
        f.clock = Some(clock.clone());
        let mut ops = Ops {
            returns: vec![(30, returned_failure("test-a.sh", member_of))],
            clock: Some(clock),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &["test-a.sh".to_string()],
            round,
            Budget {
                slots: 8,
                maxpar: 2,
            },
        );
        (f, ops, end)
    }

    #[test]
    fn a_return_that_settles_while_the_pass_runs_ejects_its_member_and_verifies_the_survivors() {
        let (f, ops, end) = slow_a_and_a_return_at_30("m2", vec![member("m1"), member("m2")]);
        assert_eq!(ops.returned.len(), 1);
        assert_eq!(ops.returned[0].0, "m2");
        assert!(ops.returned[0].1.contains("bad at x.rs:9"));
        assert!(
            ops.returned_at[0] < 60,
            "the leg sat until t+{}",
            ops.returned_at[0]
        );
        assert_eq!(ops.rebuilt[0], vec!["m1".to_string()]);
        assert_eq!(
            f.main_kind[1],
            (MainKind::Verify, vec!["test-a.sh".to_string()])
        );
        match end.unwrap() {
            RoundEnd::Land { members, .. } => {
                assert_eq!(
                    members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                    ["m1"]
                )
            }
            other => panic!("the round should land m1, got {other:?}"),
        }
    }

    #[test]
    fn a_return_naming_a_member_others_are_stacked_on_ejects_the_dependents_too() {
        let mut m1 = member("m1");
        m1.stack.insert("m2".into(), "m2-tip".into());
        let (_, ops, _) = slow_a_and_a_return_at_30("m2", vec![m1, member("m2"), member("m3")]);
        let mut gone: Vec<&str> = ops.ejected.iter().map(|(id, _)| id.as_str()).collect();
        gone.sort();
        assert_eq!(gone, ["m1", "m2"]);
        assert_eq!(
            ops.returned
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["m2"]
        );
        assert_eq!(
            ops.owner_flags.iter().find(|(id, _)| id == "m1"),
            Some(&("m1".to_string(), false))
        );
        assert_eq!(ops.rebuilt[0], vec!["m3".to_string()]);
    }

    #[test]
    fn the_pass_after_a_mid_pass_return_verifies_every_suite_already_red_and_each_return_leaves_its_member(
    ) {
        let clock = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut f = Fake::new(|s, tree| s == "test-b.sh" && tree.contains("m3"));
        f.dur.insert("test-a.sh".into(), 600);
        f.clock = Some(clock.clone());
        let mut ops = Ops {
            returns: vec![
                (6, returned_failure("test-a.sh", "m2")),
                (6, returned_failure("test-c.sh", "m1")),
            ],
            clock: Some(clock),
            ..Ops::default()
        };
        attribute_round(
            &mut f,
            &mut ops,
            &["test-b.sh".to_string(), "test-a.sh".to_string()],
            three(),
            Budget {
                slots: 8,
                maxpar: 2,
            },
        )
        .unwrap();
        let mut gone: Vec<&str> = ops.returned.iter().map(|(id, _)| id.as_str()).collect();
        gone.sort();
        assert_eq!(gone, ["m1", "m2"]);
        assert_eq!(f.main_kind[1].0, MainKind::Verify);
        assert!(
            f.main_kind[1].1.contains(&"test-b.sh".to_string()),
            "{:?}",
            f.main_kind[1]
        );
    }

    #[test]
    fn a_return_naming_no_round_member_leaves_the_pass_running() {
        let (f, ops, _) = slow_a_and_a_return_at_30("stranger", vec![member("m1"), member("m2")]);
        assert!(ops.ejected.is_empty());
        assert_eq!(f.main_kind.len(), 1);
    }

    #[test]
    fn a_member_a_mender_returned_is_ejected_with_the_diagnosis_though_attribution_did_not_own_it()
    {
        use crate::mender::{Left, Next};
        let mut f = Fake::new(|s, tree| match s {
            "test-03.sh" => tree.contains("m1") && tree.contains("m3"),
            _ => false,
        });
        let mut ops = Ops {
            menders: vec![Next::Pass {
                members: vec![member("m1")],
                left: vec![Left {
                    id: "m2".into(),
                    why: "bad at x.rs:9".into(),
                }],
                retry: false,
            }],
            ..Ops::default()
        };
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(10),
            three(),
            Budget {
                slots: 8,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.returned,
            vec![("m2".to_string(), "bad at x.rs:9".to_string())]
        );
        assert_eq!(
            ops.owner_flags.iter().find(|(id, _)| id == "m2"),
            Some(&("m2".to_string(), true))
        );
        assert_eq!(ops.rebuilt[0], vec!["m1".to_string()]);
    }

    #[test]
    fn a_mender_pass_that_returns_the_adder_does_not_verify_the_suite_it_adds_on_the_tree_without_it(
    ) {
        use crate::mender::{Left, Next};
        let mut f =
            Fake::new(|s, tree| s == "test-03.sh" && tree.contains("m1") && tree.contains("m3"));
        f.added_by = adds("test-03.sh", &["m3"]);
        let menders = vec![Next::Pass {
            members: vec![member("m1"), member("m2")],
            left: vec![Left {
                id: "m3".into(),
                why: "returned".into(),
            }],
            retry: false,
        }];
        let mut ops = Ops {
            menders,
            adds: adds("test-03.sh", &["m3"]),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(10),
            three(),
            Budget {
                slots: 8,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2),
            "{end:?}"
        );
    }

    #[test]
    fn a_page_from_the_menders_stops_the_round() {
        use crate::mender::Next;
        let mut f =
            Fake::new(|s, tree| s == "test-01.sh" && tree.contains("m1") && tree.contains("m3"));
        let mut ops = Ops {
            menders: vec![Next::Stop { page: "box".into() }],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(10),
            three(),
            Budget {
                slots: 8,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(end, RoundEnd::Blocked("box".into()));
        assert!(ops.ejected.is_empty());
        assert_eq!(ops.incidents.len(), 1);
    }

    #[test]
    fn a_stacked_dependent_leaves_with_its_owner() {
        let mut f = Fake::new(breaks("m1", "test-02.sh"));
        let mut dep = member("m2");
        dep.stack.insert("m1".into(), "m1-tip".into());
        let mut ops = Ops::default();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), dep, member("m3")],
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        let ids: Vec<&str> = ops.ejected.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(ids, ["m1", "m2"]);
        assert_eq!(ops.rebuilt, vec![vec!["m3".to_string()]]);
        assert_eq!(
            ops.owner_flags,
            [("m1".to_string(), true), ("m2".to_string(), false)],
            "the owner keeps its charge; the dependent is collateral"
        );
    }

    #[test]
    fn nothing_is_ejected_when_every_red_is_a_flake_or_the_base_s() {
        let mut f = Fake::new(|s, _| s == "test-06.sh");
        f.flaky.insert("test-02.sh".into());
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(8),
            three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
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
        let mut f = Fake::new(move |s, tree| {
            s == "test-01.sh" && !flag.get() && tree.contains("m1") && tree.contains("m2")
        });
        let mut ops = Ops {
            can_fix: true,
            fixed: Some(fixed),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3),
            "{end:?}"
        );
        assert_eq!(ops.fix_calls, 1);
        assert!(ops.ejected.is_empty());
        assert!(ops.incidents.is_empty());
    }

    #[test]
    fn an_integration_red_the_fix_does_not_clear_is_held_not_ejected() {
        let mut f =
            Fake::new(|s, tree| s == "test-01.sh" && tree.contains("m1") && tree.contains("m2"));
        let mut ops = Ops {
            can_fix: true,
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(ops.fix_calls, 1, "the fix is tried once");
        assert_eq!(
            ops.incidents,
            vec![("integration".to_string(), vec!["test-01.sh".to_string()])]
        );
        assert!(ops.ejected.is_empty());
    }

    fn lint_naming(culprit: &'static str) -> FilterFn {
        Box::new(move |tree| {
            tree.contains(culprit).then(|| FilterRed {
                name: "spira-lint".into(),
                named: vec![(
                    culprit.to_string(),
                    format!(
                        "member={culprit} ffx-vocabulary: dwarf/src/lib.rs:26: the term \"banned\""
                    ),
                )],
                failed: vec![],
            })
        })
    }

    const FAILING: [&str; 2] = [
        "FAIL loom factory::tests::a_refused_dwell_blinds",
        "FAIL loom factory::tests::a_round_open_elsewhere",
    ];

    fn unit_red_when(culprit: &'static str) -> FilterFn {
        Box::new(move |tree| {
            tree.contains(culprit).then(|| FilterRed {
                name: "unit-tests".into(),
                named: vec![],
                failed: FAILING.iter().map(|l| l.to_string()).collect(),
            })
        })
    }

    fn loom_graph() -> PackageGraph {
        PackageGraph {
            dirs: [
                ("loom", "loom"),
                ("runtime", "spira-runtime"),
                ("gate", "gate"),
            ]
            .iter()
            .map(|(p, d)| (p.to_string(), d.to_string()))
            .collect(),
            deps: std::collections::BTreeMap::from([(
                "loom".to_string(),
                vec!["runtime".to_string()],
            )]),
        }
    }

    fn diff_of(id: &str, path: &str) -> MemberDiff {
        MemberDiff {
            id: id.into(),
            paths: vec![path.into()],
            added: String::new(),
        }
    }

    fn four() -> Vec<Member> {
        vec![member("m1"), member("m2"), member("m3"), member("m4")]
    }

    #[test]
    fn a_unit_red_in_a_package_only_the_last_members_dependency_touches_ejects_that_member_and_no_other(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_when("m4"));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            diffs: vec![
                diff_of("m1", "gate/src/engine.rs"),
                diff_of("m2", "docs/x.md"),
                diff_of("m3", "gate/src/tests.rs"),
                diff_of("m4", "spira-runtime/src/tasks/e/watch.rs"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            four(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m4".to_string(), vec!["unit-tests".to_string()])]
        );
        assert!(
            f.probes.is_empty(),
            "a mapped red runs no subset: {:?}",
            f.probes
        );
        let note = &ops.eject_evidence[0].1;
        assert!(
            note.contains("mapped by package") && FAILING.iter().all(|l| note.contains(l)),
            "{note}"
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3),
            "{end:?}"
        );
    }

    const RELEASE_FAILING: &str =
        "FAIL release tests::the_shipped_delta_carries_every_required_key_with_every_task_in_act_on_systemd";

    fn release_red_when(culprit: &'static str) -> FilterFn {
        Box::new(move |tree| {
            tree.contains(culprit).then(|| FilterRed {
                name: "unit-tests".into(),
                named: vec![],
                failed: vec![RELEASE_FAILING.to_string()],
            })
        })
    }

    fn release_graph() -> PackageGraph {
        PackageGraph {
            dirs: [
                ("release", "release"),
                ("runtime", "spira-runtime"),
                ("gate", "gate"),
            ]
            .iter()
            .map(|(p, d)| (p.to_string(), d.to_string()))
            .collect(),
            deps: std::collections::BTreeMap::new(),
        }
    }

    fn diff_paths(id: &str, paths: &[&str]) -> MemberDiff {
        MemberDiff {
            id: id.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            added: String::new(),
        }
    }

    fn budget6() -> Budget {
        Budget {
            slots: 6,
            maxpar: 2,
        }
    }

    #[test]
    fn a_unit_red_in_a_crate_that_reads_a_file_outside_its_dir_ejects_the_member_touching_that_file_and_no_other(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(release_red_when("m6"));
        let mut ops = Ops {
            graph: Some(release_graph()),
            diffs: vec![
                diff_paths("m1", &["spira-config/src/x.rs"]),
                diff_paths("m2", &["runtime/src/y.rs"]),
                diff_paths("m3", &["testenv/src/z.rs"]),
                diff_paths("m4", &["batcher-cut/src/a.rs", "spira-runtime/src/b.rs"]),
                diff_paths(
                    "m5",
                    &[
                        "queue/src/c.rs",
                        "spira-lint/src/d.rs",
                        "spira-runtime/src/e.rs",
                    ],
                ),
                diff_paths(
                    "m6",
                    &[
                        "spira-runtime/src/f.rs",
                        "spira/conf.d/SPIRA_TASK_MODES",
                        "spira/config-delta.toml",
                    ],
                ),
            ],
            ..Ops::default()
        };
        let members: Vec<Member> = (1..=6).map(|i| member(&format!("m{i}"))).collect();
        let end = attribute_round(&mut f, &mut ops, &corpus(3), members, budget6()).unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m6".to_string(), vec!["unit-tests".to_string()])]
        );
        assert!(
            f.probes.is_empty(),
            "one mapped suspect runs no subset: {:?}",
            f.probes
        );
        let note = &ops.eject_evidence[0].1;
        assert!(note.contains("read by release's tests"), "{note}");
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 5),
            "{end:?}"
        );
    }

    fn two_suspects() -> Ops {
        Ops {
            graph: Some(release_graph()),
            diffs: vec![
                diff_paths("m1", &["spira-config/src/x.rs"]),
                diff_paths("m2", &["spira/conf.d/SPIRA_TASK_MODES"]),
                diff_paths("m3", &["spira/config-delta.d/m3.toml"]),
                diff_paths("m4", &["docs/x.md"]),
            ],
            ..Ops::default()
        }
    }

    #[test]
    fn two_members_touching_what_the_failing_crate_reads_are_probed_and_only_the_red_one_leaves() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(release_red_when("m2"));
        let mut ops = two_suspects();
        attribute_round(&mut f, &mut ops, &corpus(3), four(), budget6()).unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["unit-tests".to_string()])]
        );
        assert!(!f.probes.is_empty(), "the suspects are probed");
        assert!(
            f.probes
                .iter()
                .all(|p| p.iter().all(|id| id == "m2" || id == "m3")),
            "only the suspects are probed: {:?}",
            f.probes
        );
    }

    #[test]
    fn two_suspects_the_probes_cannot_tell_apart_both_leave() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = false;
        f.filter = Some(release_red_when("m2"));
        let mut ops = two_suspects();
        attribute_round(&mut f, &mut ops, &corpus(3), four(), budget6()).unwrap();
        let mut gone: Vec<&str> = ops.ejected.iter().map(|(id, _)| id.as_str()).collect();
        gone.sort();
        assert_eq!(gone, ["m2", "m3"]);
    }

    #[test]
    fn a_unit_red_no_member_maps_to_runs_the_failing_packages_on_subsets_and_ejects_only_the_red_one(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(unit_red_when("m3"));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            diffs: vec![
                diff_of("m1", "gate/src/engine.rs"),
                diff_of("m2", "docs/x.md"),
                diff_of("m3", "scripts/odd.sh"),
                diff_of("m4", "gate/src/tests.rs"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            four(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m3".to_string(), vec!["unit-tests".to_string()])]
        );
        assert!(
            !f.probes.is_empty()
                && f.scoped.iter().all(|p| p
                    == &[
                        "loom=factory::tests::a_refused_dwell_blinds",
                        "loom=factory::tests::a_round_open_elsewhere"
                    ]),
            "{:?} {:?}",
            f.probes,
            f.scoped
        );
        let note = &ops.eject_evidence[0].1;
        assert!(
            note.contains("bisected by running member subsets")
                && FAILING.iter().all(|l| note.contains(l)),
            "{note}"
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3),
            "{end:?}"
        );
    }

    #[test]
    fn a_unit_red_the_base_has_too_ejects_nobody_by_list_position() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "unit-tests".into(),
                named: vec![],
                failed: vec![FAILING[0].to_string()],
            })
        }));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            four(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(ops.ejected.is_empty(), "{:?}", ops.ejected);
        let RoundEnd::BaseRed { base, why } = end else {
            panic!("{end:?}")
        };
        assert_eq!(base, "b4se");
        assert!(why.contains(FAILING[0]), "{why}");
        assert!(why.contains("last change touching it: c0de fixes"), "{why}");
    }

    #[test]
    fn an_integration_red_the_base_reproduces_ends_base_red() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "integration-tests".into(),
                named: vec![],
                failed: vec![FAILING[0].to_string()],
            })
        }));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            four(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::BaseRed { .. }), "{end:?}");
        assert!(ops.ejected.is_empty());
    }

    #[test]
    fn a_filter_red_whose_probes_cannot_answer_still_blocks() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "unit-tests".into(),
                named: vec![],
                failed: vec![FAILING[0].to_string()],
            })
        }));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            four(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
    }

    #[test]
    fn a_lint_red_naming_one_member_ejects_it_with_that_line_and_certifies_the_rest() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(lint_naming("m2"));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let RoundEnd::Land { members, .. } = end else {
            panic!("{end:?}")
        };
        assert_eq!(
            members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m3"]
        );
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["spira-lint".to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("ffx-vocabulary"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.incidents.is_empty());
        assert_eq!(
            f.main_kind.len(),
            2,
            "the corpus ran again on the survivors"
        );
        assert_eq!(f.main_kind[1], (MainKind::Corpus, corpus(3)));
    }

    #[test]
    fn a_member_stacked_on_the_one_a_lint_red_names_leaves_with_it() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(lint_naming("m1"));
        let mut ops = Ops::default();
        let mut ms = three();
        ms[1].stack.insert("m1".into(), "m1-tip".into());
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            ms,
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let RoundEnd::Land { members, .. } = end else {
            panic!("{end:?}")
        };
        assert_eq!(
            members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m3"]
        );
        assert_eq!(
            ops.owner_flags,
            vec![("m1".to_string(), true), ("m2".to_string(), false)]
        );
    }

    const E0063: &str = "error[E0063]: missing field `directed` in initializer of `Kind`\n --> lc/src/callers.rs:726:13\n";

    fn md(id: &str, path: &str, added: &str) -> MemberDiff {
        MemberDiff {
            id: id.into(),
            paths: vec![path.into()],
            added: added.into(),
        }
    }

    /// A tree that fails to build while every named member is in it.
    fn broken_by(all: &'static [&'static str]) -> BuildFn {
        Box::new(move |tree| {
            all.iter()
                .all(|m| tree.contains(*m))
                .then(|| E0063.to_string())
        })
    }

    #[test]
    fn a_workspace_build_red_at_a_file_one_member_touches_ejects_it_and_recuts_without_a_pause() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(broken_by(&["m2"]));
        let mut ops = Ops {
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", "lc/src/callers.rs", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let RoundEnd::Land { members, .. } = end else {
            panic!("{end:?}")
        };
        assert_eq!(
            members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m3"]
        );
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![WORKSPACE_BUILD.to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("callers.rs:726"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(
            ops.incidents.is_empty(),
            "a mapped build red writes no pause: {:?}",
            ops.incidents
        );
    }

    #[test]
    fn a_merged_config_delta_that_does_not_apply_ejects_the_member_whose_merge_breaks_it() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.compiler = Some(Box::new(|tree| {
            tree.contains("m2").then(|| {
                format!(
                    "round-vm: {} — the head's config delta does not apply on the merged tree\n",
                    buildattr::CONFIG_DELTA_RED
                )
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![CONFIG_DELTA.to_string()])]
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    const IMAGE_RED: &str = "build-error: round image build 42553374751 failed at build / Build, image and tag\nround image setup failed rc=1\n";

    fn image_broken_while(culprit: &'static str) -> BuildFn {
        Box::new(move |tree| tree.contains(culprit).then(|| IMAGE_RED.to_string()))
    }

    const ABORT: &str = "round-vm: PHASE-FAILED phase=suites rc=134\nround-vm run: the remote runner exited 134, outside testenv's contract (0-4) — no verdict";

    #[test]
    fn testenv_killed_by_a_signal_ejects_the_one_member_that_changes_the_round_vm() {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m2".into(), ABORT.into()));
        let mut ops = Ops {
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", "round-vm/src/run.rs", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![RUNNER_ABORT.to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("rc=134"),
            "{:?}",
            ops.eject_evidence
        );
    }

    #[test]
    fn a_pass_that_found_the_base_moved_rebuilds_once_on_the_new_base_and_lands() {
        let mut f = Fake::new(|_, _| false);
        f.fails_once = Some(format!("{BASE_MOVED}aaa to bbb by 1234abc land sp-z — the pass built a tree the round was not cut for, not a harness fault"));
        let mut ops = Ops {
            moves: vec!["bbb".into()],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
        assert_eq!(
            f.bases,
            vec!["bbb".to_string()],
            "the runner builds on the new base"
        );
        assert!(ops.ejected.is_empty() && ops.incidents.is_empty());
    }

    #[test]
    fn a_base_that_moved_again_after_the_one_rebuild_is_an_error_not_a_loop() {
        let mut f = Fake::new(|_, _| false);
        let moved = format!("{BASE_MOVED}aaa to bbb by 1234abc land sp-z");
        f.fails_once = Some(moved.clone());
        let mut ops = Ops {
            moves: vec!["bbb".into()],
            ..Ops::default()
        };
        struct Twice(Fake, String);
        impl RoundRunner for Twice {
            fn set_members(&mut self, m: &[Member]) {
                self.0.set_members(m)
            }
            fn set_base(&mut self, sha: &str) {
                self.0.set_base(sha);
                self.0.fails_once = Some(self.1.clone());
            }
            fn start_main(&mut self, k: MainKind, s: &[String]) -> Result<(), String> {
                self.0.start_main(k, s)
            }
            fn poll_main(&mut self) -> Result<MainPoll, String> {
                self.0.poll_main()
            }
            fn stop_main(&mut self) -> Result<(), String> {
                self.0.stop_main()
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
        let mut twice = Twice(f, moved);
        let err = attribute_round(
            &mut twice,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap_err();
        assert!(err.starts_with(BASE_MOVED), "{err}");
    }

    const UNDECLARED: &str = "round-vm: harness fault — install failed\ntestenv container: SPIRA_X_SECS is not declared: the config has no x_secs — every key is required; refusing";

    #[test]
    fn an_undeclared_key_fault_ejects_the_member_whose_diff_retires_a_config_key_with_the_fault_lines(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m2".into(), UNDECLARED.into()));
        let mut ops = Ops {
            config_changes: vec![("m2".into(), "retires x_secs".into())],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![CONFIG_FAULT.to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("retires x_secs")
                && ops.eject_evidence[0]
                    .1
                    .contains("SPIRA_X_SECS is not declared"),
            "{:?}",
            ops.eject_evidence
        );
    }

    #[test]
    fn an_undeclared_key_fault_with_no_member_changing_a_key_stays_a_harness_fault() {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m2".into(), UNDECLARED.into()));
        let mut ops = Ops::default();
        let err = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap_err();
        assert!(
            err.contains("harness fault") && ops.ejected.is_empty(),
            "{err}"
        );
    }

    #[test]
    fn a_member_that_leaves_for_a_runner_abort_takes_the_suite_it_adds_out_of_the_next_corpus() {
        let mut f = Fake::new(|_, _| false);
        f.added_by = adds("test-01.sh", &["m2"]);
        f.aborts_with = Some(("m2".into(), ABORT.into()));
        let mut ops = Ops {
            adds: adds("test-01.sh", &["m2"]),
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", "round-vm/src/run.rs", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
    }

    #[test]
    fn testenv_killed_by_a_signal_with_no_or_several_runner_touching_members_stays_a_harness_fault()
    {
        for diffs in [
            vec![md("m1", "doc/x.md", "+p\n")],
            vec![
                md("m1", "round-vm/a.rs", "+p\n"),
                md("m2", "testenv/b.rs", "+p\n"),
            ],
        ] {
            let mut f = Fake::new(|_, _| false);
            f.aborts_with = Some(("m1".into(), ABORT.into()));
            let mut ops = Ops {
                diffs,
                ..Ops::default()
            };
            let err = attribute_round(
                &mut f,
                &mut ops,
                &corpus(3),
                three(),
                Budget {
                    slots: 6,
                    maxpar: 2,
                },
            )
            .unwrap_err();
            assert!(err.contains("rc=134"), "{err}");
            assert!(ops.ejected.is_empty());
        }
    }

    #[test]
    fn a_testenv_abort_no_member_touches_the_runner_for_ejects_the_member_whose_tree_alone_aborts_it(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m2".into(), ABORT.into()));
        f.bisects = true;
        let mut ops = Ops {
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", "doc/z.md", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![RUNNER_ABORT.to_string()])]
        );
        assert_eq!(ops.owner_flags, vec![("m2".to_string(), true)]);
        assert!(
            ops.eject_evidence[0].1.contains("rc=134")
                && ops.eject_evidence[0].1.contains("bisected"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.judged.is_empty() && ops.incidents.is_empty());
    }

    #[test]
    fn a_testenv_abort_several_members_touch_the_runner_for_is_bisected_to_the_member_that_aborts_it(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m3".into(), ABORT.into()));
        f.bisects = true;
        let mut ops = Ops {
            diffs: vec![
                md("m1", "round-vm/a.rs", "+p\n"),
                md("m2", "testenv/b.rs", "+p\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2"]);
        assert_eq!(
            ops.ejected,
            vec![("m3".to_string(), vec![RUNNER_ABORT.to_string()])]
        );
    }

    #[test]
    fn a_testenv_abort_in_a_one_member_round_ejects_that_member_without_a_probe() {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m1".into(), ABORT.into()));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(ops.owner_flags, vec![("m1".to_string(), true)]);
        assert!(f.probes.is_empty());
    }

    #[test]
    fn an_abandoned_round_ends_within_one_poll_and_launches_no_further_leg() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
        f.maxpar = 4;
        f.dur.insert("test-00.sh".into(), 2);
        f.abandoned_at = Some(1);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(40),
            three(),
            Budget {
                slots: 6,
                maxpar: 4,
            },
        )
        .unwrap();
        assert_eq!(end, RoundEnd::Abandoned);
        assert!(f.t <= 2, "ended at t={}, abandoned at t=1", f.t);
        assert!(f.jobs.is_empty(), "no attribution job launched");
        assert_eq!(f.main_kind.len(), 1);
        assert_eq!(f.stopped, 1, "the running leg was stopped");
        assert!(ops.ejected.is_empty() && ops.recorded.is_empty());
    }

    #[test]
    fn a_round_abandoned_while_its_image_build_is_retried_starts_no_second_run() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(Box::new(|_| Some(IMAGE_RED.to_string())));
        f.abandoned_after_starts = Some(1);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(8),
            three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(end, RoundEnd::Abandoned);
        assert_eq!(f.main_kind.len(), 1, "the retry leg never started");
        assert!(ops.incidents.is_empty(), "an abandoned round files nothing");
    }

    #[test]
    fn a_runner_fault_on_an_abandoned_round_is_not_probed_for_a_culprit() {
        let mut f = Fake::new(|_, _| false);
        f.aborts_with = Some(("m1".into(), "testenv died on a signal".into()));
        f.bisects = true;
        f.abandoned_after_starts = Some(1);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(8),
            three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(end, RoundEnd::Abandoned);
        assert!(f.probes.is_empty(), "no probe leg launched");
    }

    #[test]
    fn an_image_build_red_ejects_the_one_member_whose_diff_touches_the_image_recipe_without_a_pause(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(image_broken_while("m2"));
        let mut ops = Ops {
            diffs: vec![
                md("m1", "doc/x.md", "+p\n"),
                md("m2", ".github/workflows/round-ami-build.yml", "+x\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![WORKSPACE_BUILD.to_string()])]
        );
        assert!(
            ops.eject_evidence[0]
                .1
                .contains("round image setup failed rc=1"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn an_image_build_red_no_member_touches_the_recipe_for_is_retried_once_then_filed() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(image_broken_while("m1"));
        let mut ops = Ops {
            diffs: vec![md("m1", "doc/x.md", "+p\n")],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(f.main_kind.len(), 2, "one retry of the same members");
        assert!(ops.ejected.is_empty());
        assert_eq!(
            ops.incidents,
            vec![(
                "workspace-build".to_string(),
                vec![WORKSPACE_BUILD.to_string()]
            )]
        );
    }

    #[test]
    fn an_image_build_red_with_two_recipe_touching_members_ejects_neither_and_files() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(image_broken_while("m1"));
        let mut ops = Ops {
            diffs: vec![
                md("m1", "flake.nix", "+a\n"),
                md("m2", "rust-toolchain.toml", "+b\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert!(ops.ejected.is_empty());
        assert_eq!(ops.incidents.len(), 1);
    }

    #[test]
    fn an_image_build_red_whose_recipe_toucher_is_the_only_member_ejects_it_and_files() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(image_broken_while("m1"));
        let mut ops = Ops {
            diffs: vec![md("m1", "flake.nix", "+a\n")],
            ..Ops::default()
        };
        let only = vec![three()[0].clone()];
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            only,
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(ops.ejected.len(), 1);
    }

    #[test]
    fn an_image_build_red_that_clears_on_the_retry_lands_every_member() {
        let mut f = Fake::new(|_, _| false);
        let tried = std::cell::Cell::new(false);
        f.compiler = Some(Box::new(move |_| {
            (!tried.replace(true)).then(|| IMAGE_RED.to_string())
        }));
        let mut ops = Ops {
            diffs: vec![md("m1", "doc/x.md", "+p\n")],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
        assert!(ops.ejected.is_empty() && ops.incidents.is_empty());
    }

    #[test]
    fn a_workspace_build_red_naming_what_another_member_adds_sequences_the_caller_behind_it() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(broken_by(&["m1", "m2"]));
        let mut ops = Ops {
            diffs: vec![
                md("m1", "lc/src/events.rs", "+    directed: bool,\n"),
                md("m2", "lc/src/callers.rs", "+    Kind::Claim { x: 1 }\n"),
                md("m3", "doc/y.md", "+q\n"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let RoundEnd::Land { members, .. } = end else {
            panic!("{end:?}")
        };
        assert_eq!(
            members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m3"]
        );
        assert_eq!(
            ops.sequenced,
            vec![("m2".to_string(), vec!["m1".to_string()])]
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_workspace_build_red_that_maps_to_no_member_still_files_the_incident() {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(broken_by(&["m1"]));
        let mut ops = Ops {
            diffs: vec![md("m1", "doc/x.md", "+p\n")],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(
            ops.incidents,
            vec![(
                "workspace-build".to_string(),
                vec![WORKSPACE_BUILD.to_string()]
            )]
        );
        assert!(ops.ejected.is_empty());
    }

    fn unmapped_build_red(broken: &'static [&'static str], n: usize) -> (Fake, Ops) {
        let mut f = Fake::new(|_, _| false);
        f.compiler = Some(broken_by(broken));
        f.bisects = true;
        let ops = Ops {
            diffs: (1..=n)
                .map(|i| md(&format!("m{i}"), "doc/x.md", "+p\n"))
                .collect(),
            ..Ops::default()
        };
        (f, ops)
    }

    fn land_ids(end: RoundEnd) -> Vec<String> {
        let RoundEnd::Land { members, .. } = end else {
            panic!("{end:?}")
        };
        members.iter().map(|m| m.id.clone()).collect()
    }

    #[test]
    fn an_unmapped_workspace_build_red_that_one_member_breaks_alone_ejects_that_member_by_bisection(
    ) {
        let (mut f, mut ops) = unmapped_build_red(&["m2"], 3);
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec![WORKSPACE_BUILD.to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("bisected"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn an_unmapped_workspace_build_red_that_only_a_pair_breaks_holds_the_later_member_behind_the_earlier(
    ) {
        let (mut f, mut ops) = unmapped_build_red(&["m1", "m3"], 3);
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2"]);
        assert_eq!(
            ops.sequenced,
            vec![("m3".to_string(), vec!["m1".to_string()])]
        );
        assert!(
            ops.eject_evidence[0]
                .1
                .contains("fails to build only with m1 merged"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn an_unmapped_workspace_build_red_that_needs_three_members_is_still_filed() {
        let (mut f, mut ops) = unmapped_build_red(&["m1", "m2", "m3"], 3);
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(
            ops.incidents,
            vec![(
                "workspace-build".to_string(),
                vec![WORKSPACE_BUILD.to_string()]
            )]
        );
        assert!(ops.ejected.is_empty() && ops.sequenced.is_empty());
    }

    #[test]
    fn bisecting_an_unmapped_workspace_build_red_builds_few_subsets() {
        let (mut f, mut ops) = unmapped_build_red(&["m7"], 8);
        let ms: Vec<Member> = (1..=8)
            .map(|i| Member {
                id: format!("m{i}"),
                ..three()[0].clone()
            })
            .collect();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            ms,
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
            vec!["m7"]
        );
        assert!(f.probes.len() <= 4, "{:?}", f.probes);
    }

    #[test]
    fn bisecting_an_unmapped_pair_in_eight_members_builds_at_most_two_searches_and_a_check() {
        let (mut f, mut ops) = unmapped_build_red(&["m2", "m7"], 8);
        let ms: Vec<Member> = (1..=8)
            .map(|i| Member {
                id: format!("m{i}"),
                ..three()[0].clone()
            })
            .collect();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            ms,
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.sequenced,
            vec![("m7".to_string(), vec!["m2".to_string()])]
        );
        assert!(f.probes.len() <= 9, "{:?}", f.probes);
    }

    fn unit_red_together(culprits: &'static [&'static str]) -> FilterFn {
        Box::new(move |tree| {
            culprits
                .iter()
                .all(|c| tree.contains(*c))
                .then(|| FilterRed {
                    name: "unit-tests".into(),
                    named: vec![],
                    failed: vec![],
                })
        })
    }

    #[test]
    fn a_unit_test_red_only_a_pair_causes_holds_the_later_member_behind_the_earlier_and_lands_the_rest(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_together(&["m1", "m3"]));
        f.bisects = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2"]);
        assert_eq!(
            ops.sequenced,
            vec![("m3".to_string(), vec!["m1".to_string()])]
        );
        assert!(
            ops.eject_evidence[0].1.contains("red only with m1"),
            "{:?}",
            ops.eject_evidence
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_filter_red_ejection_carries_what_the_red_probe_printed() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_together(&["m2"]));
        f.bisects = true;
        f.probe_said = "fence-run: callers.rs:726: retires x_secs".into();
        let mut ops = Ops::default();
        attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let note = &ops.eject_evidence[0].1;
        assert!(
            note.contains("with only m2 merged") && note.contains("callers.rs:726: retires x_secs"),
            "{note}"
        );
    }

    #[test]
    fn an_ejection_carries_the_probe_of_that_member_alone_and_only_the_tail_of_a_long_one() {
        let said = vec![
            (
                vec!["m1".to_string(), "m2".to_string()],
                "pair output".to_string(),
            ),
            (
                vec!["m2".to_string()],
                format!("{}tail-é", "x".repeat(MAX_PROBE_OUTPUT * 2)),
            ),
            (vec!["m3".to_string()], "last output".to_string()),
        ];
        let eject = |id: &str| Action::Eject {
            id: id.into(),
            line: "red".into(),
        };
        let got = with_probe_output(vec![eject("m2"), eject("m9")], &said);
        let Action::Eject { line, .. } = &got[0] else {
            panic!()
        };
        assert!(
            line.starts_with("red\n")
                && line.ends_with("tail-é")
                && line.len() <= 4 + MAX_PROBE_OUTPUT + 1,
            "{}",
            line.len()
        );
        let Action::Eject { line, .. } = &got[1] else {
            panic!()
        };
        assert_eq!(
            line, "red\nlast output",
            "a member never probed alone gets the last red probe"
        );
    }

    #[test]
    fn a_unit_test_red_one_member_causes_alone_ejects_that_member() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_together(&["m2"]));
        f.bisects = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["unit-tests".to_string()])]
        );
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_unit_test_red_that_does_not_reproduce_on_the_lone_first_member_is_filed_not_a_panic() {
        let seen = std::cell::Cell::new(0);
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(Box::new(move |_| {
            seen.set(seen.get() + 1);
            (seen.get() == 1).then(|| FilterRed {
                name: "unit-tests".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        f.bisects = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
    }

    const FLIP_UNIT: &str = "FAIL loom factory::tests::a";

    fn flip_red(failed: &[&str]) -> FilterRed {
        FilterRed {
            name: "unit-tests".into(),
            named: vec![],
            failed: failed.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn red_once_on_the_whole_tree(red: FilterRed) -> FilterFn {
        let fired = std::cell::Cell::new(false);
        Box::new(move |tree| (tree.len() == 3 && !fired.replace(true)).then(|| red.clone()))
    }

    fn flip_round(f: &mut Fake, ops: &mut Ops) -> RoundEnd {
        attribute_round(
            f,
            ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap()
    }

    #[test]
    fn a_unit_red_green_on_the_base_on_every_member_alone_and_on_a_full_tree_rerun_is_disabled_and_the_round_lands_every_member(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(flip_red(&[FLIP_UNIT])));
        f.bisects = true;
        f.rerun_green = true;
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert_eq!(
            ops.disabled,
            vec![vec!["unit:loom factory::tests::a".to_string()]]
        );
        assert!(ops.ejected.is_empty(), "{:?}", ops.ejected);
        assert_eq!(
            f.main_kind.len(),
            2,
            "the round certifies again: {:?}",
            f.main_kind
        );
        assert!(
            f.scoped.iter().any(|s| s == &["loom=factory::tests::a"]),
            "{:?}",
            f.scoped
        );
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
    }

    #[test]
    fn a_flip_in_the_corpus_pass_red_is_disabled_and_the_round_runs_its_own_pass_without_it() {
        let mut f = Fake::new(|_, _| false);
        f.prior = Some(flip_red(&[FLIP_UNIT]));
        f.bisects = true;
        f.rerun_green = true;
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert_eq!(
            ops.disabled,
            vec![vec!["unit:loom factory::tests::a".to_string()]]
        );
        assert_eq!(
            f.main_kind.len(),
            1,
            "one certifying pass: {:?}",
            f.main_kind
        );
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
    }

    #[test]
    fn a_unit_red_that_reproduces_on_the_full_tree_rerun_is_not_a_flip_and_blocks() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_together(&["m1", "m2", "m3"]));
        f.bisects = true;
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert!(ops.disabled.is_empty());
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
    }

    #[test]
    fn a_unit_red_with_a_member_that_has_no_verdict_alone_is_not_a_flip_and_blocks() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(flip_red(&[FLIP_UNIT])));
        f.bisects = true;
        f.rerun_green = true;
        f.unknown_alone = Some("m2".into());
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert!(ops.disabled.is_empty());
        match end {
            RoundEnd::Blocked(why) => {
                assert!(why.contains("names no member that can be ejected"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_red_naming_more_than_the_flip_limit_of_tests_is_not_a_flip() {
        let lines: Vec<String> = (0..=MAX_FLIP_TESTS)
            .map(|i| format!("FAIL loom t::t{i}"))
            .collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(flip_red(&lines)));
        f.bisects = true;
        f.rerun_green = true;
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert!(ops.disabled.is_empty());
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
    }

    #[test]
    fn an_integration_flip_is_named_by_the_package_its_directory_resolves_to() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv lint_x".into()],
        }));
        f.bisects = true;
        f.rerun_green = true;
        let graph = PackageGraph {
            dirs: [("testenv".to_string(), "testenv".to_string())].into(),
            deps: Default::default(),
        };
        let mut ops = Ops {
            graph: Some(graph),
            ..Ops::default()
        };
        let end = flip_round(&mut f, &mut ops);
        assert_eq!(
            ops.disabled,
            vec![vec!["integration:testenv lint_x".to_string()]]
        );
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
    }

    #[test]
    fn a_flip_in_a_directory_no_package_owns_is_not_a_flip() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(flip_red(&[
            "FAIL dir:nowhere t",
        ])));
        f.bisects = true;
        f.rerun_green = true;
        let mut ops = Ops::default();
        let end = flip_round(&mut f, &mut ops);
        assert!(ops.disabled.is_empty());
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
    }

    #[test]
    fn a_failure_to_disable_a_flip_blocks_the_round_naming_it() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(red_once_on_the_whole_tree(flip_red(&[FLIP_UNIT])));
        f.bisects = true;
        f.rerun_green = true;
        let mut ops = Ops {
            refuse_disable: Some("registry locked".into()),
            ..Ops::default()
        };
        match flip_round(&mut f, &mut ops) {
            RoundEnd::Blocked(why) => assert!(
                why.contains("registry locked") && why.contains("unit:loom factory::tests::a"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_unit_test_red_that_needs_three_members_is_still_filed() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(unit_red_together(&["m1", "m2", "m3"]));
        f.bisects = true;
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(
            ops.incidents,
            vec![(
                "unattributed".to_string(),
                vec![
                    "unit-tests".to_string(),
                    "run alone: base green, m1 green, m2 green, m3 green".to_string()
                ]
            )]
        );
    }

    const INTEG_FAILING: [&str; 2] = [
        "FAIL dir:spira-runtime leg_lost::pauses",
        "FAIL dir:spira-runtime leg_lost::abandons",
    ];

    fn integration_red_when(culprits: &'static [&'static str]) -> FilterFn {
        Box::new(move |tree| {
            culprits
                .iter()
                .all(|c| tree.contains(*c))
                .then(|| FilterRed {
                    name: "integration-tests".into(),
                    named: vec![],
                    failed: INTEG_FAILING.iter().map(|l| l.to_string()).collect(),
                })
        })
    }

    #[test]
    fn an_integration_red_in_a_crate_dir_only_one_members_diff_touches_ejects_that_member_and_certifies_the_rest(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(integration_red_when(&["m3"]));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            diffs: vec![
                diff_of("m1", "gate/src/engine.rs"),
                diff_of("m2", "docs/x.md"),
                diff_of("m3", "spira-runtime/src/tasks/a/batcher_local.rs"),
            ],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m3".to_string(), vec!["integration-tests".to_string()])]
        );
        assert_eq!(land_ids(end), vec!["m1", "m2"]);
    }

    #[test]
    fn a_two_member_round_red_on_one_integration_test_runs_only_that_test_per_member_ejects_its_owner_and_lands_the_other_without_the_corpus_again(
    ) {
        let red = FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv lint_does_not_refuse_a_key".into()],
        };
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.runs_alone = true;
        f.filter = Some(Box::new({
            let red = red.clone();
            move |tree| tree.contains("m2").then(|| red.clone())
        }));
        f.prior = Some(red);
        let mut ops = Ops {
            graph: Some(PackageGraph {
                dirs: [("testenv".to_string(), "testenv".to_string())].into(),
                deps: Default::default(),
            }),
            ..Ops::default()
        };
        let two = vec![member("m1"), member("m2")];
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(40),
            two,
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["integration-tests".to_string()])]
        );
        assert!(!f.scoped.is_empty());
        assert!(
            f.scoped
                .iter()
                .all(|s| s == &["testenv=lint_does_not_refuse_a_key"]),
            "{:?}",
            f.scoped
        );
        assert!(
            f.main_kind.is_empty(),
            "no corpus or verify run: {:?}",
            f.main_kind
        );
        assert_eq!(land_ids(end), vec!["m1"]);
    }

    #[test]
    fn a_red_its_named_tests_do_not_reproduce_alone_is_attributed_on_the_whole_package() {
        let red = FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv lint_x".into()],
        };
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.runs_alone = true;
        f.narrow_clears = true;
        f.filter = Some(Box::new({
            let red = red.clone();
            move |tree| tree.contains("m2").then(|| red.clone())
        }));
        f.prior = Some(red);
        let mut ops = Ops {
            graph: Some(PackageGraph {
                dirs: [("testenv".to_string(), "testenv".to_string())].into(),
                deps: Default::default(),
            }),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["integration-tests".to_string()])]
        );
        assert!(
            f.scoped.iter().any(|s| s == &["testenv=lint_x"])
                && f.scoped.iter().any(|s| s == &["testenv"]),
            "{:?}",
            f.scoped
        );
        assert_eq!(land_ids(end), vec!["m1"]);
    }

    #[test]
    fn a_named_test_red_no_probe_can_attribute_blocks_the_round_without_running_the_corpus() {
        let red = FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv lint_x".into()],
        };
        let mut f = Fake::new(|_, _| false);
        f.prior = Some(red);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert!(f.main_kind.is_empty());
    }

    #[test]
    fn a_round_that_loses_its_only_member_to_a_named_test_red_says_who_left() {
        let red = FilterRed {
            name: "integration-tests".into(),
            named: vec![],
            failed: vec!["FAIL dir:testenv t".into()],
        };
        let mut f = Fake::new(|_, _| false);
        f.prior = Some(red);
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            end,
            RoundEnd::Emptied("integration-tests red ejected every member (m1)".into())
        );
    }

    #[test]
    fn a_red_that_ejects_one_of_two_members_names_who_left_and_who_stayed_when_the_rest_cannot_be_re_cut(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(lint_naming("m1"));
        let mut ops = Ops {
            rebuilt_empty: true,
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            vec![member("m1"), member("m2")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        match end {
            RoundEnd::Emptied(why) => assert!(why.contains("ejected m1, kept m2"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_integration_red_no_member_maps_to_is_bisected_on_the_failing_crate_alone() {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(integration_red_when(&["m2"]));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["integration-tests".to_string()])]
        );
        assert!(
            f.scoped
                .iter()
                .all(|p| p == &["runtime=leg_lost::pauses", "runtime=leg_lost::abandons"]),
            "{:?}",
            f.scoped
        );
        assert_eq!(land_ids(end), vec!["m1", "m3"]);
    }

    #[test]
    fn an_integration_red_no_single_removal_fixes_is_left_open_and_pages_with_the_failing_test_names(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.bisects = true;
        f.filter = Some(integration_red_when(&["m1", "m2", "m3"]));
        let mut ops = Ops {
            graph: Some(loom_graph()),
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let RoundEnd::Blocked(why) = end else {
            panic!("{end:?}")
        };
        assert!(ops.ejected.is_empty());
        assert!(
            why.contains("leg_lost::pauses") && why.contains("leg_lost::abandons"),
            "{why}"
        );
        let (kind, named) = &ops.incidents[0];
        assert_eq!(kind, "unattributed");
        assert!(
            named.iter().any(|n| n.contains("leg_lost::pauses")),
            "{named:?}"
        );
    }

    #[test]
    fn sp_56d3l3_a_lint_red_that_ejects_the_only_member_ends_emptied_not_blocked() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(lint_naming("m1"));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["spira-lint".to_string()])]
        );
    }

    #[test]
    fn sp_evwve7_a_filter_red_naming_no_member_ejects_the_only_member_of_the_round() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "integration-tests".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            vec![member("m1")],
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Emptied(_)), "{end:?}");
        assert_eq!(
            ops.ejected,
            vec![("m1".to_string(), vec!["integration-tests".to_string()])]
        );
        assert_eq!(ops.owner_flags, vec![("m1".to_string(), true)]);
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
    }

    #[test]
    fn a_lint_red_naming_no_member_is_run_alone_and_ejects_the_member_whose_tree_is_red() {
        let mut f = Fake::new(|_, _| false);
        f.runs_alone = true;
        f.filter = Some(Box::new(|tree| {
            (tree.contains("m2") || tree.contains("m3")).then(|| FilterRed {
                name: "spira-lint".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let ejected: Vec<&str> = ops.ejected.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(ejected, ["m2", "m3"], "{:?}", ops.eject_evidence);
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 1),
            "{end:?}"
        );
    }

    #[test]
    fn a_fence_red_naming_no_member_ejects_the_member_whose_tree_fails_the_fence_and_lands_the_rest(
    ) {
        let mut f = Fake::new(|_, _| false);
        f.runs_alone = true;
        f.filter = Some(Box::new(|tree| {
            tree.contains("m2").then(|| FilterRed {
                name: "fence-fixture-config".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        let ejected: Vec<&str> = ops.ejected.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(ejected, ["m2"], "{:?}", ops.eject_evidence);
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 2),
            "{end:?}"
        );
    }

    #[test]
    fn a_lint_red_naming_no_member_is_filed_once_and_does_not_run_the_corpus_again() {
        let mut f = Fake::new(|_, _| false);
        f.filter = Some(Box::new(|_| {
            Some(FilterRed {
                name: "spira-lint".into(),
                named: vec![],
                failed: vec![],
            })
        }));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(3),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
        assert_eq!(
            ops.incidents,
            vec![("unattributed".to_string(), vec!["spira-lint".to_string()])]
        );
        assert!(ops.ejected.is_empty());
        assert_eq!(f.main_kind.len(), 1);
    }

    #[test]
    fn a_sole_member_red_on_suites_no_one_owns_is_ejected_on_each_suite_and_the_round_ends_emptied()
    {
        let mut ops = Ops::default();
        let suites = vec!["test-01.sh".to_string(), "test-02.sh".to_string()];
        let end = eject_sole_member_on_suites(
            &mut ops,
            &[member("m1")],
            "unattributed red test-01.sh,test-02.sh",
            &suites,
        )
        .unwrap();
        assert!(
            matches!(&end, RoundEnd::Emptied(why) if why.contains("test-01.sh,test-02.sh") && why.contains("m1")),
            "{end:?}"
        );
        assert_eq!(ops.ejected, vec![("m1".to_string(), suites)]);
    }

    #[test]
    fn an_unattributed_red_with_the_base_unmoved_goes_straight_to_the_judge() {
        let mut f =
            Fake::new(|s, tree| s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2")));
        let mut ops = Ops::default();
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)));
        assert_eq!(
            ops.judged,
            vec![(
                vec!["test-01.sh".to_string()],
                vec!["m1".to_string(), "m2".to_string(), "m3".to_string()]
            )]
        );
        assert!(ops.incidents.is_empty());
        assert!(ops.ejected.is_empty());
        assert_eq!(f.main_kind.len(), 1, "no retry: the base did not move");
    }

    #[test]
    fn an_unattributed_red_is_retried_once_on_the_moved_base_then_judged() {
        let mut f =
            Fake::new(|s, tree| s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2")));
        let mut ops = Ops {
            moves: vec!["base2".into(), "base3".into()],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(matches!(end, RoundEnd::Blocked(_)));
        assert_eq!(
            f.bases,
            vec!["base2".to_string()],
            "retried once, never twice"
        );
        assert_eq!(f.main_kind.len(), 2);
        assert_eq!(
            f.main_kind[1],
            (MainKind::Verify, vec!["test-01.sh".to_string()])
        );
        assert_eq!(ops.judged.len(), 1);
    }

    #[test]
    fn an_unattributed_red_that_clears_on_the_moved_base_lands() {
        let moved = std::rc::Rc::new(std::cell::Cell::new(false));
        let m2 = moved.clone();
        let mut f = Fake::new(move |s, tree| {
            !m2.get() && s == "test-01.sh" && (tree.contains("m1") || tree.contains("m2"))
        });
        f.on_base = Some(moved);
        let mut ops = Ops {
            moves: vec!["base2".into()],
            ..Ops::default()
        };
        let end = attribute_round(
            &mut f,
            &mut ops,
            &corpus(4),
            three(),
            Budget {
                slots: 6,
                maxpar: 2,
            },
        )
        .unwrap();
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3),
            "{end:?}"
        );
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
            fn stop_main(&mut self) -> Result<(), String> {
                self.0.stop_main()
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
        let s = drive(
            &mut f,
            &Ops::default(),
            MainKind::Corpus,
            &corpus(3),
            &three(),
            Budget {
                slots: 4,
                maxpar: 2,
            },
        )
        .unwrap();
        assert_eq!(s.decision.records.len(), 1);
        assert_eq!(s.decision.records[0].suite, "test-01.sh");
        assert_eq!(
            s.decision.flaky,
            vec!["test-01.sh".to_string()],
            "its reruns were green"
        );
    }
}
