//! `lc.ship`: the scheduler uploads what lc-serve stages, in-process, so lc-serve starts no program.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::c::lc_ship::{self as task, Carry, LcShip as H, Queue, Snapshot};
use testkit::TempDir;

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "2"), (task::URL_KEY, "s3://off-box/lc/"), (task::DIR_KEY, "")];
const NOW: u64 = 1_793_664_000_000;

fn queue(n: u64) -> Queue {
    Queue { queued: n, oldest: "1-1".into() }
}

fn queued(n: u64) -> Snapshot {
    Snapshot { dir: "ship".into(), revlog: queue(n), snapshot: Queue::default() }
}

fn both() -> Snapshot {
    Snapshot { dir: "ship".into(), revlog: queue(1), snapshot: queue(1) }
}

fn stuck(effect: &str, snap: &Snapshot) -> TaskRig {
    let seen = serde_json::to_string(snap).unwrap();
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(seen.clone())).hanging(effect);
    let (_, stream, kind) = spira_runtime::tasks::b::kit::boot::<H>();
    r.tick(stream, kind).unwrap();
    r
}

fn asked(r: &TaskRig, effect: &str) -> usize {
    r.requested().iter().filter(|q| q.name == effect).count()
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
fn t3_a_drain_in_flight_is_left_alone_and_any_outcome_frees_its_kind() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(queued(2)), NOW).unwrap();
    assert_eq!((c.revlog, names(&acts)), (Some(NOW), vec![task::DRAIN]));
    assert_eq!(acts[0].input, "ship");
    let (again, none) = task::decide(&h.cfg, &c, &Input::Observed(queued(2)), NOW + 4_000).unwrap();
    assert_eq!((again, none.len()), (c.clone(), 0), "a tick that finds the drain unanswered asks for nothing");
    for (ok, body) in [(true, ""), (false, "403")] {
        let (idle, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::DRAIN.into(), ok, body: body.into() }, NOW + 1).unwrap();
        assert_eq!(idle, Carry::default(), "ok={ok}");
        let (_, next) = task::decide(&h.cfg, &idle, &Input::Observed(queued(2)), NOW + 4_000).unwrap();
        assert_eq!(names(&next), vec![task::DRAIN], "ok={ok}: the next tick asks again");
    }
}

#[test_id(t8ye48m)]
#[test]
fn a_request_past_its_deadline_with_no_outcome_is_asked_again() {
    let h = t::handler::<H>(SET);
    let late = Carry { revlog: Some(NOW), snapshot: None };
    let (_, held) = task::decide(&h.cfg, &late, &Input::Observed(queued(1)), NOW + 3_599_999).unwrap();
    assert!(held.is_empty());
    let (c, again) = task::decide(&h.cfg, &late, &Input::Observed(queued(1)), NOW + 3_600_000).unwrap();
    assert_eq!((names(&again), c.revlog), (vec![task::DRAIN], Some(NOW + 3_600_000)));
}

#[test_id(tev1p3z)]
#[test]
fn ten_ticks_with_the_runs_drain_unanswered_request_it_once() {
    let mut r = stuck(task::DRAIN, &queued(1));
    r.advance(10 * 2_000).unwrap();
    assert_eq!(asked(&r, task::DRAIN), 1);
}

#[test_id(tinlnid)]
#[test]
fn a_snapshot_drain_unanswered_does_not_hold_back_a_new_run() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(serde_json::to_string(&Snapshot { dir: "ship".into(), revlog: Queue::default(), snapshot: queue(1) }).unwrap()));
    let door = seen.clone();
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(door.lock().unwrap().clone())).hanging(task::DRAIN_SNAPSHOT);
    let (_, stream, kind) = spira_runtime::tasks::b::kit::boot::<H>();
    r.tick(stream, kind).unwrap();
    r.advance(2_000).unwrap();
    assert_eq!((asked(&r, task::DRAIN_SNAPSHOT), asked(&r, task::DRAIN)), (1, 0));
    *seen.lock().unwrap() = serde_json::to_string(&both()).unwrap();
    r.advance(2_000).unwrap();
    assert_eq!((asked(&r, task::DRAIN_SNAPSHOT), asked(&r, task::DRAIN)), (1, 1), "the run has its own drain; the snapshot's is not asked twice");
}

#[test_id(t529grr)]
#[test]
fn a_refused_drain_is_asked_again_at_the_next_tick() {
    let seen = serde_json::to_string(&queued(1)).unwrap();
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(seen.clone())).double(task::DRAIN, |_| Err("403".into()));
    let (_, stream, kind) = spira_runtime::tasks::b::kit::boot::<H>();
    r.tick(stream, kind).unwrap();
    r.advance(3 * 2_000).unwrap();
    assert_eq!(asked(&r, task::DRAIN), 4);
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
    assert_eq!((seen.revlog.queued, seen.revlog.oldest.as_str(), seen.snapshot.queued), (2, "1-1", 0));
    let none: Snapshot = serde_json::from_str(&task::observe(run.path(), "", "").unwrap()).unwrap();
    assert_eq!((none.revlog.queued, none.snapshot.queued), (0, 0));
}

#[test_id(tayo7sv)]
#[test]
fn the_drain_copies_to_a_directory_destination_and_marks_the_run_sent() {
    let run = TempDir::new("lc-ship-drain");
    let (dir, off) = (run.path().join("ship"), run.path().join("off"));
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    let name = state_store_sqlite::durability::revlog::run_name(1, 1);
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    let url = format!("file://{}", off.display());
    let said = task::drain(&url, &dir.display().to_string(), |_| None, PART_BYTES).unwrap();
    assert_eq!(said, "1 object(s) uploaded");
    assert!(off.join("revlog").join(&name).exists() && dir.join("sent/revlog").join(&name).exists() && !dir.join("queue/revlog").join(&name).exists());
    assert_eq!(std::fs::read_to_string(dir.join("shipped")).unwrap(), "1");
}

/// A loopback HTTP listener that records the target of every request and accepts it. No
/// process is started.
fn accepting_listener() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let targets: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let log = targets.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let log = log.clone();
            std::thread::spawn(move || {
                let conn = conn.unwrap();
                let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
                let mut w = conn;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    log.lock().unwrap().push(line.split_whitespace().nth(1).unwrap().to_string());
                    let mut len = 0;
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        let Some((k, v)) = h.trim_end().split_once(':') else { break };
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                    r.read_exact(&mut vec![0u8; len]).unwrap();
                    if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").is_err() {
                        return;
                    }
                }
            });
        }
    });
    (endpoint, targets)
}

/// systemd's `EnvironmentFile=` grammar as far as a credentials file uses it.
fn read_env_file(p: &std::path::Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

const PART_BYTES: u64 = 8 << 20;

#[test_id(t4okypm)]
#[test]
fn the_runtime_units_environment_files_let_the_drain_sign_an_s3_upload() {
    let run = TempDir::new("lc-ship-unit-env");
    let home = run.path().join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::write(
        home.join(".config/lc-backup.env"),
        "# the backup's write-only key\nAWS_ACCESS_KEY_ID=AKFAKE\nAWS_SECRET_ACCESS_KEY=\"fake\"\nAWS_DEFAULT_REGION=us-test-1\nAWS_ENDPOINT_URL_S3=http://s3.test\n",
    )
    .unwrap();
    let files: Vec<std::path::PathBuf> = include_str!("../../../systemd/spira-runtime.service")
        .lines()
        .filter_map(|l| l.strip_prefix("EnvironmentFile="))
        .map(|f| std::path::PathBuf::from(f.trim_start_matches('-').replace("%h", &home.display().to_string())))
        .collect();
    assert!(!files.is_empty(), "spira-runtime.service loads no EnvironmentFile");
    let mut env: std::collections::HashMap<String, String> = files.iter().flat_map(|f| read_env_file(f)).collect();
    let (endpoint, targets) = accepting_listener();
    env.insert("AWS_ENDPOINT_URL_S3".into(), endpoint);

    let (dir, name) = (run.path().join("ship"), state_store_sqlite::durability::revlog::run_name(1, 1));
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    let said = task::drain("s3://bucket/lc/", &dir.display().to_string(), |k| env.get(k).cloned(), PART_BYTES).unwrap();
    assert_eq!(said, "1 object(s) uploaded");
    let urls = targets.lock().unwrap().clone();
    assert!(urls.iter().any(|u| u.starts_with("/bucket/lc/revlog/")), "{urls:?}");
    assert!(dir.join("sent/revlog").join(&name).exists() && !dir.join("queue/revlog").join(&name).exists());

    let refused = task::drain("s3://bucket/lc/", &dir.display().to_string(), |k| env.get(k).cloned(), 1).unwrap_err();
    assert!(refused.contains("SPIRA_LC_BACKUP_PART_BYTES"), "{refused}");
}

#[test_id(ti9vbp7)]
#[test]
fn check_credentials_refuses_an_acting_s3_destination_without_the_key_and_names_it() {
    use runtime::Mode;
    let none = |_: &str| None::<String>;
    let err = task::check_credentials(Mode::Act, "s3://b/p/", none).unwrap_err();
    assert!(err.starts_with("lc.ship: ") && err.contains("AWS_ACCESS_KEY_ID"), "{err}");
    assert!(task::check_credentials(Mode::Observe, "s3://b/p/", none).is_ok());
    assert!(task::check_credentials(Mode::Act, "", none).is_ok());
    assert!(task::check_credentials(Mode::Act, "file:///tmp/off", none).is_ok());
    assert!(task::check_credentials(Mode::Act, "ftp://x", none).is_err());
    let full = |k: &str| match k {
        "AWS_ACCESS_KEY_ID" | "AWS_SECRET_ACCESS_KEY" => Some("v".to_string()),
        "AWS_DEFAULT_REGION" => Some("r".to_string()),
        _ => None,
    };
    assert!(task::check_credentials(Mode::Act, "s3://b/p/", full).is_ok());
}

fn draining_rig(fail_first: bool) -> runtime::taskrig::TaskRig {
    let asked = std::sync::atomic::AtomicUsize::new(0);
    t::rig::<H>(1, SET, &serde_json::to_string(&queued(3)).unwrap()).double(task::DRAIN, move |_| {
        let n = asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if fail_first && n < 5 { Err("403".into()) } else { Ok("3 object(s) uploaded".into()) }
    })
}

#[test_id(tcm9k2d)]
#[test]
fn a_draining_cycle_commits_no_turn() {
    let mut rig = draining_rig(true);
    let after_boot = rig.journal.commits().len();
    for _ in 0..20 {
        rig.advance(2_000).unwrap();
    }
    assert_eq!(rig.journal.commits().len(), after_boot, "{:?}", rig.journal.commits());
    let drains: Vec<_> = rig.requested().iter().filter(|q| q.name == task::DRAIN).collect();
    assert!(drains.len() >= 5, "{}", drains.len());
    assert!(drains.iter().all(|q| !q.journaled));
}

#[test_id(tr7w1xe)]
#[test]
fn a_restart_mid_drain_drains_again() {
    let mut rig = draining_rig(false).hanging(task::DRAIN);
    for _ in 0..3 {
        rig.advance(2_000).unwrap();
    }
    assert!(rig.requested().iter().any(|q| q.name == task::DRAIN));
    let before = rig.requested().iter().filter(|q| q.name == task::DRAIN).count();
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    assert!(rig.journal.state().is_empty(), "{:?}", rig.journal.state().keys());
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    rig.advance(2_000).unwrap();
    assert!(rig.requested().iter().filter(|q| q.name == task::DRAIN).count() > before);
}

#[test_id(tuvfnxm)]
#[test]
fn each_kind_drains_alone_and_an_upload_in_progress_is_not_an_object() {
    let run = TempDir::new("lc-ship-kinds");
    let (dir, off) = (run.path().join("ship"), run.path().join("off"));
    for k in ["revlog", "snapshot"] {
        std::fs::create_dir_all(dir.join("queue").join(k)).unwrap();
    }
    let name = state_store_sqlite::durability::revlog::run_name(1, 1);
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    std::fs::write(dir.join("queue/snapshot/7"), "x").unwrap();
    std::fs::write(dir.join("queue/snapshot/7.part"), "x").unwrap();
    std::fs::write(dir.join("queue/snapshot/7.complete"), "x").unwrap();
    let (url, d) = (format!("file://{}", off.display()), dir.display().to_string());
    assert_eq!(task::drain(&url, &d, |_| None, PART_BYTES).unwrap(), "1 object(s) uploaded");
    assert!(dir.join("queue/snapshot/7").exists() && !off.join("snapshot").exists(), "the runs drain leaves the snapshot queued");
    assert_eq!(task::drain_snapshot(&url, &d, |_| None, PART_BYTES).unwrap(), "1 object(s) uploaded");
    assert!(!dir.join("queue/snapshot/7").exists() && off.join("snapshot/7").exists());
    assert!(!off.join("snapshot/7.part").exists() && !off.join("snapshot/7.complete").exists() && dir.join("queue/snapshot/7.part").exists());
}

fn last_drain(dir: &std::path::Path) -> task::LastDrain {
    serde_json::from_str(&std::fs::read_to_string(dir.join(task::LAST_DRAIN)).unwrap()).unwrap()
}

#[test_id(tavd8ug)]
#[test]
fn a_refused_drain_leaves_last_drain_naming_the_refusal_and_a_good_one_clears_the_error() {
    let run = TempDir::new("lc-ship-last-drain");
    let (dir, off) = (run.path().join("ship"), run.path().join("off"));
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    let name = state_store_sqlite::durability::revlog::run_name(1, 1);
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    let d = dir.display().to_string();
    let s3 = |lookup: fn(&str) -> Option<String>| task::drain("s3://bucket/lc/", &d, lookup, PART_BYTES);

    let refused = s3(|_| None).unwrap_err();
    let last = last_drain(&dir);
    assert_eq!((last.kind.as_str(), last.uploaded, last.error.as_deref()), ("revlog", 0, Some(refused.as_str())));
    assert!(refused.contains("AWS_ACCESS_KEY_ID") && last.at_ms > 1_700_000_000_000, "{last:?}");

    let url = format!("file://{}", off.display());
    task::drain(&url, &d, |_| None, PART_BYTES).unwrap();
    assert_eq!(last_drain(&dir), task::LastDrain { at_ms: last_drain(&dir).at_ms, kind: "revlog".into(), uploaded: 1, error: None });
}

#[test_id(tks9cpu)]
#[test]
fn an_upload_refused_after_some_objects_went_records_how_many_and_why() {
    let run = TempDir::new("lc-ship-last-drain-partial");
    let dir = run.path().join("ship");
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    std::fs::write(dir.join("queue/revlog").join(state_store_sqlite::durability::revlog::run_name(1, 1)), "x").unwrap();
    let blocked = run.path().join("blocked");
    std::fs::write(&blocked, "").unwrap();
    let url = format!("file://{}", blocked.display());
    let err = task::drain(&url, &dir.display().to_string(), |_| None, PART_BYTES).unwrap_err();
    let last = last_drain(&dir);
    assert_eq!((last.uploaded, last.error.as_deref()), (0, Some(err.as_str())));
}

#[test_id(td2ur11)]
#[test]
fn a_phase_row_an_older_release_left_is_deleted_by_the_first_boot_and_not_written_again() {
    use runtime::journal::{Journal, StateRow, Turn};
    let mut rig = t::rig::<H>(1, SET, &serde_json::to_string(&queued(0)).unwrap());
    let row = StateRow { task: "lc.ship".into(), skey: "phase".into(), value: "\"draining\"".into(), at: 1 };
    rig.journal.commit(&Turn { state: vec![row], ..Turn::default() }).unwrap();
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    assert!(rig.journal.state().keys().all(|(t, _)| t != "lc.ship"), "{:?}", rig.journal.state().keys());
    let cleared: Vec<_> = rig.journal.commits().iter().filter(|c| !c.cleared.is_empty()).map(|c| c.cleared.clone()).collect();
    assert_eq!(cleared, vec![vec![("lc.ship".to_string(), "phase".to_string())]]);
    let commits = rig.journal.commits().len();
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    assert_eq!(rig.journal.commits().len(), commits);
}
