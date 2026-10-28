//! The tokio host: handlers inline, effects on the bounded pool, outcomes back as events.

use std::sync::Arc;
use std::time::Duration;

use runtime::host::{serve, EffectRunner, Submit};
use runtime::ports::WallClock;
use runtime::*;
use tokio::sync::mpsc;

struct Starter;
impl Handler for Starter {
    fn interests(&self) -> &[EventKindTag] {
        &["start", EFFECT_SUCCEEDED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            let deadline_ms = ev.kind.body.parse().unwrap();
            vec![Command::Effect(EffectSpec { name: "work".into(), input: String::new(), deadline_ms })]
        } else {
            vec![Command::Emit(EventKind::new(&format!("saw:{}", ev.kind.tag), ""))]
        }
    }
}

struct Sleepy(u64);
impl EffectRunner for Sleepy {
    fn run(&self, key: &str, _: &EffectSpec) -> std::result::Result<String, String> {
        // clock-safe: the effect under test is a blocking runner; the deadline outcome is asserted, not elapsed time
        std::thread::sleep(Duration::from_millis(self.0));
        Ok(key.into())
    }
}

async fn run_one(deadline_ms: u64, work_ms: u64) -> Vec<String> {
    let journal = MemJournal::new();
    let engine = Engine::open(journal.clone(), Arc::new(WallClock), vec![Registration::new("starter", Starter)], 1).unwrap();
    let (tx, rx) = mpsc::channel(4);
    tx.send(Submit { id: "e1".into(), stream: "s".into(), kind: EventKind::new("start", &deadline_ms.to_string()) }).await.unwrap();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), serve(engine, Arc::new(WallClock), Arc::new(Sleepy(work_ms)), 2, rx)).await.expect("serve ends").unwrap();
    journal.read_from(0, 100).unwrap().into_iter().map(|(_, e)| e.kind.tag).collect()
}

#[tokio::test]
async fn an_effect_outcome_returns_as_an_event() {
    let tags = run_one(2000, 5).await;
    assert!(tags.contains(&"saw:effect.succeeded".to_string()), "{tags:?}");
}

#[tokio::test]
async fn an_effect_past_its_deadline_returns_timed_out() {
    let tags = run_one(20, 300).await;
    assert!(tags.contains(&"saw:effect.timed_out".to_string()), "{tags:?}");
    assert!(!tags.contains(&"saw:effect.succeeded".to_string()), "{tags:?}");
}
