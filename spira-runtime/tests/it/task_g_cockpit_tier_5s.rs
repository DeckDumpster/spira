//! Acceptance T1–T6 for `cockpit-tier-5s`, from its contract
//! (`wiki/projects/spira/designs/tasks/cockpit-tier-5s.md`, Acceptance) on `TaskRig`. Red until
//! the port lands: `decide` and `step` are `unimplemented!`, and the row waits in
//! `tasks::g::PENDING`. The three sub-probes ride one observe, so T2's one-probe rule holds. T6 runs
//! two virtual days (the contract asks one).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::g::cockpit::{Carry, Fragment, FragmentState, Snapshot};
use spira_runtime::tasks::g::cockpit_tier_5s::*;
use spira_runtime::tasks;

const CADENCE_MS: u64 = 5_000;
const BOGUS: &str = "cockpit-tier-5s.bogus";
const MODULE: &str = include_str!("../../src/tasks/g/cockpit_tier_5s.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> CockpitTier5s {
    CockpitTier5s { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(supervised: bool, round: &str) -> Snapshot {
    let frag = |kv: &[(&str, &str)]| Fragment { status: FragmentState::Ok, at: 0, killed: false, kv: kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    Snapshot {
        supervised,
        fragments: [("cockpit.now".to_string(), frag(&[("SP_DWARVES", "0")])), ("cockpit.round".to_string(), frag(&[("SP_ROUND", round)])), ("cockpit.inflight".to_string(), frag(&[("SP_INFLIGHT", "")]))].into(),
    }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(PUBLISH, |i| Ok(i.into()));
    let (_, stream, kind) = CockpitTier5s::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test_id(ta34w83)]
#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence_with_its_three_probes() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::g::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    assert_eq!(PROBES.iter().map(|p| p.0).collect::<Vec<_>>(), ["cockpit.now", "cockpit.round", "cockpit.inflight", "cockpit.view"]);
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test_id(tsk3l9m)]
#[test]
fn t2_an_unchanged_snapshot_publishes_nothing_and_journals_nothing() {
    let mut r = rig(1, |_| snap(true, ""));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test_id(tsn3a9g)]
#[test]
fn a_tick_where_only_ages_moved_journals_nothing() {
    let mut r = rig(1, |n| {
        let mut s = snap(true, "");
        let now = s.fragments.get_mut("cockpit.now").unwrap();
        now.kv.insert("SP_SENTINEL_AGE".into(), (10 + n).to_string());
        now.kv.insert("SP_DWARF0_MIN".into(), (3 + n).to_string());
        s
    });
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test_id(twn0ape)]
#[test]
fn t3_never_ok_stale_error_timeout_ok_is_legal_and_stale_before_ok_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(FragmentState::Never, "stale").expect_err("a fragment never read cannot go stale");
    assert!(refused.contains("Never"), "{refused}");
}

#[test_id(tbnreqo)]
#[test]
fn t3_a_publish_from_an_unsupervised_instance_is_refused_naming_may_write() {
    let refused = decide(&Carry::default(), &snap(false, "r1")).expect_err("only the supervised instance writes");
    assert!(refused.contains("may_write"), "{refused}");
    let (next, _) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next, "the carry read is the carry recorded");
}

#[test_id(t0ju3vm)]
#[test]
fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
    let row = tasks::all().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS, "the row declares the module's effects");
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&[BOGUS], &real, &sim).is_err(), "the planted {BOGUS} must be refused");
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(t07ugmm)]
#[test]
fn t4_the_module_launches_no_process() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("{l}(\"x\")")), Some(*l), "the scan must see {l}");
    }
    assert_eq!(launcher(MODULE), None);
}

#[test_id(tnapdj2)]
#[test]
fn t5_parity_the_merged_kv_carries_every_fragments_keys() {
    let (_, acts) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [PUBLISH]);
    for kv in ["SP_DWARVES=0", "SP_ROUND=r1", "SP_INFLIGHT="] {
        assert!(acts[0].input.contains(kv), "{kv}: {}", acts[0].input);
    }
}

#[test_id(tyvyy52)]
#[test]
fn t5_parity_an_unchanged_digest_is_not_published_again() {
    let (published, _) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    let (_, acts) = decide(&published, &snap(true, "r1")).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
}

fn with_clock(mut s: Snapshot) -> Snapshot {
    let f = s.fragments.values_mut().next().unwrap();
    f.at = 10;
    f.kv.insert("SP_AT".into(), "10".into());
    s
}

fn boot_matches(make: impl Fn() -> TaskRig) {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(make, || handler().registration(), STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test_id(t1nhfs0)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig(1, |_| with_clock(snap(true, ""))), NAME);
    for k in ["digest", "fragments/cockpit.now"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test_id(t2wfkba)]
#[test]
fn a_boot_does_not_republish_a_snapshot_whose_digest_it_published() {
    boot_matches(|| rig(1, |_| snap(true, "")));
}

#[test_id(t08us4w)]
#[test]
fn a_boot_does_not_refresh_a_snapshot_no_newer_than_the_last_read() {
    boot_matches(|| rig(1, |_| with_clock(snap(true, ""))));
}

mod document {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use cockpit_ops::lcview::{document_path, Snapshot};
    use runtime::event::EffectSpec;
    use spira_runtime::tasks::g::cockpit::{self, Carry, FragmentState, WRITE_KEY};
    use spira_runtime::tasks::g::cockpit_tier_5s::{decide, PUBLISH};
    use spira_runtime::world::World;
    use spira_runtime::Effects;

    type Answers = Arc<Mutex<BTreeMap<String, String>>>;

    /// The scheduler holds one document in process, so the tests of it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    struct Rig {
        _dir: testkit::TempDir,
        run: PathBuf,
        view: PathBuf,
        pool: PathBuf,
        answers: Answers,
        fx: Effects,
    }

    fn live(rows: &[(&str, &str)]) -> String {
        serde_json::Value::Array(rows.iter().map(|(id, title)| serde_json::json!({"bead_id": id, "state": "WORKING", "repo": "spira", "title": title, "priority": 1, "updated_at": 100, "since": 100, "holder": "1@1"})).collect()).to_string()
    }

    fn rig(name: &str) -> Rig {
        let dir = testkit::TempDir::new(name);
        let (run, view, state) = (dir.join("run"), dir.join("runtime/spira-cockpit"), dir.join("vm"));
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let socket = dir.join("lc.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let answers: Answers = Arc::new(Mutex::new(BTreeMap::new()));
        let served = answers.clone();
        std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| Ok(served.lock().unwrap().get(&argv.join(" ")).cloned().unwrap_or_else(|| "[]".into()))));
        let pool = state.join("pool.json");
        std::fs::write(&pool, serde_json::json!({"leases": [{"vm": {"handle": "109", "addr": "10.0.0.9"}, "owner": null, "since": 100, "run": "r-1"}]}).to_string()).unwrap();
        let units = Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0))));
        let vm_pool = round_vm::pool::Pool { state_dir: state, retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::from_millis(10), acquire_deadline: Duration::ZERO };
        let world = World { run_dir: run.clone(), lc_socket: socket, cockpit_view_dir: view.clone(), ..World::default() };
        let fx = Effects { world, units: Some(units), vm_pool: Some(Arc::new(vm_pool)), ..Effects::default() };
        answers.lock().unwrap().insert("list --live".into(), live(&[("sp-a", "first")]));
        Rig { _dir: dir, run, view, pool, answers, fx }
    }

    fn observe(fx: &Effects, supervised: bool, probe: &str) -> cockpit::Snapshot {
        let cfg: BTreeMap<String, String> = [(WRITE_KEY.to_string(), if supervised { "1" } else { "0" }.to_string())].into();
        let spec = EffectSpec::probe("observe", cockpit::probe_input(&[(probe, 60_000)], &cfg), 60_000);
        serde_json::from_str(&cockpit::run_observe(fx, "", &spec).unwrap()).unwrap()
    }

    /// One pass of both tiers and the 5 s tier's publish, as the scheduler runs them.
    fn pass(fx: &Effects) {
        let fast = observe(fx, true, "cockpit.view");
        assert_eq!(fast.fragments["cockpit.view"].status, FragmentState::Ok);
        assert!(fast.fragments["cockpit.view"].kv.is_empty(), "the view is not journaled: it answers no keys");
        observe(fx, true, "cockpit.view_slow");
        let (_, acts) = decide(&Carry::default(), &fast).unwrap();
        assert!(acts.iter().all(|a| a.name == PUBLISH));
        let publish = EffectSpec::keyed(PUBLISH, "k", "", 1_000);
        cockpit::run_publish(fx, "", &publish).unwrap();
    }

    fn read(view: &Path) -> Snapshot {
        serde_json::from_slice(&std::fs::read(document_path(view)).unwrap()).unwrap()
    }

    fn stat(view: &Path) -> (u64, std::time::SystemTime) {
        let m = std::fs::metadata(document_path(view)).unwrap();
        (m.ino(), m.modified().unwrap())
    }

    fn age_the_file(view: &Path) -> std::time::SystemTime {
        let old = UNIX_EPOCH + Duration::from_secs(1_000);
        std::fs::File::open(document_path(view)).unwrap().set_modified(old).unwrap();
        old
    }

    #[test_id(tvccpgh)]
    #[test]
    fn the_document_is_replaced_only_by_a_change_and_touched_by_a_quiet_pass() {
        let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
        let r = rig("cockpit-document");
        assert!(!document_path(&r.view).exists());
        observe(&r.fx, true, "cockpit.view");
        cockpit::view::publish(&r.view).unwrap();
        assert!(!document_path(&r.view).exists(), "nothing is written until both groups have been read once");

        const T: u64 = 1_800_000_000;
        let at = |secs: u64| spira_config::vtime::with_now_for_test(T + secs, || pass(&r.fx));
        at(0);
        let first = read(&r.view);
        assert_eq!(first.now, T as i64);
        assert_eq!(first.rows.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["sp-a"]);
        assert_eq!((first.vm_runs.len(), first.vm_runs[0].since), (1, 100), "a VM is stored by its start, not by its age");

        let old = age_the_file(&r.view);
        let (inode, _) = stat(&r.view);
        at(5);
        let (inode_after, mtime_after) = stat(&r.view);
        assert_eq!(inode_after, inode, "a later clock and a VM that has aged since do not replace the document");
        assert!(mtime_after > old, "the pass touches it, so the file's age is the pass's");
        assert_eq!(read(&r.view).now, T as i64, "the clock fields alone are not written");

        r.answers.lock().unwrap().insert("list --live".into(), live(&[("sp-a", "first"), ("sp-b", "second")]));
        at(10);
        assert_ne!(stat(&r.view).0, inode, "a new row replaces the document");
        let changed = read(&r.view);
        assert_eq!((changed.rows.len(), changed.now), (2, T as i64 + 10));

        std::fs::write(&r.pool, "{ torn").unwrap();
        at(15);
        let kept = read(&r.view);
        assert_eq!((kept.vm_runs.len(), kept.vm_runs[0].since), (1, 100), "a source that fails keeps its last read");
        assert!(kept.errors.iter().any(|e| e.contains("pool.json")), "and names the error: {:?}", kept.errors);
        assert_eq!(kept.rows.len(), 2);
        std::fs::remove_dir_all(&r.run).unwrap();
    }

    #[test_id(t7eqvxp)]
    #[test]
    fn a_publish_with_no_runtime_directory_refuses_by_name_before_it_writes_anything() {
        let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
        let mut r = rig("cockpit-document-no-runtime-dir");
        r.fx.world.cockpit_view_dir = PathBuf::new();
        let publish = EffectSpec::keyed(PUBLISH, "k", "SP_X=1\n", 1_000);
        let refused = cockpit::run_publish(&r.fx, "", &publish).unwrap_err();
        assert!(refused.contains("XDG_RUNTIME_DIR"), "{refused}");
        assert!(!r.run.join("cockpit.env").exists(), "the env file waits for the document's directory too");
    }

    #[test_id(tfga3he)]
    #[test]
    fn an_unsupervised_instance_gathers_and_writes_nothing() {
        let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
        let r = rig("cockpit-document-unsupervised");
        let snap = observe(&r.fx, false, "cockpit.view");
        assert!(decide(&Carry::default(), &snap).unwrap_err().contains("may_write"));
        assert!(!document_path(&r.view).exists());
        assert!(!r.view.exists(), "not even its directory");
    }
}

#[test_id(tzbwt1s)]
#[test]
fn a_change_under_a_prefix_the_tiers_reads_depend_on_wakes_it_before_its_cadence() {
    let s = spira_runtime::fixture::fixture();
    let wake = spira_runtime::registrations(&s).into_iter().find(|r| r.name == NAME).and_then(|r| r.wake).expect("the tier declares its reads");
    assert!(wake.polls, "its cadence stands for the clock-driven fields");
    for key in ["lifecycle/sp-a", "readiness/sp-a", "batches/spira/b-1", "planning/watermark", "conditions/heimdall/disk", "asks/sp-q", "mendings/b-1", "landed/at/0000000001/sp-a"] {
        assert!(wake.reads.iter().any(|p| key.starts_with(p)), "{key} does not wake {NAME}: {:?}", wake.reads);
    }
}
