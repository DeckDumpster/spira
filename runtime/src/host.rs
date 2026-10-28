//! The wall-clock host: one tokio loop around the engine. Handlers run inline; effects run on
//! a bounded blocking pool, each under the remainder of its deadline, and their outcomes come
//! back through a channel to be appended like any other event.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Semaphore};

use crate::engine::{Engine, Executor, Outcome, PendingEffect};
use crate::event::{EffectSpec, EventKind};
use crate::ports::Clock;
use crate::Result;

pub trait EffectRunner: Send + Sync {
    /// Must be idempotent under `key`: a restart re-issues every effect that has no outcome.
    fn run(&self, key: &str, spec: &EffectSpec) -> std::result::Result<String, String>;
}

pub struct Submit {
    pub id: String,
    pub stream: String,
    pub kind: EventKind,
}

/// Runs until `submissions` closes and no effect is in flight.
pub async fn serve(
    mut engine: Engine,
    clock: Arc<dyn Clock>,
    runner: Arc<dyn EffectRunner>,
    pool: usize,
    mut submissions: mpsc::Receiver<Submit>,
) -> Result<()> {
    let permits = Arc::new(Semaphore::new(pool.max(1)));
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(String, Outcome)>();
    let mut in_flight: BTreeSet<String> = BTreeSet::new();
    let mut open = true;
    loop {
        engine.run_until_idle()?;
        let launch: Vec<PendingEffect> = engine.pending_effects().into_iter().filter(|p| !in_flight.contains(&p.key)).collect();
        for p in launch {
            in_flight.insert(p.key.clone());
            let (permits, runner, tx) = (permits.clone(), runner.clone(), done_tx.clone());
            let left = Duration::from_millis(p.deadline_at.saturating_sub(clock.now_ms()));
            tokio::spawn(async move {
                let work = async {
                    let _permit = permits.acquire_owned().await.ok();
                    let (r, k, spec) = (runner, p.key.clone(), p.spec.clone());
                    tokio::task::spawn_blocking(move || r.run(&k, &spec)).await
                };
                let outcome = match tokio::time::timeout(left, work).await {
                    Err(_) => Outcome::TimedOut,
                    Ok(Ok(Ok(d))) => Outcome::Succeeded(d),
                    Ok(Ok(Err(e))) => Outcome::Failed(e),
                    Ok(Err(e)) => Outcome::Failed(format!("effect task: {e}")),
                };
                let _ = tx.send((p.key, outcome));
            });
        }
        if !open && in_flight.is_empty() {
            return Ok(());
        }
        let sleep = engine.next_timer_at().map_or(Duration::from_secs(3600), |t| Duration::from_millis(t.saturating_sub(clock.now_ms())));
        tokio::select! {
            s = submissions.recv(), if open => match s {
                Some(s) => engine.submit(&s.id, &s.stream, s.kind)?,
                None => open = false,
            },
            Some((key, outcome)) = done_rx.recv() => {
                in_flight.remove(&key);
                engine.complete(&key, outcome)?;
            }
            _ = tokio::time::sleep(sleep) => {}
        }
    }
}
