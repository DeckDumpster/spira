//! Scenario files: the loader (every unknown key refused by name), the run on a virtual clock,
//! the invariants every scenario is held to, and its counted expectations.

use std::collections::BTreeMap;

use runtime::ports::Clock;
use serde::Deserialize;
use state_store::{Object, Request, Selector};

use crate::cut;
use crate::death::{UnitState, VmFact};
use crate::ledger::summary_key;
use crate::liveness::LeaseSettings;
use crate::outcome::{utc_stamp, Cause, Kind};
use crate::pass::{pass, pass_with, Step};
use crate::ports::{Ports, Signal, Verb};
use crate::resources::PassIdentity;
use crate::sim::bench::{bench_settings, bench_slot_settings, Bench};
use crate::sim::git::GitSeed;
use crate::sim::judge::{FakeLedger, JudgeCount, JudgeRun, JudgeSpec};
use crate::sim::ledger::{self as sim_ledger, IdentitySpec, LedgerConfig, LedgerStep};
use crate::sim::run_pass::{self, Advance, RunOutcome, RunPassSpec};
use crate::sim::runner::{Runner, RunnerScript};
use crate::sim::slot_user::{most_at_once, SlotUser, SlotUserScript};
use crate::sim::verbs::{Call, FakeVerbs, VerbScript};
use crate::sim::vm::{FakeVm, VmScript};
use crate::slots::{Pool, Slot, SlotGrant, SlotSettings, EC2_SLOTS_KEY, SPILL_KEY};

pub const PROFILES: &str = include_str!("../../scenarios/profiles.toml");
/// The profiles every scenario runs under, in this order.
pub const PROFILE_NAMES: [&str; 2] = ["default", "reference-minimum"];
/// The seed a scenario runs under when `SPIRA_SEED` does not name one.
pub const DEFAULT_SEED: u64 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(skip)]
    pub name: String,
    pub runner_slots: usize,
    pub vm_vcpus: u32,
    pub vm_mem_mib: u32,
    /// Virtual pass durations are multiplied by this.
    pub duration_factor: f64,
    pub pass_cap_secs: u64,
    #[serde(default)]
    pub note: String,
}

pub fn parse_profiles(text: &str) -> Result<Vec<Profile>, String> {
    let mut all: BTreeMap<String, Profile> =
        toml::from_str(text).map_err(|e| format!("profiles: {e}"))?;
    PROFILE_NAMES
        .iter()
        .map(|n| {
            let mut p = all
                .remove(*n)
                .ok_or_else(|| format!("profiles: no profile {n}"))?;
            p.name = n.to_string();
            Ok(p)
        })
        .collect()
}

pub fn profiles() -> Result<Vec<Profile>, String> {
    parse_profiles(PROFILES)
}

pub fn harness_seed(raw: Option<&str>) -> u64 {
    crate::seed_from(raw).unwrap_or(DEFAULT_SEED)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// Virtual epoch seconds the first pass runs at.
    pub epoch: u64,
    pub passes: u32,
    pub every_secs: u64,
    /// Each later pass starts up to this much later, drawn from the seeded generator.
    #[serde(default)]
    pub jitter_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSeed {
    pub key: String,
    pub data: toml::Value,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// `[[slot]]`: a runner slot the harness holds for `repo`, standing in for the schedule step
/// (round-controller/pass) until it lands. Each pass, after the steps, the harness cuts on every
/// slot no round has taken yet; a cut that opens a round takes its slot. `released` hands the cut
/// a grant whose lock was let go before the first pass.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldSlot {
    pub repo: String,
    #[serde(default = "proxmox")]
    pub pool: String,
    #[serde(default)]
    pub n: u32,
    #[serde(default = "a_day")]
    pub ttl_secs: u64,
    #[serde(default)]
    pub released: bool,
}

/// `[[round_lease]]`: a round lease its runner holds, `locks/round/<repo>/<batch>` (`[lease]` is
/// the lease settings).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundLease {
    pub repo: String,
    pub batch: String,
    #[serde(default = "a_day")]
    pub ttl_secs: u64,
}

/// `[[lease_release]]`: the round lease let go `secs` after the epoch, as its runner does when
/// it finishes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRelease {
    pub repo: String,
    pub batch: String,
    pub secs: u64,
}

/// `[[store_step]]`: `key` set to `data` just before pass `pass` (1-based) runs: what another
/// actor did in between, such as an eject moving a round's head or a new image digest.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreStep {
    pub pass: u32,
    pub key: String,
    pub data: toml::Value,
}

fn proxmox() -> String {
    "proxmox".into()
}

fn a_day() -> u64 {
    86_400
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Count {
    Calls(Option<Verb>),
    /// Outcomes of one kind, or of every kind; `outcomes:<kind>:<batch>` of one round.
    Outcomes(Option<&'static str>, Option<String>),
    /// `blockers:<word>`: rounds the Lander held with this blocker.
    Blockers(String),
    StoreWrites,
    VmRuns,
    /// The longest queue lock hold any verb reported.
    LockHoldMs,
    /// Keys under this prefix once the last pass has run; `attempts:<identity>` counts the
    /// keys under that identity's attempts.
    Keys(String),
    /// `summary:<identity>`: the attempts its summary counts, 0 with none.
    Summary(String),
    /// Runner launches tried, started or not.
    Launches,
    /// `launches:<unit>`: runner launches tried under exactly that unit name.
    LaunchesOf(String),
    /// Lease renewals the scripted runners made.
    Renewals,
    /// Attempts recorded under every `batches/<repo>/<id>/redrive`.
    RedriveAttempts,
    /// Seconds after the epoch of the first line about an orphan; `u64::MAX` when none was found.
    FirstOrphanSecs,
    /// Slots the scripted slot users were granted, in one pool or all.
    SlotGrants(Option<Pool>),
    /// Grants refused to slot users, or to hand drivers only.
    SlotRefusals {
        hand: bool,
    },
    /// Refusals whose text named the holder of every slot not given back.
    RefusalsNamingHolder,
    /// The most slot users' VMs running at once, in one pool or all.
    VmsAtOnce(Option<Pool>),
    /// Seconds after the epoch of the first grant; `u64::MAX` when none was made.
    FirstGrantSecs,
    /// What the scenario's `[judge]` recorded and ran.
    Judge(JudgeCount),
    /// `opens:<place>`: `open` calls asking for that place, `on-base` or `stack-on:<k>`.
    Opens(String),
    /// `run-ends:<word>`: runner runs that ended so (`recorded`, `stopped`, `lease-lost`, …).
    RunEnds(String),
    /// `run-lost:<reason>`: runner runs that lost their lease so (`expired`, `deadline`, `superseded`).
    RunLost(String),
    /// Sub-passes the runner runs launched; `run-launches:<plain|base|without>` of one purpose.
    RunLaunches(Option<String>),
    /// Attempts recorded in the ledger, seeded verdicts aside.
    LedgerAttempts,
    /// Passes a signal cut short before they answered.
    VmTornDown,
    /// Signals sent to passes, `vm-signals:<term|kill>`.
    VmSignals(Signal),
    SlotReleases,
    /// Slots given back while a pass's VM was still up.
    SlotReleasesWithVmUp,
    /// Seconds after the epoch of the last slot given back; `u64::MAX` when none was.
    LastSlotReleaseSecs,
    /// `live-locks:<prefix>`: locks under `locks/<prefix>` still held once the last pass ran.
    LiveLocks(String),
    /// `rounds:<STATE>`: round records in that lifecycle state once the last pass has run.
    Rounds(String),
}

impl Count {
    /// A scenario with a `[judge]` table reads the Judge's names first, so its `launches` are
    /// sub-passes, not runner launches; `attempts:<n>` and `summary:<n>` name a ledger identity
    /// only when `n` is one, and otherwise fall to the Judge's `attempts:<class>`.
    fn parse(s: &str, ids: &BTreeMap<String, PassIdentity>, judged: bool) -> Option<Count> {
        if let Some(c) = JudgeCount::parse(s).filter(|_| judged) {
            return Some(Count::Judge(c));
        }
        let (what, of) = s.split_once(':').map_or((s, None), |(w, o)| (w, Some(o)));
        match (what, of) {
            ("calls", None) => Some(Count::Calls(None)),
            ("calls", Some(v)) => Verb::from_word(v).map(|v| Count::Calls(Some(v))),
            ("outcomes", None) => Some(Count::Outcomes(None, None)),
            ("outcomes", Some(k)) => {
                let (k, batch) = k
                    .split_once(':')
                    .map_or((k, None), |(k, b)| (k, Some(b.to_string())));
                Kind::words()
                    .into_iter()
                    .find(|w| *w == k)
                    .map(|w| Count::Outcomes(Some(w), batch.filter(|b| !b.is_empty())))
            }
            ("blockers", Some(w)) if crate::land::Blocker::WORDS.contains(&w) => {
                Some(Count::Blockers(w.to_string()))
            }
            ("store-writes", None) => Some(Count::StoreWrites),
            ("vm-runs", None) => Some(Count::VmRuns),
            ("lock-hold-ms", None) => Some(Count::LockHoldMs),
            ("keys", Some(p)) if !p.is_empty() => Some(Count::Keys(p.to_string())),
            ("attempts", Some(n)) if ids.contains_key(n) => ids
                .get(n)
                .map(|id| Count::Keys(format!("{}/attempt/", summary_key(id)))),
            ("summary", Some(n)) if ids.contains_key(n) => {
                ids.get(n).map(|id| Count::Summary(summary_key(id)))
            }
            ("launches", None) => Some(Count::Launches),
            ("launches", Some(u)) if u.starts_with("round-controller-run-") => {
                Some(Count::LaunchesOf(u.to_string()))
            }
            ("renewals", None) => Some(Count::Renewals),
            ("redrive-attempts", None) => Some(Count::RedriveAttempts),
            ("first-orphan-secs", None) => Some(Count::FirstOrphanSecs),
            ("slot-grants", None) => Some(Count::SlotGrants(None)),
            ("slot-grants", Some(w)) => Pool::from_word(w).map(|p| Count::SlotGrants(Some(p))),
            ("slot-refusals", None) => Some(Count::SlotRefusals { hand: false }),
            ("slot-refusals", Some("hand")) => Some(Count::SlotRefusals { hand: true }),
            ("refusals-naming-holder", None) => Some(Count::RefusalsNamingHolder),
            ("vms-at-once", None) => Some(Count::VmsAtOnce(None)),
            ("vms-at-once", Some(w)) => Pool::from_word(w).map(|p| Count::VmsAtOnce(Some(p))),
            ("first-grant-secs", None) => Some(Count::FirstGrantSecs),
            ("opens", Some(w)) if !w.is_empty() => Some(Count::Opens(w.to_string())),
            ("run-ends", Some(w))
                if [
                    "recorded",
                    "held",
                    "unfinished",
                    "preempted",
                    "stopped",
                    "lease-lost",
                    "refused",
                ]
                .contains(&w) =>
            {
                Some(Count::RunEnds(w.to_string()))
            }
            ("run-lost", Some(w)) if ["expired", "deadline", "superseded"].contains(&w) => {
                Some(Count::RunLost(w.to_string()))
            }
            ("run-launches", None) => Some(Count::RunLaunches(None)),
            ("run-launches", Some(w)) if ["plain", "base", "without"].contains(&w) => {
                Some(Count::RunLaunches(Some(w.to_string())))
            }
            ("ledger-attempts", None) => Some(Count::LedgerAttempts),
            ("vm-torn-down", None) => Some(Count::VmTornDown),
            ("vm-signals", Some("term")) => Some(Count::VmSignals(Signal::Term)),
            ("vm-signals", Some("kill")) => Some(Count::VmSignals(Signal::Kill)),
            ("slot-releases", None) => Some(Count::SlotReleases),
            ("slot-releases-with-vm-up", None) => Some(Count::SlotReleasesWithVmUp),
            ("last-slot-release-secs", None) => Some(Count::LastSlotReleaseSecs),
            ("live-locks", Some(p)) if !p.is_empty() => Some(Count::LiveLocks(p.to_string())),
            ("rounds", Some(w)) if lifecycle::batch::BatchState::from_str(w).is_some() => {
                Some(Count::Rounds(w.to_string()))
            }
            _ => JudgeCount::parse(s).map(Count::Judge),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectDef {
    pub name: String,
    pub count: String,
    #[serde(default)]
    pub equals: Option<u64>,
    #[serde(default)]
    pub at_most: Option<u64>,
    /// A positive control: this expectation must fail, naming itself.
    #[serde(default)]
    pub inverted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    Equals(u64),
    AtMost(u64),
}

impl Bound {
    fn holds(self, got: u64) -> bool {
        match self {
            Bound::Equals(n) => got == n,
            Bound::AtMost(n) => got <= n,
        }
    }

    fn said(self) -> String {
        match self {
            Bound::Equals(n) => format!("{n}"),
            Bound::AtMost(n) => format!("at most {n}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlineSecs {
    pub boot: u64,
    pub build: u64,
    pub suites: u64,
    pub subpass: u64,
    pub certify: u64,
}

/// The lease keys a scenario runs under, in seconds.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseSpec {
    pub ttl_secs: u64,
    pub deadline_secs: DeadlineSecs,
    pub pass_wall_max_secs: u64,
    pub redrive_retries: u32,
    pub ec2_max_lifetime_secs: u64,
}

impl LeaseSpec {
    fn settings(&self) -> LeaseSettings {
        let d = &self.deadline_secs;
        LeaseSettings {
            ttl_ms: self.ttl_secs * 1000,
            deadlines_ms: [d.boot, d.build, d.suites, d.subpass, d.certify].map(|s| s * 1000),
            pass_wall_max_ms: self.pass_wall_max_secs * 1000,
            redrive_retries: self.redrive_retries,
            ec2_max_lifetime_ms: self.ec2_max_lifetime_secs * 1000,
        }
    }
}

/// A unit's `is-active` state, or `unreadable`; `until`, in epoch seconds, is when systemd stops
/// knowing it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitSeed {
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub until: Option<u64>,
}

/// What round-vm says of a unit's VM: `none`, `running`, `unreadable`, or `ec2-unreadable` with
/// `launched_at` in epoch seconds; `until`, in epoch seconds, is when the VM is gone.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmFactSeed {
    pub unit: String,
    pub fact: String,
    #[serde(default)]
    pub launched_at: Option<u64>,
    #[serde(default)]
    pub until: Option<u64>,
}

/// The slot keys a scenario runs under: `SPIRA_ROUND_VM_SPILL` and `SPIRA_ROUND_RUNNER_SLOTS_EC2`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSpec {
    pub spill: String,
    pub ec2: u32,
}

impl SlotSpec {
    fn settings(&self) -> Result<SlotSettings, String> {
        SlotSettings::from_cfg(&|k: &str| match k {
            EC2_SLOTS_KEY => Ok(self.ec2.to_string()),
            SPILL_KEY => Ok(self.spill.clone()),
            _ => Err(format!("{k} is not a slot key")),
        })
    }
}

impl VmFactSeed {
    fn fact(&self) -> Result<VmFact, String> {
        Ok(match (self.fact.as_str(), self.launched_at) {
            ("none", None) => VmFact::NoVm,
            ("running", None) => VmFact::Running {
                vm: format!("vm-{}", self.unit),
            },
            ("unreadable", None) => VmFact::Unreadable {
                why: "scripted".into(),
            },
            ("ec2-unreadable", Some(at)) => VmFact::Ec2Unreadable {
                instance: format!("i-{}", self.unit),
                launched_at_ms: at * 1000,
            },
            _ => {
                return Err(format!(
                    "vm_fact {}: {:?} takes launched_at only as ec2-unreadable, which needs it",
                    self.unit, self.fact
                ))
            }
        })
    }
}

/// One launch's answer, in order: `ok = true`, or `refuse = "<why>"`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchScript {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub refuse: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    description: String,
    repos: Vec<String>,
    run: RunSpec,
    #[serde(default)]
    lease: Option<LeaseSpec>,
    #[serde(default)]
    store: Vec<StoreSeed>,
    #[serde(default)]
    verb: Vec<VerbScript>,
    #[serde(default)]
    vm: Vec<VmScript>,
    #[serde(default)]
    runner: Vec<RunnerScript>,
    #[serde(default)]
    unit: Vec<UnitSeed>,
    #[serde(default)]
    vm_fact: Vec<VmFactSeed>,
    #[serde(default)]
    launch: Vec<LaunchScript>,
    #[serde(default)]
    slots: Option<SlotSpec>,
    #[serde(default)]
    slot_user: Vec<SlotUserScript>,
    #[serde(default)]
    expect: Vec<ExpectDef>,
    #[serde(default)]
    ledger: Option<LedgerConfig>,
    #[serde(default)]
    identity: Vec<IdentitySpec>,
    #[serde(default)]
    ledger_step: Vec<LedgerStep>,
    #[serde(default)]
    judge: Option<JudgeSpec>,
    #[serde(default)]
    slot: Vec<HeldSlot>,
    #[serde(default)]
    round_lease: Vec<RoundLease>,
    #[serde(default)]
    store_step: Vec<StoreStep>,
    #[serde(default)]
    run_pass: Vec<RunPassSpec>,
    #[serde(default)]
    git: Vec<GitSeed>,
    #[serde(default)]
    lease_release: Vec<LeaseRelease>,
}

#[derive(Clone, Debug)]
pub struct Expect {
    pub name: String,
    pub count: Count,
    pub bound: Bound,
    pub inverted: bool,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub description: String,
    pub repos: Vec<String>,
    pub run: RunSpec,
    pub leases: LeaseSettings,
    pub store: Vec<StoreSeed>,
    pub verbs: Vec<VerbScript>,
    pub vm: Vec<VmScript>,
    pub runners: Vec<RunnerScript>,
    /// Each unit's state, and the epoch milliseconds from which systemd no longer knows it.
    pub units: Vec<(String, UnitState, Option<u64>)>,
    /// Each unit's VM fact, and the epoch milliseconds from which its VM is gone.
    pub vm_facts: Vec<(String, VmFact, Option<u64>)>,
    pub launches: Vec<Result<(), String>>,
    pub slots: SlotSettings,
    pub slot_users: Vec<SlotUserScript>,
    pub expects: Vec<Expect>,
    pub ledger: Option<LedgerConfig>,
    pub identities: BTreeMap<String, PassIdentity>,
    pub ledger_steps: Vec<LedgerStep>,
    pub judge: Option<JudgeSpec>,
    pub held_slots: Vec<HeldSlot>,
    pub round_leases: Vec<RoundLease>,
    pub store_steps: Vec<StoreStep>,
    pub run_passes: Vec<RunPassSpec>,
    pub git: Vec<GitSeed>,
    pub lease_releases: Vec<LeaseRelease>,
}

pub fn parse(text: &str) -> Result<Scenario, String> {
    let f: File = toml::from_str(text).map_err(|e| format!("scenario: {e}"))?;
    let identities = sim_ledger::identities(&f.identity).map_err(|e| format!("scenario: {e}"))?;
    sim_ledger::check(&f.ledger_step, &identities, f.ledger.as_ref())
        .map_err(|e| format!("scenario: {e}"))?;
    if let Some(j) = &f.judge {
        j.check().map_err(|e| format!("scenario: {e}"))?;
    }
    for r in &f.run_pass {
        r.check().map_err(|e| format!("scenario: {e}"))?;
    }
    let judged = f.judge.is_some();
    let expects = f
        .expect
        .into_iter()
        .map(|e| {
            let count = Count::parse(&e.count, &identities, judged).ok_or_else(|| {
                format!(
                    "scenario: expectation {} counts {:?}, which is nothing the harness counts",
                    e.name, e.count
                )
            })?;
            let bound = match (e.equals, e.at_most) {
                (Some(n), None) => Bound::Equals(n),
                (None, Some(n)) => Bound::AtMost(n),
                _ => {
                    return Err(format!(
                        "scenario: expectation {} needs exactly one of equals and at_most",
                        e.name
                    ))
                }
            };
            if matches!(count, Count::Judge(_)) && !judged {
                return Err(format!(
                    "scenario: expectation {} counts {:?}, which only a [judge] table runs",
                    e.name, e.count
                ));
            }
            Ok(Expect {
                name: e.name,
                count,
                bound,
                inverted: e.inverted,
            })
        })
        .collect::<Result<_, String>>()?;
    let units = f
        .unit
        .into_iter()
        .map(|u| {
            let s = match u.state.as_str() {
                "unreadable" => UnitState::Unreadable("scripted".into()),
                s => UnitState::from_is_active(s),
            };
            (u.name, s, u.until.map(|t| t * 1000))
        })
        .collect();
    let vm_facts = f
        .vm_fact
        .iter()
        .map(|v| Ok((v.unit.clone(), v.fact()?, v.until.map(|u| u * 1000))))
        .collect::<Result<_, String>>()
        .map_err(|e| format!("scenario: {e}"))?;
    let launches = f
        .launch
        .into_iter()
        .map(|l| match (l.ok, l.refuse) {
            (true, None) => Ok(Ok(())),
            (false, Some(r)) => Ok(Err(r)),
            _ => Err("scenario: a launch is exactly one of ok = true and refuse".to_string()),
        })
        .collect::<Result<_, String>>()?;
    Ok(Scenario {
        description: f.description,
        repos: f.repos,
        run: f.run,
        leases: f.lease.map_or_else(bench_settings, |l| l.settings()),
        store: f.store,
        verbs: f.verb,
        vm: f.vm,
        runners: f.runner,
        units,
        vm_facts,
        launches,
        slots: f
            .slots
            .map_or_else(|| Ok(bench_slot_settings()), |s| s.settings())?,
        slot_users: f.slot_user,
        expects,
        ledger: f.ledger,
        identities,
        ledger_steps: f.ledger_step,
        judge: f.judge,
        held_slots: f.slot,
        round_leases: f.round_lease,
        store_steps: f.store_step,
        run_passes: f.run_pass,
        git: f.git,
        lease_releases: f.lease_release,
    })
}

#[derive(Debug)]
pub struct Report {
    pub profile: String,
    pub lines: Vec<String>,
    pub calls: Vec<Call>,
    /// What each inverted expectation said when it failed, as it must.
    pub controls: Vec<String>,
}

pub fn run(sc: &Scenario, profile: &Profile, seed: u64) -> Result<Report, String> {
    run_with(sc, profile, seed, &|p, repos| pass(p, repos))
}

/// [`run`] with `steps` in place of the registered ones: a test of the harness itself.
pub fn run_steps(
    sc: &Scenario,
    profile: &Profile,
    seed: u64,
    steps: &[Step],
) -> Result<Report, String> {
    run_with(sc, profile, seed, &|p, repos| pass_with(p, repos, steps))
}

type Pass<'a> = dyn Fn(&Ports, &[String]) + 'a;

fn run_with(sc: &Scenario, profile: &Profile, seed: u64, go: &Pass) -> Result<Report, String> {
    let at = &profile.name;
    let epoch_ms = sc.run.epoch * 1000;
    let mut b = Bench::at(sc.leases.clone(), epoch_ms, seed);
    b.slot_settings = sc.slots.clone();
    for (i, s) in sc.store.iter().enumerate() {
        let data = serde_json::to_value(&s.data).expect("a TOML value is JSON");
        let obj = Object::new(data);
        let obj = match s.ttl_secs {
            Some(t) => obj.with_ttl(t),
            None => obj,
        };
        b.store
            .create(
                &s.key,
                obj,
                &Request::new(&format!("seed-{i}"), "scenario", "seed"),
            )
            .map_err(|e| format!("store seed {}: {e}", s.key))?;
    }
    for (u, s, until) in &sc.units {
        b.units.set_until(u, s.clone(), *until);
    }
    for (u, f, until) in &sc.vm_facts {
        b.vms.set_until(u, f.clone(), *until);
    }
    for l in &sc.launches {
        b.launch.script(l.clone());
    }
    for g in &sc.git {
        b.git.seed(g);
    }
    let runners = sc
        .runners
        .iter()
        .map(|r| Runner::new(r.clone(), epoch_ms))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|e| format!("scenario: {e}"))?;
    let users = sc
        .slot_users
        .iter()
        .map(|u| SlotUser::new(u.clone(), epoch_ms))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|e| format!("scenario: {e}"))?;
    let lock = |resource: &str, ttl_secs: u64| {
        b.store
            .lock(
                resource,
                ttl_secs * 1000,
                &Request::new(&format!("seed-lock-{resource}"), "scenario", "seed"),
            )
            .map_err(|e| format!("lock seed {resource}: {e}"))
    };
    for l in &sc.round_leases {
        lock(&format!("round/{}/{}", l.repo, l.batch), l.ttl_secs)?;
    }
    let mut held: Vec<(String, Option<SlotGrant>)> = Vec::new();
    for s in &sc.held_slots {
        let pool = Pool::from_word(&s.pool)
            .ok_or_else(|| format!("scenario: slot pool {:?} is no pool", s.pool))?;
        let slot = Slot { pool, n: s.n };
        let fence = lock(&slot.lock(), s.ttl_secs)?;
        if s.released {
            b.store
                .release(&fence, &Request::new("seed-release", "scenario", "seed"))
                .map_err(|e| format!("lock seed release: {e}"))?;
        }
        held.push((s.repo.clone(), Some(SlotGrant::of_fence(slot, fence))));
    }
    let seeded = b.store.head().expect("the in-memory store has a head");
    let vm = FakeVm::new(sc.vm.clone(), profile.clone(), b.clock.clone());
    b.verbs = FakeVerbs::new(sc.verbs.clone(), vm.clone(), b.clock.clone())
        .map_err(|e| format!("scenario: {e}"))?
        .with_store(b.store.clone())
        .with_git(b.git.clone());
    b.ledger = FakeLedger::seeded(sc.judge.as_ref(), profile);
    let judge = sc.judge.as_ref().map(|j| JudgeRun::new(j, profile));
    let b = b;
    let ports = b.ports();
    let gc = state_store::gc::Controller::new(&b.store);
    let mut revokes: Vec<(u64, String, String)> = sc
        .run_passes
        .iter()
        .filter_map(|r| {
            r.revoke_lease_secs
                .map(|t| (epoch_ms + t * 1000, r.repo.clone(), r.batch.clone()))
        })
        .chain(
            sc.lease_releases
                .iter()
                .map(|r| (epoch_ms + r.secs * 1000, r.repo.clone(), r.batch.clone())),
        )
        .collect();
    revokes.sort();
    let world = std::cell::RefCell::new(World {
        sc,
        b: &b,
        ports: &ports,
        gc: &gc,
        go,
        runners,
        users,
        held,
        judge: judge.as_ref(),
        i: 0,
        next: epoch_ms,
        revokes,
        broken: Vec::new(),
        at,
        failed: false,
    });
    let mut starts: Vec<&RunPassSpec> = sc.run_passes.iter().collect();
    starts.sort_by_key(|r| r.start_secs);
    let mut ran: Vec<RunOutcome> = Vec::new();
    for spec in starts {
        world
            .borrow_mut()
            .advance_to(epoch_ms + spec.start_secs * 1000);
        let (o, faults) = run_pass::drive(
            &ports,
            spec,
            &world,
            &b.clock,
            epoch_ms,
            profile.duration_factor,
        )?;
        world.borrow_mut().broken.extend(
            faults
                .into_iter()
                .map(|f| format!("invariant no-unscripted-fault violated under {at}: {f}")),
        );
        ran.push(o);
    }
    let mut w = world.into_inner();
    w.finish();
    let World {
        runners,
        users,
        mut broken,
        ..
    } = w;
    let now = b.clock.now_ms();
    if let Some(j) = &judge {
        broken.extend(j.broken(now, at));
    }
    for r in vm
        .runs()
        .iter()
        .filter(|r| r.ends <= now && !r.collected && !r.cut_short())
    {
        broken.push(format!("invariant every-pass-has-a-verdict violated under {at}: pass {} finished at {} with no recorded verdict", r.identity, utc_stamp(r.ends)));
    }
    let faults: Vec<String> = vm
        .faults()
        .into_iter()
        .chain(b.verbs.faults())
        .chain(crate::lock(&b.proc_.0).iter().cloned())
        .chain(b.launch.faults())
        .chain(runners.iter().flat_map(|r| r.faults.clone()))
        .chain(users.iter().flat_map(|u| u.faults.clone()))
        .collect();
    if !faults.is_empty() {
        broken.push(format!(
            "invariant no-unscripted-fault violated under {at}: {}",
            faults.join("; ")
        ));
    }

    let (outcomes, calls) = (b.log.outcomes(), b.verbs.calls());
    let writes = b.store.head().expect("the in-memory store has a head") - seeded;
    let mut controls = Vec::new();
    for e in &sc.expects {
        let got = match &e.count {
            Count::Calls(v) => calls
                .iter()
                .filter(|c| v.is_none_or(|v| c.verb == v))
                .count() as u64,
            Count::Outcomes(k, batch) => outcomes
                .iter()
                .filter(|o| k.is_none_or(|k| o.kind.word() == k))
                .filter(|o| batch.is_none() || o.batch == *batch)
                .count() as u64,
            Count::Blockers(w) => outcomes
                .iter()
                .filter(|o| o.kind == Kind::Held && o.cause.word() == w)
                .count() as u64,
            Count::StoreWrites => writes,
            Count::VmRuns => vm.runs().len() as u64,
            Count::LockHoldMs => calls.iter().map(|c| c.lock_hold_ms).max().unwrap_or(0),
            Count::Keys(prefix) => b
                .store
                .list(&[prefix.as_str()], &Selector::Everything)
                .expect("the in-memory store lists")
                .0
                .len() as u64,
            Count::Summary(key) => b
                .store
                .get(key)
                .expect("the in-memory store reads")
                .and_then(|i| i.value.data.get("attempts").and_then(|a| a.as_u64()))
                .unwrap_or(0),
            Count::Launches => b.launch.launched().len() as u64,
            Count::LaunchesOf(u) => b.launch.launched().iter().filter(|l| *l == u).count() as u64,
            Count::Renewals => runners.iter().map(|r| r.renewals).sum(),
            Count::RedriveAttempts => redrive_attempts(&b),
            Count::SlotGrants(pool) => users
                .iter()
                .flat_map(|u| &u.grants)
                .filter(|(p, _)| pool.is_none_or(|w| *p == w))
                .count() as u64,
            Count::SlotRefusals { hand } => users
                .iter()
                .flat_map(|u| &u.refusals)
                .filter(|r| !hand || r.hand)
                .count() as u64,
            Count::RefusalsNamingHolder => users
                .iter()
                .flat_map(|u| &u.refusals)
                .filter(|r| r.named_every_holder)
                .count() as u64,
            Count::VmsAtOnce(pool) => most_at_once(
                users
                    .iter()
                    .flat_map(|u| &u.spans)
                    .filter(|s| pool.is_none_or(|w| s.pool == w)),
            ),
            Count::FirstGrantSecs => users
                .iter()
                .flat_map(|u| &u.grants)
                .map(|(_, at)| (at - epoch_ms) / 1000)
                .min()
                .unwrap_or(u64::MAX),
            Count::FirstOrphanSecs => outcomes
                .iter()
                .filter(|o| matches!(o.cause, Cause::Orphan { .. }))
                .map(|o| (o.at - epoch_ms) / 1000)
                .min()
                .unwrap_or(u64::MAX),
            Count::Judge(c) => judge.as_ref().map_or(0, |j| j.count(c, &b.ledger)),
            Count::Opens(w) => calls
                .iter()
                .filter(|c| c.verb == Verb::Open && c.place.as_deref() == Some(w.as_str()))
                .count() as u64,
            Count::RunEnds(w) => ran.iter().filter(|r| r.end.word() == w).count() as u64,
            Count::RunLost(w) => ran.iter().filter(|r| r.lost() == Some(w.as_str())).count() as u64,
            Count::RunLaunches(w) => ran
                .iter()
                .flat_map(|r| &r.launches)
                .filter(|l| w.as_deref().is_none_or(|w| **l == w))
                .count() as u64,
            Count::LedgerAttempts => b
                .ledger
                .attempts()
                .iter()
                .filter(|(_, by, _)| by != "seed")
                .count() as u64,
            Count::VmTornDown => vm.runs().iter().filter(|r| r.cut_short()).count() as u64,
            Count::VmSignals(sig) => vm
                .runs()
                .iter()
                .flat_map(|r| r.signals.clone())
                .filter(|(_, s)| s == sig)
                .count() as u64,
            Count::SlotReleases => crate::lock(&b.slot_releases).len() as u64,
            Count::SlotReleasesWithVmUp => {
                let runs = vm.runs();
                crate::lock(&b.slot_releases)
                    .iter()
                    .filter(|t| runs.iter().any(|r| r.started <= **t && r.gone_at() > **t))
                    .count() as u64
            }
            Count::LastSlotReleaseSecs => crate::lock(&b.slot_releases)
                .iter()
                .max()
                .map_or(u64::MAX, |t| (t - epoch_ms) / 1000),
            Count::Rounds(word) => rounds_in(&b, word),
            Count::LiveLocks(prefix) => b
                .store
                .list(&[format!("locks/{prefix}").as_str()], &Selector::Everything)
                .expect("the in-memory store lists")
                .0
                .iter()
                .filter(|i| i.lock_expires_at_ms.is_some_and(|e| e > now))
                .count() as u64,
        };
        let unmet = format!(
            "expectation {} unmet under {at}: counted {got}, expected {}",
            e.name,
            e.bound.said()
        );
        match (e.bound.holds(got), e.inverted) {
            (true, false) => {}
            (false, false) => broken.push(unmet),
            (false, true) => controls.push(unmet),
            (true, true) => broken.push(format!("inverted expectation {} held under {at}: counted {got}, so the harness cannot see what it counts", e.name)),
        }
    }
    if !broken.is_empty() {
        return Err(broken.join("\n"));
    }
    Ok(Report {
        profile: at.clone(),
        lines: outcomes.iter().map(|o| o.line()).collect(),
        calls,
        controls,
    })
}

/// The scenario's world: its passes, scripted actors, store steps and lease revocations, run in
/// time order up to whatever time it is moved on to.
struct World<'w> {
    sc: &'w Scenario,
    b: &'w Bench,
    ports: &'w Ports<'w>,
    gc: &'w state_store::gc::Controller<'w, state_store::MemBackend>,
    go: &'w Pass<'w>,
    runners: Vec<Runner>,
    users: Vec<SlotUser>,
    held: Vec<(String, Option<SlotGrant>)>,
    judge: Option<&'w JudgeRun>,
    /// Passes run so far, and when the next one runs.
    i: u32,
    next: u64,
    revokes: Vec<(u64, String, String)>,
    broken: Vec<String>,
    at: &'w str,
    /// A pass failed: the world stops running passes, and the failure is in `broken`.
    failed: bool,
}

impl World<'_> {
    fn actors_until(&mut self, until: u64) {
        loop {
            let due = |n: Option<u64>| n.filter(|t| *t <= until);
            let Some((t, is_user, i)) = self
                .runners
                .iter()
                .enumerate()
                .filter_map(|(i, r)| due(r.next_ms()).map(|t| (t, false, i)))
                .chain(
                    self.users
                        .iter()
                        .enumerate()
                        .filter_map(|(i, u)| due(u.next_ms()).map(|t| (t, true, i))),
                )
                .min()
            else {
                break;
            };
            self.b.clock.advance_to(t);
            if is_user {
                self.users[i].act(self.b, self.ports);
            } else {
                self.runners[i].act(self.b, self.ports);
            }
        }
    }

    /// Lets the round lease go behind its holder's back, as a lapsed lease taken by another does.
    fn revoke(&mut self, until: u64) {
        while let Some((t, repo, batch)) = self.revokes.first().cloned().filter(|r| r.0 <= until) {
            self.revokes.remove(0);
            self.actors_until(t);
            self.b.clock.advance_to(t);
            let res = crate::lease::Resource::round(&repo, &batch);
            if let Ok(Some(l)) = self.b.store.get(&res.lock_key()) {
                let f = state_store::Fence {
                    resource: res.lock,
                    token: l.create_rev,
                };
                let _ = self.b.store.release(
                    &f,
                    &Request::new(&format!("revoke-{batch}-{t}"), "scenario", "revoke"),
                );
            }
        }
    }

    fn pass(&mut self) -> Result<(), String> {
        let (sc, ports, next, n) = (self.sc, self.ports, self.next, self.i + 1);
        self.revoke(next);
        self.actors_until(next);
        self.b.clock.advance_to(next);
        self.gc.tick(next).map_err(|e| format!("store gc: {e}"))?;
        for s in sc.store_steps.iter().filter(|s| s.pass == n) {
            let data = serde_json::to_value(&s.data).expect("a TOML value is JSON");
            let req = Request::new(
                &format!("store-step-{}-{}", s.pass, s.key),
                "scenario",
                "step",
            );
            self.b
                .store
                .txn(
                    &state_store::Txn::new().put(&s.key, Object::new(data)),
                    &req,
                )
                .map_err(|e| format!("store step {} at pass {}: {e}", s.key, s.pass))?;
        }
        (self.go)(ports, &sc.repos);
        for (repo, slot) in self.held.iter_mut() {
            let Some(grant) = slot.as_ref() else { continue };
            let r = cut::cut(ports, repo, grant);
            cut::record(ports, repo, &r);
            if matches!(r, Ok(Some(_))) {
                *slot = None;
            }
        }
        self.broken.extend(sim_ledger::run(
            ports,
            n,
            &sc.ledger_steps,
            &sc.identities,
            sc.ledger.as_ref(),
            self.at,
        ));
        if let Some(j) = self.judge {
            j.drive(ports, &self.b.clock, n);
        }
        self.i = n;
        if self.i < sc.run.passes {
            let jitter = ports.next_u64() % (sc.run.jitter_secs * 1000 + 1);
            self.next += sc.run.every_secs * 1000 + jitter;
        }
        Ok(())
    }

    fn passes_until(&mut self, t: u64) {
        while !self.failed && self.i < self.sc.run.passes && self.next <= t {
            if let Err(e) = self.pass() {
                self.broken.push(e);
                self.failed = true;
            }
        }
    }

    /// Every pass still to run.
    fn finish(&mut self) {
        self.passes_until(u64::MAX);
    }
}

impl run_pass::Advance for World<'_> {
    fn advance_to(&mut self, t: u64) {
        self.passes_until(t);
        self.revoke(t);
        self.actors_until(t);
        self.b.clock.advance_to(t);
    }
}

fn rounds_in(b: &Bench, word: &str) -> u64 {
    let (items, _) = b
        .store
        .list(&["batches/"], &Selector::Everything)
        .expect("the in-memory store lists");
    items
        .iter()
        .filter(|i| i.key.matches('/').count() == 2)
        .filter_map(|i| {
            serde_json::from_value::<lifecycle::batch::BatchState>(i.value.data.get("state")?.clone())
                .ok()
        })
        .filter(|s| s.as_str() == word)
        .count() as u64
}

fn redrive_attempts(b: &Bench) -> u64 {
    let (items, _) = b
        .store
        .list(&["batches/"], &Selector::Everything)
        .expect("the in-memory store lists");
    items
        .iter()
        .filter(|i| i.key.ends_with("/redrive"))
        .filter_map(|i| i.value.data.get("attempts")?.as_array().map(Vec::len))
        .sum::<usize>() as u64
}

/// The scenario under every profile on one seed; every profile's
/// failure is reported, not just the first.
pub fn run_all(text: &str, seed: u64) -> Result<Vec<Report>, String> {
    let sc = parse(text)?;
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for p in profiles().expect("the shipped profiles parse") {
        match run(&sc, &p, seed) {
            Ok(r) => ok.push(r),
            Err(e) => bad.push(e),
        }
    }
    if bad.is_empty() {
        Ok(ok)
    } else {
        Err(bad.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "description = \"t\"\nrepos = [\"a\", \"b\"]\n[run]\nepoch = 1800000000\npasses = 2\nevery_secs = 30\n";

    fn profile() -> Profile {
        profiles().unwrap().remove(1)
    }

    fn go(extra: &str) -> Result<Report, String> {
        run(&parse(&format!("{BASE}{extra}")).unwrap(), &profile(), 3)
    }

    #[test]
    fn the_shipped_profiles_are_default_then_reference_minimum_and_a_missing_one_is_named() {
        let p = profiles().unwrap();
        assert_eq!(
            p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            PROFILE_NAMES
        );
        assert_eq!(
            (p[1].runner_slots, p[1].vm_vcpus, p[1].vm_mem_mib),
            (1, 2, 4096)
        );
        assert!(
            p[1].note.contains("goal") && p[1].note.contains("round-controller/tiny-hardware"),
            "{}",
            p[1].note
        );
        assert_eq!(parse_profiles("[default]\nrunner_slots = 1\nvm_vcpus = 1\nvm_mem_mib = 1\nduration_factor = 1.0\npass_cap_secs = 1\n").unwrap_err(), "profiles: no profile reference-minimum");
        assert!(parse_profiles("[default]\nslots = 1\n")
            .unwrap_err()
            .contains("slots"));
    }

    #[test]
    fn the_loader_refuses_an_unknown_key_by_name_at_any_depth() {
        assert!(parse(&format!("{BASE}pases = 3\n"))
            .unwrap_err()
            .contains("pases"));
        assert!(parse(&format!(
            "{BASE}[[vm]]\nidentity = \"h\"\nverdict = \"green\"\nduration_secs = 1\nwall = 2\n"
        ))
        .unwrap_err()
        .contains("wall"));
        assert!(parse(&format!(
            "{BASE}[[expect]]\nname = \"n\"\ncount = \"calls\"\nequal = 1\n"
        ))
        .unwrap_err()
        .contains("equal"));
    }

    #[test]
    fn an_expectation_must_count_something_the_harness_counts() {
        for bad in [
            "calls:preempt",
            "outcomes:exploded",
            "seconds",
            "vm-runs:x",
            "slot-grants:gcp",
            "slot-refusals:robot",
            "vms-at-once:gcp",
        ] {
            let e = parse(&format!(
                "{BASE}[[expect]]\nname = \"n1\"\ncount = \"{bad}\"\nequals = 0\n"
            ))
            .unwrap_err();
            assert!(
                e.contains("expectation n1 counts") && e.contains(bad),
                "{e}"
            );
        }
    }

    #[test]
    fn every_count_is_counted() {
        let expect = |n: &str, c: &str, v: u64| {
            format!("[[expect]]\nname = \"{n}\"\ncount = \"{c}\"\nequals = {v}\n")
        };
        let text = [
            expect("c", "calls", 0),
            expect("co", "calls:open", 0),
            expect("o", "outcomes", 4),
            expect("ov", "outcomes:visited", 4),
            expect("ob", "outcomes:visited:b1", 0),
            expect("oe", "outcomes:visited:", 4),
            expect("bl", "blockers:refused", 0),
            expect("w", "store-writes", 0),
            expect("v", "vm-runs", 0),
            expect("l", "lock-hold-ms", 0),
        ]
        .concat();
        let r = go(&format!("[[store]]\nkey = \"batches/a/b1\"\ndata = {{ state = \"OPEN\" }}\n[[store]]\nkey = \"telemetry/x\"\ndata = 1\nttl_secs = 60\n{text}")).unwrap();
        assert_eq!(r.lines.len(), 4);
        assert!(
            r.lines[2].starts_with("2027-01-15T08:00:30Z visited steps repo=a"),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn an_unmet_expectation_and_an_inverted_one_that_holds_are_each_named() {
        let e = go("[[expect]]\nname = \"wrong\"\ncount = \"outcomes\"\nequals = 9\n[[expect]]\nname = \"flipped\"\ncount = \"outcomes\"\nequals = 4\ninverted = true\n").unwrap_err();
        assert!(
            e.contains("expectation wrong unmet under reference-minimum: counted 4, expected 9"),
            "{e}"
        );
        assert!(
            e.contains("inverted expectation flipped held under reference-minimum"),
            "{e}"
        );
    }

    #[test]
    fn an_inverted_expectation_that_fails_is_a_control_naming_itself() {
        let r =
            go("[[expect]]\nname = \"never\"\ncount = \"outcomes\"\nequals = 0\ninverted = true\n")
                .unwrap();
        assert_eq!(
            r.controls,
            ["expectation never unmet under reference-minimum: counted 4, expected 0"]
        );
    }

    fn opening(p: &Ports, repo: &str) {
        let _ = p.verbs.open(&crate::ports::OpenReq {
            repo: repo.into(),
            members: vec![],
            name: None,
            place: crate::ports::Place::OnBase,
        });
    }

    fn certifying(p: &Ports, repo: &str) {
        let _ = p.verbs.certify(repo, "b1", None);
    }

    fn certified(p: &Ports, repo: &str) {
        let h = p.verbs.certify(repo, "b1", None).unwrap();
        assert_eq!(h.poll(), Some(Ok(crate::ports::CertifyEnd::default())));
    }

    fn spawning(p: &Ports, _: &str) {
        let _ = p.proc_.run(&runtime::ports::ProcSpec {
            prog: "git".into(),
            args: vec![],
            stdin: vec![],
            deadline: std::time::Duration::from_secs(1),
        });
    }

    fn writing(p: &Ports, repo: &str) {
        let t = state_store::Txn::new().put(
            &format!("batches/{repo}/x"),
            Object::new(serde_json::json!(1)),
        );
        p.store
            .txn(
                &t,
                &Request::new(&format!("w-{repo}-{}", p.clock.now_ms()), "t", "w"),
            )
            .unwrap();
    }

    #[test]
    fn a_step_runs_per_repo_per_pass_and_its_writes_and_calls_are_counted() {
        let sc = parse(&format!(
            "{BASE}[[expect]]\nname = \"w\"\ncount = \"store-writes\"\nequals = 4\n"
        ))
        .unwrap();
        run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "write",
                run: writing,
            }],
        )
        .unwrap();
        let open = |hold: u64| {
            format!("[[verb]]\nverb = \"open\"\nok = {{ batch = \"b1\", head = \"h1\" }}\nlock_hold_ms = {hold}\n")
        };
        let expect = "[[expect]]\nname = \"opens\"\ncount = \"calls:open\"\nequals = 4\n[[expect]]\nname = \"hold\"\ncount = \"lock-hold-ms\"\nequals = 70\n";
        let sc = parse(&format!(
            "{BASE}{}{}{}{}{expect}",
            open(5),
            open(70),
            open(5),
            open(5)
        ))
        .unwrap();
        let r = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "open",
                run: opening,
            }],
        )
        .unwrap();
        assert_eq!(
            r.calls.iter().map(|c| c.repo.as_str()).collect::<Vec<_>>(),
            ["a", "b", "a", "b"]
        );
        let sc = parse(&format!("{BASE}{}{expect}", open(70))).unwrap();
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "open",
                run: opening,
            }],
        )
        .unwrap_err();
        assert!(
            e.contains("unscripted call: queue round open b -"),
            "a scripted answer is used once: {e}"
        );
    }

    #[test]
    fn a_pass_whose_verdict_was_read_keeps_the_invariant_and_every_run_is_counted() {
        let sc = parse(&format!("{BASE}[[vm]]\nidentity = \"b1\"\nverdict = \"green\"\nduration_secs = 0\n[[expect]]\nname = \"runs\"\ncount = \"vm-runs\"\nequals = 4\n")).unwrap();
        run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "certify",
                run: certified,
            }],
        )
        .unwrap();
    }

    #[test]
    fn an_unscripted_spawn_and_an_uncollected_pass_break_the_invariants() {
        let sc = parse(&format!(
            "{BASE}[[vm]]\nidentity = \"b1\"\nverdict = \"green\"\nduration_secs = 1\n"
        ))
        .unwrap();
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "spawn",
                run: spawning,
            }],
        )
        .unwrap_err();
        assert!(
            e.contains(
                "invariant no-unscripted-fault violated under reference-minimum: spawned git"
            ),
            "{e}"
        );
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "certify",
                run: certifying,
            }],
        )
        .unwrap_err();
        assert!(e.contains("invariant every-pass-has-a-verdict violated under reference-minimum: pass b1 finished at"), "{e}");
    }

    #[test]
    fn a_seed_the_store_refuses_is_named() {
        let e = run(
            &parse(&format!(
                "{BASE}[[store]]\nkey = \"telemetry/x\"\ndata = 1\n"
            ))
            .unwrap(),
            &profile(),
            1,
        )
        .unwrap_err();
        assert!(e.starts_with("store seed telemetry/x:"), "{e}");
        let e = run(
            &parse(&format!("{BASE}[[verb]]\nverb = \"land\"\n")).unwrap(),
            &profile(),
            1,
        )
        .unwrap_err();
        assert!(e.contains("exactly one of ok and refuse"), "{e}");
    }

    #[test]
    fn a_lock_or_store_step_the_store_refuses_is_named() {
        let e = go("[[round_lease]]\nrepo = \"a\"\nbatch = \"b1\"\nttl_secs = 0\n").unwrap_err();
        assert!(e.starts_with("lock seed round/a/b1:"), "{e}");
        let e = go("[[store_step]]\npass = 2\nkey = \"locks/x\"\ndata = 1\n").unwrap_err();
        assert!(e.starts_with("store step locks/x at pass 2:"), "{e}");
        assert!(parse(&format!("{BASE}[[slot]]\nrepo = \"a\"\nslots = 1\n"))
            .unwrap_err()
            .contains("slots"));
    }

    #[test]
    fn a_held_slot_is_cut_on_once_and_a_cut_that_opens_takes_it() {
        let lane = "[[store]]\nkey = \"lanes/a\"\ndata = { landing_ref = \"b0\", image_digest = \"d\", harness_version = \"h\" }\n";
        let pool = "[[store]]\nkey = \"lifecycle/sp-a\"\ndata = { repo = \"a\", state = \"CERTIFIED\", tip = \"t\" }\n";
        let open = "[[verb]]\nverb = \"open\"\nok = { batch = \"r1\", head = \"h1\" }\n";
        let r = go(&format!("{lane}{pool}{open}[[slot]]\nrepo = \"a\"\n[[expect]]\nname = \"one\"\ncount = \"opens:on-base\"\nequals = 1\n")).unwrap();
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("opened ok repo=a batch=r1")),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn run_all_reports_every_profiles_failure() {
        let e = run_all(
            &format!("{BASE}[[expect]]\nname = \"n\"\ncount = \"calls\"\nequals = 1\n"),
            1,
        )
        .unwrap_err();
        assert!(
            e.contains("under default") && e.contains("under reference-minimum"),
            "{e}"
        );
        assert_eq!(run_all(BASE, 1).unwrap().len(), 2);
        assert!(run_all("x", 1).is_err());
    }

    #[test]
    fn the_seed_is_spira_seed_or_the_default() {
        assert_eq!(
            (harness_seed(Some("9")), harness_seed(None)),
            (9, DEFAULT_SEED)
        );
    }

    const LEASE: &str = "[lease]\nttl_secs = 30\npass_wall_max_secs = 100\nredrive_retries = 1\nec2_max_lifetime_secs = 50\n[lease.deadline_secs]\nboot = 1\nbuild = 2\nsuites = 3\nsubpass = 4\ncertify = 5\n";

    #[test]
    fn the_lease_section_is_read_in_milliseconds_and_its_absence_is_the_benchs() {
        let sc = parse(&format!("{BASE}{LEASE}")).unwrap();
        assert_eq!(
            sc.leases,
            LeaseSettings {
                ttl_ms: 30_000,
                deadlines_ms: [1_000, 2_000, 3_000, 4_000, 5_000],
                pass_wall_max_ms: 100_000,
                redrive_retries: 1,
                ec2_max_lifetime_ms: 50_000
            }
        );
        assert_eq!(parse(BASE).unwrap().leases, bench_settings());
        assert!(parse(&format!("{BASE}{}", LEASE.replace("boot", "bot")))
            .unwrap_err()
            .contains("bot"));
    }

    #[test]
    fn an_expectation_takes_exactly_one_bound_and_at_most_is_named_when_unmet() {
        for both in ["equals = 1\nat_most = 2\n", ""] {
            let e = parse(&format!(
                "{BASE}[[expect]]\nname = \"n\"\ncount = \"calls\"\n{both}"
            ))
            .unwrap_err();
            assert!(
                e.contains("expectation n needs exactly one of equals and at_most"),
                "{e}"
            );
        }
        let e = go("[[expect]]\nname = \"few\"\ncount = \"outcomes\"\nat_most = 3\n").unwrap_err();
        assert!(
            e.contains(
                "expectation few unmet under reference-minimum: counted 4, expected at most 3"
            ),
            "{e}"
        );
        go("[[expect]]\nname = \"few\"\ncount = \"outcomes\"\nat_most = 4\n").unwrap();
    }

    #[test]
    fn units_vm_facts_and_launches_are_scripted_and_a_malformed_one_is_named() {
        let sc = parse(&format!(
            "{BASE}[[unit]]\nname = \"u1\"\nstate = \"active\"\nuntil = 1800000090\n[[unit]]\nname = \"u2\"\nstate = \"unreadable\"\n\
             [[vm_fact]]\nunit = \"u1\"\nfact = \"none\"\n[[vm_fact]]\nunit = \"u2\"\nfact = \"running\"\n\
             [[vm_fact]]\nunit = \"u3\"\nfact = \"unreadable\"\n[[vm_fact]]\nunit = \"u4\"\nfact = \"ec2-unreadable\"\nlaunched_at = 9\n\
             [[launch]]\nok = true\n[[launch]]\nrefuse = \"no\"\n"
        ))
        .unwrap();
        assert_eq!(
            sc.units,
            [
                (
                    "u1".to_string(),
                    UnitState::Running("active".into()),
                    Some(1_800_000_090_000)
                ),
                (
                    "u2".to_string(),
                    UnitState::Unreadable("scripted".into()),
                    None
                )
            ]
        );
        assert_eq!(sc.vm_facts[0].1, VmFact::NoVm);
        assert!(matches!(sc.vm_facts[1].1, VmFact::Running { .. }));
        assert!(matches!(sc.vm_facts[2].1, VmFact::Unreadable { .. }));
        assert_eq!(
            sc.vm_facts[3].1,
            VmFact::Ec2Unreadable {
                instance: "i-u4".into(),
                launched_at_ms: 9_000
            }
        );
        assert_eq!(sc.launches, [Ok(()), Err("no".to_string())]);
        for bad in [
            "fact = \"ec2-unreadable\"\n",
            "fact = \"none\"\nlaunched_at = 1\n",
            "fact = \"gone\"\n",
        ] {
            let e = parse(&format!("{BASE}[[vm_fact]]\nunit = \"u\"\n{bad}")).unwrap_err();
            assert!(e.contains("vm_fact u:"), "{e}");
        }
        for bad in ["ok = true\nrefuse = \"x\"\n", "ok = false\n"] {
            let e = parse(&format!("{BASE}[[launch]]\n{bad}")).unwrap_err();
            assert!(e.contains("a launch is exactly one of"), "{e}");
        }
    }

    const RUNNER: &str = "[[runner]]\nrepo = \"a\"\nbatch = \"b1\"\nphase = \"suites\"\nstart_secs = 0\nmarks_every_secs = 10\n";

    #[test]
    fn a_runner_with_no_phase_no_marks_or_no_batch_to_lease_is_named() {
        let e = go(&RUNNER.replace("suites", "lint")).unwrap_err();
        assert!(e.contains("runner b1: no phase \"lint\""), "{e}");
        let e = go(&RUNNER.replace("= 10", "= 0")).unwrap_err();
        assert!(e.contains("marks_every_secs must be above 0"), "{e}");
        let e = go(RUNNER).unwrap_err();
        assert!(
            e.contains("invariant no-unscripted-fault violated under reference-minimum: runner b1 could not take its lease"),
            "{e}"
        );
    }

    #[test]
    fn a_runner_renews_through_the_library_and_a_lease_taken_from_it_tears_it_down() {
        let seed = "[[store]]\nkey = \"batches/a/b1\"\ndata = { state = \"CI_RUNNING\" }\n";
        let renewals =
            |n: u64| format!("[[expect]]\nname = \"r\"\ncount = \"renewals\"\nequals = {n}\n");
        // Two passes 30 s apart; the bench's TTL of 60 s renews every 20 s: at 20 s only.
        go(&format!("{seed}{RUNNER}{}", renewals(1))).unwrap();
        let sc = parse(&format!("{BASE}{seed}{RUNNER}{}", renewals(0))).unwrap();
        // Once per pass: BASE has two repos, and a second steal in one pass would resend the
        // first one's release request id, which the store answers from its cache.
        let steal = |p: &Ports, repo: &str| {
            let res = crate::lease::Resource::round("a", "b1");
            if repo != "a" {
                return;
            }
            if let Some(h) = crate::lease::holder_of(p, &res).unwrap() {
                let f = state_store::Fence {
                    resource: res.lock.clone(),
                    token: h.token,
                };
                p.store
                    .release(
                        &f,
                        &Request::new(&format!("steal-{}", p.clock.now_ms()), "t", "k"),
                    )
                    .unwrap();
                crate::lease::acquire(p, &res, "thief").unwrap();
            }
        };
        run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "steal",
                run: steal,
            }],
        )
        .unwrap();
    }

    #[test]
    fn the_orphan_counts_read_the_outcomes_and_the_redrive_records() {
        let seed = "[[store]]\nkey = \"batches/a/b1\"\ndata = { state = \"CI_RUNNING\", last_lease_token = { main = 1, sub = 0 } }\n";
        let r = go(&format!(
            "{seed}[[launch]]\nok = true\n\
             [[expect]]\nname = \"first\"\ncount = \"first-orphan-secs\"\nequals = 0\n\
             [[expect]]\nname = \"att\"\ncount = \"redrive-attempts\"\nequals = 1\n\
             [[expect]]\nname = \"l\"\ncount = \"launches\"\nequals = 1\n"
        ))
        .unwrap();
        assert!(
            r.lines[0].contains("redriven orphan repo=a batch=b1 expired=1-0"),
            "{:?}",
            r.lines
        );
        let e = go("[[expect]]\nname = \"none\"\ncount = \"first-orphan-secs\"\nat_most = 0\n")
            .unwrap_err();
        assert!(e.contains(&format!("counted {}", u64::MAX)), "{e}");
    }

    #[test]
    fn the_slot_section_is_read_through_the_keys_and_its_absence_is_the_benchs() {
        let sc = parse(&format!("{BASE}[slots]\nspill = \"ec2-always\"\nec2 = 5\n")).unwrap();
        assert_eq!(
            sc.slots,
            SlotSettings {
                ec2_slots: 5,
                spill: true
            }
        );
        assert_eq!(parse(BASE).unwrap().slots, bench_slot_settings());
        assert!(parse(&format!("{BASE}[slots]\nspill = \"\"\nec2 = -1\n")).is_err());
        assert!(parse(&format!("{BASE}[slots]\nspil = \"\"\nec2 = 1\n"))
            .unwrap_err()
            .contains("spil"));
    }

    const USER: &str = "[[slot_user]]\nname = \"u1\"\nstart_secs = 0\nhold_secs = 25\n";

    #[test]
    fn slot_users_take_hold_and_release_slots_and_every_slot_count_is_counted() {
        let expect = |n: &str, c: &str, v: u64| {
            format!("[[expect]]\nname = \"{n}\"\ncount = \"{c}\"\nequals = {v}\n")
        };
        let hand = "[[slot_user]]\nname = \"h1\"\nhand = true\nprefer = [\"ec2\"]\nstart_secs = 5\nhold_secs = 5\nretry_secs = 10\n";
        let text = [
            "[slots]\nspill = \"\"\nec2 = 1\n",
            USER,
            hand,
            &expect("g", "slot-grants", 2),
            &expect("gp", "slot-grants:proxmox", 2),
            &expect("ge", "slot-grants:ec2", 0),
            &expect("r", "slot-refusals", 2),
            &expect("rh", "slot-refusals:hand", 2),
            &expect("rn", "refusals-naming-holder", 2),
            &expect("v", "vms-at-once", 1),
            &expect("vp", "vms-at-once:proxmox", 1),
            &expect("f", "first-grant-secs", 0),
        ]
        .concat();
        // u1 holds 0-25 s; h1 asks at 5 and 15, is granted at 25 and releases at 30.
        go(&text).unwrap();
        let none = go(&expect("f", "first-grant-secs", 0)).unwrap_err();
        assert!(none.contains(&format!("counted {}", u64::MAX)), "{none}");
    }

    #[test]
    fn a_slot_user_naming_no_pool_is_named_and_a_lost_slot_is_an_unscripted_fault() {
        let e = go(&USER.replace("hold_secs", "prefer = [\"gcp\"]\nhold_secs")).unwrap_err();
        assert!(e.contains("scenario: slot_user u1: no pool \"gcp\""), "{e}");
        let sc = parse(&format!("{BASE}{}", USER.replace("= 25", "= 50"))).unwrap();
        let steal = |p: &Ports, _: &str| {
            let slot = crate::slots::Slot {
                pool: crate::slots::Pool::Proxmox,
                n: 0,
            };
            let res = crate::lease::Resource {
                lock: slot.lock(),
                record: None,
            };
            if let Some(h) = crate::lease::holder_of(p, &res).unwrap() {
                let f = state_store::Fence {
                    resource: res.lock.clone(),
                    token: h.token,
                };
                p.store
                    .release(
                        &f,
                        &Request::new(&format!("steal-{}", p.clock.now_ms()), "t", "k"),
                    )
                    .unwrap();
            }
        };
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "steal",
                run: steal,
            }],
        )
        .unwrap_err();
        assert!(
            e.contains("invariant no-unscripted-fault violated under reference-minimum: slot_user u1 lost its slot"),
            "{e}"
        );
    }

    #[test]
    fn a_vm_fact_until_a_time_reads_as_no_vm_from_then_on() {
        let sc = parse(&format!(
            "{BASE}[[vm_fact]]\nunit = \"u\"\nfact = \"running\"\nuntil = 1800000010\n"
        ))
        .unwrap();
        assert_eq!(sc.vm_facts[0].2, Some(1_800_000_010_000));
        let b = Bench::new();
        let (u, f, until) = sc.vm_facts[0].clone();
        b.vms.set_until(&u, f, until);
        b.clock.advance_to(1_800_000_009_999);
        assert!(matches!(
            crate::death::Vms::vm_of_slot(&b.vms, "u", None),
            VmFact::Running { .. }
        ));
        b.clock.advance_to(1_800_000_010_000);
        assert_eq!(
            crate::death::Vms::vm_of_slot(&b.vms, "u", None),
            VmFact::NoVm
        );
    }
}
