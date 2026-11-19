use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState};
use runtime::ports::{Clock, VirtualClock};
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

/// A wall clock that reads 6 hours later every time it is read.
struct SixHoursPerRead(AtomicU64);

impl Clock for SixHoursPerRead {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(6 * 3_600_000, Ordering::SeqCst)
    }
}

/// SQLite with the wall clock stepping, so successive commits fall in successive day files.
fn on_sqlite_across_days(body: fn(&Store<SqliteBackend>)) {
    let dir = TempDir::new("spira-lc-feed-days");
    let clock = VirtualClock::new(0);
    let opts = Options {
        clock: Arc::new(clock.clone()),
        wall: Arc::new(SixHoursPerRead(AtomicU64::new(0))),
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("control.db"), opts).unwrap();
    body(&store(b.clone(), &clock));
    let commits = b.head().unwrap();
    let day_files = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("control.events-"))
        .count();
    assert!(commits < 5 || day_files > 1, "{commits} commits left the events in {day_files} events file(s), not several");
}

/// Each scenario once on the reference backend and once on SQLite, both on the virtual clock.
macro_rules! on_each_backend {
    ($($name:ident),* $(,)?) => {
        mod mem { $( #[test] fn $name() { super::on_mem(super::$name); } )* }
        mod sqlite { $( #[test] fn $name() { super::on_sqlite(super::$name); } )* }
        mod sqlite_across_days { $( #[test] fn $name() { super::on_sqlite_across_days(super::$name); } )* }
    };
}

on_each_backend!(
    a_watch_resumed_from_a_revision_sees_every_later_change_exactly_once,
    after_history_is_forgotten_a_watch_answers_too_old_with_no_rows_and_the_revision_to_resume_from,
    history_lists_a_beads_changes_in_order_with_its_refusals_and_lost_races,
    history_outlives_forgetting_revisions,
    event_seq_names_only_the_beads_with_an_applied_event_after_the_revision,
    audits_answers_the_applied_and_refused_events_after_a_revision_in_pages,
    a_watch_of_several_prefixes_delivers_each_and_nothing_else,
    store_usage_of_a_backend_that_keeps_no_history_still_pages_by_prefix,
    store_usage_answers_one_bounded_page_with_a_cursor_and_refuses_a_page_above_the_cap,
);

/// A watch from a revision the history ring has turned past is `too_old`, and resumes from the head.
fn once_the_ring_has_turned_a_watch_answers_too_old_with_no_rows_listed<B: Backend>(
    s: &Store<B>,
) {
    create(s, "sp-a");
    let old = toggle(s, "sp-a", 1);
    for n in 2..80 {
        toggle(s, "sp-a", n);
    }
    let w = watch(s, keys::LIFECYCLE, old);
    assert!(w["too_old"]["compacted"].as_u64().unwrap() > old, "{w}");
    assert_eq!(w["items"], serde_json::json!([]), "{w}");
    assert_eq!(w["list_rev"].as_u64().unwrap(), s.contract().head().unwrap());
}

#[test]
fn a_mem_ring_that_has_turned_relists_a_feed() {
    let clock = VirtualClock::new(0);
    once_the_ring_has_turned_a_watch_answers_too_old_with_no_rows_listed(&store(
        MemBackend::with_history_bytes(6_000),
        &clock,
    ));
}

#[test]
fn a_sqlite_ring_that_has_turned_relists_a_feed() {
    let dir = TempDir::new("spira-lc-feed-ring");
    let clock = VirtualClock::new(0);
    let opts = Options {
        clock: Arc::new(clock.clone()),
        history_bytes: 6_000,
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("control.db"), opts).unwrap();
    once_the_ring_has_turned_a_watch_answers_too_old_with_no_rows_listed(&store(b, &clock));
}

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

fn after_history_is_forgotten_a_watch_answers_too_old_with_no_rows_and_the_revision_to_resume_from<B: Backend>(
    s: &Store<B>,
) {
    create(s, "sp-a");
    let old = toggle(s, "sp-a", 1);
    create(s, "sp-b");
    let compacted = toggle(s, "sp-b", 2);
    s.contract().compact(compacted).unwrap();
    let w = watch(s, keys::LIFECYCLE, old);
    assert_eq!(w["too_old"]["compacted"], compacted, "{w}");
    assert_eq!(w["items"], serde_json::json!([]), "a too_old answer reads no rows: {w}");
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
        .write("sp-a", &ev, &stale, &item, row, None, None, &req("sp-a-stale"))
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

fn history_outlives_forgetting_revisions<B: Backend>(s: &Store<B>) {
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

fn store_usage_answers_one_bounded_page_with_a_cursor_and_refuses_a_page_above_the_cap<B: Backend>(
    s: &Store<B>,
) {
    for id in ["sp-a", "sp-b", "sp-c"] {
        create(s, id);
    }
    let (code, out) = verbs::run(s, &["store-usage", "--limit", "2"].map(String::from));
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["next"].is_string(), "{out}");
    assert_eq!(v["examined"], 2);
    let (code, out) = verbs::run(s, &["store-usage", "--limit", "10001"].map(String::from));
    assert_eq!(code, 2);
    assert!(out.contains("10000"), "{out}");
}

fn audits_answers_the_applied_and_refused_events_after_a_revision_in_pages<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    let first = toggle(s, "sp-a", 1);
    let bad = refused(s, "sp-a", 2);
    let last = toggle(s, "sp-a", 3);
    let after = first.to_string();
    let got = call(s, &["audits", "--after", &after]);
    let rows = got["rows"].as_array().unwrap();
    let seqs: Vec<u64> = rows.iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, [bad, last]);
    assert_eq!(rows[0]["applied"], false);
    assert_eq!(rows[0]["lc_key"], "sp-a");
    assert_eq!(rows[1]["applied"], true);
    assert!(rows[1]["to_state"].is_string() && rows[1]["at"].is_number());
    let page = call(s, &["audits", "--after", &after, "--limit", "1"]);
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    let head = last.to_string();
    assert_eq!(call(s, &["audits", "--after", &head])["rows"], serde_json::json!([]));
    assert_eq!(call(s, &["audits", "--newest"]), serde_json::json!({ "newest_at": null }), "audits stamped long ago are outside the window");
    let zero: Vec<String> = ["audits", "--limit", "0"].map(String::from).to_vec();
    assert_ne!(verbs::run(s, &zero).0, 0);
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
    assert_eq!(
        call(s, &["event-seq", "--head"]),
        serde_json::json!({ "seq": head, "keys": [] })
    );
    let both: Vec<String> = ["event-seq", "--head", "--since", "1"].map(String::from).to_vec();
    assert_ne!(verbs::run(s, &both).0, 0, "--head and --since together are refused");
}

/// A store carrying many beads' history answers `event-seq --since <recent>` from the events
/// after the revision alone: the range statements step over the recent rows, not the history.
#[test]
fn event_seq_since_a_recent_revision_reads_only_the_recent_events_of_a_loaded_store() {
    let dir = TempDir::new("spira-lc-feed-loaded");
    let clock = VirtualClock::new(0);
    let opts = Options {
        clock: Arc::new(clock.clone()),
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("control.db"), opts).unwrap();
    let s = store(b.clone(), &clock);
    for i in 0..300 {
        create(&s, &format!("sp-{i}"));
        toggle(&s, &format!("sp-{i}"), 1);
    }
    let since = s.contract().head().unwrap();
    toggle(&s, "sp-7", 2);
    toggle(&s, "sp-9", 2);
    b.take_read_steps();
    let e = call(&s, &["event-seq", "--since", &since.to_string()]);
    assert_eq!(e["keys"], serde_json::json!(["sp-7", "sp-9"]));
    let steps = b.take_read_steps();
    assert!(steps < 5_000, "event-seq --since a recent revision stepped {steps} over a loaded store");
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

fn store_usage_of_a_backend_that_keeps_no_history_still_pages_by_prefix<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    let (code, out) = verbs::run(s, &["store-usage".to_string()]);
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["next"].is_null(), "{out}");
    assert!(v["prefixes"].as_object().is_some_and(|p| !p.is_empty()), "{out}");
}

#[test]
fn store_usage_hot_and_file_are_served_on_sqlite_and_refused_elsewhere() {
    let hot = |s: &Store<MemBackend>| verbs::run(s, &["store-usage", "--hot", "1"].map(String::from));
    let mem = store(MemBackend::new(), &VirtualClock::new(0));
    let (code, out) = hot(&mem);
    assert_eq!((code, out.starts_with("cannot tell: ")), (2, true), "{out}");
    let (code, out) = verbs::run(&mem, &["store-usage", "--versions"].map(String::from));
    assert_eq!((code, out.starts_with("cannot tell: ")), (2, true), "{out}");
    let (code, _) = verbs::run(&mem, &["store-usage", "--file"].map(String::from));
    assert_eq!(code, 2);
    on_sqlite(|s| {
        create(s, "sp-a");
        toggle(s, "sp-a", 1);
        let (code, out) = verbs::run(s, &["store-usage", "--hot", "1", "--revs", "50"].map(String::from));
        assert_eq!(code, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["keys"].as_array().map(Vec::len), Some(1), "{out}");
        let (code, out) = verbs::run(s, &["store-usage", "--hot", "1", "--revs", "999999"].map(String::from));
        assert_eq!(code, 0, "a window wider than the ring counts what the ring holds: {out}");
        let (code, out) = verbs::run(s, &["store-usage", "--versions", "--limit", "1"].map(String::from));
        assert_eq!(code, 0, "{out}");
        let (code, out) = verbs::run(s, &["store-usage", "--versions", "--after-key", "zzz"].map(String::from));
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("--after-main"), "{out}");
        let (code, out) = verbs::run(s, &["store-usage", "--versions", "--after-main", "1"].map(String::from));
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("--after-key"), "{out}");
        let (code, out) = verbs::run(s, &["store-usage", "--file"].map(String::from));
        assert_eq!(code, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["bytes"].as_u64().unwrap() > 0, "{out}");
    });
}

#[test]
fn store_usage_families_names_each_family_file_size_and_mark_and_flags_the_file_behind_it() {
    use state_store_sqlite::dated::date_of;
    const DAY_MS: u64 = 86_400_000;
    let today = 100;
    let dir = TempDir::new("spira-lc-feed-families");
    let clock = VirtualClock::new(0);
    let opts = Options {
        clock: Arc::new(clock.clone()),
        wall: Arc::new(VirtualClock::new(today as u64 * DAY_MS)),
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("control.db"), opts).unwrap();
    let plant = |name: String, bytes: usize| std::fs::write(dir.path().join(name), vec![0u8; bytes]).unwrap();
    plant(format!("control.receipts-{}.db", date_of(today - 2)), 10);
    plant(format!("control.receipts-{}.db-wal", date_of(today - 2)), 20);
    plant(format!("control.receipts-{}.db", date_of(today - 1)), 30);
    plant(format!("control.events-{}.db", date_of(today - 14)), 40);
    plant(format!("control.events-{}.db", date_of(today - 15)), 50);
    plant(format!("control.events-{}.db-shm", date_of(today - 15)), 60);
    let (code, out) = verbs::run(&store(b, &clock), &["store-usage", "--families"].map(String::from));
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let family = |name: &str| v["families"].as_array().unwrap().iter().find(|f| f["family"] == name).unwrap_or_else(|| panic!("{name} missing: {out}")).clone();
    let file = |f: &Value, name: &str| f["files"].as_array().unwrap().iter().find(|x| x["file"] == name).unwrap_or_else(|| panic!("{name} missing: {out}")).clone();
    let main = family("main");
    assert!(file(&main, "control.db")["bytes"].as_u64().unwrap() > 0, "{out}");
    let receipts = family("receipts");
    assert_eq!((receipts["keep_days"].as_i64(), receipts["mark"].as_str()), (Some(2), Some(date_of(today - 1).as_str())), "{out}");
    let old = file(&receipts, &format!("control.receipts-{}.db", date_of(today - 2)));
    assert_eq!((old["bytes"].as_u64(), old["behind_mark"].as_bool()), (Some(10), Some(true)), "{out}");
    let wal = file(&receipts, &format!("control.receipts-{}.db-wal", date_of(today - 2)));
    assert_eq!((wal["bytes"].as_u64(), wal["behind_mark"].as_bool()), (Some(20), Some(true)), "{out}");
    let kept = file(&receipts, &format!("control.receipts-{}.db", date_of(today - 1)));
    assert_eq!((kept["bytes"].as_u64(), kept["behind_mark"].as_bool()), (Some(30), Some(false)), "{out}");
    let events = family("events");
    assert_eq!(events["mark"].as_str(), Some(date_of(today - 14).as_str()), "{out}");
    assert_eq!(file(&events, &format!("control.events-{}.db", date_of(today - 14)))["behind_mark"], false, "{out}");
    let shm = file(&events, &format!("control.events-{}.db-shm", date_of(today - 15)));
    assert_eq!((shm["bytes"].as_u64(), shm["behind_mark"].as_bool()), (Some(60), Some(true)), "{out}");
}

fn watch_rows<B: Backend>(s: &Store<B>, prefixes: &[&str], from: u64) -> Value {
    let from = from.to_string();
    let mut args: Vec<String> = std::iter::once("watch").chain(prefixes.iter().copied()).map(String::from).collect();
    args.extend(["--from-rev", &from, "--wait-ms", "0", "--rows"].map(String::from));
    let request = crate::control::served::call_request_id();
    let call = crate::control::state::Call { now: 300, request: &request, inputs: &crate::control::claimable::FIXED };
    let (code, out) = verbs::run_with_rows(s, &args, &call);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

fn a_watch_with_rows_carries_the_lifecycle_rows_its_events_name<B: Backend>(s: &Store<B>) {
    create(s, "sp-a");
    create(s, "sp-b");
    let from = s.contract().head().unwrap();
    toggle(s, "sp-a", 1);
    let plain = watch(s, keys::LIFECYCLE, from);
    assert!(plain.get("rows").is_none(), "without --rows the answer is as before: {plain}");
    let w = watch_rows(s, &[keys::LIFECYCLE], from);
    let ids: Vec<&str> = w["rows"].as_array().unwrap().iter().map(|r| r["bead_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["sp-a"], "only the bead the events name: {w}");
    assert_eq!(w["events"], plain["events"]);
    assert_eq!(w["rows"][0]["express"], "1", "the row is the one `list --ids` composes: {w}");
    assert!(w["rows"][0].get("claim_view").is_some(), "the claim verdict rides with it: {w}");
    let head = s.contract().head().unwrap();
    assert_eq!(watch_rows(s, &[keys::LIFECYCLE], head)["progress_rev"], head, "a quiet watch carries no rows");
}

#[test]
fn a_mem_watch_with_rows_carries_the_lifecycle_rows_its_events_name() {
    on_mem(a_watch_with_rows_carries_the_lifecycle_rows_its_events_name);
}
