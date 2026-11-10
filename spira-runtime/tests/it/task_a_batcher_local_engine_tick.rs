//! The batcher's tick on a stored Certifying row, through the real engine and host: the observe
//! is launched, its answer reaches the task, and the settled round is dropped and the next cut.

use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::{Journal, Poison, StateRow, Turn};
use runtime::ports::{Clock, VirtualClock};
use runtime::{EffectSpec, EventId, MemJournal, NullMarker, NullSink, StreamKey};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::{open_engine, start_tasks};

const NOW: u64 = 1_794_294_300_000;
const BATCH: &str = "spira-20261110T055337Z";
const STORED: &str = r#"{"state":"Certifying","batch":"spira-20261110T055337Z","leg":"spira.e14c63e58a6e.certify2","leg_at_ms":1794291717130,"run":2,"leg_restarts":0,"leg_missing":0,"paused_on":null,"skip_why":null,"waiting":null,"stacked":false,"attributed":false,"round_key":"sp-1o7bgj:0302664 sp-5qhmb0:ce834a5 sp-kn42ye:2166196 sp-two0b3.2:41be105 sp-vwhmus:c4b4121"}"#;

struct Runner {
    clock: VirtualClock,
    ran: Mutex<Vec<String>>,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.ran.lock().unwrap().push(spec.name.clone());
        if spec.name == OBSERVE {
            let member = Member { id: "sp-olw42f".into(), state: "CERTIFIED".into(), version: 5, tip: "t1".into(), sifted_tip: Some("t1".into()), title: "work".into(), priority: 2, ..Member::default() };
            let repo = Repo { branch_tips: [("sp-olw42f".to_string(), "t1".to_string())].into(), pool: vec![member], base_sha: "b2".into(), settled: vec![BATCH.into()], live_legs: Some(vec![]), ..Repo::default() };
            return Ok(serde_json::to_string(&Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: self.clock.now_ms() }).unwrap());
        }
        if spec.name == FETCH {
            return Ok("{}".into());
        }
        if spec.name == ASSEMBLE {
            let merged = Assembled { merged: vec![("sp-olw42f".into(), "t1".into())], wt: "/w".into(), base: "b2".into(), ..Assembled::default() };
            return Ok(serde_json::to_string(&merged).unwrap());
        }
        Err(format!("no double for {}", spec.name))
    }
}

#[test]
fn a_tick_on_the_stored_certifying_row_observes_and_cuts_the_waiting_member() {
    tick_on_stored_row(false);
}

#[test]
fn a_tick_on_the_stored_certifying_row_with_a_standing_refusal_poison_observes_and_cuts_the_waiting_member() {
    tick_on_stored_row(true);
}

fn tick_on_stored_row(poisoned: bool) {
    let journal = MemJournal::new();
    journal.commit(&Turn { state: vec![StateRow { task: "batcher.local".into(), skey: "repos/spira".into(), value: STORED.into(), at: NOW }], ..Turn::default() }).unwrap();
    if poisoned {
        journal.poison(&Poison { stream: StreamKey(STREAM.into()), event: EventId("unit.ran/spira-run-batcher.local-spira.e14c63e58a6e.certify2.service/1794292447003".into()), handler: "scheduler".into(), message: "journal: effect key is 262 chars (max 255) or holds a NUL".into() }).unwrap();
    }
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner { clock: clock.clone(), ran: Mutex::new(vec![]) });
    let mut engine = open_engine(&fixture(), journal.clone(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![]).unwrap();
    start_tasks(&mut engine);
    let mut d = VirtualDriver::new(engine, clock.clone(), runner.clone());
    d.run_until(NOW + 60_000).unwrap();

    let ran: Vec<String> = runner.ran.lock().unwrap().iter().filter(|n| n.starts_with("batcher.local")).cloned().collect();
    assert!(ran.iter().any(|n| n == OBSERVE), "an observe was launched: {ran:?}");
    assert!(ran.iter().any(|n| n == ROUND_OPEN), "the settled round was dropped and the waiting member's round opened: {ran:?}");
    let stored = journal.state().get(&("batcher.local".to_string(), "repos/spira".to_string())).cloned().unwrap_or_default();
    assert!(!stored.contains(BATCH), "the Certifying row is gone: {stored}");
    let task = d.engine().status().tasks.into_iter().find(|t| t.name == "batcher.local").unwrap();
    let fired = task.last_commands.expect("the status says what the last fire returned");
    assert_eq!(fired.returned, fired.admitted, "nothing the batcher asked for was refused: {fired:?}");
}
