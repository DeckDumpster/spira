//! The finished runs the forge lists are reported once, ever: not again after a restart, with or
//! without the old script's state file still on disk.

use std::sync::{Arc, Mutex};

use runtime::taskrig::TaskRig;
use spira_runtime::watchers::repo_events::*;
use spira_runtime::watchers::INBOX_APPEND;

const SLUG: &str = "o/r";

fn run(id: u64) -> Run {
    Run { id, workflow: "Deploy".into(), conclusion: "failure".into(), branch: "main".into(), event: "push".into(), title: format!("t{id}") }
}

fn handler() -> runtime::Registration {
    RepoEvents { interval_ms: 45_000 }.registration()
}

fn rig(forge: Arc<Mutex<Vec<Run>>>, seed_keys: Vec<String>) -> TaskRig {
    let (_, stream, kind) = RepoEvents::boot();
    let mut r = TaskRig::new(1, handler()).unwrap().double(OBSERVE, move |input| {
        let carry: Carry = serde_json::from_str(input).unwrap();
        let seed = if carry.seeded { vec![] } else { seed_keys.clone() };
        Ok(serde_json::to_string(&Snapshot { seed, repos: vec![(SLUG.into(), Ok(forge.lock().unwrap().clone()))] }).unwrap())
    }).double(INBOX_APPEND, |_| Ok(String::new()));
    r.tick(stream, kind).unwrap();
    r.advance(45_000).unwrap();
    r
}

fn reports(r: &TaskRig) -> usize {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).count()
}

fn restart(r: &mut TaskRig) {
    let (_, stream, kind) = RepoEvents::boot();
    r.reopen(handler()).unwrap();
    r.tick(stream, kind).unwrap();
}

#[test]
fn a_restart_does_not_report_a_run_again() {
    let forge = Arc::new(Mutex::new(vec![run(1)]));
    let mut r = rig(forge.clone(), vec![]);
    *forge.lock().unwrap() = vec![run(3), run(2), run(1)];
    r.advance(45_000).unwrap();
    assert_eq!(reports(&r), 2);
    restart(&mut r);
    r.advance(45_000).unwrap();
    assert_eq!(reports(&r), 2, "a restart re-reported finished runs");
}

#[test]
fn a_restart_does_not_report_a_run_again_when_a_legacy_seed_is_on_disk() {
    let legacy = vec!["run|o/r|1|failure".to_string()];
    let forge = Arc::new(Mutex::new(vec![run(1)]));
    let mut r = rig(forge.clone(), legacy);
    *forge.lock().unwrap() = vec![run(3), run(2), run(1)];
    r.advance(45_000).unwrap();
    assert_eq!(reports(&r), 2);
    r.advance(45_000 * 20).unwrap();
    restart(&mut r);
    r.advance(45_000).unwrap();
    assert_eq!(reports(&r), 2, "a restart re-reported finished runs");
}
