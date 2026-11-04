use std::sync::Arc;

use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState};
use runtime::ports::VirtualClock;
use serde_json::Value;
use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::TempDir;

use crate::control::keys;
use crate::control::store::{Outcome, Store};
use crate::control::verbs;

const FLOOR_MS: u64 = 1_000;

fn store<B: Backend>(b: B, clock: &VirtualClock) -> Store<B> {
    let c = Contract::open(
        b,
        Ports::virtual_clock(clock),
        Config {
            min_lock_ttl_ms: FLOOR_MS,
        },
    )
    .unwrap();
    Store::new(c, FLOOR_MS)
}

fn on_mem(body: fn(&Store<MemBackend>)) {
    body(&store(MemBackend::new(), &VirtualClock::new(0)));
}

fn on_sqlite(body: fn(&Store<SqliteBackend>)) {
    let dir = TempDir::new("spira-lc-feed");
    let clock = VirtualClock::new(0);
    let opts = Options {
        clock: Arc::new(clock.clone()),
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("control.db"), opts).unwrap();
    body(&store(b, &clock));
}

/// Each scenario once on the reference backend and once on SQLite, both on the virtual clock.
macro_rules! on_each_backend {
    ($($name:ident),* $(,)?) => {
        mod mem { $( #[test] fn $name() { super::on_mem(super::$name); } )* }
        mod sqlite { $( #[test] fn $name() { super::on_sqlite(super::$name); } )* }
    };
}

on_each_backend!(
    a_watch_resumed_from_a_revision_sees_every_later_change_exactly_once,
    after_compaction_a_watch_answers_too_old_with_the_prefix_relisted_to_resume_from,
    history_lists_a_beads_changes_in_order_with_its_refusals_and_lost_races,
    history_outlives_compaction,
    event_seq_names_only_the_beads_with_an_applied_event_after_the_revision,
    a_watch_of_several_prefixes_delivers_each_and_nothing_else,
);

fn req(id: &str) -> Request {
    Request::new(id, "test", "t")
}

fn call<B: Backend>(s: &Store<B>, argv: &[&str]) -> Value {
    let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let (code, out) = verbs::run(s, &args);
    assert_eq!(code, 0, "{argv:?}: {out}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{argv:?}: {e}: {out}"))
}

fn create<B: Backend>(s: &Store<B>, id: &str) {
    s.create(
        id,
        Some("harness"),
        None,
        100,
        &req(&format!("create-{id}")),
    )
    .unwrap();
}

/// Toggles express on `id` at its current version: one applied change, one revision.
fn toggle<B: Backend>(s: &Store<B>, id: &str, n: u32) -> u64 {
    let r = s.record(id).unwrap().unwrap().value.row;
    let kind = if r.express {
        BeadEventKind::Unexpress
    } else {
        BeadEventKind::Express
    };
    let ev = BeadEvent {
        expect: BeadState::Ready,
        version: r.version,
        kind,
        actor: "ops".into(),
        at: Some(200),
    };
    match s.apply(id, &ev, &req(&format!("{id}-{n}"))).unwrap() {
        Outcome::Applied { rev, .. } => rev,
        other => panic!("{other:?}"),
    }
}

fn refused<B: Backend>(s: &Store<B>, id: &str, n: u32) -> u64 {
    let ev = BeadEvent {
        expect: BeadState::Ready,
        version: 99,
        kind: BeadEventKind::Express,
        actor: "ops".into(),
        at: Some(200),
    };
    match s
        .apply(id, &ev, &req(&format!("{id}-refused-{n}")))
        .unwrap()
    {
        Outcome::Refused { rev, .. } => rev,
        other => panic!("{other:?}"),
    }
}

fn watch<B: Backend>(s: &Store<B>, prefix: &str, from: u64) -> Value {
    let from = from.to_string();
    call(s, &["watch", prefix, "--from-rev", &from, "--wait-ms", "0"])
}

/// Every event under `prefix` after `from`, resuming from each answer's `through` until the
/// watch reports progress.
fn drain<B: Backend>(s: &Store<B>, prefix: &str, mut from: u64) -> Vec<(String, u64)> {
    let mut seen = Vec::new();
    loop {
        let w = watch(s, prefix, from);
        let Some(events) = w["events"].as_array() else {
            assert!(w["progress_rev"].is_u64(), "{w}");
            return seen;
        };
        for e in events {
            seen.push((
                e["key"].as_str().unwrap().to_string(),
                e["rev"]["main"].as_u64().unwrap(),
            ));
        }
        from = w["through"].as_u64().unwrap();
    }
}

fn a_watch_resumed_from_a_revision_sees_every_later_change_exactly_once<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    create(s, "sp-b");
    let resume = toggle(s, "sp-a", 1);
    let mut want = Vec::new();
    for n in 2..6 {
        for id in ["sp-a", "sp-b"] {
            want.push((keys::lifecycle(id), toggle(s, id, n)));
        }
        refused(s, "sp-a", n);
    }
    let seen = drain(s, keys::LIFECYCLE, resume);
    assert_eq!(
        seen, want,
        "a refusal moves no record, and each change arrives once"
    );
    let head = s.contract().head().unwrap();
    assert_eq!(watch(s, keys::LIFECYCLE, head)["progress_rev"], head);
    let (code, out) = verbs::run(s, &["watch".to_string(), keys::LIFECYCLE.to_string()]);
    assert_eq!(code, crate::CANNOT_TELL, "{out}");
}

fn after_compaction_a_watch_answers_too_old_with_the_prefix_relisted_to_resume_from<B: Backend>(
    s: &Store<B>,
) {
    create(s, "sp-a");
    let old = toggle(s, "sp-a", 1);
    create(s, "sp-b");
    let compacted = toggle(s, "sp-b", 2);
    s.contract().compact(compacted).unwrap();
    let w = watch(s, keys::LIFECYCLE, old);
    assert_eq!(w["too_old"]["compacted"], compacted, "{w}");
    let listed: Vec<&str> = w["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(listed, [keys::lifecycle("sp-a"), keys::lifecycle("sp-b")]);
    let list_rev = w["list_rev"].as_u64().unwrap();
    assert_eq!(list_rev, s.contract().head().unwrap());
    let later = toggle(s, "sp-a", 3);
    assert_eq!(
        drain(s, keys::LIFECYCLE, list_rev),
        [(keys::lifecycle("sp-a"), later)]
    );
}

fn history_lists_a_beads_changes_in_order_with_its_refusals_and_lost_races<B: Backend>(
    s: &Store<B>,
) {
    create(s, "sp-a");
    create(s, "sp-b");
    let stale = s.record("sp-a").unwrap().unwrap();
    let mut want = vec![(toggle(s, "sp-a", 1), "Express", true)];
    toggle(s, "sp-b", 1);
    want.push((refused(s, "sp-a", 2), "Express", false));
    let item = s.contract().get(&keys::lifecycle("sp-a")).unwrap().unwrap();
    let ev = BeadEvent {
        expect: BeadState::Ready,
        version: 0,
        kind: BeadEventKind::Express,
        actor: "ops".into(),
        at: Some(201),
    };
    let mut row = stale.value.row.clone();
    row.version += 1;
    let Outcome::LostRace { rev } = s
        .write("sp-a", &ev, &stale, &item, row, None, &req("sp-a-stale"))
        .unwrap()
    else {
        panic!("the stale write applied")
    };
    want.push((rev, "Express", false));
    want.push((toggle(s, "sp-a", 4), "Unexpress", true));

    let h = call(s, &["history", "sp-a"]);
    let got: Vec<(u64, &str, bool)> = h
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            assert_eq!(r["lc_key"], "sp-a", "{r}");
            (
                r["seq"].as_u64().unwrap(),
                r["event"].as_str().unwrap(),
                r["applied"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, want);
    assert_eq!(call(s, &["history", "sp-a", "--machine", "bead"]), h);
    let batch: Vec<String> = ["history", "sp-a", "--machine", "batch"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    assert_eq!(
        verbs::run(s, &batch).0,
        crate::CANNOT_TELL,
        "a machine the store does not keep answered as if it had no history"
    );
    assert_eq!(call(s, &["history", "sp-none"]), Value::Array(vec![]));

    s.create(
        "sp-t",
        None,
        Some("0123456789abcdef0123456789abcdef01234567"),
        100,
        &req("create-sp-t"),
    )
    .unwrap();
    let born = call(s, &["history", "sp-t"]);
    let born: Vec<(&str, &str)> = born
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["event"].as_str().unwrap(),
                r["to_state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(born, [("Submit", "SUBMITTED")], "a born-submitted record");
}

fn history_outlives_compaction<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    let first = toggle(s, "sp-a", 1);
    let second = toggle(s, "sp-a", 2);
    s.contract().compact(second).unwrap();
    let seqs: Vec<u64> = call(s, &["history", "sp-a"])
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [first, second]);
}

fn event_seq_names_only_the_beads_with_an_applied_event_after_the_revision<B: Backend>(
    s: &Store<B>,
) {
    for id in ["sp-a", "sp-b", "sp-c"] {
        create(s, id);
    }
    let since = toggle(s, "sp-a", 1);
    toggle(s, "sp-c", 2);
    toggle(s, "sp-b", 2);
    refused(s, "sp-a", 3);
    let head = s.contract().head().unwrap();
    let since_s = since.to_string();
    let e = call(s, &["event-seq", "--since", &since_s]);
    assert_eq!(
        e,
        serde_json::json!({ "seq": head, "keys": ["sp-b", "sp-c"] })
    );
    let all = call(s, &["event-seq"]);
    assert_eq!(all["keys"], serde_json::json!(["sp-a", "sp-b", "sp-c"]));
    let head_s = head.to_string();
    assert_eq!(
        call(s, &["event-seq", "--since", &head_s])["keys"],
        serde_json::json!([])
    );
}

/// Over-broad-scopes #10 (sp-ipwlic): one watch names several prefixes, so a watcher of three
/// keyspaces need not watch `""`; events under none of them never reach it.
fn a_watch_of_several_prefixes_delivers_each_and_nothing_else<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    let from = toggle(s, "sp-a", 1);
    let later = toggle(s, "sp-a", 2);
    let from_s = from.to_string();
    let both = call(s, &["watch", keys::LIFECYCLE, "nothing/", "--from-rev", &from_s, "--wait-ms", "0"]);
    let keys_seen: Vec<&str> = both["events"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!(keys_seen, [keys::lifecycle("sp-a")], "{both}");
    assert_eq!(both["through"], later);
    let neither = call(s, &["watch", "nothing/", "elsewhere/", "--from-rev", &from_s, "--wait-ms", "0"]);
    assert!(neither["progress_rev"].is_u64(), "no event under either prefix: {neither}");
}
