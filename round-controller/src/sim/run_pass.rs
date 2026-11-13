//! The runner in a scenario: a `[[run_pass]]` takes its round's lease and a slot the way `pass`
//! hands them over, then runs the real [`run::run`] against the fake queue and VM. Every sleep it
//! takes moves the scenario's world on, so passes, store steps and the scripted lease revocation
//! and halt land while it runs.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use batcher::attrib::{Job, JobResult, Purpose};
use batcher::core::Member as RoundMember;
use batcher::drive::{MainKind, MainPoll, RoundRunner};
use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::lease::{acquire, Resource};
use crate::ports::{Child, Ports};
use crate::probe::Sleep;
use crate::run::{self, RunArgs, RunEnd, RunEnv, SubPasses};
use crate::sim::judge::{job_result, JobSpec};
use crate::slots::{grant, Pool};
use crate::switches::Stops;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPassSpec {
    pub repo: String,
    pub batch: String,
    /// Seconds after the epoch `pass` hands the runner its lease and slot.
    pub start_secs: u64,
    #[serde(default = "proxmox")]
    pub pool: String,
    /// The lease is revoked behind the runner's back at this many seconds after the epoch.
    #[serde(default)]
    pub revoke_lease_secs: Option<u64>,
    /// `world.halted` appears at this many seconds after the epoch.
    #[serde(default)]
    pub halt_secs: Option<u64>,
    /// The runner is killed once it has launched this many sub-passes.
    #[serde(default)]
    pub kill_after: Option<usize>,
    #[serde(default = "poll_secs")]
    pub poll_secs: u64,
    #[serde(default = "teardown_secs")]
    pub teardown_secs: u64,
    #[serde(default)]
    pub job: Vec<JobSpec>,
}

fn proxmox() -> String {
    "proxmox".into()
}
fn poll_secs() -> u64 {
    5
}
fn teardown_secs() -> u64 {
    300
}

impl RunPassSpec {
    pub fn check(&self) -> Result<(), String> {
        Pool::from_word(&self.pool)
            .ok_or_else(|| format!("run_pass {}: pool {:?} is no pool", self.batch, self.pool))?;
        if self.poll_secs == 0 {
            return Err(format!(
                "run_pass {}: poll_secs must be above 0",
                self.batch
            ));
        }
        if let Some(w) = self
            .job
            .iter()
            .flat_map(|j| &j.results)
            .find(|w| job_result(w).is_none())
        {
            return Err(format!(
                "run_pass {}: job result {w:?} is not green, red or fault",
                self.batch
            ));
        }
        Ok(())
    }
}

/// What one run did.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub end: RunEnd,
    /// Each sub-pass it launched, by purpose.
    pub launches: Vec<&'static str>,
}

impl RunOutcome {
    pub fn lost(&self) -> Option<&'static str> {
        match &self.end {
            RunEnd::LeaseLost { reason, .. } => Some(reason.word()),
            _ => None,
        }
    }
}

/// The runner's off switches: `world.halted` from its scripted time on, over the world's own.
pub struct FakeSwitches<'w> {
    pub halt_at_ms: Option<u64>,
    pub clock: VirtualClock,
    pub world: &'w dyn Stops,
}

impl Stops for FakeSwitches<'_> {
    fn halted(&self) -> bool {
        self.halt_at_ms.is_some_and(|t| self.clock.now_ms() >= t) || self.world.halted()
    }
    fn scoped(&self, repo: &str, lane: &str) -> Option<spira_config::scoped_stop::RepoStop> {
        self.world.scoped(repo, lane)
    }
    fn round_records(&self, repo: &str) -> Vec<String> {
        self.world.round_records(repo)
    }
}

/// The sub-pass runner: each launch takes the next scripted answer for its removal set and
/// answers once its scaled virtual duration has passed; it waits by sleeping the world on.
pub struct FakeSubs<'s> {
    scripts: BTreeMap<Vec<String>, VecDeque<(JobResult, u64)>>,
    running: Vec<(u64, u64, JobResult)>,
    clock: VirtualClock,
    sleep: &'s dyn Sleep,
    factor: f64,
    kill_after: Option<usize>,
    pub launches: Vec<&'static str>,
    pub faults: Vec<String>,
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v
}

impl<'s> FakeSubs<'s> {
    pub fn new(spec: &RunPassSpec, clock: VirtualClock, sleep: &'s dyn Sleep, factor: f64) -> Self {
        let mut scripts: BTreeMap<Vec<String>, VecDeque<(JobResult, u64)>> = BTreeMap::new();
        for j in &spec.job {
            scripts.entry(sorted(&j.removal)).or_default().extend(
                j.results
                    .iter()
                    .filter_map(|w| job_result(w))
                    .map(|r| (r, j.secs)),
            );
        }
        FakeSubs {
            scripts,
            running: vec![],
            clock,
            sleep,
            factor,
            kill_after: spec.kill_after,
            launches: vec![],
            faults: vec![],
        }
    }
}

fn purpose_word(p: &Purpose) -> &'static str {
    match p {
        Purpose::Plain => "plain",
        Purpose::Base => "base",
        Purpose::Without(_) => "without",
    }
}

impl RoundRunner for FakeSubs<'_> {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, sha: &str) {
        self.faults
            .push(format!("the runner moved the base to {sha}"));
    }
    fn start_main(&mut self, _: MainKind, _: &[String]) -> Result<(), String> {
        self.faults.push("the Judge started a main run".into());
        Err("the Judge starts no main run".into())
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        Ok(MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, job: &Job) -> Result<(), String> {
        if self.kill_after.is_some_and(|k| self.launches.len() >= k) {
            return Err("the runner was killed".into());
        }
        self.launches.push(purpose_word(&job.purpose));
        let key = sorted(&job.removal);
        let (r, secs) = match self.scripts.get_mut(&key).and_then(VecDeque::pop_front) {
            Some(a) => a,
            None => {
                self.faults
                    .push(format!("unscripted sub-pass without {key:?}"));
                (JobResult::Fault, 0)
            }
        };
        let ms = (secs as f64 * self.factor * 1000.0).round() as u64;
        self.running.push((job.id, self.clock.now_ms() + ms, r));
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        let now = self.clock.now_ms();
        let (done, left): (Vec<_>, Vec<_>) = self.running.iter().partition(|(_, at, _)| *at <= now);
        self.running = left;
        done.into_iter().map(|(id, _, r)| (id, r)).collect()
    }
    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }
    fn wait(&mut self) {
        self.sleep.sleep_ms(1000);
    }
}

impl SubPasses for FakeSubs<'_> {
    fn vm(&self) -> Option<&dyn Child> {
        None
    }
    fn close(&mut self) {}
}

/// A sleep that moves the scenario's world on to the time it wakes at.
pub struct WorldSleep<'w, W: Advance> {
    pub world: &'w RefCell<W>,
    pub clock: VirtualClock,
}

pub trait Advance {
    fn advance_to(&mut self, t: u64);
}

impl<W: Advance> Sleep for WorldSleep<'_, W> {
    fn sleep_ms(&self, ms: u64) {
        let t = self.clock.now_ms() + ms;
        self.world.borrow_mut().advance_to(t);
    }
}

/// Takes the round lease and a slot as `pass` would, then runs the runner on them.
pub fn drive<W: Advance>(
    p: &Ports,
    spec: &RunPassSpec,
    world: &RefCell<W>,
    clock: &VirtualClock,
    epoch_ms: u64,
    factor: f64,
) -> Result<(RunOutcome, Vec<String>), String> {
    let res = Resource::round(&spec.repo, &spec.batch);
    let lease = acquire(p, &res, crate::orphans::HOLDER)
        .map_err(|e| format!("run_pass {}: the lease was not taken: {e:?}", spec.batch))?;
    let pool = Pool::from_word(&spec.pool).unwrap_or(Pool::Proxmox);
    let slot = grant(p, &[pool], crate::orphans::HOLDER)
        .map_err(|e| format!("run_pass {}: {e}", spec.batch))?;
    let a = RunArgs {
        repo: spec.repo.clone(),
        batch: spec.batch.clone(),
        lease: lease.token(),
        slot: slot.slot(),
        slot_token: slot.token(),
    };
    let sleep = WorldSleep {
        world,
        clock: clock.clone(),
    };
    let switches = FakeSwitches {
        halt_at_ms: spec.halt_secs.map(|s| epoch_ms + s * 1000),
        clock: clock.clone(),
        world: p.stops,
    };
    let p = &Ports {
        stops: &switches,
        installed: &crate::lane::NoRounds,
        ..*p
    };
    let env = RunEnv {
        sleep: &sleep,
        poll_ms: spec.poll_secs * 1000,
        teardown_ms: spec.teardown_secs * 1000,
        kill_grace_ms: run::KILL_GRACE_MS,
    };
    let mut subs = FakeSubs::new(spec, clock.clone(), &sleep, factor);
    let end = run::run(p, &env, &mut subs, &a);
    Ok((
        RunOutcome {
            end,
            launches: subs.launches.clone(),
        },
        subs.faults,
    ))
}
