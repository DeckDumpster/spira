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
    let said = task::drain(std::sync::Arc::new(Refusing), &url, &dir.display().to_string(), |_| None, PART_BYTES).unwrap();
    assert_eq!(said, "1 object(s) uploaded");
    assert!(off.join("revlog").join(&name).exists() && dir.join("sent/revlog").join(&name).exists() && !dir.join("queue/revlog").join(&name).exists());
    assert_eq!(std::fs::read_to_string(dir.join("shipped")).unwrap(), "1");
}

/// A fake curl that records the URL of every PUT and accepts it. No process is started.
#[derive(Default)]
struct RecordingCurl(std::sync::Mutex<Vec<String>>);

impl runtime::ports::Proc for RecordingCurl {
    fn run(&self, spec: &runtime::ports::ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
        assert_eq!(spec.prog, "curl");
        self.0.lock().unwrap().push(spec.args.last().cloned().unwrap_or_default());
        Ok(runtime::ports::ProcOutput { code: Some(0), stdout: Vec::new(), stderr: Vec::new() })
    }
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
    let env: std::collections::HashMap<String, String> = files.iter().flat_map(|f| read_env_file(f)).collect();

    let (dir, name) = (run.path().join("ship"), state_store_sqlite::durability::revlog::run_name(1, 1));
    std::fs::create_dir_all(dir.join("queue/revlog")).unwrap();
    std::fs::write(dir.join("queue/revlog").join(&name), "x").unwrap();
    let curl = std::sync::Arc::new(RecordingCurl::default());
    let said = task::drain(curl.clone(), "s3://bucket/lc/", &dir.display().to_string(), |k| env.get(k).cloned(), PART_BYTES).unwrap();
    assert_eq!(said, "1 object(s) uploaded");
    let urls = curl.0.lock().unwrap().clone();
    assert!(urls.iter().any(|u| u.starts_with("http://s3.test/bucket/lc/revlog/")), "{urls:?}");
    assert!(dir.join("sent/revlog").join(&name).exists() && !dir.join("queue/revlog").join(&name).exists());

    let refused = task::drain(curl, "s3://bucket/lc/", &dir.display().to_string(), |k| env.get(k).cloned(), 1).unwrap_err();
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
