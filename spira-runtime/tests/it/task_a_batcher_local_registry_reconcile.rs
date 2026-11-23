//! A test-registry row whose bead the lifecycle reports LANDED is cleared on the next idle tick,
//! whichever path landed the bead; a tick with no such row issues no act.

use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::ports::VirtualClock;
use runtime::{EffectSpec, MemJournal, NullMarker, NullSink};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::{open_engine, start_tasks};

const NOW: u64 = 1_794_349_965_000;

struct Runner {
    registry_landed: serde_json::Value,
    ran: Mutex<Vec<(String, String)>>,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.ran
            .lock()
            .unwrap()
            .push((spec.name.clone(), spec.input.clone()));
        match spec.name.as_str() {
            OBSERVE => {
                let snap = serde_json::json!({"repos": {"spira": {"pool": [], "blockers": [], "basefail": [], "open_round": null, "branch_tips": {}, "base_sha": "b2", "stopped": false, "declares_own_testenv": true, "live_legs": []}}, "now_ms": NOW, "registry_landed": self.registry_landed});
                Ok(snap.to_string())
            }
            "batcher.local.clear_landed" | FETCH => Ok("{}".into()),
            other => Err(format!("no double for {other}")),
        }
    }
}

fn idle_tick(registry_landed: serde_json::Value) -> Vec<(String, String)> {
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner {
        registry_landed,
        ran: Mutex::new(vec![]),
    });
    let mut engine = open_engine(
        &fixture(),
        MemJournal::new(),
        Arc::new(clock.clone()),
        Arc::new(NullMarker),
        Arc::new(NullSink),
        vec![],
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    start_tasks(&mut engine);
    let mut d = VirtualDriver::new(engine, clock, runner.clone());
    d.run_until(NOW + 10_000).unwrap();
    let ran = runner
        .ran
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n.starts_with("batcher.local"))
        .cloned()
        .collect();
    ran
}

#[test_id(t3phln2)]
#[test]
fn an_idle_tick_clears_the_registry_rows_of_beads_the_lifecycle_reports_landed() {
    let ran = idle_tick(serde_json::json!(["sp-o82moc", "sp-msth37"]));
    let clears: Vec<_> = ran
        .iter()
        .filter(|(n, _)| n == "batcher.local.clear_landed")
        .collect();
    assert_eq!(clears.len(), 1, "{ran:?}");
    let leg: Leg = serde_json::from_str(&clears[0].1).unwrap();
    let ids: Vec<&str> = leg.members.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["sp-o82moc", "sp-msth37"]);
}

#[test_id(ta4day6)]
#[test]
fn an_idle_tick_with_no_landed_registry_row_runs_no_clear_landed() {
    let ran = idle_tick(serde_json::json!([]));
    assert!(
        ran.iter().all(|(n, _)| n != "batcher.local.clear_landed"),
        "{ran:?}"
    );
}
