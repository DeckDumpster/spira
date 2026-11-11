use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_config::lc_call::{LcClient, LcError, Reply, Transport};
use store_view::{Projection, RowQuery};

/// A scripted store: `rc list` and `list` answer from what it holds, each `watch` takes the next
/// scripted answer, and every request is recorded.
#[derive(Default)]
struct Script {
    batches: Vec<Value>,
    list_rev: u64,
    rows: Vec<Value>,
    watches: VecDeque<(i32, String)>,
    seen: Vec<Vec<String>>,
    /// A watch with nothing scripted waits a moment and reports no change, as a quiet store's does.
    quiet: bool,
}

struct Double(Arc<Mutex<Script>>);

impl Transport for Double {
    fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
        let mut s = self.0.lock().unwrap();
        s.seen.push(argv.to_vec());
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new() });
        match argv[0].as_str() {
            "rc" => ok(json!({ "ok": { "items": s.batches, "list_rev": s.list_rev } }).to_string()),
            "list" => {
                let ids: Option<Vec<&str>> = argv.iter().position(|a| a == "--ids").map(|i| argv[i + 1].split(',').collect());
                let rows: Vec<&Value> = s.rows.iter().filter(|r| ids.as_ref().is_none_or(|ids| ids.contains(&r["bead_id"].as_str().unwrap()))).collect();
                ok(serde_json::to_string(&rows).unwrap())
            }
            "watch" => match s.watches.pop_front() {
                Some((code, out)) => Ok(Reply { code, out, err: String::new() }),
                None if s.quiet => {
                    drop(s);
                    // clock-safe: the double's long-poll, ending after a pause
                    std::thread::sleep(Duration::from_millis(5));
                    ok(json!({ "progress_rev": 1 }).to_string())
                }
                None => Err("the script has no watch answer left".into()),
            },
            "content" | "show-batch" => ok(format!("answer {}", s.seen.iter().filter(|a| a[0] == argv[0]).count())),
            other => Err(format!("unscripted {other}")),
        }
    }
}

/// Every test's deadline. Nothing here waits on a socket or a clock, so a test still running
/// at this bound is parked on a lock, and fails as that rather than holding the target open.
const DEADLINE: Duration = Duration::from_secs(10);

/// Runs `body` on its own thread and fails the test once it is past [`DEADLINE`]; a panic in the
/// body is the test's own failure.
fn within(body: impl FnOnce() + Send + 'static) {
    let (done, finished) = mpsc::channel();
    let t = std::thread::spawn(move || {
        body();
        let _ = done.send(());
    });
    match finished.recv_timeout(DEADLINE) {
        Ok(()) => t.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(t.join().unwrap_err()),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("still running {}s in: a test here waits on nothing but a lock, so it is deadlocked", DEADLINE.as_secs()),
    }
}

fn rev(n: u64) -> Value {
    json!({ "main": n, "sub": 0 })
}

/// A round record as the queue's own store fixture cuts it (queue/src/batches.rs `cut_round`):
/// the machine's fields, with the queue's in `round`.
fn round(repo: &str, id: &str, state: &str, opened: i64, created: u64, own: &[(&str, &str)]) -> Value {
    let own: serde_json::Map<String, Value> = own.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect();
    let mut data = json!({
        "batch_id": id, "repo": repo, "state": state, "parent": null, "head": format!("head-{id}"), "base": "b0",
        "run": null, "reason": null, "pass": 0, "phase": null, "version": 1,
        "members": [{ "bead_id": "sp-a", "tip": "ta" }], "opened_at": opened,
    });
    if !own.is_empty() {
        data["round"] = Value::Object(own);
    }
    json!({ "key": format!("batches/{repo}/{id}"), "value": { "metadata": {}, "data": data }, "mod_rev": rev(created), "create_rev": rev(created), "lock_expires_at_ms": null })
}

/// queue's `seed` for a forge batch on the store (queue/src/tests/forge_store.rs).
fn forge(id: &str, phase: &str, opened: u64, pr: &str, created: u64) -> Value {
    let branch = format!("spira/queue/{id}");
    let opened_s = opened.to_string();
    round("spira", id, "Open", opened as i64, created, &[("head", "h1"), ("base", "b0"), ("members", "sp-a:ta"), ("opened", &opened_s), ("branch", &branch), ("owner", "batcher"), ("forge.phase", phase), ("pr", pr)])
}

fn bead(id: &str, state: &str, holds: &str, verdict: Option<&str>, express: bool) -> Value {
    json!({ "bead_id": id, "state": state, "holds": holds, "express": if express { "1" } else { "0" }, "claim_verdict": verdict, "repo": "spira", "updated_at": 4_000_000_000_i64 })
}

struct World {
    script: Arc<Mutex<Script>>,
    now: Arc<Mutex<Instant>>,
    p: Projection,
}

fn world(script: Script, stale: Duration) -> World {
    let script = Arc::new(Mutex::new(script));
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock_now = Arc::clone(&now);
    let client = LcClient::over(Arc::new(Double(Arc::clone(&script))), true);
    let p = Projection::new(client, stale, Arc::new(move || *clock_now.lock().unwrap()));
    World { script, now, p }
}

impl World {
    fn advance(&self, d: Duration) {
        *self.now.lock().unwrap() += d;
    }
    fn watch(&self, answer: Value) {
        self.script.lock().unwrap().watches.push_back((0, answer.to_string()));
    }
}

#[test]
fn the_batch_view_answers_what_batch_show_prints_for_the_store_record() {
    within(|| {
        let w = world(
            Script {
                batches: vec![
                    forge("deck-1", "armed", 900, "12", 5),
                    round("spira", "deck-0", "Abandoned", 800, 3, &[("forge.phase", "armed"), ("pr", "11")]),
                    round("spira", "deck-2", "Open", 950, 7, &[("forge.phase", "armed"), ("ended", "abandoned:red")]),
                    round("spira", "spira-9", "CiRunning", 990, 8, &[]),
                    forge("other-1", "armed", 900, "40", 2).as_object().map(|o| {
                        let mut o = o.clone();
                        o["key"] = json!("batches/deck/other-1");
                        o["value"]["data"]["repo"] = json!("deck");
                        Value::Object(o)
                    }).unwrap(),
                ],
                list_rev: 9,
                ..Default::default()
            },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        let shown = w.p.batch_view("spira").unwrap();
        assert_eq!(shown.rev, 9);
        assert_eq!(
            shown.value.shown(),
            "batch_id=deck-1\nrepo=spira\nhead=h1\nbase=b0\nmembers=sp-a:ta\nopened=900\nbranch=spira/queue/deck-1\nforge.phase=armed\nowner=batcher\npr=12\n",
            "an ended, an abandoned and another repo's batch are not the open one"
        );
        let rounds = w.p.round_view("spira").unwrap().value.rounds;
        assert_eq!(rounds.iter().map(|r| (r.record.id.as_str(), r.phase.as_str())).collect::<Vec<_>>(), [("deck-1", "opened"), ("spira-9", "certifying")]);
        assert_eq!(rounds[1].record.get("members"), Some("sp-a:ta"), "a round the queue never wrote reads from the machine's fields");
        assert_eq!(w.p.batch_view("nowhere").unwrap().value.shown(), "", "no batch open, nothing shown");
    });
}

#[test]
fn a_view_refuses_before_its_first_load_and_once_its_revision_is_stale_until_the_store_confirms_it() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_millis(1500));
        assert!(matches!(w.p.batch_view("spira"), Err(LcError::Refused(m)) if m.contains("not loaded")));
        w.p.poll().unwrap();
        w.advance(Duration::from_millis(1400));
        assert_eq!(w.p.ready_counts().unwrap().rev, 4);
        w.advance(Duration::from_millis(200));
        for refusal in [w.p.batch_view("spira").err(), w.p.round_view("spira").err(), w.p.ready_counts().err()] {
            assert!(matches!(&refusal, Some(LcError::Refused(m)) if m.starts_with("stale past 1500ms")), "{refusal:?}");
        }
        assert!(w.p.poll().is_err(), "a watch with no answer");
        assert!(w.p.ready_counts().is_err(), "a failed poll confirms nothing");
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        assert_eq!(w.p.ready_counts().unwrap().rev, 6, "a progress answer confirms the views at its revision");
    });
}

#[test]
fn a_watch_delivery_moves_the_batch_and_rereads_only_the_beads_it_names() {
    within(|| {
        let w = world(
            Script {
                batches: vec![forge("deck-1", "armed", 900, "12", 5)],
                list_rev: 9,
                rows: vec![bead("sp-a", "READY", "[]", Some("claimable"), true), bead("sp-b", "READY", "[\"wait\"]", None, false), bead("sp-c", "WORKING", "[]", None, false)],
                ..Default::default()
            },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        let c = w.p.ready_counts().unwrap().value;
        assert_eq!((c.claimable, c.judged_claimable, c.express, c.by_state["READY"]), (1, 1, 1, 2));

        let mut landed = forge("deck-1", "armed", 900, "12", 5);
        landed["value"]["data"]["state"] = json!("Landed");
        let next = forge("deck-3", "opening", 990, "13", 11);
        w.script.lock().unwrap().rows = vec![bead("sp-a", "WORKING", "[]", None, true), bead("sp-b", "READY", "[]", Some("claimable"), false), bead("sp-c", "READY", "[]", None, false)];
        w.watch(json!({
            "events": [
                { "key": "batches/spira/deck-1", "rev": rev(10), "value": landed["value"], "create_rev": null, "prev_mod_rev": rev(5) },
                { "key": "batches/spira/deck-3", "rev": rev(11), "value": next["value"], "create_rev": rev(11), "prev_mod_rev": null },
                { "key": "lifecycle/sp-a", "rev": rev(12), "value": {}, "create_rev": null, "prev_mod_rev": rev(2) },
                { "key": "readiness/sp-b", "rev": rev(12), "value": {}, "create_rev": null, "prev_mod_rev": rev(2) },
            ],
            "through": 12,
        }));
        w.p.poll().unwrap();
        let shown = w.p.batch_view("spira").unwrap();
        assert_eq!((shown.rev, shown.value.open.as_ref().map(|r| r.id.as_str())), (12, Some("deck-3")), "the landed batch left, the new one is open");
        let c = w.p.ready_counts().unwrap().value;
        assert_eq!((c.claimable, c.judged_claimable, c.express, c.by_state["WORKING"]), (1, 1, 0, 2), "sp-c was not named, so its old row stands");
        let last = w.script.lock().unwrap().seen.last().cloned().unwrap();
        assert_eq!(&last, &["list", "--ids", "sp-a,sp-b"].map(String::from), "only the beads the watch named are re-read");

        w.watch(json!({ "too_old": { "compacted": 20 }, "items": [], "list_rev": 21 }));
        w.script.lock().unwrap().list_rev = 21;
        w.p.poll().unwrap();
        let reloaded = w.p.ready_counts().unwrap();
        assert_eq!((reloaded.rev, reloaded.value.by_state.get("WORKING")), (21, Some(&1)), "a compacted revision reloads every view, sp-c's row with it");
    });
}

#[test]
fn a_batches_refresh_rereads_only_the_batch_records_and_the_ready_counts_refuse_until_a_full_load() {
    within(|| {
        let w = world(
            Script { batches: vec![forge("deck-1", "armed", 900, "12", 5)], list_rev: 9, rows: vec![bead("sp-a", "READY", "[]", Some("claimable"), false)], ..Default::default() },
            Duration::from_secs(10),
        );
        w.p.refresh_batches().unwrap();
        let shown = w.p.batch_view("spira").unwrap();
        assert_eq!((shown.rev, shown.value.open.map(|r| r.id)), (9, Some("deck-1".to_string())));
        assert_eq!(w.script.lock().unwrap().seen.iter().map(|a| a[0].as_str()).collect::<Vec<_>>(), ["rc"], "no lifecycle row is read");
        assert!(matches!(w.p.ready_counts(), Err(LcError::Refused(m)) if m.contains("lifecycle rows")), "the batches alone say nothing of the rows");

        w.p.poll().unwrap();
        assert_eq!(w.p.ready_counts().unwrap().value.claimable, 1, "a poll on a batches-only picture loads every view");

        let mut landed = forge("deck-1", "armed", 900, "12", 5);
        landed["value"]["data"]["state"] = json!("Landed");
        {
            let mut s = w.script.lock().unwrap();
            s.batches = vec![landed, forge("deck-3", "opening", 990, "13", 11)];
            s.list_rev = 14;
            s.seen.clear();
        }
        w.p.refresh_batches().unwrap();
        let shown = w.p.batch_view("spira").unwrap();
        assert_eq!((shown.rev, shown.value.open.map(|r| r.id)), (14, Some("deck-3".to_string())), "the refresh replaced every batch record");
        assert_eq!(w.script.lock().unwrap().seen.len(), 1, "one rc list, nothing else");
        assert!(w.p.ready_counts().is_err(), "rows read at 9 are not passed off as revision 14");

        w.advance(Duration::from_secs(11));
        assert!(matches!(w.p.batch_view("spira"), Err(LcError::Refused(m)) if m.starts_with("stale past")));
    });
}

#[test]
fn the_ended_view_is_each_forge_batch_the_machine_or_the_queue_ended_newest_first() {
    within(|| {
        let w = world(
            Script {
                batches: vec![
                    forge("deck-1", "armed", 900, "12", 5),
                    round("spira", "deck-0", "Abandoned", 800, 3, &[("batch_id", "deck-0"), ("pr", "11"), ("opened", "800"), ("members", "sp-a:ta"), ("forge.phase", "armed"), ("reason", "red")]),
                    round("spira", "deck-2", "Open", 950, 7, &[("pr", "13"), ("opened", "950"), ("forge.phase", "armed"), ("ended", "abandoned")]),
                    round("spira", "spira-9", "Abandoned", 990, 8, &[]),
                    round("deck", "other-1", "Landed", 999, 2, &[("forge.phase", "armed"), ("pr", "40")]),
                ],
                list_rev: 9,
                ..Default::default()
            },
            Duration::from_secs(10),
        );
        assert!(matches!(w.p.batch_view_ended("spira"), Err(LcError::Refused(m)) if m.contains("not loaded")));
        w.p.refresh_batches().unwrap();
        let ended = w.p.batch_view_ended("spira").unwrap();
        assert_eq!(ended.rev, 9);
        assert_eq!(ended.value.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["deck-2", "deck-0"], "the open batch, a local round and another repo's are left out");
        assert_eq!(ended.value[1].render(), "batch_id=deck-0\nrepo=spira\nmembers=sp-a:ta\nopened=800\nforge.phase=armed\npr=11\nreason=red\n", "a batch_id the queue kept in its own fields is not shown twice");
        assert_eq!(ended.value[0].render(), "batch_id=deck-2\nrepo=spira\nopened=950\nended=abandoned\nforge.phase=armed\npr=13\n");
        assert!(w.p.batch_view_ended("nowhere").unwrap().value.is_empty());
    });
}

/// Over-broad-scopes #10 (sp-ipwlic): the view applies three prefixes, so it watches exactly
/// those three, not `""` — a watch of the whole keyspace woke every holder on every event,
/// telemetry and lock write.
#[test]
fn the_watch_names_only_the_prefixes_the_view_applies() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        let watch = w.script.lock().unwrap().seen.iter().find(|a| a[0] == "watch").cloned().unwrap();
        let prefixes: Vec<&str> = watch[1..].iter().take_while(|a| !a.starts_with("--")).map(String::as_str).collect();
        assert_eq!(prefixes, ["batches/", "lifecycle/", "readiness/", "planning/", "history/"], "{watch:?}");
    });
}

#[test]
fn a_state_change_the_watch_delivers_is_served_as_a_row_and_a_pool_with_no_further_read() {
    within(|| {
        let w = world(
            Script { list_rev: 9, rows: vec![bead("sp-a", "SUBMITTED", "[]", None, false), bead("sp-b", "CERTIFIED", "[]", None, false), bead("sp-c", "CERTIFIED", "[\"wait\"]", None, false)], ..Default::default() },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        assert_eq!(w.p.unheld("CERTIFIED", "spira").unwrap().value, ["sp-b"], "a held row is not pool");
        assert!(w.p.unheld("CERTIFIED", "other").unwrap().value.is_empty(), "another repo's pool is not this one's");
        let by = |states: &[&str]| w.p.rows(&RowQuery { states, ..Default::default() }).unwrap().value.iter().map(|r| r["bead_id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(by(&["SUBMITTED", "CERTIFIED"]), ["sp-a", "sp-b", "sp-c"]);

        w.script.lock().unwrap().rows = vec![bead("sp-a", "CERTIFIED", "[]", None, false), bead("sp-b", "LANDED", "[]", None, false), bead("sp-c", "CERTIFIED", "[\"wait\"]", None, false)];
        w.watch(json!({ "events": [
            { "key": "lifecycle/sp-a", "rev": rev(10), "value": {}, "create_rev": null, "prev_mod_rev": rev(2) },
            { "key": "lifecycle/sp-b", "rev": rev(11), "value": {}, "create_rev": null, "prev_mod_rev": rev(2) },
        ], "through": 11 }));
        w.p.poll().unwrap();
        let asked = w.script.lock().unwrap().seen.len();
        assert_eq!(w.p.unheld("CERTIFIED", "spira").unwrap().value, ["sp-a"], "visible after one watch round-trip");
        assert_eq!(by(&["CERTIFIED"]), ["sp-a", "sp-c"]);
        let live = w.p.rows(&RowQuery { live: true, ..Default::default() }).unwrap().value;
        assert_eq!(live.len(), 3, "a terminal row updated within the window is live");
        let later = 4_000_000_000 + store_view::LIVE_WINDOW_SECS + 1;
        let aged: Vec<_> = w.p.rows_at(&RowQuery { live: true, ..Default::default() }, later).unwrap().value.iter().map(|r| r["bead_id"].as_str().unwrap().to_string()).collect();
        assert_eq!(aged, ["sp-a", "sp-c"], "a terminal row older than the window is dropped from live");
        assert_eq!(w.script.lock().unwrap().seen.len(), asked, "serving rows asks the store nothing");
    });
}

#[test]
fn a_cut_watch_makes_every_reader_refuse_and_says_the_watch_is_lost() {
    within(|| {
        let w = world(Script { list_rev: 4, rows: vec![bead("sp-a", "READY", "[]", None, false)], ..Default::default() }, Duration::from_millis(1500));
        w.p.poll().unwrap();
        assert!(w.p.unheld("READY", "spira").is_ok());
        assert!(w.p.poll().is_err(), "the watch was cut");
        w.advance(Duration::from_secs(2));
        for refusal in [w.p.rows(&RowQuery::default()).err(), w.p.unheld("READY", "spira").err(), w.p.batch_view("spira").err()] {
            assert!(matches!(&refusal, Some(LcError::Refused(m)) if m.starts_with("stale past 1500ms") && m.contains("the watch is lost")), "{refusal:?}");
        }
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        assert!(w.p.unheld("READY", "spira").is_ok(), "a poll the store answered ends the refusal");
    });
}

#[test]
fn an_installed_projection_is_what_hot_serves_and_a_watched_one_reads_no_batch_records() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        let script = Arc::clone(&w.script);
        let p = Arc::new(w.p);
        let client = LcClient::over(Arc::new(Double(Arc::clone(&w.script))), true);
        let installed = store_view::install(Arc::clone(&p));
        assert!(Arc::ptr_eq(&installed, &p));
        assert!(Arc::ptr_eq(&Projection::hot(client).unwrap(), &p), "hot serves the installed projection");

        assert!(!p.watching());
        p.batches_current().unwrap();
        let rc_reads = |w: &Mutex<Script>| w.lock().unwrap().seen.iter().filter(|a| a[0] == "rc").count();
        assert_eq!(rc_reads(&script), 1, "an unwatched projection reads the batch records afresh");

        p.load().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for rev in 5..400 {
            script.lock().unwrap().watches.push_back((0, json!({ "progress_rev": rev }).to_string()));
        }
        let runner = {
            let (p, stop) = (Arc::clone(&p), Arc::clone(&stop));
            std::thread::spawn(move || p.run(&stop))
        };
        while !p.watching() {
            std::thread::yield_now();
        }
        let before = rc_reads(&script);
        p.batches_current().unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        runner.join().unwrap();
        assert_eq!(rc_reads(&script), before, "a watched projection asks the store for no batch record");
        assert!(!p.watching());
    });
}

fn asked(w: &World, verb: &str) -> usize {
    w.script.lock().unwrap().seen.iter().filter(|a| a[0] == verb).count()
}

/// Delivers `keys` as one watch answer and returns once the projection has applied it and begun
/// the next watch.
fn deliver(w: &World, keys: &[&str], through: u64) {
    let watches = |w: &World| w.script.lock().unwrap().seen.iter().filter(|a| a[0] == "watch").count();
    let events: Vec<Value> = keys.iter().map(|k| json!({ "key": k, "rev": rev(through), "value": {}, "create_rev": null, "prev_mod_rev": null })).collect();
    let before = watches(w);
    w.watch(json!({ "events": events, "through": through }));
    // clock-safe: a cap on the wait for the watch's next call, which is a named signal
    let give_up = Instant::now() + Duration::from_secs(5);
    while watches(w) < before + 2 {
        assert!(Instant::now() < give_up, "the delivery was never applied");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_held_answer_lasts_until_a_change_that_could_alter_it_and_a_bead_history_alters_none() {
    within(|| {
        let batch = round("spira", "b-1", "Open", 100, 3, &[]);
        let w = world(Script { batches: vec![batch], list_rev: 4, quiet: true, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| w.p.run(&stop));
            // clock-safe: a cap on the wait for the watch to start
            while !w.p.watching() {
                std::thread::sleep(Duration::from_millis(2));
            }
            let content = || w.p.answer(&["content", "list", "--all"]).unwrap().value.out;
            let shown = || w.p.answer(&["show-batch", "b-1"]).unwrap().value.out;
            let (c, b) = (content(), shown());
            assert_eq!((content(), shown()), (c.clone(), b.clone()), "a quiet store is asked once");
            assert_eq!((asked(&w, "content"), asked(&w, "show-batch")), (1, 1));

            deliver(&w, &["history/sp-x/00000000000000000007/r1", "lifecycle/sp-x"], 8);
            assert_eq!((content(), shown()), (c.clone(), b.clone()), "a bead's own history ends nothing held");

            deliver(&w, &["planning/watermark"], 9);
            assert_ne!(content(), c, "a planning write ends the content answer");
            assert_eq!(shown(), b, "and not the round's");

            deliver(&w, &["history/b-1/00000000000000000009/r2"], 10);
            assert_ne!(shown(), b, "an entry in a round's history ends the round's answer");
            let (c, b) = (content(), shown());

            deliver(&w, &["batches/spira/b-1"], 11);
            assert_ne!(shown(), b, "a change to a round ends the round's answer");
            assert_eq!(content(), c);
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    });
}

#[test]
fn an_unwatched_view_holds_no_answer_and_a_verb_that_is_not_a_held_read_is_refused() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        assert_eq!(w.p.answer(&["content", "list"]).unwrap().value.out, "answer 1");
        assert_eq!(w.p.answer(&["content", "list"]).unwrap().value.out, "answer 2", "no watch keeps it current, so nothing is held");
        assert!(w.p.answer(&["close", "sp-a"]).is_err());
        assert_eq!(asked(&w, "close"), 0, "and the store is not asked it");
    });
}
