//! The control store as lc-serve opens it — SQLite under the in-memory read model — answering
//! the observers' verbs: a read after a commit reflects it, a read that names a revision is at
//! least that fresh, and list, show and claim-verdicts neither read the database nor wait for a
//! commit in flight.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use runtime::ports::VirtualClock;
use state_store::{Backend, Config, Contract, LogEntry, MemBackend, Modeled, Ports, Result, Version, Write};
use state_store_sqlite::{Options, SqliteBackend};

use super::served::{MODEL_WINDOW, UNMODELED};
use super::state::{self, Call};
use super::store::Store;

const AT: i64 = 1_767_225_600;
const FLOOR_MS: u64 = 1_000;

fn store<B: Backend>(b: B) -> Store<Modeled<B>> {
    let clock = VirtualClock::new(AT as u64 * 1_000);
    let model = Modeled::open(b, UNMODELED, MODEL_WINDOW).unwrap();
    let contract = Contract::open(model, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Store::new(contract, FLOOR_MS)
}

fn run<B: Backend>(s: &Store<B>, argv: &[&str]) -> (i32, String) {
    let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let request = super::served::request_id();
    state::run(s, &args, &Call { now: AT, request: &request })
}

fn ok<B: Backend>(s: &Store<B>, argv: &[&str]) -> String {
    let (code, out) = run(s, argv);
    assert_eq!(code, 0, "{argv:?}: {out}");
    out
}

fn claim<B: Backend>(s: &Store<B>, id: &str) {
    ok(s, &["event", "bead", id, "--expect", "READY", "--version", "0", "--actor", "t", "--kind", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#]);
}

fn states(list: &str) -> Vec<(String, String)> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(list).unwrap();
    rows.iter().map(|r| (r["bead_id"].as_str().unwrap().to_string(), r["state"].as_str().unwrap().to_string())).collect()
}

fn database_reads<B: Backend>(s: &Store<Modeled<B>>) -> u64 {
    s.contract().backend().reads().1
}

/// SQLite, recording the key or prefix of every read that reaches it.
struct Recorded {
    inner: SqliteBackend,
    seen: Mutex<Vec<String>>,
}

impl Recorded {
    fn note(&self, what: &str) {
        self.seen.lock().unwrap().push(what.to_string());
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl Backend for Recorded {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.note(key);
        self.inner.get(key, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.note(prefix);
        self.inner.range(prefix, after, limit, at)
    }
    fn range_in_state(&self, prefix: &str, state: &str, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.note(prefix);
        self.inner.range_in_state(prefix, state, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> Result<()> {
        self.inner.compact(rev)
    }
}

#[test]
fn a_read_after_a_commit_reflects_it_and_never_reads_the_database() {
    let dir = testkit::TempDir::new("lc-model");
    let sqlite = SqliteBackend::open(&dir.join("control.db"), Options::default()).unwrap();
    let s = store(Recorded { inner: sqlite, seen: Mutex::new(Vec::new()) });
    ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    ok(&s, &["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    let sqlite = || s.contract().backend().inner();
    claim(&s, "sp-a");
    sqlite().take();
    let listed = ok(&s, &["list", "--state", "READY,WORKING"]);
    assert_eq!(states(&listed), [("sp-a".into(), "WORKING".into()), ("sp-b".into(), "READY".into())]);
    let shown: serde_json::Value = serde_json::from_str(&ok(&s, &["show", "sp-a"])).unwrap();
    assert_eq!(shown["bead"]["state"], "WORKING");
    assert!(shown["bead"]["lease_until"].as_str().is_some_and(|l| !l.is_empty()), "the lease is projected from the claim's lock: {shown}");
    assert_eq!(sqlite().take(), Vec::<String>::new(), "list and show read nothing from SQLite");
    let body = r#"{"computed_at":"2026-01-01T00:00:00Z","entries":[{"id":"sp-b","verdict":"claimable","runes":["builder"]}]}"#;
    ok(&s, &["claim-verdicts", "--json", body]);
    // A write's idempotency check reads its request key, which the model leaves to SQLite.
    let read = sqlite().take();
    assert!(!read.is_empty() && read.iter().all(|k| k.starts_with(state_store::REQUESTS_PREFIX)), "claim-verdicts read only its request key from SQLite: {read:?}");
    let listed: Vec<serde_json::Value> = serde_json::from_str(&ok(&s, &["list", "--ids", "sp-b"])).unwrap();
    assert_eq!(listed[0]["claim_verdict"], "claimable", "the verdict pass is read back at once");
    assert_eq!(sqlite().take(), Vec::<String>::new());
}

#[test]
fn a_request_is_stamped_with_the_revision_it_reflects_and_one_naming_a_revision_the_model_lacks_reads_through() {
    let s = store(MemBackend::new());
    ok(&s, &["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    state_store::model::track(false);
    claim(&s, "sp-a");
    let wrote = state_store::model::stamp().expect("a write is stamped");
    let model = s.contract().backend();
    assert_eq!(model.revision(), wrote, "the model holds the write before the write returns");
    assert!(model.wait_for(wrote, Duration::ZERO), "a read naming the write's revision is fresh at once");
    state_store::model::track(false);
    ok(&s, &["show", "sp-a"]);
    assert!(state_store::model::stamp().is_some_and(|r| r >= wrote), "the read reflects the write");
    assert!(!model.wait_for(wrote + 1, Duration::from_millis(20)), "a revision not yet written is not reached");
    let before = database_reads(&s);
    state_store::model::track(true);
    let shown: serde_json::Value = serde_json::from_str(&ok(&s, &["show", "sp-a"])).unwrap();
    assert_eq!(shown["bead"]["state"], "WORKING");
    state_store::model::stamp();
    assert!(database_reads(&s) > before, "a read through goes to the database");
}

/// A backend whose next commit waits until it is let go, as SQLite's writer holds a commit
/// through its fsync.
#[derive(Clone, Default)]
struct Stalled {
    inner: MemBackend,
    gate: Arc<(Mutex<(bool, bool)>, Condvar)>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Stalled {
    fn stall_next_commit(&self) {
        self.gate.0.lock().unwrap().0 = true;
    }
    fn wait_until_stalled(&self) {
        let (m, cv) = &*self.gate;
        let _held = cv.wait_while(m.lock().unwrap(), |(armed, _)| *armed).unwrap();
    }
    fn release(&self) {
        let (m, cv) = &*self.gate;
        m.lock().unwrap().1 = true;
        cv.notify_all();
    }
}

impl Backend for Stalled {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.seen.lock().unwrap().push(key.to_string());
        self.inner.get(key, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.seen.lock().unwrap().push(prefix.to_string());
        self.inner.range(prefix, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let (m, cv) = &*self.gate;
        let mut g = m.lock().unwrap();
        if g.0 {
            g.0 = false;
            cv.notify_all();
            let _released = cv.wait_while(g, |(_, released)| !*released).unwrap();
        } else {
            drop(g);
        }
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> Result<()> {
        self.inner.compact(rev)
    }
}

#[test]
fn list_show_and_a_claim_verdict_read_do_not_wait_for_a_commit_in_flight() {
    let b = Stalled::default();
    let s = Arc::new(store(b.clone()));
    for id in ["sp-a", "sp-b"] {
        ok(&s, &["create-bead", id, "--repo", "spira", "--ready"]);
    }
    claim(&s, "sp-a");
    b.stall_next_commit();
    let writer = Arc::clone(&s);
    let held = std::thread::spawn(move || claim(&writer, "sp-b"));
    b.wait_until_stalled();
    // A read that waits for the commit would wait for ever: the watchdog lets the commit go.
    let (watchdog, fired) = (b.clone(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let fired_by = Arc::clone(&fired);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5)); // clock-safe: a cap that frees a read waiting on the commit, which fails the test
        fired_by.store(true, std::sync::atomic::Ordering::SeqCst);
        watchdog.release();
    });
    let before = database_reads(&s);
    let listed = ok(&s, &["list", "--live"]);
    let shown = ok(&s, &["show", "sp-a"]);
    let waited = fired.load(std::sync::atomic::Ordering::SeqCst);
    let read = database_reads(&s) - before;
    b.release();
    held.join().unwrap();
    assert!(!waited, "the reads were answered only once the commit in flight was let go");
    assert_eq!(states(&listed), [("sp-a".into(), "WORKING".into()), ("sp-b".into(), "READY".into())], "the reads see the store as it was before the commit");
    assert!(shown.contains("WORKING"));
    assert_eq!(read, 0, "no read reached the database: {:?}", b.seen.lock().unwrap());
    let after = ok(&s, &["list", "--live"]);
    assert_eq!(states(&after)[1], ("sp-b".into(), "WORKING".into()), "and the commit once it lands");
}

/// A production-size store (13k records, 300k audit events) read by `list` and `show`, from
/// SQLite alone and through the read model, quiet and while a verdict pass commits beside it.
/// A measurement, not an assertion: `cargo test --release -p spira-lc -- --ignored --nocapture
/// production_size`.
#[test]
#[ignore]
fn production_size_list_and_show_latency() {
    const BEADS: usize = 13_000;
    const EVENTS: usize = 300_000;
    let dir = testkit::TempDir::new("lc-model-bench");
    // One record as the verbs write it, then copied under 13k ids by a bulk load.
    let proto = store(MemBackend::new());
    ok(&proto, &["create-bead", "sp-proto", "--repo", "spira", "--ready"]);
    let body = r#"{"computed_at":"2026-01-01T00:00:00Z","entries":[{"id":"sp-proto","verdict":"claimable","runes":["builder"]}]}"#;
    ok(&proto, &["claim-verdicts", "--json", body]);
    let head = proto.contract().head().unwrap();
    let (items, _) = proto.contract().list(&[""], &state_store::Selector::Everything).unwrap();
    let event = items.iter().find(|i| i.key.starts_with(state_store::EVENTS_PREFIX)).unwrap().value.encode();
    let mut load: Vec<(String, Vec<u8>)> = Vec::new();
    for n in 0..BEADS {
        let id = format!("sp-b{n:05}");
        let state = match n % 100 { 0..=3 => "Ready", 4 => "Submitted", _ => "Landed" };
        for i in items.iter().filter(|i| i.key.contains("sp-proto") && !i.key.starts_with("requests/") && !i.key.starts_with("history/")) {
            let text = String::from_utf8(i.value.encode()).unwrap().replace("sp-proto", &id);
            let text = if i.key.starts_with("lifecycle/") { text.replace("\"Ready\"", &format!("\"{state}\"")) } else { text };
            load.push((i.key.replace("sp-proto", &id), text.into_bytes()));
        }
        for h in 0..EVENTS / BEADS {
            load.push((format!("history/{id}/{h:020}/r"), event.clone()));
        }
    }
    for e in 0..EVENTS {
        load.push((state_store::events_key(e as u64 + 1), event.clone()));
    }
    load.sort_by(|a, b| a.0.cmp(&b.0));
    let sqlite = SqliteBackend::open(&dir.join("control.db"), Options::default()).unwrap();
    sqlite.bulk_load(head.max(EVENTS as u64 + 1), load).unwrap();
    let clock = VirtualClock::new(AT as u64 * 1_000);
    let contract = |b| Contract::open(b, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    let plain = Arc::new(Store::new(contract(sqlite.clone()), FLOOR_MS));
    let opened = Instant::now();
    let modeled = Arc::new(Store::new(Contract::open(Modeled::open(sqlite.clone(), UNMODELED, MODEL_WINDOW).unwrap(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap(), FLOOR_MS));
    let (keys, bytes) = modeled.contract().backend().size();
    println!("model: {keys} keys, {} KiB, loaded in {:?}", bytes / 1024, opened.elapsed());
    fn p50<B: Backend>(s: &Store<B>, argv: &[&str]) -> (Duration, Duration) {
        let mut t: Vec<Duration> = (0..41)
            .map(|_| {
                let at = Instant::now();
                ok(s, argv);
                at.elapsed()
            })
            .collect();
        t.sort();
        (t[20], t[37])
    }
    fn writer<B: Backend + 'static>(s: Arc<Store<B>>, stop: Arc<std::sync::atomic::AtomicBool>) -> std::thread::JoinHandle<(usize, Duration)> {
        std::thread::spawn(move || {
            let entries: Vec<String> = (0..BEADS).step_by(100).map(|n| format!(r#"{{"id":"sp-b{n:05}","verdict":"claimable","runes":["builder"]}}"#)).collect();
            let body = format!(r#"{{"computed_at":"2026-01-01T00:00:00Z","entries":[{}]}}"#, entries.join(","));
            let (mut n, mut took) = (0, Duration::ZERO);
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let at = Instant::now();
                ok(&s, &["claim-verdicts", "--json", &body]);
                took += at.elapsed();
                n += 1;
            }
            (n, took)
        })
    }
    let verbs: [&[&str]; 5] = [&["list"], &["list", "--live"], &["list", "--state", "READY,REWORK"], &["list", "--ids", "sp-b00003,sp-b00103,sp-b05000"], &["show", "sp-b00003"]];
    for (name, quiet_plain, quiet_model) in verbs.iter().map(|v| (v.join(" "), p50(&plain, v), p50(&modeled, v))) {
        println!("quiet  {name:40} sqlite p50 {:>10.3?} p90 {:>10.3?} | model p50 {:>10.3?} p90 {:>10.3?}", quiet_plain.0, quiet_plain.1, quiet_model.0, quiet_model.1);
    }
    for (label, run) in [("sqlite", false), ("model", true)] {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let w = if run { writer(Arc::clone(&modeled), Arc::clone(&stop)) } else { writer(Arc::clone(&plain), Arc::clone(&stop)) };
        for v in verbs {
            let (a, b) = if run { p50(&modeled, v) } else { p50(&plain, v) };
            println!("writes {:40} {label} p50 {a:>10.3?} p90 {b:>10.3?}", v.join(" "));
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let (n, took) = w.join().unwrap();
        println!("writes claim-verdicts (130 entries) {label}: {n} passes, mean {:?}", took / n.max(1) as u32);
    }
}
