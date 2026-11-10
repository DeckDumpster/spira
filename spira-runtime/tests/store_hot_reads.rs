//! The scheduler's hot reads answered from the projection the store's watch keeps current: a
//! state change shows within one watch round-trip, a cut watch makes every reader refuse, and
//! none of it touches the store's socket. One test: the projection is installed once per process.

use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_config::lc_call::{LcClient, Reply, Transport};
use spira_runtime::tasks::a::czar::{certified_pool, Read};
use spira_runtime::watchers::round_duty::{RoundState, Snapshot};
use spira_runtime::world::World;
use store_view::Projection;

struct Store {
    rows: Vec<Value>,
}

struct Double {
    store: Arc<Mutex<Store>>,
    deliveries: Mutex<Receiver<String>>,
    cut: Arc<AtomicBool>,
}

fn bead(id: &str, state: &str, holds: &str) -> Value {
    json!({ "bead_id": id, "state": state, "holds": holds, "express": "0", "repo": "alpha", "updated_at": 4_000_000_000_i64 })
}

fn open_round() -> Value {
    let data = json!({
        "batch_id": "b-open", "repo": "alpha", "state": "Open", "parent": null, "head": "h1", "base": "b0", "run": null,
        "reason": null, "pass": 0, "phase": null, "version": 1, "members": [{ "bead_id": "a-1", "tip": "t1" }], "opened_at": 1_700_000_000_i64,
    });
    json!({ "key": "batches/alpha/b-open", "value": { "metadata": {}, "data": data }, "mod_rev": { "main": 3, "sub": 0 }, "create_rev": { "main": 3, "sub": 0 }, "lock_expires_at_ms": null })
}

impl Transport for Double {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new() });
        match argv[0].as_str() {
            "rc" => ok(json!({ "ok": { "items": [open_round()], "list_rev": 5 } }).to_string()),
            "list" => {
                let rows = self.store.lock().unwrap().rows.clone();
                let ids: Option<Vec<&str>> = argv.iter().position(|a| a == "--ids").map(|i| argv[i + 1].split(',').collect());
                let kept: Vec<Value> = rows.into_iter().filter(|r| ids.as_ref().is_none_or(|ids| ids.contains(&r["bead_id"].as_str().unwrap()))).collect();
                ok(Value::Array(kept).to_string())
            }
            "watch" if self.cut.load(Ordering::SeqCst) => Err("the watch is cut".into()),
            "watch" => match self.deliveries.lock().unwrap().recv_timeout(Duration::from_millis(20)) {
                Ok(answer) => ok(answer),
                Err(RecvTimeoutError::Timeout) => ok(json!({ "progress_rev": 5 }).to_string()),
                Err(RecvTimeoutError::Disconnected) => Err("closed".into()),
            },
            other => Err(format!("unscripted {other}")),
        }
    }
}

fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let began = Instant::now();
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(began.elapsed() < Duration::from_secs(5), "never saw: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn hot_reads_follow_the_watch_refuse_when_it_is_cut_and_never_ask_the_store() {
    let dir = testkit::TempDir::new("store-hot-reads");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = asked.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let map = dir.path().join("repos");
    std::fs::write(&map, "alpha|/x|queue.local|main\n").unwrap();
    let world = World { lc_socket: sock.clone(), repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), ..World::default() };
    let snapshot = || world.round_snapshot("{}").map(|s| serde_json::from_str::<Snapshot>(&s).unwrap());

    let store = Arc::new(Mutex::new(Store { rows: vec![bead("a-1", "SUBMITTED", "[]"), bead("a-2", "SUBMITTED", "[]"), bead("a-3", "SUBMITTED", "[\"ask\"]"), bead("a-9", "CERTIFIED", "[]")] }));
    let (deliver, deliveries): (Sender<String>, Receiver<String>) = channel();
    let cut = Arc::new(AtomicBool::new(false));
    let double = Double { store: store.clone(), deliveries: Mutex::new(deliveries), cut: cut.clone() };
    let views = Projection::new(LcClient::over(Arc::new(double), true), Duration::from_millis(400), Arc::new(Instant::now));
    let _watch = spira_runtime::store::run(views).expect("the first install stands");

    let snap = snapshot().unwrap();
    assert_eq!((snap.submitted, snap.certified, snap.state, snap.open), (2, 1, RoundState::Open, 1));
    assert_eq!(snap.unadvanced, [("b-open".to_string(), 1_700_000_000)]);

    store.lock().unwrap().rows[0] = bead("a-1", "CERTIFIED", "[]");
    deliver.send(json!({ "events": [{ "key": "lifecycle/a-1", "rev": { "main": 6, "sub": 0 }, "value": {}, "create_rev": null, "prev_mod_rev": null }], "through": 6 }).to_string()).unwrap();
    let moved = until("the delivered change in the round snapshot", || snapshot().ok().filter(|s| s.certified == 2));
    assert_eq!(moved.submitted, 1);
    let lc = LcClient::socket(&sock);
    assert_eq!(certified_pool(&lc, "alpha"), Read::Observed(vec!["a-1".to_string(), "a-9".to_string()]));

    let second = Projection::new(LcClient::over(Arc::new(Double { store, deliveries: Mutex::new(channel().1), cut: cut.clone() }), true), Duration::from_secs(1), Arc::new(Instant::now));
    assert!(spira_runtime::store::run(second).is_err(), "a second projection never replaces the one readers hold");

    cut.store(true, Ordering::SeqCst);
    until("the round snapshot refusing once the watch is cut", || snapshot().err());
    assert!(matches!(certified_pool(&lc, "alpha"), Read::Unobservable(why) if why.contains("the watch is lost")));
    assert_eq!(asked.load(Ordering::SeqCst), 0, "no hot read reached the store's socket, cut or not");
}
