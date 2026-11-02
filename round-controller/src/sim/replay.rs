//! A replay: `pass` on the injected clock over a captured day. The day's certification passes
//! ([`crate::sim::fixture`]) are the planner's requests: each is cut through `round-controller
//! cut --members`, the ledger's refusals included, just in time for a runner slot; runs for its
//! recorded wall scaled by the profile; and ends with the cause the day recorded for its members
//! at their tips, typed by the Judge. What follows the cut is the controller's own: supersede,
//! orphan re-drive, the Lander, intents and the section.
//!
//! The harness stands in for what `pass` does not do yet (the schedule step and the runner) and
//! for what the capture does not hold, which the scenario states by name: each lane's base, image
//! and depth, an image change, the Judge's sub-passes for a judged red, a hand driver, and the
//! stale record.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::batch::{BatchRow, BatchState};
use runtime::ports::Clock;
use serde::Deserialize;
use serde_json::{json, Value};
use spira_desired_state::section::parse_rfc3339;
use state_store::{Object, Request, Selector, Txn};

use crate::cut;
use crate::death::{hand_unit_name, unit_name, UnitState, VmFact, HAND_HOLDER};
use crate::lease::{acquire, holder_of, token_of, Lease, Resource, LAST_TOKEN_FIELD};
use crate::ledger::{FaultPolicy, LaneFault, StoreLedger};
use crate::orphans::HOLDER;
use crate::pass::pass;
use crate::ports::{
    Attempt, Class, Fields, InitiatedBy, Ledger, Member, PassIdentity, Ports, Verb,
};
use crate::refusal::Refusal;
use crate::resources::{round_key, Round};
use crate::sim::bench::Bench;
use crate::sim::fixture::{Fixture, Recorded, Tally};
use crate::sim::judge::{DriveSpec, JobSpec, JudgeRun, JudgeSpec, MemberSpec, ResultSpec};
use crate::sim::ledger::LedgerConfig;
use crate::sim::scenario::{document, profiles, Bound, ExpectDef, LeaseSpec, Profile, Report};
use crate::sim::verbs::VerbScript;
use crate::slots::{grant, grant_for_hand, Pool, Slot, SlotGrant, SlotSettings};
use crate::stack::lane_key;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub repo: String,
    /// The sha the landing ref points at when the day opens.
    pub landing: String,
    pub image_digest: String,
    pub harness_version: String,
    pub depth: usize,
}

/// `[[fixture.image]]`: the lane's image digest moves at `at`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageStep {
    pub repo: String,
    pub at: String,
    pub digest: String,
}

/// `[[fixture.judged]]`: the first red of `member` (`<bead>@<tip>`) is judged with these
/// sub-passes, which the capture does not record.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgedSpec {
    pub repo: String,
    pub member: String,
    pub result: ResultSpec,
    pub job: Vec<JobSpec>,
    #[serde(default)]
    pub green_digests: Vec<String>,
}

/// `[[fixture.hand_driver]]`: a hand driver holds round `batch` and a hand slot from `at` to
/// `until`, and ends it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverSpec {
    pub batch: String,
    pub at: String,
    pub until: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    /// The captured slice's directory under the world sim's fixtures.
    pub slice: String,
    pub lane: Vec<LaneSpec>,
    #[serde(default)]
    pub image: Vec<ImageStep>,
    #[serde(default)]
    pub judged: Vec<JudgedSpec>,
    #[serde(default)]
    pub hand_driver: Vec<DriverSpec>,
    /// Rounds the day opened and ended with no pass: each is written open, and ended, as it was.
    #[serde(default)]
    pub stale: Vec<String>,
}

/// `[[pathology]]`: the rounds of `repo` holding any of `members`, counted together.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathologySpec {
    pub name: String,
    pub repo: String,
    #[serde(default)]
    pub members: Vec<String>,
    /// Rounds counted by their batch, with every round stacked on one of them.
    #[serde(default)]
    pub batches: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRun {
    /// The pass cadence: a pass runs on this grid from the slice's start.
    pub every_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    description: String,
    run: ReplayRun,
    lease: LeaseSpec,
    ledger: LedgerConfig,
    fixture: FixtureSpec,
    #[serde(default)]
    pathology: Vec<PathologySpec>,
    #[serde(default)]
    expect: Vec<ExpectDef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Count {
    Tally(Tally),
    /// Beads a pass in the window certified green.
    GreenMembers,
    /// Of those, the ones the controller landed exactly once.
    GreenMembersLandedOnce,
    /// Of those, the ones it landed never, or more than once.
    GreenMembersNotLandedOnce,
    /// Rounds that went GREEN and then ABANDONED, plus every `abandon` call.
    GreenRoundsAbandoned,
    /// `wasted:<pathology>`: passes under an identity that already held a verdict, plus greens
    /// that did not land.
    Wasted(String),
    /// `passes:<pathology>`: passes the controller ran, planned or re-driven.
    Passes(String),
    /// `judge-passes:<pathology>`: the Judge's sub-passes.
    JudgePasses(String),
    /// `lane-faults:<repo>[:<cause>]`: lane faults standing on the repo.
    LaneFaults(String, Option<String>),
    /// `lane-faults-naming:<digest>`: Image faults on that digest whose evidence names it.
    LaneFaultsNaming(String),
    Calls(Option<Verb>),
    /// `opens:<place>`: rounds opened there, `on-base` or `stack-on:<batch>`.
    Opens(String),
    /// Requests still waiting when the replay ended.
    Stranded,
}

impl Count {
    fn parse(s: &str, pathologies: &[PathologySpec]) -> Option<Count> {
        if let Some(t) = Tally::parse(s) {
            return Some(Count::Tally(t));
        }
        let (what, of) = s.split_once(':').map_or((s, None), |(w, o)| (w, Some(o)));
        let named = |p: &str| pathologies.iter().any(|x| x.name == p);
        match (what, of) {
            ("green-members", None) => Some(Count::GreenMembers),
            ("green-members-landed-once", None) => Some(Count::GreenMembersLandedOnce),
            ("green-members-not-landed-once", None) => Some(Count::GreenMembersNotLandedOnce),
            ("green-rounds-abandoned", None) => Some(Count::GreenRoundsAbandoned),
            ("wasted", Some(p)) if named(p) => Some(Count::Wasted(p.into())),
            ("passes", Some(p)) if named(p) => Some(Count::Passes(p.into())),
            ("judge-passes", Some(p)) if named(p) => Some(Count::JudgePasses(p.into())),
            ("lane-faults", Some(rc)) => {
                let (repo, cause) = rc.split_once(':').map_or((rc, None), |(r, c)| (r, Some(c)));
                (!repo.is_empty()
                    && cause.is_none_or(|c| ["image", "base", "install"].contains(&c)))
                .then(|| Count::LaneFaults(repo.into(), cause.map(str::to_string)))
            }
            ("lane-faults-naming", Some(d)) if !d.is_empty() => {
                Some(Count::LaneFaultsNaming(d.into()))
            }
            ("calls", None) => Some(Count::Calls(None)),
            ("calls", Some(v)) => Verb::from_word(v).map(|v| Count::Calls(Some(v))),
            ("opens", Some(w)) if !w.is_empty() => Some(Count::Opens(w.into())),
            ("stranded", None) => Some(Count::Stranded),
            _ => None,
        }
    }
}

struct Expect {
    name: String,
    word: String,
    count: Count,
    bound: Bound,
    inverted: bool,
}

pub struct Replay {
    file: File,
    fixture: Fixture,
    expects: Vec<Expect>,
    images: Vec<(u64, ImageStep)>,
    drivers: Vec<(u64, u64, String)>,
}

/// Whether `text` is a replay: a scenario with a `[fixture]` table.
pub fn is_replay(text: &str) -> bool {
    document::<toml::Table>(text).is_ok_and(|t| t.contains_key("fixture"))
}

fn stamp(what: &str, s: &str) -> Result<u64, String> {
    parse_rfc3339(s)
        .map(|t| t * 1000)
        .ok_or_else(|| format!("replay: {what} {s:?} is not a UTC stamp"))
}

pub fn parse(text: &str) -> Result<Replay, String> {
    let file: File = document(text).map_err(|e| format!("replay: {e}"))?;
    let fixture = Fixture::load(&file.fixture.slice)?;
    let lanes: BTreeSet<&str> = file.fixture.lane.iter().map(|l| l.repo.as_str()).collect();
    if let Some(p) = fixture
        .passes
        .iter()
        .find(|p| !lanes.contains(p.repo.as_str()))
    {
        return Err(format!(
            "replay: {} has passes in the slice and no [[fixture.lane]] naming its landing, image_digest, harness_version and depth",
            p.repo
        ));
    }
    for p in &file.pathology {
        if p.members.is_empty() && p.batches.is_empty() {
            return Err(format!(
                "replay: pathology {} names no members and no batches",
                p.name
            ));
        }
        if !lanes.contains(p.repo.as_str()) {
            return Err(format!(
                "replay: pathology {} names repo {}, which has no lane",
                p.name, p.repo
            ));
        }
    }
    for j in &file.fixture.judged {
        if !fixture.passes.iter().any(|p| {
            p.repo == j.repo
                && p.cause == Recorded::Red
                && p.members
                    .iter()
                    .any(|m| format!("{}@{}", m.id, m.tip) == j.member)
        }) {
            return Err(format!(
                "replay: judged {} has no red pass in {}",
                j.member, j.repo
            ));
        }
    }
    for s in &file.fixture.stale {
        fixture.span(s)?;
    }
    let mut drivers = Vec::new();
    for d in &file.fixture.hand_driver {
        fixture.span(&d.batch)?;
        drivers.push((
            stamp("hand_driver.at", &d.at)?,
            stamp("hand_driver.until", &d.until)?,
            d.batch.clone(),
        ));
    }
    let images = file
        .fixture
        .image
        .iter()
        .map(|i| Ok((stamp("image.at", &i.at)?, i.clone())))
        .collect::<Result<Vec<_>, String>>()?;
    let expects = file
        .expect
        .iter()
        .map(|e| {
            let count = Count::parse(&e.count, &file.pathology).ok_or_else(|| {
                format!(
                    "replay: expectation {} counts {:?}, which is nothing the replay counts",
                    e.name, e.count
                )
            })?;
            let bound = match (e.equals, e.at_most) {
                (Some(n), None) => Bound::Equals(n),
                (None, Some(n)) => Bound::AtMost(n),
                _ => {
                    return Err(format!(
                        "replay: expectation {} needs exactly one of equals and at_most",
                        e.name
                    ))
                }
            };
            Ok(Expect {
                name: e.name.clone(),
                word: e.count.clone(),
                count,
                bound,
                inverted: e.inverted,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Replay {
        file,
        fixture,
        expects,
        images,
        drivers,
    })
}

impl Replay {
    pub fn description(&self) -> &str {
        &self.file.description
    }

    /// Each `fixture-*` expectation and the tally it states, for a reader holding the manifest.
    pub fn tallies(&self) -> Vec<(String, Tally, u64)> {
        self.expects
            .iter()
            .filter_map(|e| match (&e.count, e.bound) {
                (Count::Tally(t), Bound::Equals(n)) => Some((e.word.clone(), t.clone(), n)),
                _ => None,
            })
            .collect()
    }
}

/// The replay under every profile; every profile's failure is reported, not just the first.
pub fn run_all(text: &str, seed: u64) -> Result<Vec<Report>, String> {
    let r = parse(text)?;
    let all = profiles().expect("the shipped profiles parse");
    let runs: Vec<Result<Report, String>> = std::thread::scope(|s| {
        let each: Vec<_> = all.iter().map(|p| s.spawn(|| run(&r, p, seed))).collect();
        each.into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("replay: a profile's run panicked".into()))
            })
            .collect()
    });
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for run in runs {
        match run {
            Ok(rep) => ok.push(rep),
            Err(e) => bad.push(e),
        }
    }
    if bad.is_empty() {
        Ok(ok)
    } else {
        Err(bad.join("\n"))
    }
}

/// The Judge's ledger: the store's, with the green digests the scenario states for a judged repo,
/// which the store's ledger cannot list.
struct ReplayLedger {
    store: StoreLedger,
    green_digests: BTreeMap<String, Vec<String>>,
}

impl Ledger for ReplayLedger {
    fn held(&self, p: &Ports, id: &PassIdentity) -> Result<Option<Class>, String> {
        self.store.held(p, id)
    }
    fn record_attempt(
        &self,
        p: &Ports,
        id: &PassIdentity,
        batch: &str,
        a: &Attempt,
    ) -> Result<u32, String> {
        self.store.record_attempt(p, id, batch, a)
    }
    fn mark_proven(
        &self,
        p: &Ports,
        id: &PassIdentity,
        proven: &[Member],
        at: u64,
    ) -> Result<(), String> {
        self.store.mark_proven(p, id, proven, at)
    }
    fn record_lane_fault(
        &self,
        p: &Ports,
        repo: &str,
        base: &str,
        image_digest: &str,
        f: &LaneFault,
    ) -> Result<(), String> {
        self.store.record_lane_fault(p, repo, base, image_digest, f)
    }
    fn green_digests(&self, _: &Ports, repo: &str, _: &str) -> Result<Vec<String>, String> {
        Ok(self.green_digests.get(repo).cloned().unwrap_or_default())
    }
}

/// A planner request: one recorded pass, or a superseded round's members re-cut.
#[derive(Clone, Debug)]
struct Req {
    repo: String,
    batch: String,
    head: String,
    members: Vec<Member>,
    /// The recorded member set whose causes its passes take: its own, or, for one cut without
    /// members that landed since, the set the day recorded it as.
    of: String,
    /// A superseded round's members, re-cut at once: no recorded pass of its own.
    recut: bool,
    /// A cut the day made while another round held these members: tried once, and dropped
    /// whatever the cut answers.
    once: bool,
    /// When a one-shot cut is moot: its driver let the round go.
    until: Option<u64>,
}

struct Run {
    repo: String,
    batch: String,
    members: Vec<Member>,
    lease: Lease,
    slot: SlotGrant,
    unit: String,
    ends: u64,
    cause: Recorded,
    identity: PassIdentity,
    pass: u32,
}

/// What one repo did with the slot it was offered.
enum Served {
    Opened,
    Nothing,
    NoSlot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Tip(usize),
    Request(usize),
    Image(usize),
    SpanOpen(usize),
    SpanEnd(usize),
    DriverStart(usize),
    DriverEnd(usize),
    /// A cut the day made of a hand-driven round's members while the driver held it.
    Recut(usize),
}

#[derive(Default)]
struct Path {
    first: Option<u64>,
    last: u64,
    passes: u64,
    wasted: u64,
    judge: u64,
    /// Passes orphan re-drive ran: not planner passes, so not wasted ones.
    redrives: u64,
}

struct Driver {
    batch: String,
    repo: String,
    held: Option<(SlotGrant, Lease, String)>,
    asking: bool,
}

struct Engine<'a> {
    r: &'a Replay,
    profile: &'a Profile,
    b: &'a Bench,
    p: Ports<'a>,
    repos: Vec<String>,
    spans: Vec<crate::sim::fixture::Span>,
    /// The cuts the day made of a driven round's members while its driver held it.
    driven_cuts: Vec<crate::sim::fixture::Span>,
    pending: Vec<Req>,
    running: Vec<Run>,
    outcomes: BTreeMap<String, Vec<(Recorded, u64)>>,
    taken: BTreeMap<String, usize>,
    /// Every round the replay cut or wrote, by batch: the request it was cut from.
    rounds: BTreeMap<String, Req>,
    /// The repo the planner offers the next slot to first.
    turn: usize,
    live_tip: BTreeMap<String, String>,
    greened: BTreeSet<String>,
    landed: BTreeSet<String>,
    judged_used: BTreeSet<usize>,
    drivers: Vec<Driver>,
    paths: BTreeMap<String, Path>,
    calls_seen: usize,
    launches_seen: usize,
    launches_scripted: usize,
    recuts: u64,
    progress: bool,
    /// When a back-off a waiting request is refused on ends.
    wake: Option<u64>,
    /// Passes run.
    ticks: u64,
    /// When the store's GC last ticked.
    gc_at: Option<u64>,
    broken: Vec<String>,
    lines: Vec<String>,
}

/// How often the store's GC ticks in a replay. Its leases are long and given back by their
/// holders, so nothing in it lapses on a shorter clock, and a GC scan every pass costs more than
/// every step of the pass together.
const GC_EVERY_MS: u64 = 3_600_000;

fn set_key(repo: &str, members: &[Member]) -> String {
    let mut m: Vec<String> = members
        .iter()
        .map(|m| format!("{}@{}", m.id, m.tip))
        .collect();
    m.sort();
    format!("{repo} {}", m.join(","))
}

fn state_json(s: BatchState) -> Value {
    serde_json::to_value(s).expect("a batch state serialises")
}

pub fn run(r: &Replay, profile: &Profile, seed: u64) -> Result<Report, String> {
    let at = profile.name.as_str();
    let (epoch, _) = r.fixture.window_ms;
    let mut b = Bench::at(r.file.lease.settings(), epoch, seed);
    b.slot_settings = SlotSettings {
        ec2_slots: profile.runner_slots.saturating_sub(1) as u32,
        spill: profile.runner_slots > 1,
    };
    let vm = crate::sim::vm::FakeVm::new(vec![], profile.clone(), b.clock.clone());
    b.verbs = crate::sim::verbs::FakeVerbs::new(vec![], vm, b.clock.clone())?
        .with_store(b.store.clone())
        .with_git(b.git.clone());
    let b = b;
    for l in &r.file.fixture.lane {
        b.seed(
            &lane_key(&l.repo),
            json!({ "landing_ref": l.landing, "image_digest": l.image_digest, "harness_version": l.harness_version, "concurrency": l.depth }),
        );
        b.git.seed(&crate::sim::git::GitSeed {
            repo: l.repo.clone(),
            landing: l.landing.clone(),
            history: vec![],
        });
    }
    let ledger = ReplayLedger {
        store: StoreLedger {
            policy: FaultPolicy {
                backoff_secs: r.file.ledger.fault_backoff_secs,
                retries: r.file.ledger.fault_retries,
            },
        },
        green_digests: r
            .file
            .fixture
            .judged
            .iter()
            .map(|j| (j.repo.clone(), j.green_digests.clone()))
            .collect(),
    };
    let mut p = b.ports();
    p.ledger = &ledger;
    let mut outcomes: BTreeMap<String, Vec<(Recorded, u64)>> = BTreeMap::new();
    for pass in &r.fixture.passes {
        outcomes
            .entry(set_key(&pass.repo, &pass.members))
            .or_default()
            .push((pass.cause, pass.wall_secs));
    }
    let spans = r
        .file
        .fixture
        .stale
        .iter()
        .chain(r.file.fixture.hand_driver.iter().map(|d| &d.batch))
        .map(|s| r.fixture.span(s))
        .collect::<Result<Vec<_>, String>>()?;
    let driven_cuts: Vec<crate::sim::fixture::Span> = r
        .drivers
        .iter()
        .filter_map(|(from, until, batch)| {
            let s = spans.iter().find(|s| s.batch == *batch)?;
            let cuts = r
                .fixture
                .opened_holding(&s.repo, &s.members, *from, *until, batch);
            Some(cuts.into_iter().map(|c| crate::sim::fixture::Span {
                ended_ms: *until,
                ..c
            }))
        })
        .flatten()
        .collect();
    let mut e = Engine {
        r,
        profile,
        b: &b,
        p,
        repos: r.file.fixture.lane.iter().map(|l| l.repo.clone()).collect(),
        spans,
        driven_cuts,
        pending: Vec::new(),
        running: Vec::new(),
        outcomes,
        taken: BTreeMap::new(),
        rounds: BTreeMap::new(),
        turn: 0,
        live_tip: BTreeMap::new(),
        greened: BTreeSet::new(),
        landed: BTreeSet::new(),
        judged_used: BTreeSet::new(),
        drivers: r
            .drivers
            .iter()
            .map(|(_, _, batch)| Driver {
                batch: batch.clone(),
                repo: String::new(),
                held: None,
                asking: false,
            })
            .collect(),
        paths: r
            .file
            .pathology
            .iter()
            .map(|p| (p.name.clone(), Path::default()))
            .collect(),
        calls_seen: 0,
        launches_seen: 0,
        launches_scripted: 0,
        recuts: 0,
        progress: false,
        wake: None,
        ticks: 0,
        gc_at: None,
        broken: Vec::new(),
        lines: Vec::new(),
    };
    e.go(epoch);
    e.report(at)
}

impl Engine<'_> {
    fn now(&self) -> u64 {
        self.b.clock.now_ms()
    }

    fn events(&self) -> Vec<(u64, Event)> {
        let r = self.r;
        let mut ev: Vec<(u64, Event)> = Vec::new();
        ev.extend(
            r.fixture
                .tips
                .iter()
                .enumerate()
                .map(|(i, t)| (t.at_ms, Event::Tip(i))),
        );
        ev.extend(
            r.fixture
                .passes
                .iter()
                .enumerate()
                .map(|(i, p)| (p.start_ms, Event::Request(i))),
        );
        ev.extend(
            r.images
                .iter()
                .enumerate()
                .map(|(i, (t, _))| (*t, Event::Image(i))),
        );
        for (i, s) in self.spans.iter().enumerate() {
            ev.push((s.opened_ms, Event::SpanOpen(i)));
            let driven = r.drivers.iter().find(|d| d.2 == s.batch);
            ev.push((driven.map_or(s.ended_ms, |d| d.1), Event::SpanEnd(i)));
        }
        for (i, (from, until, _)) in r.drivers.iter().enumerate() {
            ev.push((*from, Event::DriverStart(i)));
            ev.push((*until, Event::DriverEnd(i)));
        }
        ev.extend(
            self.driven_cuts
                .iter()
                .enumerate()
                .map(|(i, s)| (s.opened_ms, Event::Recut(i))),
        );
        ev.sort();
        ev
    }

    /// Passes run on the cadence's grid. A grid point with nothing new since the last pass (no
    /// request, no finished pass, no step, no back-off ending) is skipped: that pass would read
    /// the store the last one left and decide the same, its section's heartbeat aside. The replay
    /// ends once nothing is left to arrive, run or wake.
    fn go(&mut self, epoch: u64) {
        let events = self.events();
        let every = self.r.file.run.every_secs * 1000;
        let grid = |t: u64| epoch + t.saturating_sub(epoch).div_ceil(every) * every;
        let (mut ei, mut dirty, mut last, mut ticks) = (0usize, false, None::<u64>, 0u64);
        loop {
            let t_ev = events.get(ei).map(|e| e.0);
            let t_run = self.running.iter().map(|r| r.ends).min();
            let after_last = |t: u64| match last {
                Some(l) if grid(t) <= l => l + every,
                _ => grid(t),
            };
            let wake = self.wake.filter(|_| !dirty).map(after_last);
            let t_tick = if dirty {
                Some(after_last(self.now()))
            } else {
                wake
            };
            let Some(t) = [t_ev, t_run, t_tick].into_iter().flatten().min() else {
                return;
            };
            self.b.clock.advance_to(t);
            let now = self.now();
            if t_run.is_some_and(|e| e <= now) {
                let (done, rest): (Vec<Run>, Vec<Run>) = std::mem::take(&mut self.running)
                    .into_iter()
                    .partition(|r| r.ends <= now);
                self.running = rest;
                for run in done {
                    self.end(run);
                }
                dirty = true;
                continue;
            }
            if t_ev.is_some_and(|e| e <= now) {
                let ev = events[ei].1;
                ei += 1;
                dirty |= !matches!(ev, Event::Tip(_));
                self.event(ev);
                continue;
            }
            self.progress = false;
            self.wake = None;
            self.tick();
            last = Some(now);
            ticks += 1;
            self.ticks = ticks;
            dirty = self.progress;
            if ticks > 200_000 {
                self.broken.push(format!("invariant the-replay-ends violated under {}: still running after {ticks} passes", self.profile.name));
                return;
            }
        }
    }

    fn event(&mut self, ev: Event) {
        let r = self.r;
        match ev {
            Event::Tip(i) => {
                let t = &r.fixture.tips[i];
                self.live_tip.insert(t.bead.clone(), t.tip.clone());
            }
            Event::Request(i) => {
                let pass = &r.fixture.passes[i];
                self.touch(&pass.repo, &pass.members, &pass.batch, pass.start_ms);
                self.pending.push(Req {
                    repo: pass.repo.clone(),
                    batch: pass.batch.clone(),
                    head: pass.head.clone(),
                    members: pass.members.clone(),
                    of: set_key(&pass.repo, &pass.members),
                    recut: false,
                    once: false,
                    until: None,
                });
            }
            Event::Image(i) => {
                let step = &r.images[i].1;
                self.update(&lane_key(&step.repo), |d| {
                    d["image_digest"] = json!(step.digest)
                });
            }
            Event::SpanOpen(i) => {
                let s = self.spans[i].clone();
                let base = self.lane(&s.repo).map(|l| l.0).unwrap_or_default();
                let mut row = BatchRow::cut(s.batch.clone(), s.repo.clone(), "", base);
                row.head = None;
                let round = Round {
                    row,
                    members: s.members.clone(),
                    tested_base: None,
                    last_lease_token: None,
                };
                self.put(
                    &round_key(&s.repo, &s.batch),
                    serde_json::to_value(&round).unwrap_or_default(),
                );
                let of = set_key(&s.repo, &s.members);
                self.rounds.insert(
                    s.batch.clone(),
                    Req {
                        repo: s.repo.clone(),
                        batch: s.batch.clone(),
                        head: String::new(),
                        members: s.members.clone(),
                        of,
                        recut: false,
                        once: false,
                        until: None,
                    },
                );
                self.touch(&s.repo, &s.members, &s.batch, s.opened_ms);
            }
            Event::SpanEnd(i) => {
                let s = self.spans[i].clone();
                self.set_state(&s.repo, &s.batch, BatchState::Abandoned);
                self.touch(&s.repo, &s.members, &s.batch, self.now());
            }
            Event::DriverStart(i) => {
                let span = self
                    .spans
                    .iter()
                    .find(|s| s.batch == self.drivers[i].batch)
                    .cloned();
                if let Some(s) = span {
                    self.drivers[i].repo = s.repo;
                }
                self.drivers[i].asking = true;
                self.drive(i);
            }
            Event::Recut(i) => {
                let s = self.driven_cuts[i].clone();
                self.touch(&s.repo, &s.members, &s.batch, s.opened_ms);
                self.pending.push(Req {
                    of: set_key(&s.repo, &s.members),
                    head: format!("{}-head", s.batch),
                    repo: s.repo,
                    batch: s.batch,
                    members: s.members,
                    recut: false,
                    once: true,
                    until: Some(s.ended_ms),
                });
            }
            Event::DriverEnd(i) => {
                self.drivers[i].asking = false;
                let d = &mut self.drivers[i];
                if let Some((slot, lease, unit)) = d.held.take() {
                    let (repo, batch) = (d.repo.clone(), d.batch.clone());
                    self.set_state(&repo, &batch, BatchState::Abandoned);
                    let _ = lease.release(&self.p);
                    self.b
                        .units
                        .set(&unit, UnitState::Stopped("inactive".into()));
                    self.b.vms.set(&unit, VmFact::NoVm);
                    let _ = slot.release(&self.p);
                }
            }
        }
    }

    /// A hand driver takes a hand slot and the round's lease, as `release round-recert` does.
    fn drive(&mut self, i: usize) {
        if !self.drivers[i].asking || self.drivers[i].held.is_some() {
            return;
        }
        let Ok(mut slot) = grant_for_hand(&self.p, HAND_HOLDER) else {
            return;
        };
        let (repo, batch) = (self.drivers[i].repo.clone(), self.drivers[i].batch.clone());
        match acquire(&self.p, &Resource::round(&repo, &batch), HAND_HOLDER) {
            Ok(lease) => {
                let unit = hand_unit_name(&batch, lease.token()).unwrap_or_default();
                let _ = slot.record_holder(&self.p, &unit, None);
                self.b.units.set(&unit, UnitState::Running("active".into()));
                self.drivers[i].held = Some((slot, lease, unit));
                self.progress = true;
            }
            Err(e) => {
                let _ = slot.release(&self.p);
                self.broken.push(format!("invariant no-unscripted-fault violated under {}: the hand driver could not lease {batch}: {e:?}", self.profile.name));
                self.drivers[i].asking = false;
            }
        }
    }

    fn tick(&mut self) {
        let now = self.now();
        if self.gc_at.is_none_or(|t| now >= t + GC_EVERY_MS) {
            self.gc_at = Some(now);
            if let Err(e) = state_store::gc::Controller::new(&self.b.store).tick(now) {
                self.broken.push(format!("store gc: {e}"));
            }
        }
        self.script_launches();
        pass(&self.p, &self.repos);
        self.after_pass();
        for i in 0..self.drivers.len() {
            self.drive(i);
        }
        self.note_landed();
        self.plan();
    }

    /// Every orphan's re-drive is answered `ok`: the replay's launches all start.
    fn script_launches(&mut self) {
        let mut orphans = 0;
        for repo in &self.repos {
            orphans += crate::orphans::orphans(&self.p, repo).map_or(0, |o| o.len());
        }
        let queued =
            self.launches_scripted - self.b.launch.tried().len().min(self.launches_scripted);
        for _ in queued..orphans {
            self.b.launch.script(Ok(()));
            self.launches_scripted += 1;
        }
    }

    fn after_pass(&mut self) {
        let calls = self.b.verbs.calls();
        for c in &calls[self.calls_seen..] {
            self.progress = true;
            if c.verb != Verb::Supersede {
                continue;
            }
            let Some(batch) = c.batch.clone() else {
                continue;
            };
            if let Some(cut) = self.rounds.get(&batch).cloned() {
                self.recuts += 1;
                if let Some(i) = self.running.iter().position(|r| r.batch == batch) {
                    let run = self.running.remove(i);
                    self.cut_short(run);
                }
                self.pending.push(Req {
                    batch: format!("{batch}-s{}", self.recuts),
                    head: format!("{}-s{}", cut.head, self.recuts),
                    recut: true,
                    once: false,
                    until: None,
                    ..cut
                });
            }
        }
        self.calls_seen = calls.len();
        let tried = self.b.launch.tried();
        for t in &tried[self.launches_seen..] {
            self.progress = true;
            let found = self.rounds.iter().find_map(|(batch, cut)| {
                let item = self.b.store.get(&round_key(&cut.repo, batch)).ok()??;
                let token = token_of(item.value.data.get(LAST_TOKEN_FIELD)?)?;
                (unit_name(batch, token).ok()? == t.unit)
                    .then(|| (cut.repo.clone(), batch.clone(), token))
            });
            match (found, slot_of(&t.slot)) {
                (Some((repo, batch, token)), Some(slot)) => {
                    let grant = SlotGrant::handed(&self.p, slot, t.slot_token);
                    let lease = Lease::handed(Resource::round(&repo, &batch), token);
                    self.start(&batch, lease, grant, t.unit.clone(), true);
                }
                _ => self.broken.push(format!(
                    "invariant no-unscripted-fault violated under {}: a launch of {} on {} names no round the replay cut",
                    self.profile.name, t.unit, t.slot
                )),
            }
        }
        self.launches_seen = tried.len();
    }

    /// A pass whose round was superseded under it: its VM goes and its slot is given back, with
    /// no verdict, as a runner that lost its lease does.
    fn cut_short(&mut self, run: Run) {
        self.b
            .units
            .set(&run.unit, UnitState::Stopped("inactive".into()));
        self.b.vms.set(&run.unit, VmFact::NoVm);
        self.lines.push(format!(
            "replay: {} cut short: its round was superseded",
            run.batch
        ));
        let _ = run.lease.release(&self.p);
        let _ = run.slot.release(&self.p);
    }

    fn landed_member(&self, id: &str) -> bool {
        self.landed.iter().any(|b| {
            self.rounds
                .get(b)
                .is_some_and(|c| c.members.iter().any(|m| m.id == id))
        })
    }

    fn left(&self, of: &str) -> usize {
        let all = self.outcomes.get(of).map_or(0, Vec::len);
        all.saturating_sub(self.taken.get(of).copied().unwrap_or(0))
    }

    /// The planner, just in time for each runner slot: the slot is offered to each repo in turn,
    /// and a repo takes it with its oldest request the cut opens. A request is dropped once a
    /// member's live tip moved or its recorded passes are spent, and cut without the members
    /// that landed since; one the cut refuses for a reason that can lift waits.
    fn plan(&mut self) {
        let mut slot: Option<SlotGrant> = None;
        let n = self.repos.len();
        let mut idle = 0;
        while idle < n && !self.pending.is_empty() {
            let repo = self.repos[self.turn % n].clone();
            self.turn = (self.turn + 1) % n;
            match self.serve(&repo, &mut slot) {
                Served::Opened => idle = 0,
                Served::Nothing => idle += 1,
                Served::NoSlot => break,
            }
        }
        if let Some(g) = slot {
            let _ = g.release(&self.p);
        }
    }

    fn serve(&mut self, repo: &str, slot: &mut Option<SlotGrant>) -> Served {
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].repo != repo {
                i += 1;
                continue;
            }
            let mut req = self.pending[i].clone();
            req.members.retain(|m| !self.landed_member(&m.id));
            let stale_tip = req
                .members
                .iter()
                .any(|m| self.live_tip.get(&m.id).is_some_and(|t| *t != m.tip));
            let late = req.until.is_some_and(|u| self.now() >= u);
            if late
                || stale_tip
                || req.members.is_empty()
                || (!req.recut && !req.once && self.left(&req.of) == 0)
            {
                self.pending.remove(i);
                continue;
            }
            if slot.is_none() {
                match grant(&self.p, &Pool::ALL, HOLDER) {
                    Ok(g) => *slot = Some(g),
                    Err(_) => return Served::NoSlot,
                }
            }
            let script = VerbScript {
                verb: "open".into(),
                repo: Some(req.repo.clone()),
                batch: None,
                ok: Some(Fields::from([
                    ("batch".to_string(), req.batch.clone()),
                    ("head".to_string(), req.head.clone()),
                ])),
                refuse: None,
                lock_hold_ms: 0,
            };
            if let Err(e) = self.b.verbs.script(script) {
                self.broken.push(e);
                return Served::NoSlot;
            }
            let got = cut::explicit(&self.p, &req.repo, req.members.clone());
            cut::record(
                &self.p,
                &req.repo,
                &got.as_ref().map(|o| Some(o.clone())).map_err(Clone::clone),
            );
            let refusal = match got {
                Ok(o) => {
                    self.pending.remove(i);
                    self.progress = true;
                    let batch = o.batch.clone();
                    self.rounds.insert(
                        batch.clone(),
                        Req {
                            batch: batch.clone(),
                            ..req
                        },
                    );
                    if self.rounds[&batch].once {
                        let (repo, members) = (
                            self.rounds[&batch].repo.clone(),
                            self.rounds[&batch].members.clone(),
                        );
                        for name in self.paths_of(&repo, &members, &batch) {
                            self.paths
                                .get_mut(&name)
                                .expect("a pathology the scenario named")
                                .wasted += 1;
                        }
                    }
                    let Some(mut g) = slot.take() else {
                        return Served::Opened;
                    };
                    let repo = self.rounds[&batch].repo.clone();
                    match acquire(&self.p, &Resource::round(&repo, &batch), HOLDER) {
                        Ok(lease) => {
                            let unit = unit_name(&batch, lease.token()).unwrap_or_default();
                            let _ = g.record_holder(&self.p, &unit, Some(&format!("vm-{unit}")));
                            self.start(&batch, lease, g, unit, false);
                        }
                        Err(e) => {
                            self.broken.push(format!("invariant no-unscripted-fault violated under {}: {batch} opened and could not be leased: {e:?}", self.profile.name));
                            let _ = g.release(&self.p);
                        }
                    }
                    return Served::Opened;
                }
                Err(r) => r,
            };
            self.b.verbs.unscript(Verb::Open, &req.repo);
            if req.once {
                self.pending.remove(i);
                continue;
            }
            match refusal {
                Refusal::RedIdentity { .. } | Refusal::CutOfJudgedRed { .. } => {
                    self.pending.remove(i);
                }
                Refusal::BackingOff { key } => {
                    self.wake_at_backoff(&key);
                    i += 1;
                }
                Refusal::StackFull { .. } | Refusal::LaneFault { .. } => i += 1,
                Refusal::BadMembers { key } if key.starts_with("in-a-live-round/") => i += 1,
                other => {
                    self.broken.push(format!(
                        "invariant no-unscripted-fault violated under {}: the cut of {} refused: {other:?}",
                        self.profile.name, req.batch
                    ));
                    self.pending.remove(i);
                }
            }
        }
        Served::Nothing
    }

    /// The next recorded cause of the set `of`; a re-cut past them repeats the last.
    fn outcome(&mut self, of: &str, recut: bool) -> Option<(Recorded, u64)> {
        let all = self.outcomes.get(of)?;
        let n = self.taken.entry(of.to_string()).or_default();
        let got = all.get(*n).or(all.last().filter(|_| recut)).copied()?;
        *n += 1;
        Some(got)
    }

    fn start(&mut self, batch: &str, lease: Lease, slot: SlotGrant, unit: String, redriven: bool) {
        let now = self.now();
        let Some(cut) = self.rounds.get(batch).cloned() else {
            self.broken.push(format!("invariant no-unscripted-fault violated under {}: {batch} started and was never cut", self.profile.name));
            let _ = lease.release(&self.p);
            let _ = slot.release(&self.p);
            return;
        };
        let (repo, batch, members) = (cut.repo.clone(), cut.batch.clone(), cut.members.clone());
        let Some((cause, wall)) = self.outcome(&cut.of, cut.recut) else {
            self.lines.push(format!(
                "replay: {batch} re-driven past its recorded passes: ended"
            ));
            self.set_state(&repo, &batch, BatchState::Abandoned);
            let _ = lease.release(&self.p);
            let _ = slot.release(&self.p);
            return;
        };
        self.b.units.set(&unit, UnitState::Running("active".into()));
        self.b.vms.set(
            &unit,
            VmFact::Running {
                vm: format!("vm-{unit}"),
            },
        );
        let mut pass = 0;
        self.update(&round_key(&repo, &batch), |d| {
            pass = d.get("pass").and_then(Value::as_u64).unwrap_or(0) as u32 + 1;
            d["state"] = state_json(BatchState::CiRunning);
            d["pass"] = json!(pass);
        });
        let identity = match self.identity(&repo, &batch, &members) {
            Ok(i) => i,
            Err(e) => {
                self.broken.push(format!(
                    "invariant no-unscripted-fault violated under {}: {e}",
                    self.profile.name
                ));
                let _ = lease.release(&self.p);
                let _ = slot.release(&self.p);
                return;
            }
        };
        let held = self
            .p
            .ledger
            .held(&self.p, &identity)
            .ok()
            .flatten()
            .is_some();
        let ms = (wall as f64 * self.profile.duration_factor * 1000.0).round() as u64;
        let cap = self.profile.pass_cap_secs * 1000;
        if ms > cap {
            self.broken.push(format!("invariant no-over-cap violated under {}: {batch} would run {ms} ms > its {cap} ms cap", self.profile.name));
        }
        for name in self.paths_of(&repo, &members, &batch) {
            let p = self
                .paths
                .get_mut(&name)
                .expect("a pathology the scenario named");
            if redriven {
                p.redrives += 1;
            } else {
                p.passes += 1;
                p.wasted += held as u64;
            }
            p.first.get_or_insert(now);
        }
        self.running.push(Run {
            repo,
            batch,
            members,
            lease,
            slot,
            unit,
            ends: now + ms,
            cause,
            identity,
            pass,
        });
    }

    /// Wakes a pass when the back-off the summary at `key` states ends.
    fn wake_at_backoff(&mut self, key: &str) {
        let until = self
            .b
            .store
            .get(key)
            .ok()
            .flatten()
            .and_then(|i| serde_json::from_value::<crate::resources::Summary>(i.value.data).ok())
            .and_then(|s| match s.backoff_until {
                Some(crate::resources::Backoff::Until(t)) => Some(t),
                _ => None,
            });
        if let Some(t) = until {
            self.wake = Some(self.wake.map_or(t, |w| w.min(t)));
        }
    }

    fn identity(
        &self,
        repo: &str,
        batch: &str,
        members: &[Member],
    ) -> Result<PassIdentity, String> {
        let (landing, digest, harness) = self
            .lane(repo)
            .ok_or_else(|| format!("{repo} has no lane record"))?;
        let item = self
            .b
            .store
            .get(&round_key(repo, batch))
            .ok()
            .flatten()
            .ok_or_else(|| format!("{batch} has no round record"))?;
        let base = item
            .value
            .data
            .get("tested_base")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(landing);
        PassIdentity::new(repo, members.to_vec(), &base, &digest, &harness, "all")
            .map_err(|e| format!("{batch}: {e}"))
    }

    fn lane(&self, repo: &str) -> Option<(String, String, String)> {
        let d = self.b.store.get(&lane_key(repo)).ok()??.value.data;
        let s = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
        Some((s("landing_ref")?, s("image_digest")?, s("harness_version")?))
    }

    /// The pathologies a round counts toward: its repo's, holding one of its members, or naming
    /// its batch or the round it is stacked on.
    fn paths_of(&self, repo: &str, members: &[Member], batch: &str) -> Vec<String> {
        let parent = self
            .b
            .store
            .get(&round_key(repo, batch))
            .ok()
            .flatten()
            .and_then(|i| i.value.data.get("parent")?.as_str().map(str::to_string));
        self.r
            .file
            .pathology
            .iter()
            .filter(|p| p.repo == repo)
            .filter(|p| {
                members.iter().any(|m| p.members.contains(&m.id))
                    || p.batches
                        .iter()
                        .any(|b| b == batch || Some(b) == parent.as_ref())
            })
            .map(|p| p.name.clone())
            .collect()
    }

    fn touch(&mut self, repo: &str, members: &[Member], batch: &str, at: u64) {
        for name in self.paths_of(repo, members, batch) {
            let p = self
                .paths
                .get_mut(&name)
                .expect("a pathology the scenario named");
            p.first = Some(p.first.map_or(at, |f| f.min(at)));
            p.last = p.last.max(at);
        }
    }

    /// A finished pass: typed by the Judge into one attempt, then the round moves as its runner
    /// would leave it: GREEN for the Lander, ATTRIBUTING for a re-drive on a fault, ended on a red.
    fn end(&mut self, run: Run) {
        let now = self.now();
        self.progress = true;
        self.b
            .units
            .set(&run.unit, UnitState::Stopped("inactive".into()));
        self.b.vms.set(&run.unit, VmFact::NoVm);
        let res = Resource::round(&run.repo, &run.batch);
        let ours = holder_of(&self.p, &res)
            .ok()
            .flatten()
            .is_some_and(|h| h.token == run.lease.token());
        if !ours
            || self
                .b
                .store
                .get(&round_key(&run.repo, &run.batch))
                .ok()
                .flatten()
                .is_none()
        {
            self.lines.push(format!(
                "replay: {} cut short: its lease went with its round",
                run.batch
            ));
            let _ = run.slot.release(&self.p);
            return;
        }
        let members = run.members.clone();
        let judged = self.r.file.fixture.judged.iter().position(|j| {
            j.repo == run.repo
                && members
                    .iter()
                    .any(|m| format!("{}@{}", m.id, m.tip) == j.member)
        });
        let state = match run.cause {
            Recorded::Red => match judged.filter(|j| self.judged_used.insert(*j)) {
                Some(j) => {
                    let spec = &self.r.file.fixture.judged[j];
                    self.judge(&run, spec.result.clone(), spec.job.clone());
                    BatchState::Abandoned
                }
                None => {
                    let a = Attempt {
                        class: Class::Red {
                            step: "replayed".into(),
                        },
                        initiated_by: InitiatedBy::Planner,
                        sub_passes: vec![],
                        evidence: "the day's recorded red".into(),
                        at: now,
                    };
                    if let Err(e) =
                        self.p
                            .ledger
                            .record_attempt(&self.p, &run.identity, &run.batch, &a)
                    {
                        self.broken.push(format!(
                            "invariant every-pass-has-a-verdict violated under {}: {}: {e}",
                            self.profile.name, run.batch
                        ));
                    }
                    BatchState::Abandoned
                }
            },
            Recorded::Green => {
                self.judge(
                    &run,
                    ResultSpec {
                        exit: 0,
                        red: vec![],
                        reason: None,
                        phase: None,
                        output: String::new(),
                    },
                    vec![],
                );
                BatchState::Green
            }
            Recorded::Infra | Recorded::OverCap => {
                let reason =
                    (run.cause == Recorded::OverCap).then(|| crate::judge::OVER_CAP.to_string());
                self.judge(
                    &run,
                    ResultSpec {
                        exit: crate::judge::FAULT,
                        red: vec![],
                        reason,
                        phase: None,
                        output: String::new(),
                    },
                    vec![],
                );
                BatchState::Attributing
            }
        };
        self.set_state(&run.repo, &run.batch, state);
        self.lines.push(format!(
            "replay: {} ended {} as {}",
            run.batch,
            run.cause.word(),
            state.as_str()
        ));
        if state == BatchState::Green {
            self.greened.insert(run.batch.clone());
            let s = VerbScript {
                verb: "land".into(),
                repo: Some(run.repo.clone()),
                batch: Some(run.batch.clone()),
                ok: Some(Fields::new()),
                refuse: None,
                lock_hold_ms: 0,
            };
            if let Err(e) = self.b.verbs.script(s) {
                self.broken.push(e);
            }
        }
        let _ = run.lease.release(&self.p);
        if let Err(e) = run.slot.release(&self.p) {
            self.broken.push(format!("invariant no-unscripted-fault violated under {}: {} could not give its slot back: {e:?}", self.profile.name, run.batch));
        }
        self.touch(&run.repo, &members, &run.batch, self.now());
    }

    fn judge(&mut self, run: &Run, result: ResultSpec, job: Vec<JobSpec>) {
        let base = run.identity.base.clone();
        let spec = JudgeSpec {
            repo: run.repo.clone(),
            batch: run.batch.clone(),
            round_pass: run.pass,
            base,
            image_digest: run.identity.image_digest.clone(),
            harness_version: run.identity.harness_version.clone(),
            selection: run.identity.selection.clone(),
            green_digests: vec![],
            member: run
                .members
                .iter()
                .map(|m| MemberSpec {
                    id: m.id.clone(),
                    tip: m.tip.clone(),
                    stack: vec![],
                    changed: vec![],
                })
                .collect(),
            result,
            job,
            held: vec![],
            drive: vec![DriveSpec {
                pass: 1,
                kill_after: None,
            }],
        };
        let j = JudgeRun::new(&spec, self.profile);
        j.drive(&self.p, &self.b.clock, 1);
        let launched = j.launches();
        for name in self.paths_of(&run.repo, &run.members, &run.batch) {
            self.paths
                .get_mut(&name)
                .expect("a pathology the scenario named")
                .judge += launched;
        }
        self.broken.extend(j.broken(self.now(), &self.profile.name));
        let judged = crate::lock(&j.judged);
        if !judged
            .iter()
            .all(|j| matches!(j, crate::judge::Judged::Recorded { .. }))
            || judged.is_empty()
        {
            self.broken.push(format!(
                "invariant every-pass-has-a-verdict violated under {}: {} judged {:?}",
                self.profile.name, run.batch, *judged
            ));
        }
    }

    fn note_landed(&mut self) {
        let now = self.now();
        let fresh: Vec<String> = self
            .greened
            .iter()
            .filter(|b| !self.landed.contains(*b))
            .filter(|b| {
                self.rounds.get(*b).is_some_and(|c| {
                    self.b
                        .store
                        .get(&round_key(&c.repo, b))
                        .ok()
                        .flatten()
                        .and_then(|i| i.value.data.get("state").cloned())
                        == Some(state_json(BatchState::Landed))
                })
            })
            .cloned()
            .collect();
        for b in fresh {
            self.progress = true;
            self.landed.insert(b.clone());
            if let Some(c) = self.rounds.get(&b).cloned() {
                self.touch(&c.repo, &c.members, &c.batch, now);
            }
        }
    }

    fn put(&self, key: &str, data: Value) {
        let req = Request::new(
            &format!("replay-{key}-{}", self.b.store.head().unwrap_or(0)),
            "replay",
            "write",
        );
        let _ = self
            .b
            .store
            .txn(&Txn::new().put(key, Object::new(data)), &req);
    }

    fn update(&self, key: &str, f: impl FnOnce(&mut Value)) {
        let Some(item) = self.b.store.get(key).ok().flatten() else {
            return;
        };
        let mut v = item.value.clone();
        f(&mut v.data);
        let req = Request::new(
            &format!("replay-{key}-{}", self.b.store.head().unwrap_or(0)),
            "replay",
            "write",
        );
        let _ = self.b.store.txn(&Txn::new().put(key, v), &req);
    }

    fn set_state(&self, repo: &str, batch: &str, s: BatchState) {
        self.update(&round_key(repo, batch), |d| d["state"] = state_json(s));
    }

    fn report(mut self, at: &str) -> Result<Report, String> {
        let b = self.b;
        let unlanded: Vec<(String, Vec<Member>, String)> = self
            .greened
            .iter()
            .filter(|g| !self.landed.contains(*g))
            .filter_map(|g| {
                self.rounds
                    .get(g)
                    .map(|c| (c.repo.clone(), c.members.clone(), c.batch.clone()))
            })
            .collect();
        for (repo, members, batch) in unlanded {
            for name in self.paths_of(&repo, &members, &batch) {
                self.paths
                    .get_mut(&name)
                    .expect("a pathology the scenario named")
                    .wasted += 1;
            }
        }
        let faults: Vec<String> = b
            .verbs
            .faults()
            .into_iter()
            .chain(b.launch.faults())
            .chain(crate::lock(&b.proc_.0).iter().cloned())
            .collect();
        if !faults.is_empty() {
            self.broken.push(format!(
                "invariant no-unscripted-fault violated under {at}: {}",
                faults.join("; ")
            ));
        }
        let (items, _) = b
            .store
            .list(&["batches/"], &Selector::Everything)
            .expect("the in-memory store lists");
        let mut landings: BTreeMap<String, u64> = BTreeMap::new();
        let mut ended_green = 0;
        for i in items.iter().filter(|i| i.key.matches('/').count() == 2) {
            let Ok(round) = serde_json::from_value::<Round>(i.value.data.clone()) else {
                continue;
            };
            if round.row.state == BatchState::Landed {
                for m in &round.members {
                    *landings.entry(m.id.clone()).or_default() += 1;
                }
            }
            if round.row.state == BatchState::Abandoned
                && self.greened.contains(&round.row.batch_id)
            {
                ended_green += 1;
            }
        }
        let calls = b.verbs.calls();
        let green = self.r.fixture.certified_green();
        let (faults_kept, _) = b
            .store
            .list(&["faults/"], &Selector::Everything)
            .expect("the in-memory store lists");
        self.lines
            .push(format!("replay-passes {at} {}", self.ticks));
        for (name, p) in &self.paths {
            let secs = p.first.map_or(0, |f| p.last.saturating_sub(f) / 1000);
            self.lines.push(format!("virtual-wall {name} {at} {secs}"));
            self.lines
                .push(format!("judge-passes {name} {at} {}", p.judge));
            self.lines
                .push(format!("redrives {name} {at} {}", p.redrives));
        }
        let mut controls = Vec::new();
        for e in &self.r.expects {
            let got = match &e.count {
                Count::Tally(t) => self.r.fixture.tally(t),
                Count::GreenMembers => green.len() as u64,
                Count::GreenMembersLandedOnce => green
                    .iter()
                    .filter(|g| landings.get(*g) == Some(&1))
                    .count() as u64,
                Count::GreenMembersNotLandedOnce => green
                    .iter()
                    .filter(|g| landings.get(*g) != Some(&1))
                    .count() as u64,
                Count::GreenRoundsAbandoned => {
                    ended_green + calls.iter().filter(|c| c.verb == Verb::Abandon).count() as u64
                }
                Count::Wasted(n) => self.paths[n].wasted,
                Count::Passes(n) => self.paths[n].passes,
                Count::JudgePasses(n) => self.paths[n].judge,
                Count::LaneFaults(repo, cause) => faults_kept
                    .iter()
                    .filter(|i| i.key.starts_with(&format!("faults/{repo}/")))
                    .filter_map(|i| serde_json::from_value::<LaneFault>(i.value.data.clone()).ok())
                    .filter(|f| cause.as_ref().is_none_or(|c| f.cause.word() == c))
                    .count() as u64,
                Count::LaneFaultsNaming(d) => faults_kept
                    .iter()
                    .filter(|i| i.key.ends_with(&format!("/{d}")))
                    .filter_map(|i| serde_json::from_value::<LaneFault>(i.value.data.clone()).ok())
                    .filter(|f| {
                        f.evidence.contains(d.as_str())
                            && f.cause == crate::ports::Cause::Image { digest: d.clone() }
                    })
                    .count() as u64,
                Count::Calls(v) => calls
                    .iter()
                    .filter(|c| v.is_none_or(|v| c.verb == v))
                    .count() as u64,
                Count::Opens(w) => calls
                    .iter()
                    .filter(|c| c.verb == Verb::Open && c.place.as_deref() == Some(w.as_str()))
                    .count() as u64,
                Count::Stranded => self.pending.len() as u64,
            };
            let unmet = format!(
                "expectation {} unmet under {at}: counted {got}, expected {}",
                e.name,
                e.bound.said()
            );
            match (e.bound.holds(got), e.inverted) {
                (true, false) => {}
                (false, false) => self.broken.push(unmet),
                (false, true) => controls.push(unmet),
                (true, true) => self.broken.push(format!("inverted expectation {} held under {at}: counted {got}, so the replay cannot see what it counts", e.name)),
            }
        }
        if !self.broken.is_empty() {
            let off: Vec<String> = green
                .iter()
                .filter(|g| landings.get(*g) != Some(&1))
                .map(|g| format!("{g}={}", landings.get(g).copied().unwrap_or(0)))
                .collect();
            self.broken.push(format!(
                "replay under {at}: certified green and not landed once: {}",
                off.join(" ")
            ));
            self.broken.push(format!(
                "replay under {at}: {} requests still waiting: {:?}",
                self.pending.len(),
                self.pending
                    .iter()
                    .take(8)
                    .map(|r| &r.batch)
                    .collect::<Vec<_>>()
            ));
            let walls = self.lines.iter().filter(|l| {
                [
                    "virtual-wall ",
                    "replay-passes ",
                    "judge-passes ",
                    "redrives ",
                    "replay: ",
                ]
                .iter()
                .any(|p| l.starts_with(p))
            });
            self.broken.extend(walls.cloned().collect::<Vec<_>>());
            return Err(self.broken.join("\n"));
        }
        let outcomes = b.log.outcomes();
        let mut lines: Vec<String> = outcomes.iter().map(|o| o.line()).collect();
        lines.extend(self.lines);
        let decisions = calls.iter().map(|c| format!("{c:?}")).collect();
        Ok(Report {
            profile: at.into(),
            lines,
            calls,
            controls,
            decisions,
        })
    }
}

fn slot_of(word: &str) -> Option<Slot> {
    let (pool, n) = word.split_once('/')?;
    Some(Slot {
        pool: Pool::from_word(pool)?,
        n: n.parse().ok()?,
    })
}
