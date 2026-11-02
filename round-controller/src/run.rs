//! `round-controller run`: one pass and the Judge's sub-passes under the round lease and the slot
//! `pass` handed over. Both are renewed by token only while the child's progress marks are inside
//! the phase deadline and the pass inside its wall. A runner that loses its lease kills its VM
//! first and gives the slot back only once the child is gone; every write it makes is fenced on
//! the lease, so a lost runner records nothing. It never lands.

use std::cell::Cell;
use std::collections::BTreeMap;

use batcher::attrib::{Budget, Job, JobResult};
use batcher::core::Member as RoundMember;
use batcher::drive::{MainKind, MainPoll, RoundRunner};
use state_store::{Error, Fence, Item, Rev, Selector, Txn, TxnResult};

use crate::death::unit_name;
use crate::judge::{self, Judged, PassResult, Progress, Round};
use crate::lease::{fenced_txn, Lease, LeaseError, Resource};
use crate::liveness::Phase;
use crate::outcome::Outcome;
use crate::ports::{
    Attempt, CertifyEnd, Child, Class, InitiatedBy, LaneFault, Ledger, Mark, Member, PassIdentity,
    Ports, Signal, Store, Verb, VerbRefusal,
};
use crate::probe::Sleep;
use crate::resources::RoundView;
use crate::slots::{Pool, Slot, SlotGrant};
use crate::stack;

/// The longest a child gets to tear its VM down after TERM before it is killed: round-vm's own
/// bound for destroying its VM.
pub const TEARDOWN_MS: u64 = 300_000;
/// How long a killed child's group gets to be reaped before the runner gives up on it.
pub const KILL_GRACE_MS: u64 = 10_000;
pub const POLL_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// A renewal was refused: the lease lapsed or was taken.
    Expired,
    /// The runner stopped renewing: progress stalled past its phase deadline, or the pass ran
    /// past its wall.
    Deadline,
    /// The renewal was refused because the round turned terminal.
    Superseded,
}

impl Lost {
    pub fn word(self) -> &'static str {
        match self {
            Lost::Expired => "expired",
            Lost::Deadline => "deadline",
            Lost::Superseded => "superseded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEnd {
    Judged(Judged),
    /// No attempt. `gone` says whether the child was shown gone, so the slot was given back.
    LeaseLost {
        reason: Lost,
        gone: bool,
    },
    /// An off switch is set: the pass in hand is recorded, nothing new started, the VM gone,
    /// the locks given back, the round left non-terminal.
    Stopped {
        why: String,
    },
    /// The runner could not start its pass.
    Refused {
        why: String,
    },
}

/// `run`'s exits, as `--help` lists them.
pub const EXITS: &str = "exits of run:
  0  the pass was judged and its attempt recorded
  3  held: the attempt is recorded and the round waits on a refused eject
  4  unfinished: no attempt yet; the progress so far stands for the re-drive
  5  preempted: the verb that stopped the pass records it
  6  stopped: an off switch is set; the pass in hand is recorded, nothing new started
  7  lease lost: a renewal was refused; the VM torn down, no attempt
  8  lease lost: progress stalled past its phase deadline or the pass ran past its wall
  9  lease lost: the round was superseded under the runner
  10 refused: the runner could not start its pass";

impl RunEnd {
    pub fn code(&self) -> i32 {
        match self {
            RunEnd::Judged(Judged::Recorded { .. }) => 0,
            RunEnd::Judged(Judged::Held { .. }) => 3,
            RunEnd::Judged(Judged::Unfinished { .. }) => 4,
            RunEnd::Judged(Judged::Preempted) => 5,
            RunEnd::Stopped { .. } => 6,
            RunEnd::LeaseLost { reason, .. } => match reason {
                Lost::Expired => 7,
                Lost::Deadline => 8,
                Lost::Superseded => 9,
            },
            RunEnd::Refused { .. } => 10,
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            RunEnd::Judged(j) => j.word(),
            RunEnd::LeaseLost { .. } => "lease-lost",
            RunEnd::Stopped { .. } => "stopped",
            RunEnd::Refused { .. } => "refused",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunArgs {
    pub repo: String,
    pub batch: String,
    pub lease: Rev,
    pub slot: Slot,
    pub slot_token: Rev,
}

pub use crate::lease::token_word;

pub fn token_of_word(w: &str) -> Option<Rev> {
    let (main, sub) = w.split_once('-')?;
    Some(Rev::new(main.parse().ok()?, sub.parse().ok()?))
}

fn slot_of_word(w: &str) -> Option<Slot> {
    let (pool, n) = w.split_once('/')?;
    Some(Slot {
        pool: Pool::from_word(pool)?,
        n: n.parse().ok()?,
    })
}

/// `--repo <r> --batch <b> --lease-token <t> --slot <pool>/<n> --slot-token <s>`, each once.
pub fn parse_args(args: &[&str]) -> Result<RunArgs, String> {
    let mut got: BTreeMap<&str, &str> = BTreeMap::new();
    let mut it = args.iter();
    while let Some(&flag) = it.next() {
        let key = match flag {
            k @ ("--repo" | "--batch" | "--lease-token" | "--slot" | "--slot-token") => k,
            other => return Err(format!("unknown argument {other:?}")),
        };
        let v = it.next().ok_or_else(|| format!("{key} needs a value"))?;
        if got.insert(key, v).is_some() {
            return Err(format!("{key} is given twice"));
        }
    }
    let need = |k: &str| {
        got.get(k)
            .copied()
            .ok_or_else(|| format!("{k} is required"))
    };
    let token = |k: &str| {
        need(k)
            .and_then(|v| token_of_word(v).ok_or_else(|| format!("{k} {v:?} is not <main>-<sub>")))
    };
    let repo = need("--repo")?.to_string();
    let batch = need("--batch")?.to_string();
    let lease = token("--lease-token")?;
    let slot = need("--slot")?;
    Ok(RunArgs {
        repo,
        batch,
        lease,
        slot: slot_of_word(slot).ok_or_else(|| format!("--slot {slot:?} is not <pool>/<n>"))?,
        slot_token: token("--slot-token")?,
    })
}

/// The VM that serves the Judge's sub-passes, and the runner it answers through.
pub trait SubPasses: RoundRunner {
    /// The child serving the sub-passes, once one is up.
    fn vm(&self) -> Option<&dyn Child>;
    /// Tells it the round is done with the VM.
    fn close(&mut self);
}

pub struct RunEnv<'a> {
    pub sleep: &'a dyn Sleep,
    pub poll_ms: u64,
    pub teardown_ms: u64,
    pub kill_grace_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Killed {
    /// It tore down within its deadline after TERM.
    Term,
    /// It was killed after the deadline and is gone.
    Kill,
    /// It is still not gone after KILL: its slot is not given back.
    Lingering,
}

/// Terminates `child`'s process group, waits up to `teardown_ms` for it to tear its VM down,
/// then kills the group and waits up to `kill_grace_ms` for it to be gone.
/// `keep` runs on every poll: the slot is kept until the child is gone.
pub fn kill_child(p: &Ports, env: &RunEnv, child: &dyn Child, keep: &mut dyn FnMut()) -> Killed {
    if child.gone() {
        return Killed::Term;
    }
    let _ = child.signal(Signal::Term);
    if wait_gone(p, env, child, env.teardown_ms, keep) {
        return Killed::Term;
    }
    let _ = child.signal(Signal::Kill);
    if wait_gone(p, env, child, env.kill_grace_ms, keep) {
        return Killed::Kill;
    }
    Killed::Lingering
}

fn wait_gone(
    p: &Ports,
    env: &RunEnv,
    child: &dyn Child,
    within_ms: u64,
    keep: &mut dyn FnMut(),
) -> bool {
    let end = p.clock.now_ms().saturating_add(within_ms);
    loop {
        if child.gone() {
            return true;
        }
        keep();
        let now = p.clock.now_ms();
        if now >= end {
            return false;
        }
        env.sleep.sleep_ms(env.poll_ms.min(end - now).max(1));
    }
}

/// round-vm's phase word, as the deadline it runs under.
pub fn phase_of(word: &str) -> Phase {
    match word {
        "vm" => Phase::Boot,
        "build" | "unit-build" => Phase::Build,
        "done" => Phase::Certify,
        _ => Phase::Suites,
    }
}

/// The setup phase a red stopped in, from round-vm's phases: still leasing and booting the VM
/// is image setup; the container install and build before any suite is install.
pub fn setup_phase(phases: &[String]) -> Option<&'static str> {
    match phases
        .iter()
        .rfind(|p| !matches!(p.as_str(), "done" | "salvage"))
        .map(String::as_str)
    {
        Some("vm") => Some(judge::IMAGE_SETUP),
        Some("build") => Some(judge::INSTALL),
        _ => None,
    }
}

/// certify's `fault=` reason as the Judge reads it: over-cap once or twice is over-cap.
pub fn fault_reason(field: &str) -> String {
    match field.split_whitespace().next().unwrap_or("") {
        "capacity" => judge::OVER_CAP.into(),
        w => w.into(),
    }
}

/// certify answered a pass rather than refusing to run one: green, a red naming its steps, a
/// fault, a preempt or a lost VM. Any other exit (a refused or unknown argument, a round already
/// being certified) judged nothing, so it records no attempt and backs no identity off.
pub fn is_verdict(end: &CertifyEnd) -> bool {
    match end.exit {
        0 | judge::FAULT | judge::PREEMPTED | judge::VM_LOST => true,
        1 => end
            .fields
            .get("red")
            .is_some_and(|r| r.split(',').any(|s| !s.is_empty())),
        _ => false,
    }
}

/// What the Judge is handed for a finished pass.
pub fn pass_result(end: &CertifyEnd, phases: &[String]) -> PassResult {
    let red: Vec<String> = end
        .fields
        .get("red")
        .map(|r| {
            r.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    PassResult {
        exit: end.exit,
        phase: (!red.is_empty())
            .then(|| setup_phase(phases))
            .flatten()
            .map(str::to_string),
        red,
        reason: end.fields.get("fault").map(|f| fault_reason(f)),
        output: end.output.clone(),
    }
}

/// The lease and slot the runner holds, renewed together, and the VM it has recorded.
struct Keeper<'a, 'p> {
    p: &'a Ports<'p>,
    a: &'a RunArgs,
    lease: &'a Lease,
    slot: &'a mut SlotGrant,
    unit: String,
    started: u64,
    next_renew: u64,
    /// The last renewal of both that held.
    renewed: u64,
    vm: Option<String>,
}

impl Keeper<'_, '_> {
    /// Renews both when one is due, if progress allows: `last_mark` is when the child last
    /// showed progress in `phase`.
    fn tick(&mut self, last_mark: u64, phase: Phase) -> Result<(), Lost> {
        let now = self.p.clock.now_ms();
        if now < self.next_renew {
            return Ok(());
        }
        self.next_renew = now + self.p.leases.renew_every_ms();
        if !self
            .p
            .leases
            .gate()
            .may_renew(now, last_mark, phase, self.started)
        {
            return Err(Lost::Deadline);
        }
        // A supersede revokes the lease only once the queue verb does; the round's own state
        // says so first.
        if self.lost_why() == Lost::Superseded {
            return Err(Lost::Superseded);
        }
        match (self.lease.renew(self.p), self.slot.renew(self.p)) {
            (Ok(()), Ok(())) => {
                self.renewed = now;
                Ok(())
            }
            (Err(LeaseError::Lost), _) | (_, Err(LeaseError::Lost)) => Err(self.lost_why()),
            // The store did not answer: the lease holds until its TTL from the last renewal, so
            // the next interval tries again while one is left.
            _ if now.saturating_sub(self.renewed) + self.p.leases.renew_every_ms()
                < self.p.leases.ttl_ms =>
            {
                Ok(())
            }
            _ => Err(self.lost_why()),
        }
    }

    /// Renews the slot alone when due, whatever the lease: a runner keeps its slot until its
    /// VM is gone.
    fn keep_slot(&mut self) {
        let now = self.p.clock.now_ms();
        if now >= self.next_renew {
            self.next_renew = now + self.p.leases.renew_every_ms();
            let _ = self.slot.renew(self.p);
        }
    }

    fn lost_why(&self) -> Lost {
        lost_why(self.p, self.a)
    }

    fn note_vm(&mut self, vm: Option<&str>) {
        if let Some(v) = vm.filter(|v| Some(*v) != self.vm.as_deref()) {
            if self.slot.record_holder(self.p, &self.unit, Some(v)).is_ok() {
                self.vm = Some(v.to_string());
            }
        }
    }
}

/// The store a lease holder writes through: every txn is fenced on the lease, and once the
/// runner has lost its lease every write is refused, whatever the lock still says.
struct Fenced<'a> {
    inner: &'a dyn Store,
    clock: &'a dyn runtime::ports::Clock,
    fence: Fence,
    lost: &'a Cell<Option<Lost>>,
}

impl Fenced<'_> {
    fn refused(&self) -> Error {
        Error::LockLost {
            resource: self.fence.resource.clone(),
        }
    }

    fn holds(&self, now: u64) -> bool {
        self.inner
            .get(&self.fence.key())
            .ok()
            .flatten()
            .is_some_and(|i| {
                i.create_rev == self.fence.token && i.lock_expires_at_ms.is_some_and(|e| e > now)
            })
    }
}

impl Store for Fenced<'_> {
    fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
        self.inner.get(key)
    }
    fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
        self.inner.list(prefixes, sel)
    }
    fn txn(&self, t: &Txn, req: &state_store::Request) -> state_store::Result<TxnResult> {
        if self.lost.get().is_some() {
            return Err(self.refused());
        }
        let r = self.inner.txn(&t.clone().lock_check(&self.fence), req)?;
        if !r.succeeded && !self.holds(self.clock.now_ms()) {
            self.lost.set(Some(Lost::Expired));
            return Err(self.refused());
        }
        Ok(r)
    }
    fn lock(
        &self,
        resource: &str,
        ttl_ms: u64,
        req: &state_store::Request,
    ) -> state_store::Result<Fence> {
        self.inner.lock(resource, ttl_ms, req)
    }
    fn renew(&self, f: &Fence, req: &state_store::Request) -> state_store::Result<()> {
        self.inner.renew(f, req)
    }
    fn release(&self, f: &Fence, req: &state_store::Request) -> state_store::Result<()> {
        self.inner.release(f, req)
    }
}

/// The ledger as a lease holder writes it: nothing once the lease is lost.
struct Guarded<'a> {
    inner: &'a dyn Ledger,
    lost: &'a Cell<Option<Lost>>,
}

impl Guarded<'_> {
    fn open(&self) -> Result<(), String> {
        match self.lost.get() {
            Some(l) => Err(format!(
                "the runner's lease is lost ({}): nothing is recorded",
                l.word()
            )),
            None => Ok(()),
        }
    }
}

impl Ledger for Guarded<'_> {
    fn held(&self, p: &Ports, id: &PassIdentity) -> Result<Option<Class>, String> {
        self.inner.held(p, id)
    }
    fn record_attempt(
        &self,
        p: &Ports,
        id: &PassIdentity,
        batch: &str,
        a: &Attempt,
    ) -> Result<u32, String> {
        self.open()?;
        self.inner.record_attempt(p, id, batch, a)
    }
    fn mark_proven(
        &self,
        p: &Ports,
        id: &PassIdentity,
        proven: &[Member],
        at: u64,
    ) -> Result<(), String> {
        self.open()?;
        self.inner.mark_proven(p, id, proven, at)
    }
    fn record_lane_fault(
        &self,
        p: &Ports,
        repo: &str,
        base: &str,
        image_digest: &str,
        f: &LaneFault,
    ) -> Result<(), String> {
        self.open()?;
        self.inner.record_lane_fault(p, repo, base, image_digest, f)
    }
    fn green_digests(&self, p: &Ports, repo: &str, base: &str) -> Result<Vec<String>, String> {
        self.inner.green_digests(p, repo, base)
    }
}

/// The sub-pass runner as the Judge drives it: each wait renews the leases under the sub-pass
/// deadline, each launch first checks the off switches, and once the lease is lost the VM is
/// killed and every pending sub-pass answers a fault, which the fenced store refuses to record.
struct Guard<'g, 'a, 'p, R: SubPasses> {
    keeper: &'g mut Keeper<'a, 'p>,
    env: &'g RunEnv<'g>,
    inner: &'g mut R,
    lost: &'g Cell<Option<Lost>>,
    stopped: Option<String>,
    pending: Vec<u64>,
    last_progress: u64,
    killed: Option<Killed>,
}

impl<R: SubPasses> Guard<'_, '_, '_, R> {
    fn lose(&mut self, l: Lost) {
        self.lost.set(Some(l));
        let keeper = &mut *self.keeper;
        self.killed = Some(match self.inner.vm() {
            Some(c) => kill_child(keeper.p, self.env, c, &mut || keeper.keep_slot()),
            None => Killed::Term,
        });
    }
}

impl<R: SubPasses> RoundRunner for Guard<'_, '_, '_, R> {
    fn set_members(&mut self, members: &[RoundMember]) {
        self.inner.set_members(members)
    }
    fn set_base(&mut self, sha: &str) {
        self.inner.set_base(sha)
    }
    fn start_main(&mut self, kind: MainKind, suites: &[String]) -> Result<(), String> {
        self.inner.start_main(kind, suites)
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        self.inner.poll_main()
    }
    fn stop_main(&mut self) -> Result<(), String> {
        self.inner.stop_main()
    }
    fn launch(&mut self, job: &Job) -> Result<(), String> {
        if let Some(l) = self.lost.get() {
            return Err(format!("the lease is lost ({})", l.word()));
        }
        let repo = &self.keeper.a.repo;
        if let Some(why) = crate::switches::check(self.keeper.p, repo).map(|s| s.why(repo)) {
            self.stopped = Some(why.clone());
            return Err(format!("no sub-pass starts: {why}"));
        }
        self.inner.launch(job)?;
        self.pending.push(job.id);
        self.last_progress = self.keeper.p.clock.now_ms();
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        if self.lost.get().is_some() {
            return self
                .pending
                .drain(..)
                .map(|id| (id, JobResult::Fault))
                .collect();
        }
        let got = self.inner.poll_jobs();
        if !got.is_empty() {
            self.pending.retain(|id| !got.iter().any(|(g, _)| g == id));
            self.last_progress = self.keeper.p.clock.now_ms();
        }
        got
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
    fn wait(&mut self) {
        if self.lost.get().is_some() {
            return;
        }
        // A sub-pass's progress is its launch and its answer: the run directory's progress file
        // is shared with every other pass and cannot vouch for this one.
        if let Err(l) = self.keeper.tick(self.last_progress, Phase::SubPass) {
            self.lose(l);
            return;
        }
        self.inner.wait()
    }
}

/// Why a renewal was refused: the round is gone or terminal, or the lease lapsed.
fn lost_why(p: &Ports, a: &RunArgs) -> Lost {
    match RoundView::read(p.store, &a.repo, &a.batch) {
        Ok(None) => Lost::Superseded,
        Ok(Some(v)) if v.round.row.state.is_terminal() => Lost::Superseded,
        _ => Lost::Expired,
    }
}

fn progress_key(a: &RunArgs) -> String {
    format!("batches/{}/{}/progress", a.repo, a.batch)
}

fn read_progress(p: &Ports, a: &RunArgs) -> Result<Option<Progress>, String> {
    p.store
        .get(&progress_key(a))
        .map_err(|e| format!("progress: {e}"))?
        .map(|i| serde_json::from_value(i.value.data).map_err(|e| format!("progress: {e}")))
        .transpose()
}

fn request(p: &Ports, what: &str) -> state_store::Request {
    state_store::Request::new(
        &format!("rc-run-{what}-{:016x}", p.next_u64()),
        crate::orphans::HOLDER,
        &format!("run.{what}"),
    )
}

/// The round the Judge is handed: the record's members and tested base, on the lane's image,
/// harness and selection. One VM serves the sub-passes, one job at a time.
fn round_of(view: &RoundView, lane: &stack::Lane, a: &RunArgs) -> Round {
    let r = &view.round;
    Round {
        repo: a.repo.clone(),
        batch: a.batch.clone(),
        pass: r.row.pass,
        members: r
            .members
            .iter()
            .map(|m| RoundMember {
                id: m.id.clone(),
                tip: m.tip.clone(),
                title: String::new(),
                priority: None,
                express: false,
                base_fix: false,
                certified_at: 0,
                stack: BTreeMap::new(),
                blocked_by: vec![],
            })
            .collect(),
        base: r
            .tested_base
            .clone()
            .or_else(|| r.row.base.clone())
            .unwrap_or_default(),
        image_digest: lane.image_digest.clone(),
        harness_version: lane.harness_version.clone(),
        selection: lane.selection.clone(),
        initiated_by: InitiatedBy::Planner,
        budget: Budget {
            slots: 1,
            maxpar: 1,
        },
        changed: BTreeMap::new(),
    }
}

fn give_back(p: &Ports, slot: SlotGrant, lease: Lease) {
    let _ = slot.release(p);
    let _ = lease.release(p);
}

fn log(p: &Ports, a: &RunArgs, verb: Verb, r: &Result<(), VerbRefusal>) {
    p.log.write(&Outcome::of(
        p.clock.now_ms(),
        &a.repo,
        Some(&a.batch),
        verb,
        r,
    ));
}

/// One pass of `a.batch` and its Judge, under the lease and slot `pass` handed over. Both are
/// renewed first, so a runner launched on a lease that already lapsed starts nothing.
pub fn run<R: SubPasses>(p: &Ports, env: &RunEnv, subs: &mut R, a: &RunArgs) -> RunEnd {
    let lease = Lease::handed(Resource::round(&a.repo, &a.batch), a.lease);
    let mut slot = SlotGrant::handed(p, a.slot, a.slot_token);
    if lease.renew(p).is_err() || slot.renew(p).is_err() {
        let reason = lost_why(p, a);
        let _ = slot.release(p);
        let _ = lease.release(p);
        return RunEnd::LeaseLost { reason, gone: true };
    }
    let refuse = |slot: SlotGrant, lease: Lease, why: String| {
        give_back(p, slot, lease);
        RunEnd::Refused { why }
    };
    let view = match RoundView::read(p.store, &a.repo, &a.batch) {
        Ok(Some(v)) if !v.round.row.state.is_terminal() => v,
        Ok(Some(v)) => {
            return refuse(
                slot,
                lease,
                format!("round {} is {:?}", a.batch, v.round.row.state),
            )
        }
        Ok(None) => return refuse(slot, lease, format!("no round {}", a.batch)),
        Err(e) => return refuse(slot, lease, format!("round {}: {e}", a.batch)),
    };
    let lane = match stack::lane(p, &a.repo) {
        Ok(Some(l)) => l,
        Ok(None) => return refuse(slot, lease, format!("no lane record for {}", a.repo)),
        Err(e) => return refuse(slot, lease, format!("lane of {}: {e:?}", a.repo)),
    };
    let resumed = match read_progress(p, a) {
        Ok(g) => g
            .filter(|g| g.pass == view.round.row.pass)
            .and_then(|g| g.main),
        Err(e) => return refuse(slot, lease, e),
    };
    if resumed.is_none() {
        if let Some(why) = crate::switches::check(p, &a.repo).map(|s| s.why(&a.repo)) {
            give_back(p, slot, lease);
            return RunEnd::Stopped { why };
        }
    }
    let unit = match unit_name(&a.batch, a.lease) {
        Ok(u) => u,
        Err(why) => return refuse(slot, lease, why),
    };
    if let Err(e) = slot.record_holder(p, &unit, None) {
        return lost_before(p, slot, lease, &e);
    }
    let started = p.clock.now_ms();
    let lost: Cell<Option<Lost>> = Cell::new(None);
    let mut keeper = Keeper {
        p,
        a,
        lease: &lease,
        slot: &mut slot,
        unit,
        started,
        next_renew: started + p.leases.renew_every_ms(),
        renewed: started,
        vm: None,
    };
    let (pass_result, round) = match resumed {
        Some(r) => (r, round_of(&view, &lane, a)),
        None => match main_pass(&mut keeper, env, &lane, a) {
            Ok(got) => got,
            Err(Ended::Refused(why)) => {
                drop(keeper);
                return refuse(slot, lease, why);
            }
            Err(Ended::Lost(reason, killed)) => {
                drop(keeper);
                return lost_after(p, slot, lease, reason, killed);
            }
        },
    };
    let fenced = Fenced {
        inner: p.store,
        clock: p.clock,
        fence: lease.fence().clone(),
        lost: &lost,
    };
    let guarded = Guarded {
        inner: p.ledger,
        lost: &lost,
    };
    let rng = std::sync::Mutex::new(runtime::ports::SeededRng::new(p.next_u64()));
    let jp = Ports {
        clock: p.clock,
        proc_: p.proc_,
        rng: &rng,
        store: &fenced,
        verbs: p.verbs,
        ledger: &guarded,
        log: p.log,
        units: p.units,
        vms: p.vms,
        launcher: p.launcher,
        git: p.git,
        leases: p.leases,
        slots: p.slots,
        lc: p.lc,
        stops: p.stops,
    };
    let mut guard = Guard {
        last_progress: p.clock.now_ms(),
        keeper: &mut keeper,
        env,
        inner: subs,
        lost: &lost,
        stopped: None,
        pending: vec![],
        killed: None,
    };
    let judged = judge::judge(&jp, &mut guard, &round, &pass_result);
    let (stopped, killed) = (guard.stopped.take(), guard.killed);
    drop(guard);
    if let Some(reason) = lost.get() {
        // A lease the fenced store found lost was not lost in a renewal, so nothing killed yet.
        let killed = killed.unwrap_or_else(|| match subs.vm() {
            Some(c) => kill_child(p, env, c, &mut || keeper.keep_slot()),
            None => Killed::Term,
        });
        drop(keeper);
        return lost_after(p, slot, lease, reason, killed);
    }
    if matches!(judged, Judged::Recorded { .. } | Judged::Preempted) {
        let t = fenced_txn(
            &lease,
            vec![],
            vec![state_store::Op::Delete(progress_key(a))],
        );
        let _ = p.store.txn(&t, &request(p, "clear"));
    }
    let gone = if subs.vm().is_some() {
        subs.close();
        subs.vm().is_none_or(|c| {
            let keep = &mut || keeper.keep_slot();
            wait_gone(p, env, c, env.teardown_ms, keep)
                || kill_child(p, env, c, keep) != Killed::Lingering
        })
    } else {
        true
    };
    drop(keeper);
    if gone {
        give_back(p, slot, lease);
    } else {
        let _ = lease.release(p);
    }
    match (stopped, &judged) {
        (Some(why), Judged::Unfinished { .. }) => RunEnd::Stopped { why },
        _ => RunEnd::Judged(judged),
    }
}

enum Ended {
    Refused(String),
    Lost(Lost, Killed),
}

/// pass-start, then certify under the lease until it answers, renewing as progress allows; the
/// finished pass is saved to progress before the Judge sees it.
fn main_pass(
    k: &mut Keeper,
    env: &RunEnv,
    lane: &stack::Lane,
    a: &RunArgs,
) -> Result<(PassResult, Round), Ended> {
    let p = k.p;
    let token = token_word(a.lease);
    let started = p.verbs.pass_start(&a.repo, &a.batch).map(|_| ());
    log(p, a, Verb::PassStart, &started);
    started.map_err(|r| Ended::Refused(format!("pass-start: {}", r.text)))?;
    let view = match RoundView::read(p.store, &a.repo, &a.batch) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Err(Ended::Refused(format!(
                "round {} went under pass-start",
                a.batch
            )))
        }
        Err(e) => return Err(Ended::Refused(format!("round {}: {e}", a.batch))),
    };
    let child = p
        .verbs
        .certify(&a.repo, &a.batch, Some(&token))
        .map_err(|r| Ended::Refused(format!("certify: {}", r.text)))?;
    let since = p.clock.now_ms();
    let head = view.round.row.head.clone().unwrap_or_default();
    // The progress file is the run directory's: a mark is this pass's only when it names this
    // round's tree and is no older than the certify that runs it.
    let ours = |m: &Mark| (m.round == head || m.round == a.batch) && m.at_ms + 1_000 >= since;
    let mut phases: Vec<String> = vec![];
    let end = loop {
        let mark = child.mark().filter(|m| ours(m));
        if let Some(m) = &mark {
            k.note_vm(m.vm.as_deref());
            phases = m.phases.clone();
        }
        if let Some(end) = child.poll() {
            break end;
        }
        let (at, phase) = mark.map_or((k.started, Phase::Boot), |m| (m.at_ms, phase_of(&m.phase)));
        if let Err(l) = k.tick(at, phase) {
            let killed = kill_child(p, env, &*child, &mut || k.keep_slot());
            return Err(Ended::Lost(l, killed));
        }
        env.sleep.sleep_ms(env.poll_ms);
    };
    if let Some(m) = child.mark().filter(|m| ours(m)) {
        phases = m.phases;
    }
    let result = match end {
        Ok(e) if is_verdict(&e) => pass_result(&e, &phases),
        Ok(e) => {
            return Err(Ended::Refused(format!(
                "certify exited {} with no verdict: {}",
                e.exit, e.output
            )))
        }
        Err(r) => return Err(Ended::Refused(format!("certify: {}", r.text))),
    };
    let progress = Progress {
        pass: view.round.row.pass,
        main: Some(result.clone()),
        ..Progress::default()
    };
    let data = serde_json::to_value(&progress).unwrap_or_default();
    let t = fenced_txn(
        k.lease,
        vec![],
        vec![state_store::Op::Put(
            progress_key(a),
            state_store::Object::new(data),
        )],
    );
    match p.store.txn(&t, &request(p, "main")) {
        Ok(r) if r.succeeded => {}
        _ => return Err(Ended::Lost(k.lost_why(), Killed::Term)),
    }
    Ok((result, round_of(&view, lane, a)))
}

/// The slot was lost before anything started: nothing to kill.
fn lost_before(p: &Ports, slot: SlotGrant, lease: Lease, e: &LeaseError) -> RunEnd {
    drop(slot);
    let _ = lease.release(p);
    RunEnd::LeaseLost {
        reason: match e {
            LeaseError::Terminal => Lost::Superseded,
            _ => Lost::Expired,
        },
        gone: true,
    }
}

/// The child is killed: the slot goes back only once it is gone.
fn lost_after(p: &Ports, slot: SlotGrant, lease: Lease, reason: Lost, killed: Killed) -> RunEnd {
    let gone = killed != Killed::Lingering;
    if gone {
        let _ = slot.release(p);
    }
    let _ = lease.release(p);
    RunEnd::LeaseLost { reason, gone }
}

#[cfg(test)]
mod tests;
