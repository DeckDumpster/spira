//! `World::clear_round_records`: the queue's round record files cleared against a spira-lc double
//! on a socket, over a real queue directory. What the store holds open, or cannot answer for, stays.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

type States = Arc<Mutex<BTreeMap<String, String>>>;
type OpenByRepo = Arc<Mutex<BTreeMap<String, Vec<String>>>>;

const REFUSING_REPO: &str = "refusing";

fn listed(prefixes: &[String], open: &OpenByRepo) -> String {
    let items: Vec<_> = open
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(repo, ids)| ids.iter().map(move |id| (repo.clone(), id.clone())))
        .filter(|(repo, id)| prefixes.iter().any(|p| format!("batches/{repo}/{id}").starts_with(p.as_str())))
        .enumerate()
        .map(|(n, (repo, id))| {
            serde_json::json!({
                "key": format!("batches/{repo}/{id}"),
                "value": { "data": { "batch_id": id, "repo": repo, "state": "Open" } },
                "mod_rev": { "main": n + 1, "sub": 0 },
                "create_rev": { "main": n + 1, "sub": 0 },
                "lock_expires_at_ms": null,
            })
        })
        .collect();
    serde_json::json!({ "ok": { "items": items, "list_rev": 1 } }).to_string()
}

struct Rig {
    dir: testkit::TempDir,
    states: States,
    open: OpenByRepo,
    world: World,
}

fn rig(on_store: bool, states: &[(&str, &str)]) -> Rig {
    let dir = testkit::TempDir::new("world-round-records");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let states: States = Arc::new(Mutex::new(states.iter().map(|(b, s)| (b.to_string(), s.to_string())).collect()));
    let served = states.clone();
    let open: OpenByRepo = Arc::default();
    let listing = open.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv {
            [v, batch] if v == "show-batch" && batch == "unreadable" => Err(RuntimeError::new("cannot tell: store down")),
            [v, batch] if v == "show-batch" => match served.lock().unwrap().get(batch) {
                Some(s) => Ok(serde_json::json!({ "state": s }).to_string()),
                None => Err(RuntimeError::new("{}")),
            },
            [rc, list, json, body] if rc == "rc" && list == "list" && json == "--json" => {
                let prefixes: Vec<String> = serde_json::from_str::<serde_json::Value>(body).unwrap()["prefixes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_string())
                    .collect();
                match prefixes.iter().any(|p| p.contains(&format!("/{REFUSING_REPO}/"))) {
                    true => Err(RuntimeError::new("store down")),
                    false => Ok(listed(&prefixes, &listing)),
                }
            }
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let queue = dir.path().join("queue");
    std::fs::create_dir_all(queue.join("alpha")).unwrap();
    let world = World { lc_socket: sock, queue_dir: queue, batches_on_store: on_store, ..World::default() };
    Rig { dir, states, open, world }
}

fn plant(r: &Rig, name: &str, text: &str) -> PathBuf {
    let p = r.dir.path().join("queue/alpha").join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn paths(v: &[&Path]) -> String {
    serde_json::to_string(&v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()).unwrap()
}

#[test_id(tyf8yff)]
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

#[test_id(twqsreh)]
#[test]
fn a_round_that_opened_after_the_observation_is_not_cleared() {
    let r = rig(true, &[]);
    let f = plant(&r, "round.b-new", "batch_id=b-new\n");
    r.states.lock().unwrap().insert("b-new".into(), "OPEN".into());
    r.world.clear_round_records(&paths(&[&f])).unwrap();
    assert!(f.exists());
}

#[test_id(t7z5e7s)]
#[test]
fn a_path_nobody_observed_as_clearable_is_never_removed() {
    let r = rig(true, &[]);
    let f = plant(&r, "round.b-gone", "batch_id=b-gone\n");
    let other = r.dir.path().join("keep.txt");
    std::fs::write(&other, "x").unwrap();
    r.world.clear_round_records(&paths(&[&other])).unwrap();
    assert!(f.exists() && other.exists());
}

#[test_id(t1dfihz)]
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

#[test_id(t6vegm3)]
#[test]
fn nothing_is_cleared_while_the_batch_machine_is_not_on_the_store() {
    let r = rig(false, &[]);
    let f = plant(&r, "round.b-gone", "batch_id=b-gone\n");
    r.world.clear_round_records(&paths(&[&f])).unwrap();
    assert!(f.exists(), "an absent batch is no evidence the record is stale before the cutover");
}

fn forge_state_rig(repo: &str, open: &[&str]) -> (Rig, PathBuf) {
    let r = rig(true, &[]);
    std::fs::create_dir_all(r.dir.path().join("queue").join(repo)).unwrap();
    let f = r.dir.path().join("queue").join(repo).join(install::round_records::FORGE_STATE);
    std::fs::write(&f, "pending\n").unwrap();
    r.open.lock().unwrap().insert(repo.into(), open.iter().map(|b| b.to_string()).collect());
    (r, f)
}

#[test_id(th500yn)]
#[test]
fn a_pending_forge_state_is_live_while_its_repo_has_an_open_batch() {
    let (r, f) = forge_state_rig("alpha", &["alpha-20261115T130339Z"]);
    let p = r.world.round_record_partition();
    assert!(p.clear.is_empty() && p.held.is_empty(), "{:?} {:?}", p.clear, p.held);
    r.world.clear_round_records(&paths(&[&f])).unwrap();
    assert!(f.exists());
}

#[test_id(t9vpzz2)]
#[test]
fn a_pending_forge_state_is_refused_by_name_when_its_repo_has_no_open_batch() {
    let (r, f) = forge_state_rig("alpha", &[]);
    r.open.lock().unwrap().insert("beta".into(), vec!["beta-1".into()]);
    let p = r.world.round_record_partition();
    assert!(p.clear.is_empty(), "{:?}", p.clear);
    assert_eq!(p.held.len(), 1, "{:?}", p.held);
    assert!(p.held[0].contains(&f.display().to_string()) && p.held[0].contains("no open batch for alpha"), "{}", p.held[0]);
}

#[test_id(tks5kn8)]
#[test]
fn a_pending_forge_state_is_refused_when_the_store_cannot_say_whether_its_repo_has_an_open_batch() {
    let (r, f) = forge_state_rig(REFUSING_REPO, &[]);
    let p = r.world.round_record_partition();
    assert!(p.clear.is_empty(), "{:?}", p.clear);
    assert_eq!(p.held.len(), 1, "{:?}", p.held);
    assert!(p.held[0].contains(&f.display().to_string()) && p.held[0].contains("cannot tell whether refusing has an open batch"), "{}", p.held[0]);
}
