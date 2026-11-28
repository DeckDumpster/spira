//! A restart does not repeat a keyed effect: the task's carry, committed with the resolution,
//! stops the re-request, with no row of the closed effect to consult.

use std::sync::{Arc, Mutex};

use cr::record::{Kind, State};
use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::VirtualClock;
use runtime::{EffectSpec, MemJournal, NullMarker, NullSink};
use spira_runtime::settings::fixture;
use spira_runtime::watchers::cr_cadence::*;
use spira_runtime::{open_engine, start_tasks};

const NOW: u64 = 1_794_294_300_000;
const DUE: u64 = NOW / 1000 - 3600;

#[derive(Default)]
struct Runner(Mutex<Vec<String>>);

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.0.lock().unwrap().push(spec.name.clone());
        if spec.name == OBSERVE {
            let closed = ReviewObs { number: 1, kind: Kind::Scheduled, state: State::Closed, staged_at: 0, closed_at: Some(DUE * 1000) };
            return Ok(serde_json::to_string(&Snapshot::new(vec![closed], DUE)).unwrap());
        }
        if spec.name == OPEN {
            return Ok("002 unit".into());
        }
        Err(format!("no double for {}", spec.name))
    }
}

fn run_for_a_while(journal: Arc<MemJournal>, from: u64) -> (usize, usize) {
    let clock = VirtualClock::new(from);
    let runner = Arc::new(Runner::default());
    let mut engine = open_engine(&fixture(), journal, Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![], std::time::Duration::from_secs(10)).unwrap();
    start_tasks(&mut engine);
    VirtualDriver::new(engine, clock, runner.clone()).run_until(from + 60_000).unwrap();
    let ran = runner.0.lock().unwrap();
    let count = |name: &str| ran.iter().filter(|n| *n == name).count();
    (count(OBSERVE), count(OPEN))
}

#[test_id(tz8mfwi)]
#[test]
fn a_restart_that_sees_the_same_reviews_opens_no_second_review() {
    let journal = MemJournal::new();
    let (_, opened) = run_for_a_while(journal.clone(), NOW);
    assert_eq!(opened, 1);
    assert!(journal.boot().unwrap().effects.iter().all(|e| e.spec.name != OPEN), "the open resolved before the restart");
    let (observed, reopened) = run_for_a_while(journal, NOW + 600_000);
    assert!(observed > 0, "the restarted task looked at the same reviews");
    assert_eq!(reopened, 0);
}
