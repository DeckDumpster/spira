//! A quiet tick of each task that read the store on every tick is silent on the store's socket:
//! sentinel.pass, batcher.local/forge, sift and the cockpit round probe answer from the
//! projection the watch keeps current. The store behind the projection is a double that counts what
//! it is asked; the socket the tasks are handed is a listener that counts connections. One test:
//! the projection is installed once per process.

use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use batcher_cut::io::{self, Env, Hypervisor, Land, Repo};
use serde_json::{json, Value};
use spira_store::lc_call::{LcClient, Reply, Transport};
use spira_runtime::tasks::a::sentinel_pass::beads_on;
use spira_runtime::world::World;
use store_view::Projection;

struct Store {
    rows: Vec<Value>,
    asked: Mutex<Vec<String>>,
}

impl Transport for Store {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new() });
        let named = |flag: &str| argv.iter().position(|a| a == flag).map(|i| argv[i + 1].clone());
        match argv[0].as_str() {
            "rc" => ok(json!({ "ok": { "items": [], "list_rev": 5 } }).to_string()),
            "list" if argv.iter().any(|a| a == "--batches") => {
                self.asked.lock().unwrap().push("list --batches".into());
                ok("[]".into())
            }
            "list" => ok(Value::Array(self.rows.clone()).to_string()),
            "watch" => {
                // clock-safe: the double's long-poll, ending after a pause
                std::thread::sleep(Duration::from_millis(5));
                ok(json!({ "progress_rev": 5 }).to_string())
            }
            "content" => {
                self.asked.lock().unwrap().push("content".into());
                ok("[]".into())
            }
            "show-batch" => {
                self.asked.lock().unwrap().push("show-batch".into());
                ok(json!({ "pass": 1 }).to_string())
            }
            "plan" => {
                self.asked.lock().unwrap().push("plan".into());
                let items: Vec<Value> = named("--ids").iter().flat_map(|ids| ids.split(',')).map(|id| json!({ "id": id, "title": "t", "priority": 2, "description": "d", "notes": "", "dependencies": [] })).collect();
                ok(json!({ "watermark": 1, "items": items }).to_string())
            }
            other => Err(format!("unscripted {other}")),
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn row(id: &str, state: &str, tip: &str, stack: &str) -> Value {
    json!({ "bead_id": id, "state": state, "tip": tip, "holds": "[]", "express": "0", "repo": "alpha", "stack": stack, "updated_at": 4_000_000_000_i64 })
}

fn env(dir: &Path) -> Env {
    Env {
        lc_rows: Default::default(),
        home: dir.to_path_buf(),
        run: dir.join("run"),
        queue_dir: dir.join("queue"),
        forge: "forge".into(),
        tsd_bin: None,
        round_vm: "round-vm".into(),
        queue_bin: "queue".into(),
        rebase_stale_bin: "rebase-stale".into(),
        testenv_bin: "testenv".into(),
        gate_bin: "gate".into(),
        round_slots: None,
        hypervisor: Hypervisor { host_mib: 0, round_vm_mib: 0, runner_mib: 0 },
        poll: Duration::from_secs(1),
        maxpar: 1,
        wall_secs: 1,
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_bin: None,
        lc_timeout: 1,
        verdicts: dir.join("verdicts"),
        land_lock_attempts: 1,
        land_lock_wait: Duration::from_secs(1),
        orphan_redrive_after: 1,
        pause_lift_after: 1,
        land_unpause: None,
    }
}

#[test]
fn a_quiet_tick_of_each_task_that_read_the_store_every_tick_never_reaches_its_socket() {
    let dir = testkit::TempDir::new("store-quiet-tick");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let reached = Arc::new(AtomicUsize::new(0));
    let counted = reached.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });

    let repo_dir = dir.path().join("alpha");
    std::fs::create_dir(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    git(&repo_dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
    let tip = git(&repo_dir, &["rev-parse", "HEAD"]);
    git(&repo_dir, &["branch", "spira/sp-a"]);

    let store = Arc::new(Store { rows: vec![row("sp-a", "CERTIFIED", &tip, "{\"sp-p\":\"t0\"}"), row("sp-p", "LANDED", "t0", "{}")], asked: Mutex::new(vec![]) });
    struct Shared(Arc<Store>);
    impl Transport for Shared {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
            self.0.exchange(argv, stdin, d)
        }
    }
    let views = Projection::new(LcClient::over(Arc::new(Shared(store.clone())), true), Duration::from_secs(30), Arc::new(std::time::Instant::now));
    let _watch = spira_runtime::store::run(views).expect("the first install stands");
    let installed = store_view::shared().unwrap();
    // clock-safe: a cap on the wait for the watch thread to start
    for _ in 0..1000 {
        if installed.watching() {
            break;
        }
        // clock-safe: a poll interval under that cap
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(installed.watching());

    let lc = LcClient::socket(&sock);
    let env = env(dir.path());
    let repo = Repo { name: "alpha".into(), path: repo_dir.clone(), base: "main".into(), forge: "forge".into(), land: Land::Local };
    let map = format!("alpha|{}|queue.local|main\n", repo_dir.display());
    let reg = spira_config::repos::Registry::new(Some(&map), &BTreeMap::new(), dir.path());
    let reads = round_controller::sifts::Reads { lc: &lc, views: &installed, reg: &reg, wiki: "", identity_projects: "", work: dir.path().join("sift") };

    let map = dir.path().join("repos");
    std::fs::write(&map, "alpha|/x|queue.local|main\n").unwrap();
    let world = World { lc_socket: sock.clone(), repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), ..World::default() };

    let tick = || {
        assert!(beads_on(&lc, &[], &[]).is_some(), "sentinel.pass reads its content rows");
        let pool = io::certified_pool(&env, &repo).expect("batcher reads its pool");
        assert_eq!(pool.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["sp-a"]);
        assert_eq!(io::lcq_held(&env, &["show-batch", "b-1"]).unwrap(), json!({ "pass": 1 }).to_string(), "batcher reads a round's pass");
        round_controller::sifts::observe_repo(&reads, "alpha", true).expect("sift reads its repo");
        assert_eq!(world.supersede_snapshot("{}").unwrap(), json!({ "held": [] }).to_string(), "supersede-duty lists the manual holds");
        assert_eq!(world.held_asks().unwrap(), Vec::<String>::new(), "the asks task lists the ask holds");
        assert_eq!(world.waiting_rows().unwrap().as_array().map(Vec::len), Some(0), "watch.throughput counts the waiting rows");
        assert!(spira_runtime::reconcile_host::lc_read(&sock, &["list", "--state", "CERTIFIED"]).unwrap().contains("sp-a"), "the reconciler lists CERTIFIED");
        assert!(cockpit_collect::probes::lc::state_index_of(&["sp-a".to_string()]).unwrap().contains_key("sp-a"), "the cockpit reads its ids");
        assert!(cockpit_collect::probes::lc::held_rows("manual").unwrap().is_empty(), "the cockpit lists the manual holds");
        assert_eq!(cockpit_collect::probes::round::list_batches_live().as_deref(), Some("[]"), "the cockpit reads the round history");
    };
    tick();
    let first = store.asked.lock().unwrap().clone();
    tick();
    tick();
    assert_eq!(*store.asked.lock().unwrap(), first, "a quiet store is asked each question once, however many ticks follow");
    for verb in ["content", "plan", "show-batch", "list --batches"] {
        assert!(first.iter().any(|a| a == verb), "the tick never read {verb} through the views: {first:?}");
    }
    assert_eq!(reached.load(Ordering::SeqCst), 0, "no task reached the store's socket on a quiet tick");
}
