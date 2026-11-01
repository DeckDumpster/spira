//! The replay: a captured slice of the loop's own history (`fixtures/replay/<slice>/`) on the
//! simulator's virtual clock. Each recorded line arrives at its recorded time and updates a
//! typed [`ReplayWorld`]; publishers turn that world into sections through spira-lc's own
//! section verbs on the contract's in-memory store, and a registered pass reads the store every
//! 60 virtual seconds. Nothing a hook does changes the world: what the replay shows is what the
//! recorded day did, and what a reader of its sections would have seen.

pub mod round_controller;

use std::cell::{Ref, RefCell, RefMut};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;
use serde_json::Value;
use spira_desired_state::intent::{parse_intent, Intent};
use spira_desired_state::section::parse_rfc3339;
use spira_otel::metrics::MetricsSink;
use state_store::{Config, Contract, MemBackend, Ports};

use crate::Sim;

pub const PASS_PERIOD_MS: u64 = 60_000;
/// A pass runs this long after the minute, so it reads what that minute's publishers wrote
/// whatever order the seed gives same-instant steps.
pub const PASS_OFFSET_MS: u64 = 1;
const MIN_LOCK_TTL_MS: u64 = 1_000;

/// One captured slice: its directory under the fixtures root and the window (UTC epoch
/// seconds, end exclusive) its manifest names. Records before the window are its lead-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slice {
    /// 2026-10-31T00:00Z to 2026-11-01T00:00Z.
    Oct31,
    /// Pacific 10-21..10-25: 2026-10-21T07:00Z to 2026-10-26T07:00Z.
    HeldOut,
}

impl Slice {
    pub fn dir(self) -> &'static str {
        match self {
            Slice::Oct31 => "1031",
            Slice::HeldOut => "1021-1025",
        }
    }

    pub fn window(self) -> (u64, u64) {
        let at = |s| parse_rfc3339(s).expect("a slice bound is a stamp");
        match self {
            Slice::Oct31 => (at("2026-10-31T00:00:00Z"), at("2026-11-01T00:00:00Z")),
            Slice::HeldOut => (at("2026-10-21T07:00:00Z"), at("2026-10-26T07:00:00Z")),
        }
    }
}

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("replay")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchState {
    None,
    Open,
    CiRunning,
    Green,
    Attributing,
    Rebuilding,
    Abandoned,
    Landed,
}

impl BatchState {
    pub fn terminal(self) -> bool {
        matches!(self, BatchState::Abandoned | BatchState::Landed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BatchState::None => "NONE",
            BatchState::Open => "OPEN",
            BatchState::CiRunning => "CI_RUNNING",
            BatchState::Green => "GREEN",
            BatchState::Attributing => "ATTRIBUTING",
            BatchState::Rebuilding => "REBUILDING",
            BatchState::Abandoned => "ABANDONED",
            BatchState::Landed => "LANDED",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub bead: String,
    pub tip: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Round {
    pub at: String,
    pub repo: String,
    pub batch: String,
    pub state_from: BatchState,
    pub state_to: BatchState,
    pub members: Vec<Member>,
    pub parent: Option<String>,
    pub cause: String,
    pub verdict: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertPass {
    pub at: String,
    pub repo: String,
    pub batch: String,
    pub pass: Option<u32>,
    pub pass_key: Option<String>,
    pub cause: String,
    pub place: String,
    pub wall_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Transition {
    Batch {
        at: String,
        id: String,
        from: BatchState,
        to: BatchState,
        event: String,
        member: Option<String>,
        tip: Option<String>,
        sha: Option<String>,
        n: Option<u32>,
        suites_s: Option<u64>,
        build_s: Option<u64>,
        red_suites: Option<u32>,
    },
    Bead {
        at: String,
        id: String,
        from: String,
        to: String,
        event: String,
        tip: Option<String>,
        code: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopKind {
    Halted,
    Released,
    Paused,
    Unpaused,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub at: String,
    pub kind: StopKind,
    pub repo: Option<String>,
    pub lanes: Option<Vec<String>>,
    pub bead: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Land {
    pub at: String,
    pub repo: String,
    pub bead: String,
    pub merge_sha: Option<String>,
    pub reverts: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmAttempt {
    pub at: String,
    pub caller: String,
    pub kind: String,
    pub outcome: String,
    pub reason_code: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    pub at: String,
    pub repo: String,
    pub batch: String,
    pub pass: u32,
    pub owner: Vec<String>,
    pub innocent: Vec<String>,
    pub wall_secs: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Round(Round),
    CertPass(CertPass),
    Transition(Transition),
    Stop(Stop),
    Land(Land),
    AlarmAttempt(AlarmAttempt),
    Attribution(Attribution),
}

fn typed<T: for<'de> Deserialize<'de>>(line: &str, wrap: fn(T) -> Record) -> Result<Record, String> {
    serde_json::from_str(line).map(wrap).map_err(|e| e.to_string())
}

type Parse = fn(&str) -> Result<Record, String>;

/// The slice's files in the order records of one second are applied: a round's own line
/// before the lifecycle events that echo it, and those before what was measured of them.
pub const FILES: [(&str, Parse); 7] = [
    ("round.ndjson", |l| typed(l, Record::Round)),
    ("transition.ndjson", |l| typed(l, Record::Transition)),
    ("cert_pass.ndjson", |l| typed(l, Record::CertPass)),
    ("attribution.ndjson", |l| typed(l, Record::Attribution)),
    ("land.ndjson", |l| typed(l, Record::Land)),
    ("stop.ndjson", |l| typed(l, Record::Stop)),
    ("alarm_attempt.ndjson", |l| typed(l, Record::AlarmAttempt)),
];

impl Record {
    pub fn at(&self) -> &str {
        match self {
            Record::Round(x) => &x.at,
            Record::CertPass(x) => &x.at,
            Record::Transition(Transition::Batch { at, .. } | Transition::Bead { at, .. }) => at,
            Record::Stop(x) => &x.at,
            Record::Land(x) => &x.at,
            Record::AlarmAttempt(x) => &x.at,
            Record::Attribution(x) => &x.at,
        }
    }
}

/// Every line of every file in `dir`, each typed and stamped in epoch seconds, ordered by
/// time; one second's lines keep their file and line order.
pub fn load(dir: &Path) -> Result<Vec<(u64, Record)>, String> {
    let mut out = Vec::new();
    for (file, parse) in FILES {
        let path = dir.join(file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        for (i, line) in text.lines().enumerate() {
            let at_line = |e: String| format!("{file}:{}: {e}", i + 1);
            let r = parse(line).map_err(at_line)?;
            let secs = parse_rfc3339(r.at()).ok_or_else(|| at_line(format!("{:?} is not a UTC stamp", r.at())))?;
            out.push((secs, r));
        }
    }
    out.sort_by_key(|(at, _)| *at);
    Ok(out)
}

/// A round that has not reached a terminal state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpenRound {
    pub repo: String,
    pub state: BatchState,
    /// When it entered `state`.
    pub since: u64,
    pub members: Vec<Member>,
    pub parent: Option<String>,
}

impl OpenRound {
    /// The fixtures carry no lease heartbeats, so a round counts as leased exactly while a
    /// pass runs on it.
    pub fn leased(&self) -> bool {
        matches!(self.state, BatchState::CiRunning | BatchState::Rebuilding)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassVerdict {
    Green,
    Red,
}

/// A (member, tip) that has passed green and not landed. A later red at the same tip does not
/// unmake the green it already had; a new tip, a landing or leaving delivery ends it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GreenWait {
    pub repo: String,
    /// Its first green.
    pub since: u64,
    pub batch: String,
    pub parent: Option<String>,
}

/// A (member, tip) cut again after a red verdict at that tip, still open.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Repeat {
    pub repo: String,
    pub since: u64,
    pub cuts: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Lane {
    pub rounds_since_landing: u32,
    /// Consecutive passes that ended with neither a green nor a red verdict.
    pub passes_without_verdict: u32,
    pub last_landed: Option<u64>,
}

/// The recorded day as of the last applied line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ReplayWorld {
    pub rounds: BTreeMap<String, OpenRound>,
    /// Each repo's latest cut round.
    pub pointers: BTreeMap<String, String>,
    pub verdicts: BTreeMap<Member, PassVerdict>,
    /// Each bead's lifecycle state.
    pub beads: BTreeMap<String, String>,
    /// Paused repos, with the bead the pause names.
    pub paused: BTreeMap<String, Option<String>>,
    pub halted: bool,
    pub green_waiting: BTreeMap<Member, GreenWait>,
    pub repeats: BTreeMap<Member, Repeat>,
    pub lanes: BTreeMap<String, Lane>,
    pub passes: BTreeMap<(String, String), u32>,
    pub lands: BTreeMap<String, u32>,
    pub alarms: BTreeMap<(String, String, String), u32>,
    pub applied: usize,
}

const RED_CAUSES: [&str; 2] = ["red-attributed", "PassRed"];

impl ReplayWorld {
    /// A stable digest of every field: equal worlds, equal fingerprints, on every run.
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    pub fn apply(&mut self, at: u64, r: &Record) {
        match r {
            Record::Round(x) => self.round(at, x),
            Record::Transition(Transition::Bead { id, to, event, .. }) => self.bead(id, to, event),
            Record::Transition(Transition::Batch { .. }) => {}
            Record::CertPass(x) => *self.passes.entry((x.repo.clone(), x.cause.clone())).or_default() += 1,
            Record::Attribution(x) => self.attribution(x),
            Record::Land(x) => {
                *self.lands.entry(x.repo.clone()).or_default() += 1;
                self.landed(&x.bead);
            }
            Record::Stop(x) => self.stop(x),
            Record::AlarmAttempt(x) => {
                *self.alarms.entry((x.caller.clone(), x.kind.clone(), x.outcome.clone())).or_default() += 1
            }
        }
        self.applied += 1;
    }

    fn round(&mut self, at: u64, r: &Round) {
        let lane = self.lanes.entry(r.repo.clone()).or_default();
        if r.state_from == BatchState::None {
            lane.rounds_since_landing += 1;
            self.pointers.insert(r.repo.clone(), r.batch.clone());
        }
        let ended = r.state_from == BatchState::CiRunning && r.state_to == BatchState::Attributing;
        let red = ended && RED_CAUSES.contains(&r.cause.as_str());
        match r.state_to {
            BatchState::Green | BatchState::Landed => lane.passes_without_verdict = 0,
            _ if red => lane.passes_without_verdict = 0,
            _ if ended => lane.passes_without_verdict += 1,
            _ => {}
        }
        if r.state_to == BatchState::Landed {
            lane.rounds_since_landing = 0;
            lane.last_landed = Some(at);
        }

        let before: BTreeSet<Member> = self.rounds.get(&r.batch).map(|o| o.members.iter().cloned().collect()).unwrap_or_default();
        for m in r.members.iter().filter(|m| !before.contains(m)) {
            self.cut(at, &r.repo, m);
        }
        match r.state_to {
            BatchState::Green => {
                for m in &r.members {
                    self.verdicts.insert(m.clone(), PassVerdict::Green);
                    self.repeats.remove(m);
                    let since = self.green_waiting.get(m).map_or(at, |g| g.since);
                    let wait = GreenWait {
                        repo: r.repo.clone(),
                        since,
                        batch: r.batch.clone(),
                        parent: r.parent.clone(),
                    };
                    self.green_waiting.insert(m.clone(), wait);
                }
            }
            BatchState::Landed => r.members.iter().for_each(|m| self.landed(&m.bead)),
            // A red with several members is only some member's: its attribution names whose.
            _ if red && r.members.len() == 1 => {
                self.verdicts.insert(r.members[0].clone(), PassVerdict::Red);
            }
            _ => {}
        }

        if r.state_to.terminal() {
            self.rounds.remove(&r.batch);
            return;
        }
        let prev = self.rounds.get(&r.batch);
        let since = prev.filter(|o| o.state == r.state_to).map_or(at, |o| o.since);
        let parent = r.parent.clone().or_else(|| prev.and_then(|o| o.parent.clone()));
        self.rounds.insert(
            r.batch.clone(),
            OpenRound {
                repo: r.repo.clone(),
                state: r.state_to,
                since,
                members: r.members.clone(),
                parent,
            },
        );
    }

    /// `m` enters a round: an earlier tip of its bead is no longer the identity waiting or
    /// being re-cut, and this tip, if it was red, is cut again.
    fn cut(&mut self, at: u64, repo: &str, m: &Member) {
        let other_tip = |k: &Member| k.bead == m.bead && k.tip != m.tip;
        self.repeats.retain(|k, _| !other_tip(k));
        self.green_waiting.retain(|k, _| !other_tip(k));
        if self.verdicts.get(m) == Some(&PassVerdict::Red) {
            let r = self.repeats.entry(m.clone()).or_insert_with(|| Repeat {
                repo: repo.to_string(),
                since: at,
                cuts: 0,
            });
            r.cuts += 1;
        }
    }

    fn landed(&mut self, bead: &str) {
        self.green_waiting.retain(|k, _| k.bead != bead);
        self.repeats.retain(|k, _| k.bead != bead);
    }

    /// A bead that leaves delivery (landed, sent back, held or withdrawn) is no longer waiting
    /// to land and no longer re-cut.
    fn bead(&mut self, id: &str, to: &str, event: &str) {
        self.beads.insert(id.to_string(), to.to_string());
        if matches!(to, "LANDED" | "REWORK" | "DROPPED" | "SUPERSEDED") || event == "Hold" {
            self.landed(id);
        }
    }

    /// An attributed red pass is its owners' red, at the tips the round holds them at, and
    /// not its innocent members'.
    fn attribution(&mut self, a: &Attribution) {
        let Some(round) = self.rounds.get(&a.batch) else { return };
        for m in &round.members {
            if a.owner.contains(&m.bead) {
                self.verdicts.insert(m.clone(), PassVerdict::Red);
            } else if a.innocent.contains(&m.bead) && self.verdicts.get(m) == Some(&PassVerdict::Red) {
                self.verdicts.remove(m);
            }
        }
    }

    fn stop(&mut self, s: &Stop) {
        match (s.kind, &s.repo) {
            (StopKind::Halted, _) => self.halted = true,
            (StopKind::Released, _) => self.halted = false,
            (StopKind::Paused, Some(repo)) => {
                self.paused.insert(repo.clone(), s.bead.clone());
            }
            (StopKind::Unpaused, Some(repo)) => {
                self.paused.remove(repo);
            }
            (StopKind::Paused | StopKind::Unpaused, None) => {}
        }
    }
}

/// The contract's in-memory reference backend on the replay's clock, reached only through
/// spira-lc's section, intent and lock verbs.
pub struct Store {
    contract: Contract<MemBackend>,
    clock: VirtualClock,
    sink: MetricsSink,
    requests: u64,
}

impl Store {
    fn new(clock: VirtualClock) -> Store {
        let contract = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: MIN_LOCK_TTL_MS })
            .expect("an empty in-memory store opens");
        Store {
            contract,
            clock,
            sink: MetricsSink::new(None, "sim-replay", std::time::Duration::from_millis(1)),
            requests: 0,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// `spira-lc <argv…>` with `body` as its `--json`: the exit code and the JSON answer.
    pub fn call(&mut self, argv: &[&str], body: Option<&Value>) -> (i32, Value) {
        self.requests += 1;
        let request = format!("replay/{}", self.requests);
        let mut args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        if let Some(b) = body {
            args.push("--json".into());
            args.push(b.to_string());
        }
        let call = spira_lc::sections::Call {
            now_ms: self.clock.now_ms(),
            request: &request,
            metrics: &self.sink,
        };
        let (code, out) = spira_lc::sections::run(&self.contract, &args, &call);
        let v = serde_json::from_str(&out).unwrap_or_else(|e| panic!("spira-lc {argv:?} answered non-JSON {out:?}: {e}"));
        (code, v)
    }

    /// The answer of a call that must succeed.
    pub fn ok(&mut self, argv: &[&str], body: Option<&Value>) -> Value {
        let (code, v) = self.call(argv, body);
        assert_eq!(code, 0, "spira-lc {argv:?}: {v}");
        v
    }

    pub fn contract(&self) -> &Contract<MemBackend> {
        &self.contract
    }
}

/// What a section's owner answers an intent with. An applied ack is only an ack: the replayed
/// world goes on as recorded, so a step's effect is whatever the day did next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scripted {
    Applied,
    Refused(String),
    NoAck,
}

type ScriptFn = Box<dyn FnMut(&Intent) -> Scripted>;

/// The owner of one section, acknowledging its intents as a script says.
pub struct ScriptedOwner {
    kind: String,
    name: String,
    script: ScriptFn,
    /// Intents the script chose not to answer: it is asked once per intent.
    unanswered: BTreeSet<String>,
    pub acked: Vec<(String, Scripted)>,
}

impl ScriptedOwner {
    /// Acks every intent on `kind/name` applied.
    pub fn new(kind: &str, name: &str) -> ScriptedOwner {
        ScriptedOwner {
            kind: kind.to_string(),
            name: name.to_string(),
            script: Box::new(|_| Scripted::Applied),
            unanswered: BTreeSet::new(),
            acked: Vec::new(),
        }
    }

    pub fn script(mut self, f: impl FnMut(&Intent) -> Scripted + 'static) -> ScriptedOwner {
        self.script = Box::new(f);
        self
    }

    /// Every unacknowledged intent on this section, answered per the script.
    fn answer(&mut self, store: &mut Store) {
        let owner = spira_desired_state::sections::lookup(&self.kind)
            .expect("a scripted owner's kind is registered")
            .owner;
        let prefix = spira_desired_state::intent::intent_key(&self.kind, &self.name, "");
        let snap = store.ok(&["section", "snapshot"], None);
        let pending: Vec<(String, Intent)> = snap["intents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["key"].as_str().is_some_and(|k| k.starts_with(&prefix)))
            .filter_map(|i| Some((i["key"].as_str()?.to_string(), parse_intent(&i["intent"]).ok()?)))
            .filter(|(k, i)| i.ack.is_none() && !self.unanswered.contains(k))
            .collect();
        for (key, intent) in pending {
            let reply = (self.script)(&intent);
            match &reply {
                Scripted::Applied => {
                    store.ok(&["intent", "ack", &key, "--actor", owner, "--result", "applied"], None);
                }
                Scripted::Refused(reason) => {
                    store.ok(&["intent", "ack", &key, "--actor", owner, "--result", "refused", "--reason", reason], None);
                }
                Scripted::NoAck => {
                    self.unanswered.insert(key);
                    continue;
                }
            }
            self.acked.push((key, reply));
        }
    }
}

type PublishFn = Box<dyn FnMut(&ReplayWorld, &mut Store, u64)>;
type PassFn = Box<dyn FnMut(&mut Store, u64)>;

struct Publisher {
    name: String,
    period_ms: u64,
    run: PublishFn,
    runs: usize,
}

struct Inner {
    world: ReplayWorld,
    store: Store,
    records: Vec<(u64, Record)>,
    next: usize,
    window_ms: (u64, u64),
    publishers: Vec<Publisher>,
    passes: Vec<PassFn>,
    pass_runs: usize,
    owners: Vec<ScriptedOwner>,
}

impl Inner {
    /// Every record due by `now_ms`, so a hook at an instant sees every line stamped with it.
    fn apply_due(&mut self, now_ms: u64) {
        while let Some((at, r)) = self.records.get(self.next) {
            if at * 1000 > now_ms {
                break;
            }
            self.world.apply(*at, r);
            self.next += 1;
        }
    }
}

const RECORDS: &str = "replay";
const PASS: &str = "replay-pass";

/// A slice on the simulator, its hooks registered before [`Replay::run`].
pub struct Replay {
    pub sim: Sim,
    inner: Rc<RefCell<Inner>>,
    clock: VirtualClock,
    lines: usize,
}

impl Replay {
    pub fn new(seed: u64, slice: Slice) -> Result<Replay, String> {
        Replay::from_dir(seed, &fixtures_root().join(slice.dir()), slice.window())
    }

    /// The slice in `dir`, its hooks running over `window` (epoch seconds, end exclusive).
    pub fn from_dir(seed: u64, dir: &Path, window: (u64, u64)) -> Result<Replay, String> {
        if window.0 >= window.1 {
            return Err(format!("the window {window:?} is empty"));
        }
        let records = load(dir)?;
        let lines = records.len();
        let clock = VirtualClock::new(0);
        let inner = Inner {
            world: ReplayWorld::default(),
            store: Store::new(clock.clone()),
            records,
            next: 0,
            window_ms: (window.0 * 1000, window.1 * 1000),
            publishers: Vec::new(),
            passes: Vec::new(),
            pass_runs: 0,
            owners: Vec::new(),
        };
        Ok(Replay {
            sim: Sim::new(seed),
            inner: Rc::new(RefCell::new(inner)),
            clock,
            lines,
        })
    }

    /// `f` runs every `period_ms` across the window, from its start.
    pub fn register_publisher(&mut self, name: &str, period_ms: u64, f: impl FnMut(&ReplayWorld, &mut Store, u64) + 'static) {
        assert!(period_ms > 0, "a publisher's period is positive");
        self.inner.borrow_mut().publishers.push(Publisher {
            name: name.to_string(),
            period_ms,
            run: Box::new(f),
            runs: 0,
        });
    }

    /// `f` runs every 60 virtual seconds across the window, after the owners have answered.
    pub fn register_pass(&mut self, f: impl FnMut(&mut Store, u64) + 'static) {
        self.inner.borrow_mut().passes.push(Box::new(f));
    }

    pub fn register_owner(&mut self, o: ScriptedOwner) {
        self.inner.borrow_mut().owners.push(o);
    }

    /// Schedules every record at its time and every hook at its period, and runs to the end.
    pub fn run(&mut self) {
        let (start, end) = self.inner.borrow().window_ms;
        let times: BTreeSet<u64> = self.inner.borrow().records.iter().map(|(at, _)| at * 1000).collect();
        let clock = self.clock.clone();
        let inner = self.inner.clone();
        self.sim.add_scenario(
            RECORDS,
            Box::new(move |sim| {
                clock.advance_to(sim.now);
                inner.borrow_mut().apply_due(sim.now);
            }),
        );
        for t in times {
            self.sim.schedule_step(t, RECORDS);
        }

        let publishers: Vec<(String, u64)> = self.inner.borrow().publishers.iter().map(|p| (p.name.clone(), p.period_ms)).collect();
        for (i, (name, period)) in publishers.into_iter().enumerate() {
            let step = format!("replay-publish:{name}");
            let (clock, inner, again) = (self.clock.clone(), self.inner.clone(), step.clone());
            self.sim.add_scenario(
                &step,
                Box::new(move |sim| {
                    clock.advance_to(sim.now);
                    let mut g = inner.borrow_mut();
                    g.apply_due(sim.now);
                    let Inner { world, store, publishers, .. } = &mut *g;
                    let p = &mut publishers[i];
                    (p.run)(world, store, sim.now);
                    p.runs += 1;
                    if sim.now + period < end {
                        sim.schedule_step(sim.now + period, &again);
                    }
                }),
            );
            self.sim.schedule_step(start, &step);
        }

        let (clock, inner) = (self.clock.clone(), self.inner.clone());
        self.sim.add_scenario(
            PASS,
            Box::new(move |sim| {
                clock.advance_to(sim.now);
                let mut g = inner.borrow_mut();
                g.apply_due(sim.now);
                let Inner { store, owners, passes, .. } = &mut *g;
                for o in owners.iter_mut() {
                    o.answer(store);
                }
                for p in passes.iter_mut() {
                    p(store, sim.now);
                }
                g.pass_runs += 1;
                if sim.now + PASS_PERIOD_MS < end {
                    sim.schedule_step(sim.now + PASS_PERIOD_MS, PASS);
                }
            }),
        );
        self.sim.schedule_step(start + PASS_OFFSET_MS, PASS);
        self.sim.run();
    }

    pub fn world(&self) -> Ref<'_, ReplayWorld> {
        Ref::map(self.inner.borrow(), |i| &i.world)
    }

    pub fn store(&self) -> RefMut<'_, Store> {
        RefMut::map(self.inner.borrow_mut(), |i| &mut i.store)
    }

    pub fn owners(&self) -> Ref<'_, [ScriptedOwner]> {
        Ref::map(self.inner.borrow(), |i| i.owners.as_slice())
    }

    pub fn publisher_runs(&self, name: &str) -> Option<usize> {
        self.inner.borrow().publishers.iter().find(|p| p.name == name).map(|p| p.runs)
    }

    pub fn pass_runs(&self) -> usize {
        self.inner.borrow().pass_runs
    }

    /// Lines loaded from the slice's files.
    pub fn lines(&self) -> usize {
        self.lines
    }

    pub fn trace_signature(&self) -> Vec<(u64, crate::EventKind, bool)> {
        self.sim.trace_signature()
    }
}

#[cfg(test)]
mod tests;
