//! A green publish PR of a `queue.local` repo is merged within one `queue.step` tick of its checks
//! concluding (the boot tick of `publish` has just read it pending), not at `publish`'s own cadence — through the real engine and host.

use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::ports::VirtualClock;
use runtime::{EffectSpec, MemJournal, NullMarker, NullSink};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::a::{publish, queue_step};
use spira_runtime::{open_engine, start_tasks};

const NOW: u64 = 1_794_294_300_000;

struct Runner {
    asked: Mutex<Vec<(String, String)>>,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let first_publish_observe = {
            let mut asked = self.asked.lock().unwrap();
            asked.push((spec.name.clone(), spec.input.clone()));
            spec.name == publish::OBSERVE && asked.iter().filter(|(n, _)| n == publish::OBSERVE).count() == 1
        };
        if spec.name == queue_step::OBSERVE {
            let rec = queue_step::PublishRec { pr: 1123, head: "f89d".into(), branch: "spira/publish/1".into() };
            let repo = queue_step::Repo { mode: "queue.local".into(), lc_ok: true, publish_rec: Some(rec), publish_check: queue_step::Check::Green, base_sha: "b1".into(), ..queue_step::Repo::default() };
            return Ok(serde_json::to_string(&queue_step::Snapshot { repos: [("spira".to_string(), repo)].into(), world_running: true }).unwrap());
        }
        if spec.name == publish::OBSERVE {
            let rec = publish::PublishRec { pr: 1123, head: "f89d".into(), branch: "spira/publish/1".into(), ..publish::PublishRec::default() };
            let repo = publish::Repo {
                mode: "queue.local".into(),
                publish_mode: "pr".into(),
                publish_rec: Some(rec),
                pr: publish::Pr::Open { ci: if first_publish_observe { "pending" } else { "green" }.into(), behind: false, tries: 0 },
                open_publish_prs: vec![1123],
                forge_sha: Some("f0".into()),
                head_sha: "f89d".into(),
                diverged: Some(false),
                observed_at: NOW,
                ..publish::Repo::default()
            };
            return Ok(serde_json::to_string(&publish::Snapshot { repos: [("spira".to_string(), repo)].into(), world_running: true }).unwrap());
        }
        Ok(String::new())
    }
}

#[test]
fn a_green_standing_publish_pr_is_settled_within_one_queue_step_tick() {
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner { asked: Mutex::new(vec![]) });
    let mut engine = open_engine(&fixture(), MemJournal::new(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![]).unwrap();
    start_tasks(&mut engine);
    let mut d = VirtualDriver::new(engine, clock.clone(), runner.clone());
    d.run_until(NOW + queue_step::CADENCE_MS + 30_000).unwrap();

    assert!(queue_step::CADENCE_MS < publish::CADENCE_MS);
    let asked = runner.asked.lock().unwrap();
    let settled = asked.iter().find(|(n, _)| n == publish::SETTLE).unwrap_or_else(|| panic!("publish.settle was not requested: {:?}", asked.iter().map(|(n, _)| n).collect::<Vec<_>>()));
    assert!(settled.1.contains("1123"), "{}", settled.1);
}
