//! The wall-clock host: one tokio loop that owns the engine only between turns. A turn, and
//! every effect, runs on the offload pool; the loop awaits results, kicks and keeps time.
//! Work the pool refuses stays undone in the journal and is re-offered with a growing backoff.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};

use crate::engine::{Engine, Executor, Outcome, PendingEffect};
use crate::event::{EffectSpec, EventKind};
use crate::offload::{self, Offloaded, Pool, Spill};
use crate::pools::{ClassPools, Deferred, EffectClass, PoolCaps};
use crate::ports::{Clock, VirtualClock};
use crate::sources::{self, Source, Sources};
use crate::wide::{Fields, WideSink};
use crate::{Result, RuntimeError};

/// A turn held longer than this leaves an overrun record.
pub const TURN_BUDGET: Duration = Duration::from_secs(5);
const BACKOFF_FIRST: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(2);
const JOURNAL_BACKOFF_MAX: Duration = Duration::from_secs(10);

static JOURNAL_RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub trait EffectRunner: Send + Sync {
    /// Must be idempotent under `key`: a restart re-issues every effect that has no outcome.
    fn run(&self, key: &str, spec: &EffectSpec) -> std::result::Result<String, String>;

    /// [`run`](Self::run) for the effect `task` asked for; a runner that names its store calls
    /// by task overrides this.
    fn run_for_task(&self, _task: &str, key: &str, spec: &EffectSpec) -> std::result::Result<String, String> {
        self.run(key, spec)
    }

    /// The effector class whose cap `spec` runs under.
    fn class(&self, _spec: &EffectSpec) -> EffectClass {
        EffectClass::Host
    }
}

/// An event from outside the engine. `reply`, when set, hears `Ok` once the turn that handled the
/// event committed, `Err` when the engine or the store refused it. A dropped reply means the host
/// stopped before then.
pub struct Submit {
    pub id: String,
    pub stream: String,
    pub kind: EventKind,
    pub reply: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

impl Submit {
    pub fn new(id: &str, stream: &str, kind: EventKind) -> Self {
        Submit { id: id.into(), stream: stream.into(), kind, reply: None }
    }

    /// The submission and the receiver that hears whether it was accepted.
    pub fn with_reply(id: &str, stream: &str, kind: EventKind) -> (Self, oneshot::Receiver<std::result::Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        (Submit { reply: Some(tx), ..Submit::new(id, stream, kind) }, rx)
    }
}

impl std::fmt::Debug for Submit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Submit").field("id", &self.id).field("stream", &self.stream).field("kind", &self.kind).field("reply", &self.reply.is_some()).finish()
    }
}

/// Queues `s`; its reply is answered when the turn handling it has committed, or at once if the store refuses it. A refusal is absorbed; a fault is returned, since it may clear on restart.
fn accept(engine: &mut Engine, s: Submit) -> Result<()> {
    // loop-safe: accept runs on the offload pool (Host) or under virtual time (VirtualDriver), never on the loop thread
    let r = engine.submit(&s.id, &s.stream, s.kind);
    if let Some(reply) = s.reply {
        match &r {
            Ok(()) => engine.answer_on_commit(&s.id, reply),
            Err(e) => {
                let _ = reply.send(Err(e.to_string()));
            }
        }
    }
    match r {
        Err(e) if e.is_refused() => Ok(()),
        r => r,
    }
}

#[derive(Default)]
pub struct ServeOpts {
    /// Called at the top of every wake, on the loop's own thread, unless a handler is parked.
    pub kick: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The longest the loop sleeps between wakes, so a kick is never later than this.
    pub max_sleep_ms: Option<u64>,
    /// Receives the loop's own records: long turns and offload spills. Must not block.
    pub sink: Option<Arc<dyn WideSink>>,
    /// The pool effects run on; the process-wide one when `None`. Engine turns never run
    /// here: each host gives its turns a worker of their own, so a pool full of long effects
    /// cannot hold the loop's turn.
    pub pool: Option<Arc<Pool>>,
    /// Called at the start of every turn, after the turn's submissions and outcomes are applied
    /// and before any handler runs: the one point where the engine is between ticks.
    pub before_turn: Option<TurnHook>,
}

/// See [`ServeOpts::before_turn`].
pub type TurnHook = Arc<dyn Fn(&mut Engine) + Send + Sync>;

enum Done {
    Started(String),
    Deferred(String, EffectClass),
    Outcome(String, Outcome),
    Spilled(String, Spill),
}

struct Turn {
    engine: Engine,
    subs: Vec<Submit>,
    done: Vec<Done>,
    in_flight: BTreeSet<String>,
    before: Option<TurnHook>,
}

struct TurnOut {
    engine: Engine,
    result: Result<()>,
    subs: Vec<Submit>,
    done: Vec<Done>,
    launch: Vec<PendingEffect>,
    next_timer_at: Option<u64>,
}

/// Applies the turn's submissions and outcomes in order. On an error what it had not applied stays
/// in `t`, so a turn that is retried loses nothing; a submission that failed was answered.
fn apply(t: &mut Turn) -> Result<()> {
    let mut subs = std::mem::take(&mut t.subs).into_iter();
    while let Some(s) = subs.next() {
        let again = s.reply.is_none().then(|| Submit::new(&s.id, &s.stream, s.kind.clone()));
        if let Err(e) = accept(&mut t.engine, s) {
            t.subs = again.filter(|_| e.is_cannot_tell()).into_iter().chain(subs).collect();
            return Err(e);
        }
    }
    let mut done = std::mem::take(&mut t.done).into_iter();
    while let Some(d) = done.next() {
        match d {
            Done::Started(key) => t.engine.note_attempt(&key),
            Done::Outcome(key, o) => {
                if let Err(e) = t.engine.complete(&key, o.clone()) { // loop-safe: runs on the offload pool
                    t.done = std::iter::once(Done::Outcome(key, o)).chain(done).collect();
                    return Err(e);
                }
            }
            Done::Deferred(key, class) => t.engine.note_deferred(&key, class.name()),
            Done::Spilled(..) => {}
        }
    }
    Ok(())
}

fn run_turn(mut t: Turn) -> TurnOut {
    let result = apply(&mut t).and_then(|()| {
        if let Some(before) = &t.before {
            before(&mut t.engine);
        }
        t.engine.run_until_idle() // loop-safe: runs on the offload pool
    });
    let launch: Vec<PendingEffect> = t.engine.pending_effects().into_iter().filter(|p| !t.in_flight.contains(&p.key)).collect();
    // The host now owns each of these until it starts: its deadline waits for the runner.
    for p in &launch {
        t.engine.note_queued(&p.key);
    }
    let next_timer_at = t.engine.next_timer_at();
    TurnOut { engine: t.engine, result, subs: t.subs, done: t.done, launch, next_timer_at }
}

/// Runs until `submissions` closes and no effect is in flight.
pub async fn serve(engine: Engine, clock: Arc<dyn Clock>, runner: Arc<dyn EffectRunner>, caps: impl Into<PoolCaps>, submissions: mpsc::Receiver<Submit>) -> Result<()> {
    serve_with(engine, clock, runner, caps, submissions, ServeOpts::default()).await
}

fn spill_record(s: &Spill) -> Fields {
    let mut f = Fields::new();
    f.insert("unit".into(), "offload.spill".into());
    f.insert("what".into(), s.what.into());
    f.insert("offload.depth".into(), s.depth.into());
    f.insert("offload.cap".into(), s.cap.into());
    f.insert("offload.spilled_total".into(), offload::spilled_total(s.what).into());
    f
}

struct Running {
    handle: Offloaded<TurnOut>,
    began: Instant,
    overrun: bool,
}

pub async fn serve_with(
    engine: Engine,
    clock: Arc<dyn Clock>,
    runner: Arc<dyn EffectRunner>,
    caps: impl Into<PoolCaps>,
    mut submissions: mpsc::Receiver<Submit>,
    opts: ServeOpts,
) -> Result<()> {
    let workers = opts.pool.clone().unwrap_or_else(Pool::global);
    // One turn runs at a time, so one worker and one slot are enough.
    let turns = Pool::new(1, 1);
    let engine = classified(engine, &runner);
    let busy = engine.busy();
    let max_sleep = opts.max_sleep_ms.map(Duration::from_millis);
    let pools = ClassPools::new(caps.into());
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Done>();
    // Each effect in flight, with the stream (the task) that asked for it.
    let mut in_flight: BTreeMap<String, String> = BTreeMap::new();
    let mut engine = Some(engine);
    let mut running: Option<Running> = None;
    let (mut subs, mut done): (Vec<Submit>, Vec<Done>) = (vec![], vec![]);
    let mut open = true;
    let mut dirty = true;
    let mut next_timer: Option<u64> = None;
    let (mut backoff, mut retry_at) = (BACKOFF_FIRST, Instant::now());
    let mut journal_backoff = BACKOFF_FIRST;
    let emit = |f: Fields| {
        if let Some(s) = &opts.sink {
            s.emit(&f); // loop-safe: try_send, drops when full
        }
    };
    let back_off = |backoff: &mut Duration, retry_at: &mut Instant| {
        *retry_at = Instant::now() + *backoff;
        *backoff = (*backoff * 2).min(BACKOFF_MAX);
    };
    let spilled = |s: &Spill, backoff: &mut Duration, retry_at: &mut Instant| {
        emit(spill_record(s));
        back_off(backoff, retry_at);
    };
    loop {
        if let Some(k) = &opts.kick {
            if max_sleep.is_none_or(|m| busy.held_for() <= m) {
                k();
            }
        }
        if running.is_none() && next_timer.is_some_and(|t| t <= clock.now_ms()) {
            dirty = true;
        }
        let wakes = |done: &[Done]| done.iter().any(|d| !matches!(d, Done::Deferred(..) | Done::Started(_)));
        if let Some(e) = engine.take_if(|_| running.is_none() && (dirty || !subs.is_empty() || wakes(&done)) && Instant::now() >= retry_at) {
            let turn = Turn { engine: e, subs: std::mem::take(&mut subs), done: std::mem::take(&mut done), in_flight: in_flight.keys().cloned().collect(), before: opts.before_turn.clone() };
            let slot = Arc::new(Mutex::new(Some(turn)));
            let s = slot.clone();
            let job = move || run_turn(s.lock().unwrap_or_else(|e| e.into_inner()).take().expect("the turn is put in the slot before it is offered")); // loop-safe: the slot is uncontended; the pool takes it after the offer
            match turns.offload("engine.turn", None, job) {
                Ok(handle) => {
                    backoff = BACKOFF_FIRST;
                    dirty = false;
                    running = Some(Running { handle, began: Instant::now(), overrun: false });
                }
                Err(sp) => {
                    let t = slot.lock().unwrap_or_else(|e| e.into_inner()).take().expect("a refused turn never ran"); // loop-safe: the job never ran, so nothing holds the slot
                    (engine, subs, done) = (Some(t.engine), t.subs, t.done);
                    spilled(&sp, &mut backoff, &mut retry_at);
                }
            }
        }
        if !open && in_flight.is_empty() && running.is_none() && subs.is_empty() && !wakes(&done) && !dirty {
            return Ok(());
        }
        let now = Instant::now();
        let mut wait = next_timer.filter(|_| running.is_none()).map_or(Duration::from_secs(3600), |t| Duration::from_millis(t.saturating_sub(clock.now_ms())));
        if let Some(m) = max_sleep {
            wait = wait.min(m);
        }
        if let Some(r) = &running {
            if !r.overrun {
                wait = wait.min((r.began + TURN_BUDGET).saturating_duration_since(now));
            }
        } else if dirty || !subs.is_empty() || wakes(&done) {
            wait = wait.min(retry_at.saturating_duration_since(now));
        }
        tokio::select! {
            s = submissions.recv(), if open => match s {
                Some(s) => subs.push(s),
                None => open = false,
            },
            Some(d) = done_rx.recv() => {
                match &d {
                    Done::Started(_) => backoff = BACKOFF_FIRST,
                    Done::Deferred(..) => {}
                    Done::Outcome(key, _) => {
                        in_flight.remove(key);
                    }
                    Done::Spilled(key, sp) => {
                        in_flight.remove(key);
                        spilled(sp, &mut backoff, &mut retry_at);
                    }
                }
                if !matches!(d, Done::Started(_)) {
                    dirty = true;
                }
                done.push(d);
            }
            res = async { (&mut running.as_mut().expect("guarded").handle).await }, if running.is_some() => {
                let r = running.take().expect("guarded");
                let out = res.map_err(|e| RuntimeError::new(format!("engine turn: {e}")))?;
                let mut f = Fields::new();
                if r.overrun {
                    f.insert("unit".into(), "loop.turn".into());
                    f.insert("outcome".into(), (if out.result.is_ok() { "ok" } else { "error" }).into());
                    f.insert("turn.elapsed_ms".into(), (r.began.elapsed().as_millis() as u64).into());
                    emit(f);
                }
                let retry = match out.result {
                    Err(e) if e.is_cannot_tell() => Some(e),
                    r => {
                        r?;
                        journal_backoff = BACKOFF_FIRST;
                        None
                    }
                };
                if let Some(e) = &retry {
                    subs.extend(out.subs);
                    done.extend(out.done);
                    dirty = true;
                    retry_at = Instant::now() + journal_backoff;
                    let mut f = Fields::new();
                    f.insert("unit".into(), "journal.retry".into());
                    f.insert("journal.retries_total".into(), (JOURNAL_RETRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1).into());
                    f.insert("journal.retry_in_ms".into(), (journal_backoff.as_millis() as u64).into());
                    f.insert("error".into(), e.0.as_str().into());
                    emit(f);
                    journal_backoff = (journal_backoff * 2).min(JOURNAL_BACKOFF_MAX);
                }
                for p in out.launch {
                    let class = runner.class(&p.spec);
                    let mine = in_flight.values().filter(|s| **s == p.stream.0).count();
                    if mine >= task_fan_out(pools.cap(class)) {
                        // Held back unrefused: one of the task's own outcomes wakes the next offer.
                        continue;
                    }
                    match pools.try_take(class) {
                        Ok(permit) => {
                            in_flight.insert(p.key.clone(), p.stream.0.clone());
                            launch_effect(&workers, permit, &runner, &done_tx, p);
                        }
                        Err(d) => {
                            emit(deferred_record(&p, &d, pools.deferred_total(d.class)));
                            done.push(Done::Deferred(p.key.clone(), d.class));
                        }
                    }
                }
                next_timer = out.next_timer_at;
                engine = Some(out.engine);
            }
            _ = tokio::time::sleep(wait) => {
                if let Some(r) = running.as_mut() {
                    if !r.overrun && r.began.elapsed() >= TURN_BUDGET {
                        r.overrun = true;
                        let mut f = Fields::new();
                        f.insert("unit".into(), "loop.turn".into());
                        f.insert("outcome".into(), "overrun".into());
                        f.insert("turn.elapsed_ms".into(), (r.began.elapsed().as_millis() as u64).into());
                        f.insert("handler.in_flight".into(), (busy.held_for() > Duration::ZERO).into());
                        emit(f);
                    }
                }
            }
        }
    }
}

/// The most effects one task may have in flight in a class of cap `cap`: half of it, at least
/// one, so a task fanning out one effect per item (sweep.archive stores one per transcript)
/// leaves the rest of the class to the others. The rest stay pending and are offered again as
/// the task's own effects finish.
pub fn task_fan_out(cap: usize) -> usize {
    (cap / 2).max(1)
}

/// The engine reports each effect under the class the runner gives it.
fn classified(engine: Engine, runner: &Arc<dyn EffectRunner>) -> Engine {
    let r = runner.clone();
    engine.with_classifier(Arc::new(move |spec: &EffectSpec| r.class(spec).name().to_string()))
}

/// A class at its cap refused `p`. It stays pending and is offered again when a slot frees: any outcome starts a turn.
fn deferred_record(p: &PendingEffect, d: &Deferred, total: u64) -> Fields {
    let mut f = Fields::new();
    f.insert("unit".into(), "effect.deferred".into());
    f.insert("task".into(), p.stream.0.as_str().into());
    f.insert("effect".into(), p.spec.name.as_str().into());
    f.insert("class".into(), d.class.name().into());
    f.insert("class.in_use".into(), d.in_use.into());
    f.insert("class.cap".into(), d.cap.into());
    f.insert("class.deferred_total".into(), total.into());
    f.insert("waits_for".into(), format!("{}.slot", d.class.name()).into());
    f
}

/// Runs `p` on `workers` under `permit`. Its deadline counts from the moment a worker starts the
/// runner, so time spent queued for a worker is not charged to it. The permit is given back when
/// the runner returns or when the deadline writes it off, whichever is first: a runner still
/// stuck past its deadline no longer holds its class's capacity.
fn launch_effect(workers: &Arc<Pool>, permit: OwnedSemaphorePermit, runner: &Arc<dyn EffectRunner>, tx: &mpsc::UnboundedSender<Done>, p: PendingEffect) {
    let (workers, runner, tx) = (workers.clone(), runner.clone(), tx.clone());
    let deadline = Duration::from_millis(p.spec.deadline_ms);
    let permit = Arc::new(Mutex::new(Some(permit)));
    tokio::spawn(async move {
        let outcome = async {
            let (r, k, spec, held, task) = (runner, p.key.clone(), p.spec.clone(), permit.clone(), p.task.clone());
            let (began_tx, began) = oneshot::channel::<()>();
            let job = move || {
                let _ = began_tx.send(());
                let out = r.run_for_task(&task, &k, &spec);
                drop(held.lock().unwrap_or_else(|e| e.into_inner()).take()); // loop-safe: runs on the offload pool; the lock is held only to take the permit
                out
            };
            let mut h = match workers.offload("effect", None, job) {
                Ok(h) => h,
                Err(sp) => {
                    drop(permit.lock().unwrap_or_else(|e| e.into_inner()).take()); // loop-safe: the job never ran, so nothing else holds the lock
                    let _ = tx.send(Done::Spilled(p.key.clone(), sp));
                    return None;
                }
            };
            // A job dropped unrun answers `h` with an error at once, so this wait always ends.
            let _ = began.await;
            let _ = tx.send(Done::Started(p.key.clone()));
            Some(match tokio::time::timeout(deadline, &mut h).await {
                Err(_) => {
                    drop(permit.lock().unwrap_or_else(|e| e.into_inner()).take()); // loop-safe: uncontended except for the instant the runner returns
                    Outcome::TimedOut
                }
                Ok(Ok(Ok(d))) => Outcome::Succeeded(d),
                Ok(Ok(Err(e))) => Outcome::Failed(e),
                Ok(Err(e)) => Outcome::Failed(format!("effect task: {e}")),
            })
        }
        .await;
        if let Some(outcome) = outcome {
            let _ = tx.send(Done::Outcome(p.key, outcome));
        }
    });
}

/// Which timeline the host runs on. `Wall` is the tokio loop above. `Virtual` jumps the clock
/// to the next timer or source time and answers every effect synchronously through the runner,
/// so the same registrations and the same runner (with doubles in it) run under either.
pub enum Driver {
    Wall(Arc<dyn Clock>),
    Virtual { clock: VirtualClock, until_ms: u64 },
}

/// Runs `engine` on `driver`. Under `Wall`, `sources` are started on their own threads and
/// submit beside `submissions`; it returns once both are closed and nothing is in flight.
/// Under `Virtual` it returns at `until_ms`, having taken whatever `submissions` held at each
/// step: virtual time does not wait for the wall.
pub async fn drive(driver: Driver, engine: Engine, runner: Arc<dyn EffectRunner>, caps: impl Into<PoolCaps>, mut submissions: mpsc::Receiver<Submit>, sources: Sources, opts: ServeOpts) -> Result<()> {
    match driver {
        Driver::Wall(clock) => {
            if sources.is_empty() {
                return serve_with(engine, clock, runner, caps, submissions, opts).await;
            }
            let (tx, rx) = mpsc::channel(SOURCE_QUEUE);
            let _threads = sources.start(tx.clone())?;
            tokio::spawn(async move {
                while let Some(s) = submissions.recv().await {
                    if tx.send(s).await.is_err() {
                        return;
                    }
                }
            });
            serve_with(engine, clock, runner, caps, rx, opts).await
        }
        Driver::Virtual { clock, until_ms } => {
            let mut v = VirtualDriver::new(engine, clock, runner).with_sources(sources)?;
            loop {
                while let Ok(s) = submissions.try_recv() {
                    v.submit(s)?;
                }
                if !v.step(until_ms)? {
                    return Ok(());
                }
            }
        }
    }
}

/// The bound on submissions queued between the sources and the wall-clock loop.
pub const SOURCE_QUEUE: usize = 256;

/// One virtual instant's work is refused past this many effect rounds: a handler answering
/// every outcome with a new effect never lets virtual time move.
pub const SETTLE_LIMIT: usize = 10_000;

/// The virtual-time host: no threads, no sleeps, no pool. Effects run in key order on the
/// caller's thread, so one seed and one script give one trace.
pub struct VirtualDriver {
    engine: Engine,
    clock: VirtualClock,
    runner: Arc<dyn EffectRunner>,
    sources: Vec<Box<dyn Source>>,
    held: BTreeSet<String>,
    effects_run: u64,
}

impl VirtualDriver {
    pub fn new(engine: Engine, clock: VirtualClock, runner: Arc<dyn EffectRunner>) -> Self {
        let engine = classified(engine, &runner);
        VirtualDriver { engine, clock, runner, sources: Vec::new(), held: BTreeSet::new(), effects_run: 0 }
    }

    /// Refuses a set holding any source that reads the real world.
    pub fn with_sources(mut self, sources: Sources) -> Result<Self> {
        self.sources.extend(sources.into_scripted()?);
        Ok(self)
    }

    /// Effects with these names are left pending for the caller to complete; the runner never sees them.
    pub fn with_held(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.held.extend(names);
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn into_engine(self) -> Engine {
        self.engine
    }

    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// Effects and probes answered by the runner so far.
    pub fn effects_run(&self) -> u64 {
        self.effects_run
    }

    pub fn submit(&mut self, s: Submit) -> Result<()> {
        accept(&mut self.engine, s)
    }

    /// Runs the engine and answers every pending effect until none is left at this instant.
    pub fn settle(&mut self) -> Result<()> {
        for _ in 0..SETTLE_LIMIT {
            // loop-safe: VirtualDriver runs under virtual time (sim and tests); it has no event loop
            self.engine.run_until_idle()?;
            let pending: Vec<PendingEffect> = self.engine.pending_effects().into_iter().filter(|p| !self.held.contains(&p.spec.name)).collect();
            if pending.is_empty() {
                return Ok(());
            }
            for p in pending {
                self.engine.note_attempt(&p.key);
                let outcome = match self.runner.run_for_task(&p.task, &p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(e) => Outcome::Failed(e),
                };
                self.effects_run += 1;
                // loop-safe: VirtualDriver runs under virtual time (sim and tests); it has no event loop
                self.engine.complete(&p.key, outcome)?;
            }
        }
        Err(RuntimeError::new(format!("virtual time {} did not settle within {SETTLE_LIMIT} effect rounds", self.clock.now_ms())))
    }

    fn deliver_due(&mut self) -> Result<()> {
        let now = self.clock.now_ms();
        let mut due = Vec::new();
        for s in self.sources.iter_mut() {
            if let Some(x) = s.scripted() {
                due.extend(x.take_due(now));
            }
        }
        for s in due {
            accept(&mut self.engine, s)?;
        }
        Ok(())
    }

    /// Settles now, moves the clock to the next timer or source time at or before `limit_ms`
    /// and settles there. `false` once nothing is due by then; the clock is left at `limit_ms`.
    pub fn step(&mut self, limit_ms: u64) -> Result<bool> {
        self.deliver_due()?;
        self.settle()?;
        let now = self.clock.now_ms();
        let next = [self.engine.next_timer_at(), sources::next_at(&mut self.sources)].into_iter().flatten().min();
        let more = matches!(next, Some(t) if t <= limit_ms && now < limit_ms);
        self.clock.advance_to(match next {
            Some(t) if more => t.max(now + 1),
            _ => limit_ms,
        });
        self.deliver_due()?;
        self.settle()?;
        Ok(more)
    }

    pub fn run_until(&mut self, limit_ms: u64) -> Result<()> {
        while self.step(limit_ms)? {}
        Ok(())
    }
}
