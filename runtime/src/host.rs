//! The wall-clock host: one tokio loop that owns the engine only between turns. A turn, and
//! every effect, runs on the offload pool; the loop awaits results, kicks and keeps time.
//! Work the pool refuses stays undone in the journal and is re-offered with a growing backoff.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Semaphore};

use crate::engine::{Engine, Executor, Outcome, PendingEffect};
use crate::event::{EffectSpec, EventKind};
use crate::offload::{self, Offloaded, Pool, Spill};
use crate::ports::Clock;
use crate::wide::{Fields, WideSink};
use crate::{Result, RuntimeError};

/// A turn held longer than this leaves an overrun record.
pub const TURN_BUDGET: Duration = Duration::from_secs(5);
const BACKOFF_FIRST: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(2);

pub trait EffectRunner: Send + Sync {
    /// Must be idempotent under `key`: a restart re-issues every effect that has no outcome.
    fn run(&self, key: &str, spec: &EffectSpec) -> std::result::Result<String, String>;
}

pub struct Submit {
    pub id: String,
    pub stream: String,
    pub kind: EventKind,
}

#[derive(Default)]
pub struct ServeOpts {
    /// Called at the top of every wake, on the loop's own thread, unless a handler is parked.
    pub kick: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The longest the loop sleeps between wakes, so a kick is never later than this.
    pub max_sleep_ms: Option<u64>,
    /// Receives the loop's own records: long turns and offload spills. Must not block.
    pub sink: Option<Arc<dyn WideSink>>,
    /// The pool engine turns and effects run on; the process-wide one when `None`.
    pub pool: Option<Arc<Pool>>,
}

enum Done {
    Started(String),
    Outcome(String, Outcome),
    Spilled(String, Spill),
}

struct Turn {
    engine: Engine,
    subs: Vec<Submit>,
    done: Vec<Done>,
    in_flight: BTreeSet<String>,
}

struct TurnOut {
    engine: Engine,
    result: Result<()>,
    launch: Vec<PendingEffect>,
    next_timer_at: Option<u64>,
}

fn run_turn(mut t: Turn) -> TurnOut {
    let result = (|| {
        for s in t.subs.drain(..) {
            t.engine.submit(&s.id, &s.stream, s.kind)?;
        }
        for d in t.done.drain(..) {
            match d {
                Done::Started(key) => t.engine.note_attempt(&key),
                Done::Outcome(key, o) => {
                    t.engine.complete(&key, o)?;
                }
                Done::Spilled(..) => {}
            }
        }
        t.engine.run_until_idle()
    })();
    let launch = t.engine.pending_effects().into_iter().filter(|p| !t.in_flight.contains(&p.key)).collect();
    let next_timer_at = t.engine.next_timer_at();
    TurnOut { engine: t.engine, result, launch, next_timer_at }
}

/// Runs until `submissions` closes and no effect is in flight.
pub async fn serve(engine: Engine, clock: Arc<dyn Clock>, runner: Arc<dyn EffectRunner>, pool: usize, submissions: mpsc::Receiver<Submit>) -> Result<()> {
    serve_with(engine, clock, runner, pool, submissions, ServeOpts::default()).await
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
    pool: usize,
    mut submissions: mpsc::Receiver<Submit>,
    opts: ServeOpts,
) -> Result<()> {
    let workers = opts.pool.clone().unwrap_or_else(Pool::global);
    let busy = engine.busy();
    let max_sleep = opts.max_sleep_ms.map(Duration::from_millis);
    let permits = Arc::new(Semaphore::new(pool.max(1)));
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Done>();
    let mut in_flight: BTreeSet<String> = BTreeSet::new();
    let mut engine = Some(engine);
    let mut running: Option<Running> = None;
    let (mut subs, mut done): (Vec<Submit>, Vec<Done>) = (vec![], vec![]);
    let mut open = true;
    let mut dirty = true;
    let mut next_timer: Option<u64> = None;
    let (mut backoff, mut retry_at) = (BACKOFF_FIRST, Instant::now());
    let emit = |f: Fields| {
        if let Some(s) = &opts.sink {
            s.emit(&f); // loop-safe: try_send, drops when full
        }
    };
    let spilled = |s: &Spill, backoff: &mut Duration, retry_at: &mut Instant| {
        emit(spill_record(s));
        *retry_at = Instant::now() + *backoff;
        *backoff = (*backoff * 2).min(BACKOFF_MAX);
    };
    loop {
        if let Some(k) = &opts.kick {
            if max_sleep.is_none_or(|m| busy.held_for() <= m) {
                k();
            }
        }
        if next_timer.is_some_and(|t| t <= clock.now_ms()) {
            dirty = true;
        }
        if let Some(e) = engine.take_if(|_| running.is_none() && (dirty || !subs.is_empty() || !done.is_empty()) && Instant::now() >= retry_at) {
            let turn = Turn { engine: e, subs: std::mem::take(&mut subs), done: std::mem::take(&mut done), in_flight: in_flight.clone() };
            let slot = Arc::new(Mutex::new(Some(turn)));
            let s = slot.clone();
            let job = move || run_turn(s.lock().unwrap_or_else(|e| e.into_inner()).take().expect("the turn is put in the slot before it is offered"));
            match workers.offload("engine.turn", None, job) {
                Ok(handle) => {
                    backoff = BACKOFF_FIRST;
                    dirty = false;
                    running = Some(Running { handle, began: Instant::now(), overrun: false });
                }
                Err(sp) => {
                    let t = slot.lock().unwrap_or_else(|e| e.into_inner()).take().expect("a refused turn never ran");
                    (engine, subs, done) = (Some(t.engine), t.subs, t.done);
                    spilled(&sp, &mut backoff, &mut retry_at);
                }
            }
        }
        if !open && in_flight.is_empty() && running.is_none() && subs.is_empty() && done.is_empty() && !dirty {
            return Ok(());
        }
        let now = Instant::now();
        let mut wait = next_timer.map_or(Duration::from_secs(3600), |t| Duration::from_millis(t.saturating_sub(clock.now_ms())));
        if let Some(m) = max_sleep {
            wait = wait.min(m);
        }
        if let Some(r) = &running {
            if !r.overrun {
                wait = wait.min((r.began + TURN_BUDGET).saturating_duration_since(now));
            }
        } else if dirty || !subs.is_empty() || !done.is_empty() {
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
                out.result?;
                for p in out.launch {
                    in_flight.insert(p.key.clone());
                    launch_effect(&workers, &permits, &runner, &done_tx, clock.now_ms(), p);
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

fn launch_effect(workers: &Arc<Pool>, permits: &Arc<Semaphore>, runner: &Arc<dyn EffectRunner>, tx: &mpsc::UnboundedSender<Done>, now: u64, p: PendingEffect) {
    let (workers, permits, runner, tx) = (workers.clone(), permits.clone(), runner.clone(), tx.clone());
    let until = tokio::time::Instant::now() + Duration::from_millis(p.deadline_at.saturating_sub(now));
    tokio::spawn(async move {
        let outcome = async {
            let Ok(Ok(permit)) = tokio::time::timeout_at(until, permits.acquire_owned()).await else { return Some(Outcome::TimedOut) };
            let (r, k, spec) = (runner, p.key.clone(), p.spec.clone());
            let mut h = match workers.offload("effect", Some(permit), move || r.run(&k, &spec)) {
                Ok(h) => h,
                Err(sp) => {
                    let _ = tx.send(Done::Spilled(p.key.clone(), sp));
                    return None;
                }
            };
            let _ = tx.send(Done::Started(p.key.clone()));
            Some(match tokio::time::timeout_at(until, &mut h).await {
                Err(_) => Outcome::TimedOut,
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
