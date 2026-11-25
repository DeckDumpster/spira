//! `watch.off_box`: the Concierge's inbox hears when the control store's newest off-box revision
//! falls behind, measured from what was uploaded.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_off_box::*;
use testkit::TempDir;

const BOUND_S: u64 = 90;
const CYCLE_S: u64 = 334;
const SNAPSHOT_S: u64 = 1_200;
const KEY_ERROR: &str = "AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not in the environment";

type World = Arc<Mutex<Result<Snapshot, String>>>;

fn config() -> Config {
    Config { max_age_s: BOUND_S, snapshot_s: SNAPSHOT_S, cycle_s: CYCLE_S }
}

fn seen(head: u64, shipped: u64, last_error: Option<&str>) -> Snapshot {
    Snapshot::Seen { head, shipped, shipped_at_ms: 1_795_369_405_000, last_error: last_error.map(str::to_string), oldest_snapshot_ms: None }
}

fn registration() -> runtime::event::Registration {
    WatchOffBox::new(config()).registration()
}

fn rig(world: &World) -> TaskRig {
    rig_with(world, config())
}

fn rig_with(world: &World, cfg: Config) -> TaskRig {
    let w = world.clone();
    let mut r = TaskRig::new(1, WatchOffBox::new(cfg).registration()).unwrap().double(OBSERVE, move |_| w.lock().unwrap().clone().map(|s| serde_json::to_string(&s).unwrap())).double("inbox.append", |_| Ok(String::new()));
    let (_, stream, kind) = WatchOffBox::boot();
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == "inbox.append").map(|q| q.input.clone()).collect()
}

fn run_for(r: &mut TaskRig, secs: u64) {
    for _ in 0..secs / 15 {
        r.advance(CADENCE_MS).unwrap();
    }
    acc::assert_healthy(r, STREAM);
}

fn stalled_world(error: Option<&str>) -> World {
    Arc::new(Mutex::new(Ok(seen(1_859_610, 1_859_605, error))))
}

fn grow(world: &World, by: u64) {
    if let Ok(Snapshot::Seen { head, .. }) = &mut *world.lock().unwrap() {
        *head += by;
    }
}

#[test_id(t8doxi1)]
#[test]
fn a_stalled_uploader_pages_once_with_the_wait_the_revision_and_the_error_and_once_more_when_current() {
    let world = stalled_world(Some(KEY_ERROR));
    let mut r = rig(&world);
    for _ in 0..(BOUND_S / 15 + 2) {
        grow(&world, 10);
        r.advance(CADENCE_MS).unwrap();
    }
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("[watch:off-box] RED: ") && said[0].contains("1859605") && said[0].contains(KEY_ERROR) && said[0].contains(&format!("past {BOUND_S}s")), "{said:?}");
    for _ in 0..80 {
        grow(&world, 10);
        r.advance(CADENCE_MS).unwrap();
    }
    assert_eq!(lines(&r).len(), 1, "the standing red says nothing more");
    let head = match &*world.lock().unwrap() {
        Ok(Snapshot::Seen { head, .. }) => *head,
        other => panic!("{other:?}"),
    };
    *world.lock().unwrap() = Ok(seen(head, head, None));
    r.advance(CADENCE_MS).unwrap();
    let said = lines(&r);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].contains("CLEAR: ") && said[1].contains(&head.to_string()), "{said:?}");
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 2);
}

#[test_id(tksy815)]
#[test]
fn a_stalled_stager_with_the_queue_empty_pages_the_same() {
    let world = stalled_world(None);
    let mut r = rig(&world);
    run_for(&mut r, CYCLE_S - 15);
    assert!(lines(&r).is_empty(), "inside the shipper's worst healthy cycle a wait with no refusal is not news");
    run_for(&mut r, 60);
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("RED: ") && said[0].contains("1859605") && said[0].contains("error: none") && said[0].contains(&format!("past {CYCLE_S}s")), "{said:?}");
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 1);
}

#[test_id(tvhoh7u)]
#[test]
fn a_copy_that_follows_its_head_inside_the_bound_is_silent_and_so_is_an_idle_store() {
    let world: World = Arc::new(Mutex::new(Ok(seen(1_000, 1_000, None))));
    let mut r = rig(&world);
    for i in 0..400u64 {
        *world.lock().unwrap() = Ok(seen(1_000 + 10 * (i + 1), 1_000 + 10 * i, None));
        r.advance(CADENCE_MS).unwrap();
    }
    assert!(lines(&r).is_empty(), "{:?}", lines(&r));
    *world.lock().unwrap() = Ok(seen(5_000, 5_000, None));
    run_for(&mut r, 6 * 3_600);
    assert!(lines(&r).is_empty(), "an idle store whose shipped is hours old is not behind");
}

#[test_id(tj7vx9p)]
#[test]
fn an_observe_failing_past_the_bound_says_cannot_tell_with_the_probes_error_once() {
    let world: World = Arc::new(Mutex::new(Err("lc-serve: connection refused".into())));
    let mut r = rig(&world);
    run_for(&mut r, BOUND_S - 15);
    assert!(lines(&r).is_empty(), "inside the bound a failed observe is not yet news");
    run_for(&mut r, 60);
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("[watch:off-box] CANNOT TELL: ") && said[0].contains("lc-serve: connection refused"), "{said:?}");
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 1);
    *world.lock().unwrap() = Ok(seen(7, 7, None));
    r.advance(CADENCE_MS).unwrap();
    assert!(lines(&r).last().is_some_and(|l| l.contains("CLEAR: ")), "{:?}", lines(&r));
}

#[test_id(t8skra3)]
#[test]
fn an_observe_that_times_out_past_the_bound_says_cannot_tell_too() {
    let world: World = Arc::new(Mutex::new(Ok(seen(1, 1, None))));
    let mut r = TaskRig::new(1, registration()).unwrap().timing_out().double("inbox.append", |_| Ok(String::new()));
    let (_, stream, kind) = WatchOffBox::boot();
    r.tick(stream, kind).unwrap();
    run_for(&mut r, BOUND_S + 60);
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("CANNOT TELL: ") && said[0].contains("timed out"), "{said:?}");
    drop(world);
}

#[test_id(tbw6hry)]
#[test]
fn a_restart_mid_red_neither_pages_again_nor_loses_the_crossing() {
    let world = stalled_world(Some(KEY_ERROR));
    let mut r = rig(&world);
    run_for(&mut r, BOUND_S + 30);
    assert_eq!(lines(&r).len(), 1);
    r.reopen(registration()).unwrap();
    let (_, stream, kind) = WatchOffBox::boot();
    r.tick(stream, kind).unwrap();
    run_for(&mut r, 300);
    assert_eq!(lines(&r).len(), 1, "{:?}", lines(&r));
    *world.lock().unwrap() = Ok(seen(1_859_610, 1_859_610, None));
    r.advance(CADENCE_MS).unwrap();
    assert!(lines(&r).last().is_some_and(|l| l.contains("CLEAR: ")), "the red survived the restart: {:?}", lines(&r));
}

#[test_id(trqodug)]
#[test]
fn the_clock_of_a_wait_survives_a_restart() {
    let world = stalled_world(None);
    let mut r = rig(&world);
    run_for(&mut r, CYCLE_S - 30);
    assert!(lines(&r).is_empty());
    r.reopen(registration()).unwrap();
    let (_, stream, kind) = WatchOffBox::boot();
    r.tick(stream, kind).unwrap();
    run_for(&mut r, 60);
    assert_eq!(lines(&r).len(), 1, "the wait counts from before the restart: {:?}", lines(&r));
}

#[test_id(t1pz7wk)]
#[test]
fn a_staged_snapshot_past_its_bound_pages_with_its_own_wording_and_clears() {
    let stale = |at: Option<u64>| Ok(Snapshot::Seen { head: 9, shipped: 9, shipped_at_ms: 1, last_error: None, oldest_snapshot_ms: at });
    let world: World = Arc::new(Mutex::new(stale(Some(0))));
    let mut r = rig(&world);
    run_for(&mut r, SNAPSHOT_S - 60);
    assert!(lines(&r).is_empty());
    run_for(&mut r, 120);
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("RED: ") && said[0].contains("snapshot") && said[0].contains(&format!("past {SNAPSHOT_S}s")), "{said:?}");
    run_for(&mut r, 300);
    assert_eq!(lines(&r).len(), 1);
    *world.lock().unwrap() = stale(None);
    r.advance(CADENCE_MS).unwrap();
    assert!(lines(&r).last().is_some_and(|l| l.contains("CLEAR: ") && l.contains("snapshot")), "{:?}", lines(&r));
}

#[test_id(ti1k80d)]
#[test]
fn no_destination_is_never_red() {
    let world: World = Arc::new(Mutex::new(Ok(Snapshot::NoDestination)));
    let mut r = rig(&world);
    run_for(&mut r, 3_600);
    assert!(lines(&r).is_empty());
}

#[test_id(tke398n)]
#[test]
fn the_real_observe_reads_shipped_and_the_drains_error_and_refuses_a_missing_or_unreadable_shipped() {
    let dir = TempDir::new("off-box-observe");
    let head = || Ok(120);
    assert!(observe_dir("s3://b/p/", dir.path(), head).is_err(), "an absent shipped is an error, never 0");
    std::fs::write(dir.path().join("shipped"), "junk").unwrap();
    assert!(observe_dir("s3://b/p/", dir.path(), head).is_err(), "an unparsable shipped is an error");
    std::fs::write(dir.path().join("shipped"), "100\n").unwrap();
    match observe_dir("s3://b/p/", dir.path(), head).unwrap() {
        Snapshot::Seen { head, shipped, shipped_at_ms, last_error, oldest_snapshot_ms } => {
            assert_eq!((head, shipped, last_error, oldest_snapshot_ms), (120, 100, None, None));
            assert!(shipped_at_ms > 1_700_000_000_000, "{shipped_at_ms}");
        }
        other => panic!("{other:?}"),
    }
    std::fs::write(dir.path().join("last-drain.json"), r#"{"at_ms":1,"kind":"revlog","uploaded":0,"error":"403"}"#).unwrap();
    std::fs::create_dir_all(dir.path().join("queue/snapshot")).unwrap();
    std::fs::write(dir.path().join("queue/snapshot/7"), "x").unwrap();
    match observe_dir("s3://b/p/", dir.path(), head).unwrap() {
        Snapshot::Seen { last_error, oldest_snapshot_ms, .. } => assert_eq!((last_error.as_deref(), oldest_snapshot_ms.is_some()), (Some("403"), true)),
        other => panic!("{other:?}"),
    }
    let asked = Mutex::new(false);
    assert_eq!(observe_dir("", dir.path(), || { *asked.lock().unwrap() = true; Ok(0) }).unwrap(), Snapshot::NoDestination);
    assert!(!*asked.lock().unwrap(), "no destination asks lc-serve nothing");
    assert!(observe_dir("s3://b/p/", dir.path(), || Err("no control store".into())).unwrap_err().contains("no control store"));
}

#[test_id(t0d08rx)]
#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_fifteen_second_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), 15_000);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test_id(tf0u05h)]
#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_off_box.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tyaaqin)]
#[test]
fn the_carry_is_kept_as_rows_and_a_boot_repeats_no_page() {
    let red = || rig(&stalled_world(Some(KEY_ERROR)));
    let mut r = red();
    run_for(&mut r, BOUND_S + 30);
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert_eq!(rows, ["blind_red", "blind_since_ms", "red", "snapshot_red", "waiting"]);
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(red, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test_id(tyjr72g)]
#[test]
fn removing_the_destination_clears_both_open_pages() {
    let stale = Ok(Snapshot::Seen { head: 9, shipped: 5, shipped_at_ms: 1, last_error: None, oldest_snapshot_ms: Some(0) });
    let world: World = Arc::new(Mutex::new(stale));
    let mut r = rig(&world);
    run_for(&mut r, SNAPSHOT_S + 60);
    assert_eq!(lines(&r).len(), 2, "{:?}", lines(&r));
    *world.lock().unwrap() = Ok(Snapshot::NoDestination);
    r.advance(CADENCE_MS).unwrap();
    let said = lines(&r);
    assert_eq!(said.len(), 4, "{said:?}");
    assert!(said[2..].iter().all(|l| l.contains("CLEAR: ")) && said[2..].iter().any(|l| l.contains("snapshot")), "{said:?}");
}

const SHAPE_MAX_AGE_S: u64 = 60;

fn shape_config() -> Config {
    Config { max_age_s: SHAPE_MAX_AGE_S, ..config() }
}

#[test_id(tn1xsn4)]
#[test]
fn the_waits_the_scheduler_stalls_make_page_nothing() {
    let drained = || Ok(seen(10, 10, None));
    let stalled = || Ok(seen(11, 10, None));
    let blind = || Err("lc-serve: connection refused".to_string());
    let shapes: [Vec<(Result<Snapshot, String>, u64)>; 5] = [
        vec![(stalled(), 75), (drained(), 30)],
        vec![(stalled(), 60), (drained(), 30)],
        vec![(stalled(), 90), (blind(), 15), (drained(), 30)],
        vec![(stalled(), 60), (drained(), 15), (Ok(seen(12, 10, None)), 60), (drained(), 30)],
        vec![(blind(), 30), (drained(), 600)],
    ];
    for (n, shape) in shapes.into_iter().enumerate() {
        let world: World = Arc::new(Mutex::new(drained()));
        let mut r = rig_with(&world, shape_config());
        for (answer, secs) in shape {
            *world.lock().unwrap() = answer;
            run_for(&mut r, secs);
        }
        assert!(lines(&r).is_empty(), "shape {n}: {:?}", lines(&r));
    }
}

#[test_id(ts7b6xs)]
#[test]
fn a_refused_revlog_drain_pages_past_the_bound_and_an_error_free_stall_only_past_the_cycle() {
    let world = stalled_world(Some(KEY_ERROR));
    let mut r = rig_with(&world, shape_config());
    run_for(&mut r, SHAPE_MAX_AGE_S - 15);
    assert!(lines(&r).is_empty());
    run_for(&mut r, 30);
    assert_eq!(lines(&r).len(), 1, "{:?}", lines(&r));
    assert!(lines(&r)[0].contains(KEY_ERROR), "{:?}", lines(&r));
    let world = stalled_world(None);
    let mut r = rig_with(&world, shape_config());
    run_for(&mut r, CYCLE_S + 30);
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 1, "{:?}", lines(&r));
}

#[test_id(t3x44cj)]
#[test]
fn a_snapshot_drains_refusal_is_not_a_run_uploads() {
    let dir = TempDir::new("off-box-snapshot-refusal");
    std::fs::write(dir.path().join("shipped"), "100\n").unwrap();
    std::fs::write(dir.path().join("last-drain.json"), r#"{"at_ms":1,"kind":"snapshot","uploaded":0,"error":"403"}"#).unwrap();
    match observe_dir("s3://b/p/", dir.path(), || Ok(120)).unwrap() {
        Snapshot::Seen { last_error, .. } => assert_eq!(last_error, None),
        other => panic!("{other:?}"),
    }
    std::fs::write(dir.path().join("last-drain.json"), r#"{"at_ms":1,"kind":"revlog","uploaded":0,"error":"403"}"#).unwrap();
    match observe_dir("s3://b/p/", dir.path(), || Ok(120)).unwrap() {
        Snapshot::Seen { last_error, .. } => assert_eq!(last_error.as_deref(), Some("403")),
        other => panic!("{other:?}"),
    }
}

#[test_id(tmbj1jp)]
#[test]
fn a_standing_red_survives_an_err_and_blindness_says_cannot_tell_once_across_a_reopen_then_clears() {
    let world = stalled_world(Some(KEY_ERROR));
    let mut r = rig(&world);
    run_for(&mut r, BOUND_S + 30);
    assert_eq!(lines(&r).len(), 1);
    *world.lock().unwrap() = Err("lc-serve: connection refused".into());
    run_for(&mut r, BOUND_S - 15);
    assert_eq!(lines(&r).len(), 1, "an Err inside the bound neither clears the RED nor adds a line: {:?}", lines(&r));
    run_for(&mut r, 60);
    let said = lines(&r);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].contains("CANNOT TELL: ") && said[1].contains("stands"), "{said:?}");
    r.reopen(registration()).unwrap();
    let (_, stream, kind) = WatchOffBox::boot();
    r.tick(stream, kind).unwrap();
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 2, "blindness is said once, across a reopen: {:?}", lines(&r));
    *world.lock().unwrap() = Ok(seen(1_859_610, 1_859_610, None));
    r.advance(CADENCE_MS).unwrap();
    let said = lines(&r);
    assert_eq!(said.len(), 4, "{said:?}");
    assert!(said[2..].iter().all(|l| l.contains("CLEAR: ")), "{said:?}");
    run_for(&mut r, 600);
    assert_eq!(lines(&r).len(), 4);
}

#[test_id(tntjdq8)]
#[test]
fn the_cycle_is_an_upload_deadline_plus_the_staging_period_plus_two_ship_ticks_plus_a_tick() {
    assert_eq!(cycle_s(60, 2), 334);
    assert_eq!(cycle_s(100, 4), 300 + 25 + 8 + 15);
    assert_eq!(cycle_s(8, 5), 300 + 5 + 10 + 15);
}

#[test_id(tw2w1y7)]
#[test]
fn a_snapshot_refusal_beside_a_stalled_revlog_pages_only_past_the_cycle() {
    let dir = TempDir::new("off-box-snapshot-stall");
    std::fs::write(dir.path().join("shipped"), "100\n").unwrap();
    std::fs::write(dir.path().join("last-drain.json"), r#"{"at_ms":1,"kind":"snapshot","uploaded":0,"error":"403"}"#).unwrap();
    let observed = observe_dir("s3://b/p/", dir.path(), || Ok(120)).unwrap();
    let world: World = Arc::new(Mutex::new(Ok(observed)));
    let mut r = rig(&world);
    run_for(&mut r, CYCLE_S - 15);
    assert!(lines(&r).is_empty(), "{:?}", lines(&r));
    run_for(&mut r, 45);
    assert_eq!(lines(&r).len(), 1, "{:?}", lines(&r));
}

const BASE: u64 = 2_346_000;

fn states(r: &TaskRig) -> usize {
    r.journaled().iter().filter(|e| e.kind.tag == STATE).count()
}

fn own_writes_head(r: &TaskRig, others: u64) -> u64 {
    BASE + others + states(r) as u64
}

/// One tick with the head every carry write has moved; `lag` ships the previous tick's head.
fn tick_following(r: &mut TaskRig, world: &World, others: u64, prev_head: &mut u64, lag: bool) {
    let head = own_writes_head(r, others);
    let shipped = if lag { *prev_head } else { head };
    *world.lock().unwrap() = Ok(seen(head, shipped, None));
    *prev_head = head;
    r.advance(CADENCE_MS).unwrap();
}

#[test_id(tzr0w3k)]
#[test]
fn an_idle_store_gains_no_carry_writes_from_its_own_carry_write_whether_the_shipper_lags_a_tick_or_not() {
    for lag in [true, false] {
        let world: World = Arc::new(Mutex::new(Ok(seen(BASE, BASE, None))));
        let mut r = rig(&world);
        let mut prev = BASE;
        for _ in 0..4 {
            tick_following(&mut r, &world, 1, &mut prev, lag);
        }
        let settled = states(&r);
        for _ in 0..236 {
            tick_following(&mut r, &world, 1, &mut prev, lag);
        }
        assert!(states(&r) == settled && settled <= 3, "lag={lag}: {} state events", states(&r));
        assert!(lines(&r).is_empty(), "{:?}", lines(&r));
    }
}

#[test_id(t4m8cpe)]
#[test]
fn another_writers_revision_costs_at_most_two_carry_writes_and_none_once_shipped() {
    let world: World = Arc::new(Mutex::new(Ok(seen(BASE, BASE, None))));
    let mut r = rig(&world);
    let mut prev = BASE;
    let mut others = 0;
    for _ in 0..8 {
        tick_following(&mut r, &world, others, &mut prev, true);
    }
    let quiet = states(&r);
    others += 1;
    for _ in 0..40 {
        tick_following(&mut r, &world, others, &mut prev, true);
    }
    assert!(states(&r) - quiet <= 2, "{} writes for one revision", states(&r) - quiet);
    let after = states(&r);
    for _ in 0..40 {
        tick_following(&mut r, &world, others, &mut prev, true);
    }
    assert_eq!(states(&r), after);
    assert!(lines(&r).is_empty());
}

#[test_id(t9h2vqa)]
#[test]
fn another_writers_revision_that_is_never_shipped_pages_red_past_the_cycle_despite_the_tasks_own_write() {
    let world: World = Arc::new(Mutex::new(Ok(seen(BASE, BASE, None))));
    let mut r = rig(&world);
    let mut prev = BASE;
    for _ in 0..8 {
        tick_following(&mut r, &world, 0, &mut prev, false);
    }
    let shipped = own_writes_head(&r, 0);
    let settled = states(&r);
    let mut after_cycle = false;
    for tick in 0..(CYCLE_S / 15 + 3) {
        let head = shipped + 1 + (states(&r) - settled) as u64;
        *world.lock().unwrap() = Ok(seen(head, shipped, None));
        r.advance(CADENCE_MS).unwrap();
        if (tick + 1) * 15 <= CYCLE_S - 15 {
            assert!(lines(&r).is_empty(), "no page inside the cycle: {:?}", lines(&r));
        }
        after_cycle = !lines(&r).is_empty();
    }
    assert!(after_cycle, "the unshipped revision pages");
    let said = lines(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("RED: "), "{said:?}");
}
