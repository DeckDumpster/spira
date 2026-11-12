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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainEnd {
    /// The suites ran (green or red).
    Ran,
    /// The round was abandoned while it ran; no verdict is owed.
    Abandoned,
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
    /// The round was abandoned: nothing drives it any more, and nothing more may be launched.
    fn abandoned(&mut self) -> bool {
        false
    }
    /// The low-pass filter (lint, a fence) that ended the corpus pass before any suite ran, if one did.
    fn filter_red(&mut self) -> Option<FilterRed> {
        None
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
    /// Like `filter_green`, judging only the tests of `packages`, the ones that failed.
    fn filter_green_in(
        &mut self,
        name: &str,
        _packages: &[String],
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
    fn eject(&mut self, member: &Member, suites: &[String], owner: bool);
    /// Ejects `member` on a filter's red, carrying the line that names it as the evidence.
    fn eject_on(&mut self, member: &Member, filter: &str, _line: &str, owner: bool) {
        self.eject(member, &[filter.to_string()], owner);
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
    /// Holds `member` behind `behind` — it leaves this round and is admitted again only once
    /// they have landed — carrying the compiler error as the reason.
    fn sequence(&mut self, member: &Member, behind: &[Id], line: &str) {
        self.eject_on(
            member,
            WORKSPACE_BUILD,
            &format!("held behind {}: {line}", behind.join(",")),
            false,
        );
    }
    /// `kind`: `base`, `unattributed`, `integration` or `workspace-build`.
    fn incident(&mut self, kind: &str, suites: &[String]);
    /// The members whose diff adds the file `suite`, which the base lacks.
    fn adding(&self, _suite: &str, _members: &[Id]) -> Vec<Id> {
        vec![]
    }
    /// The members whose diff touches `suite`.
    fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id>;
    /// Commit the deletion of every proven flip into the round tree and re-run the checks
    /// the deletion can break; an error blocks the round.
    fn delete_flips(&mut self, suites: &[String]) -> Result<(), String>;
    fn record(&mut self, iteration: u32, decision: &Decision);
    /// `member` owns red `suite`; `rerun` is that suite rerun on the full round tree (`None`
    /// when the rerun faulted): green there means it flipped.
    fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>);
    /// The pass was stopped at a red `owner` alone owns; `unreached` are the suites it had not
    /// reported.
    fn preempted(&mut self, owner: &Id, unreached: &[String]);
    /// Waits for every mender of this red pass and decides the next pass once. `None` when no
    /// mender owns the pass, which leaves the owners to be ejected as before.
    fn settle_mending(&mut self, _: &Decision, _: &[Member]) -> Option<Result<Mended, String>> {
        None
    }
}

/// The round-level pseudo-suite name a --with-bins build failure (exit 4) reports under —
/// never a real `test-*.sh` file, so it can't collide with one a suite selection would pick.
pub const WORKSPACE_BUILD: &str = "workspace-build";

/// What a main run's error starts with when the pass built a tree the round was not cut for
/// because the base moved under it: the round is rebuilt once on the new base, never sent to
/// attribution as a harness fault.
pub const BASE_MOVED: &str = "round-vm: base moved from ";
pub const RUNNER_ABORT: &str = "runner-abort";

/// Ids of the settled-owner reruns start here, clear of the attributor's own.
const RERUN_ID_BASE: u64 = 1 << 40;

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
    loop {
        if let Some((_, r)) = runner.poll_jobs().into_iter().find(|(id, _)| *id == job.id) {
            return (r != JobResult::Fault).then_some(r);
        }
        runner.wait();
    }
}

/// A main run stopped at the first red a single member owns, instead of run out.
pub struct Preempt {
    pub owner: Id,
    /// The suites the run had not reported when it was stopped.
    pub unreached: Vec<String>,
    /// Every suite that did not finish green: what the survivors still owe.
    pub rerun: Vec<String>,
}

pub struct Settled {
    pub preempt: Option<Preempt>,
    pub decision: Decision,
    pub end: MainEnd,
    /// When the main run ended and when the last red settled (runner clock).
    pub main_end_at: u64,
    pub settled_at: u64,
    pub filter: Option<FilterRed>,
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
    let mut green_suites = std::collections::BTreeSet::new();
    let mut main_end_at = None;
    loop {
        if runner.abandoned() {
            runner.stop_main()?;
            let now = runner.now();
            return Ok(Settled {
                preempt: None,
                decision: Decision::default(),
                end: MainEnd::Abandoned,
                main_end_at: now,
                settled_at: now,
                filter: None,
            });
        }
        let poll = runner.poll_main()?;
        let now = runner.now();
        for (suite, green) in poll.results {
            if seen.insert(suite.clone()) {
                if green {
                    green_suites.insert(suite.clone());
                }
                if !green {
                    red_main(&mut a, ops, &suite, &ids, now);
                } else {
                    a.on_main_result(&suite, green, &[], now);
                }
            }
        }
        if let Some(end) = poll.done {
            if end == MainEnd::WorkspaceBuild {
                return Ok(Settled {
                    preempt: None,
                    decision: Decision::default(),
                    end,
                    main_end_at: now,
                    settled_at: now,
                    filter: None,
                });
            }
            if kind == MainKind::Corpus && main_end_at.is_none() {
                if let Some(filter) = runner.filter_red() {
                    return Ok(Settled {
                        preempt: None,
                        decision: Decision::default(),
                        end,
                        main_end_at: now,
                        settled_at: now,
                        filter: Some(filter),
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
            }
        }
        if !a.main_done() {
            if let Some((owner, owned)) = a.sole_owner() {
                runner.stop_main()?;
                let preempt = Preempt {
                    owner,
                    unreached: suites
                        .iter()
                        .filter(|s| !seen.contains(*s))
                        .cloned()
                        .collect(),
                    rerun: suites
                        .iter()
                        .filter(|s| !green_suites.contains(*s))
                        .cloned()
                        .collect(),
                };
                return Ok(Settled {
                    preempt: Some(preempt),
                    decision: a.decision_of(&owned),
                    end: MainEnd::Ran,
                    main_end_at: now,
                    settled_at: now,
                    filter: None,
                });
            }
        }
        if a.settled() {
            return Ok(Settled {
                preempt: None,
                decision: a.decision(),
                end: MainEnd::Ran,
                main_end_at: main_end_at.unwrap_or(now),
                settled_at: now,
                filter: None,
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
    while !queue.is_empty() || running > 0 {
        while running < budget.slots as usize {
            let Some(job) = queue.pop_front() else { break };
            if unproven.contains(&job.suite) && !(lone && base_jobs.contains(&job.id)) {
                continue;
            }
            match runner.launch(&job) {
                Ok(()) => running += 1,
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
        "batcher: flip proven, deleting from the round: {}",
        flaky.join(",")
    );
    ops.delete_flips(flaky).map_err(NotAFlip::fault)
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
    /// Nothing lands, but every culprit was ejected: the pool holds only survivors to cut again.
    Emptied(String),
    /// Someone else abandoned the round: it is over, and this driver writes no verdict.
    Abandoned,
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
        let mapped = graph
            .map(|g| unitattr::reaching(&g, &packages, &ops.diffs(&ids)))
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
        if actions.is_empty() {
            let red = format!("{} red{tests}", f.name);
            actions = bisect_members(
                &red,
                &mut |m| runner.filter_green_in(&f.name, &packages, m),
                members,
            )
            .unwrap_or_default();
            if actions.is_empty() {
                actions = sole_member_culprit(&red, members);
            }
        }
        let mut kind = "unattributed";
        let mut cleared = String::new();
        if actions.is_empty() && members.len() > 1 {
            match red_alone(
                &format!("{} red{tests}", f.name),
                &mut |m| runner.filter_green_in(&f.name, &packages, m),
                members,
            ) {
                Alone::Owners(found) => actions = found,
                Alone::Base(said) => {
                    kind = "base";
                    cleared = said;
                }
                Alone::Cleared(said) => cleared = said,
            }
        }
        if actions.is_empty() {
            let mut page = vec![f.name.clone()];
            page.extend(f.failed.iter().take(MAX_NOTED_TESTS).cloned());
            if !cleared.is_empty() {
                page.push(format!("run alone: {cleared}"));
            }
            ops.incident(kind, &page);
            return Ok(FilterEject::Unnamed(format!(
                "{tests}{}",
                if cleared.is_empty() {
                    String::new()
                } else {
                    format!(" (run alone: {cleared})")
                }
            )));
        }
        return Ok(
            match apply_build_actions(ops, members, &actions, &f.name)? {
                Some(next) => FilterEject::Survivors(next),
                None => FilterEject::Emptied,
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
                Some((_, line)) => ops.eject_on(m, &f.name, line, true),
                None => ops.eject(m, std::slice::from_ref(&f.name), false),
            }
        }
    }
    let survivors: Vec<Member> = members
        .iter()
        .filter(|m| !out.contains_key(&m.id))
        .cloned()
        .collect();
    if survivors.is_empty() {
        return Ok(FilterEject::Emptied);
    }
    let rebuilt = ops.rebuild(&survivors)?;
    Ok(if rebuilt.is_empty() {
        FilterEject::Emptied
    } else {
        FilterEject::Survivors(rebuilt)
    })
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

enum FilterEject {
    Unnamed(String),
    Emptied,
    Survivors(Vec<Member>),
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
    let mut out: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    for a in actions {
        let (Action::Eject { id, .. } | Action::Sequence { id, .. }) = a;
        out.insert(id.clone(), vec![filter.to_string()]);
    }
    with_dependents(members, &mut out);
    for m in members {
        match actions.iter().find(
            |a| matches!(a, Action::Eject { id, .. } | Action::Sequence { id, .. } if *id == m.id),
        ) {
            Some(Action::Eject { line, .. }) => ops.eject_on(m, filter, line, true),
            Some(Action::Sequence { behind, line, .. }) => ops.sequence(m, behind, line),
            None if out.contains_key(&m.id) => ops.eject(m, &[filter.to_string()], false),
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
        Alone::Cleared(said)
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

/// The whole round: corpus with concurrent attribution; then, while anyone was ejected, the
/// owners' suites (only those) on the survivors' release build, attributed the same way.
pub fn attribute_round<R: RoundRunner, O: RoundOps>(
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
                        "testenv died on a signal and every member left".into(),
                    ))
                }
                Some(next) => {
                    println!("batcher: testenv died on a signal — its one runner-touching member left, re-cutting {} member(s)", next.len());
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
                FilterEject::Emptied => {
                    return Ok(RoundEnd::Emptied(format!(
                        "{} red ejected every member",
                        f.name
                    )))
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
        let mut d = s.decision;
        let preempt = s.preempt;
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
                let suites = d.unattributed.join(",");
                println!(
                    "batcher: {suites} red names no member; the round's only member is the culprit"
                );
                apply_build_actions(ops, &members, &alone, &suites)?;
                return Ok(RoundEnd::Emptied(format!(
                    "{suites} red ejected the only member"
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
                        Alone::Cleared(w) => said.push(format!("{suite}: {w}")),
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
                return Ok(RoundEnd::Blocked(format!(
                    "base red: {} ({})",
                    base_red.join(","),
                    said.join("; ")
                )));
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
                        ops.eject(only, &red.reds, true);
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
        if let Some(p) = &preempt {
            println!(
                "batcher: pass preempted at +{}s: {} alone owns {}; {} suite(s) unreached",
                s.settled_at.saturating_sub(t0),
                p.owner,
                d.owned_suites().join(","),
                p.unreached.len()
            );
            ops.preempted(&p.owner, &p.unreached);
        }
        let mended = if preempt.is_some() {
            None
        } else {
            ops.settle_mending(&d, &members)
        };
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
                            ops.eject(member, &own, d.owners.contains_key(&l.id));
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
        for m in members.iter().filter(|_| preempt.is_none()) {
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
        (suites, kind) = match preempt {
            Some(p) => (p.rerun, MainKind::Corpus),
            None => (d.owned_suites(), MainKind::Verify),
        };
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
        /// A runner that cannot build a tree with members taken out: every such job fails to launch.
        pub refuses_removals: bool,
        /// Suite → the members whose diff adds it: a run naming it on a tree holding none of them dies as round-vm does.
        pub added_by: BTreeMap<String, Vec<String>>,
        /// The round is abandoned from this many main runs started, or from this time.
        pub abandoned_after_starts: Option<usize>,
        pub abandoned_at: Option<u64>,
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
                compiler: None,
                build_failed: None,
                aborts_with: None,
                fails_once: None,
                bisects: false,
                probes: vec![],
                runs_alone: false,
                alone: vec![],
                scoped: vec![],
                refuses_removals: false,
                added_by: BTreeMap::new(),
                abandoned_after_starts: None,
                abandoned_at: None,
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
            if let Some(text) = self.fails_once.take() {
                return Err(text);
            }
            if let Some((m, text)) = &self.aborts_with {
                if self.members.contains(m) {
                    return Err(text.clone());
                }
            }
            if self.build_failed.is_some() {
                p.done = Some(MainEnd::WorkspaceBuild);
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
        fn filter_red(&mut self) -> Option<FilterRed> {
            self.filtered.take()
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
            assert!(self.t < 100_000, "the loop never settled");
        }
    }

    #[derive(Default)]
    pub struct Ops {
        pub touch: BTreeMap<String, Vec<String>>,
        pub ejected: Vec<(String, Vec<String>)>,
        pub eject_evidence: Vec<(String, String)>,
        pub owner_flags: Vec<(String, bool)>,
        pub rebuilt: Vec<Vec<String>>,
        pub incidents: Vec<(String, Vec<String>)>,
        pub can_fix: bool,
        pub fix_calls: u32,
        pub fixed: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        pub recorded: Vec<(u32, Decision)>,
        pub deleted: Vec<Vec<String>>,
        pub escaped: Vec<(String, String, Option<JobResult>)>,
        pub preempts: Vec<(String, Vec<String>)>,
        pub judged: Vec<(Vec<String>, Vec<String>)>,
        /// Base shas the base will move to, one per `base_moved` call; empty means it never moves.
        pub moves: Vec<String>,
        /// Suite → the members whose diff adds it.
        pub adds: BTreeMap<String, Vec<String>>,
        /// The next pass the menders decide, once per red pass; empty means no mender owns it.
        pub menders: Vec<crate::mender::Next>,
        pub diffs: Vec<MemberDiff>,
        pub sequenced: Vec<(String, Vec<String>)>,
        pub graph: Option<PackageGraph>,
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
        fn eject_on(&mut self, member: &Member, filter: &str, line: &str, owner: bool) {
            self.eject_evidence
                .push((member.id.clone(), line.to_string()));
            self.eject(member, &[filter.to_string()], owner);
        }
        fn diffs(&self, _: &[Id]) -> Vec<MemberDiff> {
            self.diffs.clone()
        }
        fn package_graph(&self) -> Option<PackageGraph> {
            self.graph.clone()
        }
        fn sequence(&mut self, member: &Member, behind: &[Id], line: &str) {
            self.sequenced.push((member.id.clone(), behind.to_vec()));
            self.eject_evidence
                .push((member.id.clone(), line.to_string()));
        }
        fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
            self.rebuilt
                .push(survivors.iter().map(|m| m.id.clone()).collect());
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
        fn delete_flips(&mut self, suites: &[String]) -> Result<(), String> {
            self.deleted.push(suites.to_vec());
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
        fn preempted(&mut self, owner: &Id, unreached: &[String]) {
            self.preempts.push((owner.clone(), unreached.to_vec()));
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
    fn a_red_one_member_alone_owns_preempts_the_pass_and_the_restart_runs_without_it() {
        let mut f = Fake::new(breaks("m2", "test-00.sh"));
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
        assert_eq!(f.stopped, 1, "the first pass was stopped, not run out");
        assert_eq!(
            ops.ejected,
            vec![("m2".to_string(), vec!["test-00.sh".to_string()])]
        );
        assert_eq!(ops.rebuilt, vec![vec!["m1".to_string(), "m3".to_string()]]);
        assert_eq!(f.main_kind.len(), 2);
        let (kind, restart) = &f.main_kind[1];
        assert_eq!(*kind, MainKind::Corpus);
        assert!(
            restart.contains(&"test-00.sh".to_string())
                && restart.contains(&"test-39.sh".to_string())
        );
        assert!(
            restart.len() < 40,
            "suites that finished green are not run again"
        );
        let (owner, unreached) = &ops.preempts[0];
        assert_eq!(owner, "m2");
        assert!(
            unreached.contains(&"test-39.sh".to_string())
                && !unreached.contains(&"test-00.sh".to_string())
        );
        assert!(
            matches!(end, RoundEnd::Land { ref members, .. } if members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>() == ["m1", "m3"])
        );
    }

    #[test]
    fn a_red_that_also_fails_on_the_base_preempts_nothing_and_ejects_no_one() {
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
        assert!(ops.preempts.is_empty() && ops.ejected.is_empty());
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
            f.jobs.is_empty(),
            "a rerun on a member subset ran: {:?}",
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
    fn a_red_no_single_member_owns_preempts_nothing() {
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
        assert!(ops.preempts.is_empty() && ops.ejected.is_empty());
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
        assert!(matches!(end, RoundEnd::Blocked(_)), "{end:?}");
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
        assert_eq!(ops.deleted, vec![vec!["test-04.sh".to_string()]]);
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
        assert_eq!(ops.deleted, vec![vec!["test-04.sh".to_string()]]);
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
        assert!(ops.deleted.is_empty());
        assert!(ops.incidents.is_empty(), "{:?}", ops.incidents);
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
        assert!(ops.deleted.is_empty());
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

    #[test]
    fn a_flaky_red_preempts_nothing_and_ejects_no_one() {
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
        assert!(ops.preempts.is_empty() && ops.ejected.is_empty());
        assert_eq!(ops.recorded[0].1.flaky, vec!["test-04.sh".to_string()]);
        assert!(matches!(end, RoundEnd::Land { ref members, .. } if members.len() == 3));
    }

    #[test]
    fn every_owner_is_ejected_with_its_suites_one_pass_at_a_time() {
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
        assert_eq!(f.stopped, 2, "each owner stopped the pass it was found in");
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
            !f.probes.is_empty() && f.scoped.iter().all(|p| p == &["loom".to_string()]),
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
        let mut ops = Ops { moves: vec!["bbb".into()], ..Ops::default() };
        let end = attribute_round(&mut f, &mut ops, &corpus(3), three(), Budget { slots: 6, maxpar: 2 }).unwrap();
        assert_eq!(land_ids(end), vec!["m1", "m2", "m3"]);
        assert_eq!(f.bases, vec!["bbb".to_string()], "the runner builds on the new base");
        assert!(ops.ejected.is_empty() && ops.incidents.is_empty());
    }

    #[test]
    fn a_base_that_moved_again_after_the_one_rebuild_is_an_error_not_a_loop() {
        let mut f = Fake::new(|_, _| false);
        let moved = format!("{BASE_MOVED}aaa to bbb by 1234abc land sp-z");
        f.fails_once = Some(moved.clone());
        let mut ops = Ops { moves: vec!["bbb".into()], ..Ops::default() };
        struct Twice(Fake, String);
        impl RoundRunner for Twice {
            fn set_members(&mut self, m: &[Member]) { self.0.set_members(m) }
            fn set_base(&mut self, sha: &str) { self.0.set_base(sha); self.0.fails_once = Some(self.1.clone()); }
            fn start_main(&mut self, k: MainKind, s: &[String]) -> Result<(), String> { self.0.start_main(k, s) }
            fn poll_main(&mut self) -> Result<MainPoll, String> { self.0.poll_main() }
            fn stop_main(&mut self) -> Result<(), String> { self.0.stop_main() }
            fn launch(&mut self, j: &Job) -> Result<(), String> { self.0.launch(j) }
            fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> { self.0.poll_jobs() }
            fn now(&self) -> u64 { self.0.now() }
            fn wait(&mut self) { self.0.wait() }
        }
        let mut twice = Twice(f, moved);
        let err = attribute_round(&mut twice, &mut ops, &corpus(3), three(), Budget { slots: 6, maxpar: 2 }).unwrap_err();
        assert!(err.starts_with(BASE_MOVED), "{err}");
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
            f.scoped.iter().all(|p| p == &["runtime".to_string()]),
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
