use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_config::lc_call::{LcClient, LcError, Reply, Transport};
use spira_runtime::store::Projection;

/// A scripted store: `rc list` and `list` answer from what it holds, each `watch` takes the next
/// scripted answer, and every request is recorded.
#[derive(Default)]
struct Script {
    batches: Vec<Value>,
    list_rev: u64,
    rows: Vec<Value>,
    watches: VecDeque<(i32, String)>,
    seen: Vec<Vec<String>>,
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
                None => Err("the script has no watch answer left".into()),
            },
            other => Err(format!("unscripted {other}")),
        }
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
    json!({ "bead_id": id, "state": state, "holds": holds, "express": if express { "1" } else { "0" }, "claim_verdict": verdict })
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
}

#[test]
fn a_view_refuses_before_its_first_load_and_once_its_revision_is_stale_until_the_store_confirms_it() {
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
}

#[test]
fn a_watch_delivery_moves_the_batch_and_rereads_only_the_beads_it_names() {
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
}
