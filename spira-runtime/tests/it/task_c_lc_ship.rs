//! `lc.ship`: the scheduler uploads what lc-serve stages, so lc-serve starts no `curl`.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::lc_ship::{self as task, Carry, LcShip as H, Phase, Snapshot};
use testkit::TempDir;

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "2"), (task::URL_KEY, "s3://off-box/lc/"), (task::DIR_KEY, "")];
const NOW: u64 = 1_793_664_000_000;

fn queued(n: u64) -> Snapshot {
    Snapshot { dir: "ship".into(), queued: n, oldest: "1-1".into() }
}

#[test_id(tphqn4u)]
#[test]
fn t1_registered_boots_and_arms_the_configured_tick() {
    t::t1_registered_and_boots::<H>(SET, 2_000);
}

#[test_id(ttclq50)]
#[test]
fn t2_an_empty_queue_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &serde_json::to_string(&queued(0)).unwrap());
}

#[test_id(t2qwnmb)]
#[test]
fn t3_a_queued_object_is_drained_and_a_refusal_keeps_it_queued_and_says_so() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(queued(2)), NOW).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Draining, vec![task::DRAIN]));
    assert_eq!(acts[0].input, "ship");
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::DRAIN.into(), ok: true, body: String::new() }, NOW).unwrap();
    assert_eq!(c.phase, Phase::Idle);
    let (again, none) = task::decide(&h.cfg, &Carry { phase: Phase::Draining }, &Input::Observed(queued(2)), NOW).unwrap();
    assert_eq!((again.phase, none.len()), (Phase::Idle, 0), "a tick that finds the drain unanswered asks for nothing");
    let refused = task::decide(&h.cfg, &Carry { phase: Phase::Draining }, &Input::Outcome { effect: task::DRAIN.into(), ok: false, body: "403".into() }, NOW).unwrap_err();
    assert!(refused.contains("stays queued") && refused.contains("403"), "{refused}");
}

#[test_id(tsuscur)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/lc_ship.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(tiwzyla)]
#[test]
fn the_probe_counts_the_queue_and_sees_nothing_without_a_destination() {
    let run = TempDir::new("lc-ship-observe");
    let q = run.path().join("lc-ship/queue/revlog");
    std::fs::create_dir_all(&q).unwrap();
    std::fs::write(q.join("2-3"), "x").unwrap();
    std::fs::write(q.join("1-1"), "x").unwrap();
    std::fs::write(q.join("4-4.tmp"), "x").unwrap();
    let seen: Snapshot = serde_json::from_str(&task::observe(run.path(), "s3://b/p/", "").unwrap()).unwrap();
    assert_eq!((seen.queued, seen.oldest.as_str()), (2, "1-1"));
    let none: Snapshot = serde_json::from_str(&task::observe(run.path(), "", "").unwrap()).unwrap();
    assert_eq!(none.queued, 0);
}

#[test_id(tayo7sv)]
#[test]
fn the_drain_uploads_through_the_given_process_port_only() {
    struct Refusing;
    impl runtime::ports::Proc for Refusing {
        fn run(&self, _: &runtime::ports::ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            Err("no process".into())
        }
    }
    let run = TempDir::new("lc-ship-drain");
    let (dir, off) = (run.path().join("ship"), run.path().join("off"));
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    let name = state_store_sqlite::durability::revlog::run_name(1, 1);
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    let url = format!("file://{}", off.display());
    let said = task::drain(std::sync::Arc::new(Refusing), &url, &dir.display().to_string()).unwrap();
    assert_eq!(said, "1 object(s) uploaded");
    assert!(off.join("revlog").join(&name).exists() && dir.join("sent/revlog").join(&name).exists() && !dir.join("queue/revlog").join(&name).exists());
    assert_eq!(std::fs::read_to_string(dir.join("shipped")).unwrap(), "1");
}
