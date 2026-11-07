//! `World::clear_round_records`: the queue's round record files cleared against a spira-lc double
//! on a socket, over a real queue directory. What the store holds open, or cannot answer for, stays.

use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

type States = Arc<Mutex<BTreeMap<String, String>>>;

struct Rig {
    dir: testkit::TempDir,
    states: States,
    world: World,
}

fn rig(on_store: bool, states: &[(&str, &str)]) -> Rig {
    let dir = testkit::TempDir::new("world-round-records");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let states: States = Arc::new(Mutex::new(states.iter().map(|(b, s)| (b.to_string(), s.to_string())).collect()));
    let served = states.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv {
            [v, batch] if v == "show-batch" && batch == "unreadable" => Err(RuntimeError::new("cannot tell: store down")),
            [v, batch] if v == "show-batch" => match served.lock().unwrap().get(batch) {
                Some(s) => Ok(serde_json::json!({ "state": s }).to_string()),
                None => Err(RuntimeError::new("{}")),
            },
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let queue = dir.path().join("queue");
    std::fs::create_dir_all(queue.join("alpha")).unwrap();
    let world = World { lc_socket: sock, queue_dir: queue, batches_on_store: on_store, ..World::default() };
    Rig { dir, states, world }
}

fn plant(r: &Rig, name: &str, text: &str) -> PathBuf {
    let p = r.dir.path().join("queue/alpha").join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn paths(v: &[&Path]) -> String {
    serde_json::to_string(&v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()).unwrap()
}

#[test]
fn a_record_naming_an_ended_or_unknown_round_is_removed_and_one_naming_an_open_round_stays() {
    let r = rig(true, &[("b-done", "LANDED"), ("b-live", "CI_RUNNING")]);
    let done = plant(&r, "round.b-done", "batch_id=b-done\n");
    let gone = plant(&r, "round.b-gone", "batch_id=b-gone\n");
    let live = plant(&r, "round.b-live", "batch_id=b-live\n");
    let out = r.world.clear_round_records(&paths(&[&done, &gone, &live])).unwrap();
    assert_eq!((done.exists(), gone.exists(), live.exists()), (false, false, true), "{out}");
    assert_eq!(out, "cleared 2 of 3; 1 refused");
}

#[test]
fn a_round_that_opened_after_the_observation_is_not_cleared() {
    let r = rig(true, &[]);
    let f = plant(&r, "round.b-new", "batch_id=b-new\n");
    r.states.lock().unwrap().insert("b-new".into(), "OPEN".into());
    r.world.clear_round_records(&paths(&[&f])).unwrap();
    assert!(f.exists());
}

#[test]
fn a_path_nobody_observed_as_clearable_is_never_removed() {
    let r = rig(true, &[]);
    let f = plant(&r, "round.b-gone", "batch_id=b-gone\n");
    let other = r.dir.path().join("keep.txt");
    std::fs::write(&other, "x").unwrap();
    r.world.clear_round_records(&paths(&[&other])).unwrap();
    assert!(f.exists() && other.exists());
}

#[test]
fn a_store_that_cannot_answer_and_a_record_naming_no_round_are_refused_by_name() {
    let r = rig(true, &[]);
    let unread = plant(&r, "round.unreadable", "batch_id=unreadable\n");
    let bare = plant(&r, "round", "tip=abc\n");
    let pr_only = plant(&r, "round.pr-only", "pr=9\n");
    let held = r.world.round_record_partition();
    assert!(held.clear.is_empty(), "{:?}", held.clear);
    let all = held.held.join("\n");
    for f in [&unread, &bare, &pr_only] {
        assert!(all.contains(&f.display().to_string()), "{all}");
    }
    r.world.clear_round_records(&paths(&[&unread, &bare, &pr_only])).unwrap();
    assert!(unread.exists() && bare.exists() && pr_only.exists());
}

#[test]
fn nothing_is_cleared_while_the_batch_machine_is_not_on_the_store() {
    let r = rig(false, &[]);
    let f = plant(&r, "round.b-gone", "batch_id=b-gone\n");
    r.world.clear_round_records(&paths(&[&f])).unwrap();
    assert!(f.exists(), "an absent batch is no evidence the record is stale before the cutover");
}
