#[macro_use]
extern crate spira_test_id;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_store::lc_call::{LcClient, LcError, Reply, Transport};
use store_view::{Polled, Projection, RowQuery};

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
    content_error: Option<String>,
}

struct Double(Arc<Mutex<Script>>);

impl Transport for Double {
    fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
        let mut s = self.0.lock().unwrap();
        s.seen.push(argv.to_vec());
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new(), read_ended: false });
        match argv[0].as_str() {
            "rc" => {
                let body: Value = serde_json::from_str(argv.last().unwrap()).unwrap();
                let states: Vec<&str> = body["states"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let prefix = body["prefixes"][0].as_str().unwrap_or_default();
                let items: Vec<&Value> = s
                    .batches
                    .iter()
                    .filter(|b| b["key"].as_str().is_some_and(|k| k.starts_with(prefix)))
                    .filter(|b| states.is_empty() || states.contains(&b["value"]["data"]["state"].as_str().unwrap_or_default()))
                    .collect();
                if let Some(limit) = body["limit"].as_u64() {
                    assert!(items.len() as u64 <= limit, "the store refuses more than {limit}");
                }
                ok(json!({ "ok": { "items": items, "list_rev": s.list_rev } }).to_string())
            }
            "list" if argv.iter().any(|a| a == "--batches") => {
                let window = argv.iter().position(|a| a == "--window").map(|i| argv[i + 1].clone()).unwrap_or_default();
                let rounds: Vec<Value> = s.batches.iter().map(|b| b["value"]["data"].clone()).collect();
                ok(serde_json::to_string_pretty(&json!({ "window": window, "rounds": rounds })).unwrap())
            }
            "list" => {
                let ids: Option<Vec<&str>> = argv.iter().position(|a| a == "--ids").map(|i| argv[i + 1].split(',').collect());
                let states: Option<Vec<&str>> = argv.iter().position(|a| a == "--state").map(|i| argv[i + 1].split(',').collect());
                let live = argv.iter().any(|a| a == "--live");
                let terminal = |r: &Value| matches!(r["state"].as_str(), Some("LANDED" | "SUPERSEDED" | "DROPPED" | "DONE"));
                let rows: Vec<&Value> = s
                    .rows
                    .iter()
                    .filter(|r| ids.as_ref().is_none_or(|ids| ids.contains(&r["bead_id"].as_str().unwrap())))
                    .filter(|r| states.as_ref().is_none_or(|st| st.contains(&r["state"].as_str().unwrap())))
                    .filter(|r| !live || !terminal(r))
                    .collect();
                ok(serde_json::to_string(&rows).unwrap())
            }
            "watch" => match s.watches.pop_front() {
                Some((code, out)) => Ok(Reply { code, out, err: String::new(), read_ended: false }),
                None if s.quiet => {
                    drop(s);
                    // clock-safe: the double's long-poll, ending after a pause
                    std::thread::sleep(Duration::from_millis(5));
                    ok(json!({ "progress_rev": 1 }).to_string())
                }
                None => Err("the script has no watch answer left".into()),
            },
            "claimable" => ok(json!({ "rev": 1, "content_error": s.content_error, "beads": [] }).to_string()),
            "content" | "show-batch" | "condition" => ok(format!("answer {}", s.seen.iter().filter(|a| a[0] == argv[0]).count())),
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
    json!({ "bead_id": id, "state": state, "holds": holds, "express": if express { "1" } else { "0" }, "claim_view": verdict.map(|v| json!({ "verdict": v, "blocker": null, "runes": [] })), "repo": "spira", "updated_at": 4_000_000_000_i64 })
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

#[test_id(t7ra3eh)]
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

#[test_id(tssxdmo)]
#[test]
fn a_view_refuses_before_its_first_load_and_once_its_revision_is_stale_until_the_store_confirms_it() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_millis(1500));
        assert!(matches!(w.p.batch_view("spira"), Err(LcError::Refused(m)) if m.contains("not loaded")));
        w.p.poll().unwrap();
        w.advance(Duration::from_millis(1400));
        assert_eq!(w.p.ready_counts().unwrap().rev, 4);
        w.advance(Duration::from_millis(30_200));
        for refusal in [w.p.batch_view("spira").err(), w.p.round_view("spira").err(), w.p.ready_counts().err()] {
            assert!(matches!(&refusal, Some(LcError::Refused(m)) if m.starts_with("stale past 31500ms")), "{refusal:?}");
        }
        assert!(w.p.poll().is_err(), "a watch with no answer");
        assert!(w.p.ready_counts().is_err(), "a failed poll confirms nothing");
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        assert_eq!(w.p.ready_counts().unwrap().rev, 6, "a progress answer confirms the views at its revision");
    });
}

#[test_id(t8e4dr1)]
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

#[test_id(to0sjha)]
#[test]
fn a_blocker_that_closes_rereads_the_held_beads_waiting_on_it_however_deep() {
    within(|| {
        let waiting = |id: &str, on: &str| {
            let mut b = bead(id, "READY", "[]", Some("blocked"), false);
            b["blocked_by"] = json!([on]);
            b["claim_view"]["blocker"] = json!(on);
            b
        };
        let w = world(
            Script { list_rev: 9, rows: vec![bead("sp-b", "READY", "[]", Some("claimable"), false), waiting("sp-a", "sp-b"), waiting("sp-top", "sp-a"), bead("sp-x", "READY", "[]", Some("claimable"), false)], ..Default::default() },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        assert_eq!(w.p.ready_counts().unwrap().value.judged_claimable, 2);

        w.script.lock().unwrap().rows = vec![bead("sp-b", "LANDED", "[]", None, false), bead("sp-a", "READY", "[]", Some("claimable"), false), waiting("sp-top", "sp-a"), bead("sp-x", "READY", "[]", Some("claimable"), false)];
        w.watch(json!({ "events": [{ "key": "lifecycle/sp-b", "rev": rev(10), "value": {}, "create_rev": null, "prev_mod_rev": rev(2) }], "through": 10 }));
        w.p.poll().unwrap();
        let last = w.script.lock().unwrap().seen.last().cloned().unwrap();
        assert_eq!(&last, &["list", "--ids", "sp-a,sp-b,sp-top"].map(String::from), "the waiters are re-read with the blocker, an unrelated bead is not");
        let held = w.p.list_answer(&["list", "--live"]).unwrap().unwrap();
        let verdicts: Vec<(String, String)> = serde_json::from_str::<Vec<Value>>(&held).unwrap().iter().map(|r| (r["bead_id"].as_str().unwrap().into(), r["claim_view"]["verdict"].as_str().unwrap().into())).collect();
        assert!(verdicts.contains(&("sp-a".into(), "claimable".into())), "{verdicts:?}");
        assert!(!verdicts.iter().any(|(id, _)| id == "sp-b"), "{verdicts:?}");
    });
}

#[test_id(ts5qtzu)]
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

#[test_id(tr7ldig)]
#[test]
fn load_and_refresh_ask_the_store_for_the_open_states_only_and_hold_only_the_open_rounds() {
    within(|| {
        let mut batches: Vec<Value> = (0..300).map(|n| round("spira", &format!("r-{n}"), if n % 2 == 0 { "Landed" } else { "ABANDONED" }, n, n as u64 + 1, &[])).collect();
        batches.push(forge("deck-1", "armed", 900, "12", 400));
        batches.push(round("spira", "spira-open", "CiRunning", 901, 401, &[]));
        let w = world(Script { batches, list_rev: 9, rows: vec![bead("sp-a", "READY", "[]", Some("claimable"), false)], ..Default::default() }, Duration::from_secs(10));
        for refresh in [false, true] {
            w.script.lock().unwrap().seen.clear();
            if refresh { w.p.refresh_batches().unwrap() } else { w.p.load().unwrap() }
            let body: Value = {
                let s = w.script.lock().unwrap();
                serde_json::from_str(s.seen.iter().find(|a| a[0] == "rc").unwrap().last().unwrap()).unwrap()
            };
            let states: Vec<&str> = body["states"].as_array().expect("a state filter").iter().map(|v| v.as_str().unwrap()).collect();
            assert!(!states.is_empty() && !states.iter().any(|s| ["Landed", "LANDED", "Abandoned", "ABANDONED", "Settled", "Discarded"].contains(s)), "{body}");
            assert!(body["limit"].is_u64(), "{body}");
            let open: Vec<String> = w.p.round_view("spira").unwrap().value.rounds.into_iter().map(|r| r.record.id).collect();
            assert_eq!(open, ["deck-1", "spira-open"], "only the open rounds are held");
        }
    });
}

#[test_id(twpleh0)]
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
        w.script.lock().unwrap().seen.clear();
        let ended = w.p.batch_view_ended("spira").unwrap();
        let body: Value = serde_json::from_str(w.script.lock().unwrap().seen.last().unwrap().last().unwrap()).unwrap();
        assert_eq!(body["prefixes"], json!(["batches/spira/"]));
        assert!(body["states"].as_array().unwrap().iter().all(|s| ["Landed", "Settled", "Abandoned", "Discarded", "LANDED", "SETTLED", "ABANDONED", "DISCARDED"].contains(&s.as_str().unwrap())), "{body}");
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
#[test_id(t8icp24)]
#[test]
fn the_watch_names_only_the_prefixes_the_view_applies() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        let watch = w.script.lock().unwrap().seen.iter().find(|a| a[0] == "watch").cloned().unwrap();
        let prefixes: Vec<&str> = watch[1..].iter().take_while(|a| !a.starts_with("--")).map(String::as_str).collect();
        assert_eq!(prefixes, ["batches/", "lifecycle/", "readiness/", "planning/", "history/", "conditions/"], "{watch:?}");
    });
}

#[test_id(tdrgdpz)]
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
        let by = |states: &[&str]| w.p.rows(&RowQuery { states, ..Default::default() }).unwrap().unwrap().value.iter().map(|r| r["bead_id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
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
        let live = w.p.rows(&RowQuery { live: true, ..Default::default() }).unwrap().unwrap().value;
        assert_eq!(live.len(), 2, "a bead the watch moved to LANDED is no longer held");
        assert!(w.p.rows(&RowQuery { ids: Some(&["sp-b".to_string()]), ..Default::default() }).is_none(), "a terminal bead is the store's to answer");
        assert_eq!(w.script.lock().unwrap().seen.len(), asked, "serving rows asks the store nothing");
    });
}

#[test_id(temktvi)]
#[test]
fn a_cut_watch_makes_every_reader_refuse_and_says_the_watch_is_lost() {
    within(|| {
        let w = world(Script { list_rev: 4, rows: vec![bead("sp-a", "READY", "[]", None, false)], ..Default::default() }, Duration::from_millis(1500));
        w.p.poll().unwrap();
        assert!(w.p.unheld("READY", "spira").is_ok());
        assert!(w.p.poll().is_err(), "the watch was cut");
        w.advance(Duration::from_secs(2));
        for refusal in [w.p.rows(&RowQuery { live: true, ..Default::default() }).unwrap().err(), w.p.unheld("READY", "spira").err(), w.p.batch_view("spira").err()] {
            assert!(matches!(&refusal, Some(LcError::Refused(m)) if m.starts_with("stale past 1500ms") && m.contains("the watch is lost")), "{refusal:?}");
        }
        w.watch(json!({ "progress_rev": 6 }));
        w.p.poll().unwrap();
        assert!(w.p.unheld("READY", "spira").is_ok(), "a poll the store answered ends the refusal");
    });
}

#[test_id(t5fgrp6)]
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
            std::thread::spawn(move || p.run(&stop, &mut |_| {}))
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
    let events: Vec<Value> = keys.iter().map(|k| json!({ "key": k, "rev": rev(through), "value": {}, "create_rev": null, "prev_mod_rev": null })).collect();
    let before = {
        let mut s = w.script.lock().unwrap();
        let before = s.seen.iter().filter(|a| a[0] == "watch").count();
        s.watches.push_back((0, json!({ "events": events, "through": through }).to_string()));
        before
    };
    // clock-safe: a cap on the wait for the watch's next call, which is a named signal
    let give_up = Instant::now() + Duration::from_secs(30);
    while asked(w, "watch") < before + 2 {
        // clock-safe: the cap on the wait for the watch's next call
        assert!(Instant::now() < give_up, "the delivery was never applied");
        // clock-safe: a poll interval under that cap
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test_id(tf379qx)]
#[test]
fn a_held_answer_lasts_until_a_change_that_could_alter_it_and_a_bead_history_alters_none() {
    within(|| {
        let batch = round("spira", "b-1", "Open", 100, 3, &[]);
        let w = world(Script { batches: vec![batch], list_rev: 4, quiet: true, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| w.p.run(&stop, &mut |_| {}));
            // clock-safe: a cap on the wait for the watch to start
            while !w.p.watching() {
                // clock-safe: a poll interval under that cap
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

#[test_id(tt9niio)]
#[test]
fn a_held_condition_list_lasts_until_a_condition_under_conditions_changes_or_the_views_reload() {
    within(|| {
        let w = world(Script { list_rev: 4, quiet: true, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| w.p.run(&stop, &mut |_| {}));
            // clock-safe: a cap on the wait for the watch to start
            while !w.p.watching() {
                // clock-safe: a poll interval under that cap
                std::thread::sleep(Duration::from_millis(2));
            }
            let conds = || w.p.answer(&["condition", "list", "--owner", "heimdall"]).unwrap().value.out;
            let first = conds();
            assert_eq!((conds(), conds()), (first.clone(), first.clone()));
            assert_eq!(asked(&w, "condition"), 1, "three reads ask the store once");

            deliver(&w, &["lifecycle/sp-x"], 8);
            assert_eq!(conds(), first, "a lifecycle change leaves it held");

            deliver(&w, &["conditions/heimdall/disk"], 9);
            assert_ne!(conds(), first, "a condition set ends it");
            assert_eq!(asked(&w, "condition"), 2);

            let held = conds();
            w.watch(json!({ "too_old": { "compacted": 20 }, "items": [], "list_rev": 21 }));
            w.script.lock().unwrap().list_rev = 21;
            let before = asked(&w, "watch");
            while asked(&w, "watch") < before + 2 {
                // clock-safe: a poll interval; the wait ends on the watch's next call
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_ne!(conds(), held, "a reload ends it");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    });
}

#[test_id(tghodwp)]
#[test]
fn a_stale_view_refuses_a_condition_list() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_millis(1500));
        w.p.poll().unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| w.p.run(&stop, &mut |_| {}));
            // clock-safe: a cap on the wait for the watch to start
            while !w.p.watching() {
                // clock-safe: a poll interval under that cap
                std::thread::sleep(Duration::from_millis(2));
            }
            w.advance(Duration::from_secs(60));
            let refusal = w.p.answer(&["condition", "list"]).err();
            assert!(matches!(&refusal, Some(LcError::Refused(m)) if m.starts_with("stale past")), "{refusal:?}");
            assert_eq!(asked(&w, "condition"), 0, "the store is not asked instead");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    });
}

#[test_id(tyf6s2n)]
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

#[test_id(tz6fyyt)]
#[test]
fn a_list_read_is_answered_from_the_held_rows_and_a_stale_view_refuses_it() {
    within(|| {
        let mut rows = vec![
            bead("sp-a", "READY", "[\"manual\"]", None, true),
            bead("sp-b", "CERTIFIED", "[]", None, false),
            bead("sp-c", "CERTIFIED", "[]", None, false),
            bead("sp-d", "READY", "[]", None, false),
        ];
        rows[2]["repo"] = json!("alpha");
        let w = world(Script { list_rev: 3, rows, ..Default::default() }, Duration::from_secs(10));
        w.p.load().unwrap();
        let asked = w.script.lock().unwrap().seen.len();
        let ids = |argv: &[&str]| -> Vec<String> {
            let text = w.p.list_answer(argv).expect("a held read").expect("a fresh view");
            serde_json::from_str::<Vec<Value>>(&text).unwrap().iter().map(|r| r["bead_id"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(ids(&["list", "--live", "--hold", "manual"]), ["sp-a"]);
        assert_eq!(ids(&["list", "--express", "--live"]), ["sp-a"]);
        assert_eq!(ids(&["list", "--state", "CERTIFIED", "--repo", "alpha"]), ["sp-c"]);
        assert_eq!(ids(&["list", "--ids", "sp-a,sp-b"]), ["sp-a", "sp-b"]);
        assert_eq!(ids(&["list", "--live"]), ["sp-a", "sp-b", "sp-c", "sp-d"]);
        assert_eq!(w.script.lock().unwrap().seen.len(), asked, "answering asks the store nothing");

        for argv in [&["list", "--delivery"][..], &["list", "--live", "--window", "5"], &["list"], &["list", "--state", "LANDED"], &["list", "--state", "READY,LANDED"], &["list", "--ids", "sp-gone"], &["show", "sp-a"], &[]] {
            assert!(w.p.list_answer(argv).is_none(), "{argv:?} is the store's to answer");
        }

        w.advance(Duration::from_secs(11));
        assert!(matches!(w.p.list_answer(&["list", "--live"]), Some(Err(_))), "a stale view refuses");
    });
}

#[test_id(tdg2fep)]
#[test]
fn a_fresh_projection_reads_the_live_set_and_holds_no_terminal_row() {
    within(|| {
        let rows = vec![bead("sp-a", "READY", "[]", None, false), bead("sp-b", "LANDED", "[]", None, false), bead("sp-c", "DROPPED", "[]", None, false)];
        let w = world(Script { list_rev: 3, rows, ..Default::default() }, Duration::from_secs(10));
        w.p.load().unwrap();
        let lists: Vec<Vec<String>> = w.script.lock().unwrap().seen.iter().filter(|a| a[0] == "list").cloned().collect();
        assert_eq!(lists, [["list", "--live"]], "the only lifecycle read is the live set");
        let held = w.p.list_answer(&["list", "--ids", "sp-a"]).unwrap().unwrap();
        assert!(held.contains("sp-a"));
        assert!(w.p.list_answer(&["list", "--ids", "sp-b"]).is_none(), "a terminal bead is not held, so the store is asked");
        assert!(w.p.list_answer(&["list", "--state", "LANDED"]).is_none());
        assert!(w.p.show("sp-b").is_none());
        assert!(w.p.show("sp-a").is_some());
    });
}

#[test_id(t6730w2)]
#[test]
fn a_rounds_list_is_the_stores_answer_at_the_same_revision_held_until_a_round_changes() {
    within(|| {
        let batches = vec![round("spira", "b-open", "Open", 100, 3, &[]), round("spira", "b-done", "Landed", 90, 2, &[])];
        let w = world(Script { batches, list_rev: 4, quiet: true, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| w.p.run(&stop, &mut |_| {}));
            // clock-safe: a cap on the wait for the watch to start
            while !w.p.watching() {
                // clock-safe: a poll interval under that cap
                std::thread::sleep(Duration::from_millis(2));
            }
            let listed = |argv: &[&str]| w.p.list_answer(argv).expect("a held read").expect("a fresh view");
            let asked_rounds = || w.script.lock().unwrap().seen.iter().filter(|a| a.iter().any(|x| x == "--batches")).count();
            for argv in [&["list", "--batches", "--window", "86400"][..], &["list", "--batches"], &["list", "--batches", "--batch", "b-open"]] {
                let held = listed(argv);
                let direct = LcClient::over(Arc::new(Double(w.script.clone())), true).answer(argv).unwrap();
                assert_eq!(held, direct, "{argv:?}");
            }
            let before = asked_rounds();
            listed(&["list", "--batches", "--window", "86400"]);
            listed(&["list", "--batches", "--window", "86400"]);
            assert_eq!(asked_rounds(), before, "a repeated read of an unchanged store is held");
            deliver(&w, &["batches/spira/b-open"], 11);
            listed(&["list", "--batches", "--window", "86400"]);
            assert_eq!(asked_rounds(), before + 1, "a change to a round ends the held answer");
            let held_at = asked_rounds();
            w.advance(store_view::WINDOW_HOLD + Duration::from_secs(1));
            let give_up = Instant::now() + Duration::from_secs(5); // clock-safe: a cap on the wait for the watch to confirm the view at the new time
            while Instant::now() < give_up && !matches!(w.p.list_answer(&["list", "--batches", "--window", "86400"]), Some(Ok(_))) {
                // clock-safe: a poll interval under that cap
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(asked_rounds() > held_at, "a window answer older than the hold is read afresh: rounds leave a window by the clock");
            let whole = asked_rounds();
            listed(&["list", "--batches"]);
            listed(&["list", "--batches"]);
            assert!(asked_rounds() <= whole + 1, "an answer with no window is not aged out");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    });
}

#[test_id(t26mc89)]
#[test]
fn a_wake_that_carries_its_rows_is_one_store_call_whatever_the_number_of_keys() {
    within(|| {
        let w = world(
            Script { list_rev: 9, rows: vec![bead("sp-a", "READY", "[]", Some("claimable"), false), bead("sp-b", "READY", "[]", Some("claimable"), false), bead("sp-c", "READY", "[]", None, false)], ..Default::default() },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        let before = w.script.lock().unwrap().seen.len();
        let moved = |id: &str, state: &str| bead(id, state, "[]", None, false);
        let events: Vec<Value> = ["lifecycle/sp-a", "readiness/sp-b", "lifecycle/sp-c"].iter().map(|k| json!({ "key": k, "rev": rev(10), "value": {}, "create_rev": null, "prev_mod_rev": null })).collect();
        w.watch(json!({ "events": events, "through": 10, "rows": [moved("sp-a", "WORKING"), moved("sp-b", "WORKING"), moved("sp-c", "LANDED")] }));
        w.p.poll().unwrap();
        let calls: Vec<Vec<String>> = w.script.lock().unwrap().seen[before..].to_vec();
        assert_eq!(calls.len(), 1, "one call for the wake: {calls:?}");
        assert_eq!(calls[0][0], "watch");
        assert!(calls[0].iter().any(|a| a == "--rows"), "{calls:?}");
        assert_eq!(w.p.ready_counts().unwrap().value.by_state.get("WORKING"), Some(&2));
        assert_eq!(w.p.ready_counts().unwrap().value.by_state.get("READY"), None, "the carried LANDED row left the held set");
    });
}

#[test_id(tg8qu06)]
#[test]
fn a_wake_that_carries_rows_asks_only_for_the_waiters_it_did_not_carry() {
    within(|| {
        let mut waiting = bead("sp-a", "READY", "[]", Some("blocked"), false);
        waiting["blocked_by"] = json!(["sp-b"]);
        let w = world(Script { list_rev: 9, rows: vec![bead("sp-b", "READY", "[]", Some("claimable"), false), waiting], ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.script.lock().unwrap().rows = vec![bead("sp-b", "LANDED", "[]", None, false), bead("sp-a", "READY", "[]", Some("claimable"), false)];
        w.watch(json!({
            "events": [{ "key": "lifecycle/sp-b", "rev": rev(10), "value": {}, "create_rev": null, "prev_mod_rev": null }],
            "through": 10,
            "rows": [bead("sp-b", "LANDED", "[]", None, false)],
        }));
        w.p.poll().unwrap();
        let last = w.script.lock().unwrap().seen.last().cloned().unwrap();
        assert_eq!(&last, &["list", "--ids", "sp-a"].map(String::from));
        assert_eq!(w.p.ready_counts().unwrap().value.judged_claimable, 1);
    });
}

#[test_id(tlt8iv3)]
#[test]
fn a_terminal_row_is_asked_of_the_store_once_and_forgotten_when_a_change_names_it() {
    within(|| {
        let w = world(
            Script { list_rev: 9, rows: vec![bead("sp-a", "READY", "[]", None, false), bead("sp-old", "LANDED", "[]", None, false)], ..Default::default() },
            Duration::from_secs(10),
        );
        w.p.poll().unwrap();
        let ids = ["sp-a".to_string(), "sp-old".to_string()];
        let states = |w: &World| w.p.ids_answer(&ids).unwrap().value.iter().map(|r| r["state"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let asks = |w: &World| w.script.lock().unwrap().seen.iter().filter(|a| a[..2] == ["list", "--ids"]).count();
        assert_eq!(states(&w), ["READY", "LANDED"]);
        assert_eq!(asks(&w), 1);
        assert_eq!(states(&w), ["READY", "LANDED"]);
        assert_eq!(asks(&w), 1, "the terminal row is remembered");

        w.script.lock().unwrap().rows = vec![bead("sp-a", "READY", "[]", None, false), bead("sp-old", "REWORK", "[]", None, false)];
        w.watch(json!({ "events": [{ "key": "lifecycle/sp-old", "rev": rev(10), "value": {}, "create_rev": null, "prev_mod_rev": null }], "through": 10, "rows": [bead("sp-old", "REWORK", "[]", None, false)] }));
        w.p.poll().unwrap();
        assert_eq!(states(&w), ["READY", "REWORK"], "a reopened bead is live again and held");
        assert_eq!(asks(&w), 1);
    });
}

#[test_id(ty7nfix)]
#[test]
fn the_remembered_terminal_rows_are_bounded() {
    within(|| {
        let many: Vec<Value> = (0..5_000).map(|i| bead(&format!("sp-{i:05}"), "DONE", "[]", None, false)).collect();
        let w = world(Script { list_rev: 9, rows: many, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let ids: Vec<String> = (0..5_000).map(|i| format!("sp-{i:05}")).collect();
        for chunk in ids.chunks(200) {
            w.p.ids_answer(chunk).unwrap();
        }
        let asks = |w: &World| w.script.lock().unwrap().seen.iter().filter(|a| a[..2] == ["list", "--ids"]).count();
        let before = asks(&w);
        w.p.ids_answer(&ids[4_900..4_950]).unwrap();
        assert_eq!(asks(&w), before, "the newest rows are still remembered");
        w.p.ids_answer(&ids[..50]).unwrap();
        assert_eq!(asks(&w), before + 1, "the oldest were dropped at the bound and are asked again");
    });
}

fn event(key: &str, at: u64) -> Value {
    json!({ "key": key, "rev": rev(at), "value": {}, "create_rev": null, "prev_mod_rev": null })
}

#[test_id(tn5scp3)]
#[test]
fn a_poll_that_applied_events_reports_their_keys_and_the_revision() {
    within(|| {
        let w = world(Script { list_rev: 4, rows: vec![bead("sp-a", "READY", "[]", None, false)], ..Default::default() }, Duration::from_secs(10));
        assert_eq!(w.p.poll().unwrap(), Polled::Reloaded { rev: 4 }, "nothing held is a load");
        w.script.lock().unwrap().rows = vec![bead("sp-a", "WORKING", "[]", None, false)];
        w.watch(json!({ "events": [event("lifecycle/sp-a", 8), event("work-requests/r1", 9), event("lifecycle/sp-a", 9)], "through": 9 }));
        assert_eq!(w.p.poll().unwrap(), Polled::Applied { through: 9, keys: vec!["lifecycle/sp-a".into(), "work-requests/r1".into()] });
        assert_eq!(w.p.ready_counts().unwrap().value.by_state["WORKING"], 1, "the row is applied as before");
    });
}

#[test_id(ta7qb1t)]
#[test]
fn a_feed_too_old_reports_a_reload_at_the_listed_revision_and_progress_reports_quiet() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.script.lock().unwrap().list_rev = 21;
        w.watch(json!({ "too_old": { "compacted": 20 }, "items": [], "list_rev": 21 }));
        assert_eq!(w.p.poll().unwrap(), Polled::Reloaded { rev: 21 });
        w.watch(json!({ "progress_rev": 23 }));
        assert_eq!(w.p.poll().unwrap(), Polled::Quiet { rev: 23 });
    });
}

#[test_id(tg7j83d)]
#[test]
fn added_prefixes_join_the_watch_and_without_them_the_five_are_asked_as_before() {
    within(|| {
        let watch_argv = |w: &World| w.script.lock().unwrap().seen.iter().find(|a| a[0] == "watch").cloned().unwrap();
        let plain = world(Script { list_rev: 4, quiet: true, ..Default::default() }, Duration::from_secs(10));
        plain.p.poll().unwrap();
        plain.watch(json!({ "progress_rev": 5 }));
        plain.p.poll().unwrap();
        assert_eq!(watch_argv(&plain)[..8], ["watch", "batches/", "lifecycle/", "readiness/", "planning/", "history/", "conditions/", "--from-rev"].map(String::from));
        let mut w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p = Projection::new(LcClient::over(Arc::new(Double(Arc::clone(&w.script))), true), Duration::from_secs(10), Arc::new(Instant::now)).with_prefixes(&["work-requests/"]);
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        assert_eq!(watch_argv(&w)[..9], ["watch", "batches/", "lifecycle/", "readiness/", "planning/", "history/", "conditions/", "work-requests/", "--from-rev"].map(String::from));
    });
}

#[test_id(tm31ksw)]
#[test]
fn every_views_watch_sends_wait_ms_30000() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        let sent = w.script.lock().unwrap().seen.iter().find(|a| a[0] == "watch").cloned().unwrap();
        assert_eq!(sent[sent.iter().position(|a| a == "--wait-ms").unwrap() + 1], "30000");
    });
}

#[test_id(tzq0k4n)]
#[test]
fn a_watched_view_stays_readable_through_a_quiet_hold() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        w.advance(Duration::from_secs(35));
        assert_eq!(w.p.ready_counts().unwrap().rev, 5, "35 s of quiet is inside the hold beyond the 10 s bound");
        w.advance(Duration::from_secs(6));
        assert!(matches!(w.p.ready_counts(), Err(LcError::Refused(m)) if m.starts_with("stale past 40000ms")));
    });
}

#[test_id(tb8m2wd)]
#[test]
fn a_lost_watch_shrinks_the_bound_back_to_the_configured_staleness() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        w.script.lock().unwrap().watches.push_back((1, String::new()));
        assert!(w.p.poll().is_err(), "the watch was lost");
        w.advance(Duration::from_secs(11));
        assert!(matches!(w.p.ready_counts(), Err(LcError::Refused(m)) if m.contains("stale past 10000ms") && m.contains("the watch is lost")));
    });
}

#[test_id(tj5c9ar)]
#[test]
fn an_unwatched_view_keeps_the_configured_staleness() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.refresh_batches().unwrap();
        w.advance(Duration::from_secs(11));
        assert!(matches!(w.p.batch_view("spira"), Err(LcError::Refused(m)) if m.contains("stale past 10000ms")));
    });
}

#[test_id(tos8vaw)]
#[test]
fn a_future_rev_answer_reloads_and_the_views_read() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.script.lock().unwrap().list_rev = 3;
        w.watch(json!({ "future_rev": { "rev": 5, "head": 3 } }));
        assert_eq!(w.p.poll().unwrap(), Polled::Reloaded { rev: 3 });
        assert_eq!(w.p.ready_counts().unwrap().rev, 3);
    });
}

#[test_id(tetut9g)]
#[test]
fn after_a_failed_watch_the_next_one_sends_wait_ms_0_and_its_quiet_answer_confirms_at_once() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        w.script.lock().unwrap().watches.push_back((1, String::new()));
        assert!(w.p.poll().is_err());
        w.watch(json!({ "progress_rev": 5 }));
        w.p.poll().unwrap();
        let waits: Vec<String> = w.script.lock().unwrap().seen.iter().filter(|a| a[0] == "watch").map(|a| a[a.iter().position(|x| x == "--wait-ms").unwrap() + 1].clone()).collect();
        assert_eq!(waits, ["30000", "30000", "0"]);
        w.advance(Duration::from_secs(11));
        assert_eq!(w.p.ready_counts().unwrap().rev, 5, "the quiet answer was confirmed, and the next watch holds");
    });
}

/// Polls `w` through one watch answer that changes `keys`.
fn changed(w: &World, keys: &[&str], through: u64) {
    let events: Vec<Value> = keys.iter().map(|k| json!({ "key": k, "rev": through, "value": {} })).collect();
    w.watch(json!({ "events": events, "through": through }));
    w.p.poll().unwrap();
}

fn asked_for(w: &World, verb: &str) -> usize {
    w.script.lock().unwrap().seen.iter().filter(|a| a[0] == verb).count()
}

#[test_id(t78tlzk)]
#[test]
fn the_claim_view_is_held_until_a_row_readiness_or_plan_changes_or_its_hold_runs_out() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(3600));
        w.p.poll().unwrap();
        let _watching = w.p.hold_watching();
        let read = || w.p.answer(&["claimable"]).unwrap().value.out;
        let first = read();
        assert_eq!((read(), asked_for(&w, "claimable")), (first, 1), "unchanged: held");
        changed(&w, &["conditions/x/y"], 5);
        read();
        assert_eq!(asked_for(&w, "claimable"), 1, "a condition is no input of a verdict");
        for (n, key) in ["lifecycle/sp-a", "readiness/sp-a", "planning/watermark"].iter().enumerate() {
            changed(&w, &[key], 6 + n as u64);
            read();
            read();
            assert_eq!(asked_for(&w, "claimable"), 2 + n, "{key}");
        }
        w.advance(store_view::CLAIM_VIEW_HOLD + Duration::from_secs(1));
        read();
        assert_eq!(asked_for(&w, "claimable"), 5, "the verdicts turn on the clock too");
    });
}

#[test_id(t1zzd5d)]
#[test]
fn a_claim_view_the_content_store_could_not_describe_is_asked_again() {
    within(|| {
        let w = world(Script { list_rev: 4, content_error: Some("down".into()), ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let _watching = w.p.hold_watching();
        w.p.answer(&["claimable"]).unwrap();
        w.p.answer(&["claimable"]).unwrap();
        assert_eq!(asked_for(&w, "claimable"), 2);
    });
}

#[test_id(tuha893)]
#[test]
fn an_id_with_no_row_is_asked_once_under_a_watch_until_a_change_names_it() {
    within(|| {
        let w = world(Script { list_rev: 4, ..Default::default() }, Duration::from_secs(10));
        w.p.poll().unwrap();
        let _watching = w.p.hold_watching();
        let ids = ["sp-gone".to_string()];
        let read = || w.p.ids_answer(&ids).unwrap().value;
        let reads = || w.script.lock().unwrap().seen.iter().filter(|a| a[0] == "list" && a[1] == "--ids").count();
        assert_eq!((read(), read(), reads()), (vec![], vec![], 1));
        w.script.lock().unwrap().rows = vec![bead("sp-gone", "DONE", "[]", None, false)];
        changed(&w, &["lifecycle/sp-gone"], 5);
        let before = reads();
        assert_eq!(read().len(), 1, "the row it gained is read");
        assert!(reads() <= before + 1);
    });
}
