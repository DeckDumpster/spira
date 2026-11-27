use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState, HoldKind, Stack};
use lifecycle::reason::HoldCause;
use runtime::ports::VirtualClock;
use serde_json::Value;
use state_store::{Config, Contract, MemBackend, Ports, Request};

use crate::control::keys;
use crate::control::store::{Outcome, Store};
use crate::control::verbs;

const FLOOR_MS: u64 = 1_000;
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

fn fresh() -> Store<MemBackend> {
    let clock = VirtualClock::new(0);
    let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Store::new(c, FLOOR_MS)
}

fn req(id: &str) -> Request {
    Request::new(id, "test", "t")
}

fn ev(expect: BeadState, version: u64, kind: BeadEventKind, at: i64) -> BeadEvent {
    BeadEvent { expect, version, kind, actor: "dwarf-1".into(), at: Some(at) }
}

fn version<B: state_store::Backend>(s: &Store<B>, id: &str) -> u64 {
    s.record(id).unwrap().unwrap().value.row.version
}

fn create<B: state_store::Backend>(s: &Store<B>, id: &str, repo: &str, at: i64) {
    s.create(id, Some(repo), None, at, &req(&format!("create-{id}"))).unwrap();
}

/// `id` claimed at `at`, and submitted at `submit` when given; WORKING otherwise.
fn work(s: &Store<MemBackend>, id: &str, repo: &str, at: i64, submit: Option<i64>) {
    create(s, id, repo, at - 10);
    let claim = BeadEventKind::Claim { holder: "dwarf-1".into(), lease_until: at + 1_000_000, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4, persona: None, directed: None };
    let (out, fence) = s.claim(id, &ev(BeadState::Ready, version(s, id), claim, at), &req(&format!("claim-{id}"))).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    if let Some(t) = submit {
        let sub = BeadEventKind::Submit { tip: TIP.into() };
        let out = s.holder_apply(id, &fence.unwrap(), &ev(BeadState::Working, version(s, id), sub, t), &req(&format!("submit-{id}"))).unwrap();
        assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    }
}

fn ask(s: &Store<MemBackend>, id: &str, at: i64) {
    let hold = BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some("which?".into()) };
    let out = s.apply(id, &ev(BeadState::Ready, version(s, id), hold, at), &req(&format!("ask-{id}"))).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
}

/// The sweep thread's warm-up to `window` before `now`, which a request otherwise finds done.
fn warm<B: state_store::Backend>(s: &Store<B>, now: i64, window: u64) {
    s.warm_dwell(now, window, 100_000).unwrap();
}

fn dwell(s: &Store<MemBackend>, argv: &[&str]) -> Value {
    let flag = |name: &str| argv.iter().position(|a| *a == name).and_then(|i| argv.get(i + 1)).and_then(|v| v.parse::<i64>().ok());
    warm(s, flag("--now").unwrap_or(0), flag("--window").unwrap_or(3_600) as u64);
    let mut args = vec!["dwell".to_string()];
    args.extend(argv.iter().map(|a| a.to_string()));
    let (code, out) = verbs::run(s, &args);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

#[test_id(tg5kn6y)]
#[test]
fn every_waiting_record_answers_with_its_state_repo_and_entry_time_and_terminal_ones_do_not() {
    let s = fresh();
    create(&s, "sp-r", "harness", 1_000);
    work(&s, "sp-w", "harness", 2_000, None);
    work(&s, "sp-s", "other", 2_000, Some(3_000));
    s.create_open("sp-epic", Some("harness"), 500, &req("open")).unwrap();
    let d = dwell(&s, &["--now", "4000", "--window", "100000"]);
    let beads: Vec<(String, String, String, i64)> = d["beads"].as_array().unwrap().iter().map(|b| (b["id"].as_str().unwrap().into(), b["state"].as_str().unwrap().into(), b["repo"].as_str().unwrap().into(), b["since"].as_i64().unwrap())).collect();
    assert_eq!(
        beads,
        [("sp-r".into(), "READY".into(), "harness".into(), 1_000), ("sp-s".into(), "SUBMITTED".into(), "other".into(), 3_000), ("sp-w".into(), "WORKING".into(), "harness".into(), 2_000)],
        "OPEN is a container and waits for nothing: {d}"
    );
}

#[test_id(tj4vop3)]
#[test]
fn the_newest_claim_and_submit_per_repo_come_from_the_audit_log() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    work(&s, "sp-b", "harness", 3_000, None);
    work(&s, "sp-c", "other", 1_000, Some(1_200));
    let d = dwell(&s, &["--now", "4000", "--window", "100000"]);
    assert_eq!(d["last"]["harness"]["Claim"], 3_000, "{d}");
    assert_eq!(d["last"]["harness"]["Submit"], 2_500, "{d}");
    assert_eq!(d["last"]["other"]["Submit"], 1_200, "{d}");
    assert!(d["last"]["harness"].get("Delivered").is_none(), "nothing delivered: {d}");
    assert_eq!(d["complete"], true, "{d}");
}

#[test_id(t3wkszk)]
#[test]
fn an_ask_held_record_carries_when_its_ask_was_raised() {
    let s = fresh();
    create(&s, "sp-q", "harness", 1_000);
    ask(&s, "sp-q", 1_700);
    create(&s, "sp-n", "harness", 1_000);
    let d = dwell(&s, &["--now", "4000"]);
    let q = d["beads"].as_array().unwrap().iter().find(|b| b["id"] == "sp-q").unwrap().clone();
    assert_eq!((q["ask_held"].as_bool(), q["ask_since"].as_i64()), (Some(true), Some(1_700)), "{d}");
    let n = d["beads"].as_array().unwrap().iter().find(|b| b["id"] == "sp-n").unwrap().clone();
    assert_eq!((n["ask_held"].as_bool(), n.get("ask_since")), (Some(false), None), "{d}");
}

#[test_id(tvnxvof)]
#[test]
fn a_scan_that_stops_short_names_its_horizon_and_is_not_complete() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    let d = dwell(&s, &["--now", "4000", "--window", "10"]);
    assert_eq!(d["complete"], true, "it read past now - window");
    assert!(d["horizon"].as_i64().unwrap() <= 3_990, "{d}");
    let empty = fresh();
    let d = dwell(&empty, &["--now", "4000", "--window", "60"]);
    assert_eq!((d["complete"].as_bool(), d["horizon"].as_i64(), d["scanned"].as_u64()), (Some(true), Some(3_940), Some(0)), "an empty log is read to its start: {d}");
}

#[test_id(t04xl69)]
#[test]
fn landed_after_is_every_delivery_newer_than_the_mark_oldest_first() {
    let mut idx = super::super::LastIndex::default();
    let mut repo = |_: &str| Ok(Some("harness".to_string()));
    for at in [90, 120, 150, 110] {
        idx.fold("Delivered", "sp-a", at, "", &mut repo).unwrap();
    }
    idx.fold("Renew", "sp-a", 500, "", &mut repo).unwrap();
    assert!(
        !idx.last["harness"].contains_key("Renew"),
        "only the four kinds are kept"
    );
    let times: Vec<i64> = idx.landed_after(100, 0)["harness"]
        .iter()
        .map(|l| l.at)
        .collect();
    assert_eq!(
        times,
        [110, 120, 150],
        "every delivery newer than the mark, oldest first"
    );
    assert_eq!(idx.last["harness"]["Delivered"], 150);
}

#[test_id(tobo5hm)]
#[test]
fn a_bad_flag_cannot_tell() {
    let s = fresh();
    let (code, out) = verbs::run(&s, &["dwell".into(), "--window".into(), "soon".into()]);
    assert_eq!(code, crate::CANNOT_TELL, "{out}");
}

/// `records` lifecycle rows (`waiting` of them waiting) and `txns` audited txns, `recent` of them
/// one a second inside the last day, over the 600 beads in flight. Written straight through the
/// backend. Holds the store, the clock it is read at, and the newest time per repo and kind (and
/// every Delivered) at or after `now - 86_400`, as the audits say.
struct Sized {
    store: Store<Counting<state_store_sqlite::SqliteBackend>>,
    now: i64,
    waiting: u64,
    last: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>>,
    delivered: std::collections::BTreeMap<String, Vec<i64>>,
    _dir: testkit::TempDir,
}

/// A backend that counts the `events/` and `history/` keys it hands out and every read call.
struct Counting<B> {
    inner: B,
    events: std::sync::atomic::AtomicUsize,
    history: std::sync::atomic::AtomicUsize,
    calls: std::sync::atomic::AtomicUsize,
}

impl<B> Counting<B> {
    fn new(inner: B) -> Self {
        Counting { inner, events: Default::default(), history: Default::default(), calls: Default::default() }
    }
    fn events_read(&self) -> usize {
        self.events.swap(0, std::sync::atomic::Ordering::SeqCst)
    }
    fn history_read(&self) -> usize {
        self.history.swap(0, std::sync::atomic::Ordering::SeqCst)
    }
    fn calls_made(&self) -> usize {
        self.calls.swap(0, std::sync::atomic::Ordering::SeqCst)
    }
    fn handed_out<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        use std::sync::atomic::Ordering::SeqCst;
        for key in keys {
            if key.starts_with(state_store::EVENTS_PREFIX) {
                self.events.fetch_add(1, SeqCst);
            } else if key.starts_with(state_store::HISTORY_PREFIX) {
                self.history.fetch_add(1, SeqCst);
            }
        }
    }
}

impl<B: state_store::Backend> state_store::Backend for Counting<B> {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.due(now, limit, at)
    }
    fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.owned_by(owner, after, limit, at)
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let found = self.inner.get(key, at)?;
        if found.is_some() {
            self.handed_out([key]);
        }
        Ok(found)
    }
    fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let found = self.inner.get_many(keys, at)?;
        self.handed_out(keys.iter().zip(&found).filter(|(_, v)| v.is_some()).map(|(k, _)| *k));
        Ok(found)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let rows = self.inner.range(prefix, after, limit, at)?;
        self.handed_out(rows.iter().map(|(k, _)| k.as_str()));
        Ok(rows)
    }
    fn range_in_state(&self, prefix: &str, state: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.range_in_state(prefix, state, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[state_store::Write]) -> state_store::Result<u64> {
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<state_store::LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> state_store::Result<()> {
        self.inner.compact(rev)
    }
    fn compacted(&self) -> state_store::Result<u64> {
        self.inner.compacted()
    }
}

#[derive(Clone, Copy)]
struct Shape {
    records: u64,
    waiting: u64,
    txns: u64,
    recent: u64,
    chunk: u64,
}

/// Production's size, for the by-hand timing run only: one sqlite commit per txn makes it a
/// minute's build, which a loaded gate host turns into a flip.
const PRODUCTION: Shape = Shape { records: 13_000, waiting: 400, txns: 300_000, recent: 80_000, chunk: super::CHUNK_REVS };

/// The smallest store whose day outruns a cold read's chunk and a warm call's budget several times.
const CHUNKED: Shape = Shape { records: 600, waiting: 100, txns: 850, recent: 250, chunk: 10 };

fn sized(shape: Shape) -> Sized {
    let Shape { records, waiting, txns, recent, chunk } = shape;
    assert!(recent > 2 * 10 * chunk && records >= 600 && records <= txns - recent, "the shape cannot exercise the chunked warm");
    use state_store::{events_key, Backend, Object, Rev, Write};
    use state_store_sqlite::{Options, SqliteBackend};
    let dir = testkit::TempDir::new("spira-lc-dwell-size");
    let b = SqliteBackend::open(&dir.path().join("control.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
    let now: i64 = 2_000_000_000;
    let put = |key: String, data: Value, main: u64| Write::Put { key, value: Object::new(data).encode(), create_rev: Rev::new(main, 0), prev: None };
    let audit = |id: &str, event: &str, at: i64| serde_json::json!({ "refused": false, "evidence": { "then": [{ "machine": "bead", "lc_key": id, "event": event, "expect": "READY", "from_state": "READY", "to_state": "READY", "applied": true, "refusal": null, "evidence": null, "actor": "a", "at": at }] } });
    const ASK_HELD: u64 = 9;
    let mut head = b.head().unwrap();
    let mut repo_of = std::collections::BTreeMap::new();
    let mut seen: Vec<(String, &str, i64)> = vec![];
    for main_n in 1..=txns {
        let main = head + 1;
        let in_window = main_n > txns - recent;
        let i = if in_window { main_n % 600 } else { main_n % records };
        let id = format!("sp-{i:05}");
        let at = if in_window { now - ((txns - main_n) as i64) } else { now - 86_400 - ((txns - main_n) as i64) };
        let event = ["Claim", "Submit", "Deliver", "Delivered", "Renew"][(main_n % 5) as usize];
        let mut batch = vec![put(events_key(main), audit(&id, event, at), main)];
        if main_n <= records {
            let state = if main_n <= waiting { "Working" } else { "Landed" };
            let repo = ["spira", "deckdumpster", "pokedumpster"][(main_n % 3) as usize];
            let asked = main_n <= ASK_HELD;
            let rec = serde_json::json!({ "bead_id": id, "state": state, "tip": null, "gate_key": null, "holder": null, "lease_until": null, "holds": if asked { vec!["Ask"] } else { vec![] }, "ask_since": if asked { Some(at) } else { None }, "reason": null, "version": 1, "since": at, "repo": repo, "updated_at": at });
            batch.push(put(keys::lifecycle(&id), rec, main));
            repo_of.insert(id.clone(), repo.to_string());
        }
        seen.push((id, event, at));
        head = b.commit(head, &batch).unwrap();
    }
    for n in 1..=ASK_HELD {
        let id = format!("sp-{n:05}");
        let batch: Vec<Write> = (0..100usize).map(|k| { let event = ["Hold", "Claim", "Submit"][k % 3]; put(format!("history/{id}/0/import/{k}"), serde_json::json!({ "seq": k + 1, "audit": { "machine": "bead", "lc_key": id, "event": event, "applied": true, "at": k as i64, "evidence": { "Hold": { "kind": "Wait" } } } }), head + 1) }).collect();
        head = b.commit(head, &batch).unwrap();
    }
    let (mut last, mut delivered) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::<String, Vec<i64>>::new());
    for (id, event, at) in seen.into_iter().filter(|(_, e, at)| super::KINDS.contains(e) && *at >= now - 86_400) {
        let repo = repo_of[&id].clone();
        let newest: &mut i64 = last.entry(repo.clone()).or_insert_with(std::collections::BTreeMap::new).entry(event.to_string()).or_insert(at);
        *newest = (*newest).max(at);
        if event == "Delivered" {
            delivered.entry(repo).or_default().push(at);
        }
    }
    let clock = VirtualClock::new(0);
    let c = Contract::open(Counting::new(b), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    let mut store = Store::new(c, FLOOR_MS);
    store.dwell_chunk_revs = chunk;
    Sized { store, now, waiting, last, delivered, _dir: dir }
}

/// `beads` ask-held beads, each raised `entries` times (every raise is one applied history
/// entry), on a counting store; the newest raise per bead.
fn ask_held(beads: usize, entries: usize) -> (Store<Counting<MemBackend>>, Vec<(String, i64)>) {
    let clock = VirtualClock::new(0);
    let c = Contract::open(Counting::new(MemBackend::new()), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    let s = Store::new(c, FLOOR_MS);
    let mut newest = vec![];
    for b in 0..beads {
        let id = format!("sp-{b:03}");
        create(&s, &id, "harness", 100);
        let mut at = 0;
        for n in 0..entries {
            at = 1_000 + (b * entries + n) as i64;
            let hold = BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some("which?".into()) };
            let out = s.apply(&id, &ev(BeadState::Ready, version(&s, &id), hold, at), &req(&format!("ask-{id}-{n}"))).unwrap();
            assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
        }
        newest.push((id, at));
    }
    (s, newest)
}

fn second_dwell_reads(s: &Store<Counting<MemBackend>>) -> (Value, usize, usize, usize) {
    let now = 1_000_000;
    warm(s, now, 3_600);
    let (code, _) = verbs::run(s, &["dwell".into(), "--now".into(), now.to_string(), "--window".into(), "3600".into()]);
    assert_eq!(code, 0);
    let counting = s.contract().backend();
    let _ = (counting.history_read(), counting.events_read(), counting.calls_made());
    let (code, out) = verbs::run(s, &["dwell".into(), "--now".into(), now.to_string(), "--window".into(), "3600".into()]);
    assert_eq!(code, 0, "{out}");
    let counting = s.contract().backend();
    (serde_json::from_str(&out).unwrap(), counting.history_read(), counting.events_read(), counting.calls_made())
}

#[test_id(t1kpl9q)]
#[test]
fn a_warm_dwell_reads_the_same_keys_however_many_beads_are_ask_held_and_however_long_their_history() {
    let (a, a_newest) = ask_held(1, 5);
    let (b, b_newest) = ask_held(10, 30);
    let (da, a_history, a_events, a_calls) = second_dwell_reads(&a);
    let (db, b_history, b_events, b_calls) = second_dwell_reads(&b);
    assert_eq!((a_history, b_history), (0, 0), "no history/ key is read");
    assert_eq!((a_events, b_events), (0, 0), "nothing new was committed");
    assert_eq!(a_calls, b_calls, "the same backend calls at both sizes");
    for (d, newest) in [(&da, &a_newest), (&db, &b_newest)] {
        for (id, at) in newest {
            let bead = d["beads"].as_array().unwrap().iter().find(|x| x["id"] == id.as_str()).unwrap();
            assert_eq!(bead["ask_since"].as_i64(), Some(*at), "{id}");
        }
    }
}

#[test_id(t9vv4rf)]
#[test]
fn an_ask_held_record_with_no_ask_since_cannot_tell_while_a_dated_ask_answers() {
    use state_store::{Backend, Object, Rev, Write};
    let b = MemBackend::new();
    let rec = serde_json::json!({ "bead_id": "sp-old", "state": "Rework", "holds": ["Ask"], "version": 1, "since": 5, "repo": "harness", "updated_at": 5 });
    let dated = serde_json::json!({ "bead_id": "sp-new", "state": "Rework", "holds": ["Ask"], "version": 1, "since": 5, "ask_since": 40, "repo": "harness", "updated_at": 5 });
    let main = b.head().unwrap() + 1;
    b.commit(
        main - 1,
        &[
            Write::Put { key: keys::lifecycle("sp-old"), value: Object::new(rec).encode(), create_rev: Rev::new(main, 0), prev: None },
            Write::Put { key: keys::lifecycle("sp-new"), value: Object::new(dated).encode(), create_rev: Rev::new(main, 1), prev: None },
        ],
    )
    .unwrap();
    let clock = VirtualClock::new(0);
    let s = Store::new(Contract::open(b, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap(), FLOOR_MS);
    let (code, out) = verbs::run(&s, &["dwell".into(), "--now".into(), "100".into()]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = |id: &str| v["beads"].as_array().unwrap().iter().find(|b| b["id"] == id).unwrap().clone();
    let old = row("sp-old");
    assert!(old["cannot_tell"].as_str().unwrap().contains("cannot tell: sp-old"), "{out}");
    assert!(old.get("ask_since").is_none(), "{out}");
    assert_eq!(row("sp-new")["ask_since"], 40, "{out}");
    assert!(row("sp-new").get("cannot_tell").is_none(), "{out}");
}

/// Run by hand, release as lc-serve runs:
/// `cargo test -p spira-lc --bin spira-lc --release dwell_ -- --ignored --nocapture`
#[test_id(tqoh3nc)]
#[test]
#[ignore]
fn dwell_over_a_day_on_a_production_sized_store_answers_from_the_index_inside_200ms_and_matches_the_audits() {
    let z = sized(PRODUCTION);
    let (s, now) = (&z.store, z.now);
    let mark = now - 3_600;
    let mut complete = false;
    for _ in 0..10 {
        complete = s.warm_dwell(now, 86_400, 1_000_000).unwrap().0;
        if complete {
            break;
        }
    }
    assert!(complete, "the index never covered the day");
    let mut times = vec![];
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let d = s.dwell(now, 86_400, Some(mark)).unwrap();
        times.push(t.elapsed());
        assert_eq!(d.beads.len() as u64, z.waiting);
        assert!(d.complete, "{d:?}");
        assert_eq!(d.scanned, 0, "nothing new to read");
        assert_eq!(d.last, z.last);
        let landed: std::collections::BTreeMap<String, Vec<i64>> = z
            .delivered
            .iter()
            .filter_map(|(r, ts)| {
                Some((
                    r.clone(),
                    ts.iter()
                        .copied()
                        .filter(|t| *t > mark)
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>(),
                ))
                .filter(|(_, v)| !v.is_empty())
            })
            .collect();
        let got: std::collections::BTreeMap<String, Vec<i64>> = d
            .landed_after
            .iter()
            .map(|(r, ls)| (r.clone(), ls.iter().map(|l| l.at).collect()))
            .collect();
        assert_eq!(got, landed);
    }
    times.sort();
    eprintln!("dwell --window 86400: median {:?}, max {:?}", times[2], times[4]);
    assert!(times[2] < std::time::Duration::from_millis(200), "{times:?}");
}

#[test_id(t9gvvr0)]
#[test]
fn a_warm_dwell_reads_only_the_revisions_committed_since_the_last_answer() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    warm(&s, 4_000, 100_000);
    let first = s.dwell(4_000, 100_000, None).unwrap();
    assert_eq!(first.scanned, 0, "the warm-up read it all");
    assert_eq!(s.dwell(4_000, 100_000, None).unwrap().scanned, 0, "nothing new");
    work(&s, "sp-b", "harness", 3_000, None);
    let d = s.dwell(4_000, 100_000, None).unwrap();
    assert_eq!(d.last["harness"]["Claim"], 3_000, "{d:?}");
    assert!(d.scanned > 0 && d.scanned < 8, "{d:?}");
}

#[test_id(tm8tswp)]
#[test]
fn a_narrower_window_does_not_report_what_a_wider_one_indexed() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    warm(&s, 4_000, 100_000);
    let wide = s.dwell(4_000, 100_000, None).unwrap();
    assert_eq!(wide.last["harness"]["Submit"], 2_500);
    let narrow = s.dwell(4_000, 100, None).unwrap();
    assert!(narrow.last.get("harness").is_none_or(|k| !k.contains_key("Submit")), "{narrow:?}");
    assert!(narrow.complete);
}

#[test_id(t22nuqv)]
#[test]
fn a_refused_or_other_machines_audit_is_not_a_bead_event_but_still_dates_the_scan() {
    let a = serde_json::json!({ "machine": "bead", "lc_key": "sp-a", "event": "Submit", "applied": false, "at": 7 });
    assert_eq!(super::super::bead_audit(&a), (None, Some(7)));
    let b = serde_json::json!({ "machine": "delivery", "lc_key": "sp-a", "event": "Delivered", "applied": true, "at": 8 });
    assert_eq!(super::super::bead_audit(&b), (None, Some(8)));
    let c = serde_json::json!({ "machine": "bead", "lc_key": "sp-a", "event": "Submit", "applied": true, "at": 9 });
    assert_eq!(super::super::bead_audit(&c), (Some(("Submit", "sp-a")), Some(9)));
}

#[test_id(tfzq1t2)]
#[test]
fn a_delivery_is_reported_with_the_commit_its_audit_recorded() {
    use state_store::{events_key, Backend, Object, Rev, Write};
    let b = MemBackend::new();
    let put = |key: String, data: Value, main: u64| Write::Put {
        key,
        value: Object::new(data).encode(),
        create_rev: Rev::new(main, 0),
        prev: None,
    };
    let rec = serde_json::json!({ "bead_id": "sp-a", "state": "Landed", "tip": null, "gate_key": null, "holder": null, "lease_until": null, "holds": [], "reason": null, "version": 1, "since": 1_000, "repo": "harness", "updated_at": 1_000 });
    let audit = |event: &str, evidence: Value, at: i64| serde_json::json!({ "refused": false, "evidence": { "then": [{ "machine": "bead", "lc_key": "sp-a", "event": event, "expect": "IN_DELIVERY", "from_state": "IN_DELIVERY", "to_state": "LANDED", "applied": true, "refusal": null, "evidence": evidence, "actor": "a", "at": at }] } });
    let sha = "1111111111111111111111111111111111111111";
    let head = b.head().unwrap();
    let head = b
        .commit(
            head,
            &[
                put(keys::lifecycle("sp-a"), rec, head + 1),
                put(
                    events_key(head + 1),
                    audit(
                        "Delivered",
                        serde_json::to_value(BeadEventKind::Delivered { merge_sha: sha.into(), proof: "p".into() }).unwrap(),
                        3_000,
                    ),
                    head + 1,
                ),
            ],
        )
        .unwrap();
    assert!(head > 0);
    let clock = VirtualClock::new(0);
    let s = Store::new(
        Contract::open(
            b,
            Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: FLOOR_MS,
            },
        )
        .unwrap(),
        FLOOR_MS,
    );
    let d = dwell(
        &s,
        &["--now", "4000", "--window", "3600", "--landed-after", "0"],
    );
    assert_eq!(
        d["landed_after"]["harness"],
        serde_json::json!([{ "at": 3_000, "commit": sha }]),
        "{d}"
    );
}

#[test_id(tsqanyl)]
#[test]
fn a_delivery_whose_audit_recorded_no_commit_is_reported_with_an_empty_one() {
    use state_store::{events_key, Backend, Object, Rev, Write};
    let b = MemBackend::new();
    let put = |key: String, data: Value, main: u64| Write::Put {
        key,
        value: Object::new(data).encode(),
        create_rev: Rev::new(main, 0),
        prev: None,
    };
    let rec = serde_json::json!({ "bead_id": "sp-a", "state": "Landed", "tip": null, "gate_key": null, "holder": null, "lease_until": null, "holds": [], "reason": null, "version": 1, "since": 1_000, "repo": "harness", "updated_at": 1_000 });
    let audit = |event: &str, evidence: Value, at: i64| serde_json::json!({ "refused": false, "evidence": { "then": [{ "machine": "bead", "lc_key": "sp-a", "event": event, "expect": "IN_DELIVERY", "from_state": "IN_DELIVERY", "to_state": "LANDED", "applied": true, "refusal": null, "evidence": evidence, "actor": "a", "at": at }] } });
    let head = b.head().unwrap();
    let head = b
        .commit(
            head,
            &[
                put(keys::lifecycle("sp-a"), rec, head + 1),
                put(
                    events_key(head + 1),
                    audit(
                        "Delivered",
                        serde_json::json!({ "Delivered": { "proof": "p" } }),
                        3_000,
                    ),
                    head + 1,
                ),
            ],
        )
        .unwrap();
    assert!(head > 0);
    let clock = VirtualClock::new(0);
    let s = Store::new(
        Contract::open(
            b,
            Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: FLOOR_MS,
            },
        )
        .unwrap(),
        FLOOR_MS,
    );
    let d = dwell(
        &s,
        &["--now", "4000", "--window", "3600", "--landed-after", "0"],
    );
    assert_eq!(
        d["landed_after"]["harness"],
        serde_json::json!([{ "at": 3_000, "commit": "" }]),
        "{d}"
    );
}

#[test_id(t4xbg64)]
#[test]
fn loop_transitions_and_dwell_share_one_advance() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    warm(&s, 4_000, 100_000);
    s.dwell(4_000, 100_000, None).unwrap();
    let t = s.loop_transitions(4_000, 100_000).unwrap();
    assert_eq!((t.scanned, t.complete), (0, true), "{t:?}");
}

#[test_id(t0w0t9m)]
#[test]
fn the_fold_keeps_landings_and_certified_stays_by_state_whatever_the_event() {
    let mut idx = super::super::LastIndex::default();
    let mut repo = |_: &str| Ok(Some("harness".to_string()));
    for (at, from, to) in [(100, "SUBMITTED", "CERTIFIED"), (150, "CERTIFIED", "CERTIFIED"), (400, "CERTIFIED", "LANDED"), (500, "READY", "WORKING")] {
        idx.fold_states("sp-a", at, (from, to), &mut repo).unwrap();
        idx.fold_states("sp-a", at, (from, to), &mut repo).unwrap();
    }
    let kept = &idx.transitions["sp-a"].1;
    assert_eq!(kept.iter().map(|t| t.0).collect::<Vec<_>>(), [100, 400], "idempotent, and only moves touching LANDED or CERTIFIED");
    idx.prune(200);
    assert_eq!(idx.transitions["sp-a"].1.len(), 1);
    idx.prune(1_000);
    assert!(idx.transitions.is_empty());
}

#[test_id(t9i2x8w)]
#[test]
fn a_stay_ending_in_the_window_is_reported_and_one_with_no_entry_in_the_index_is_not() {
    let s = fresh();
    s.create("sp-a", Some("harness"), Some(TIP), 1_000, &req("c")).unwrap();
    let gate = BeadEventKind::GatePass { tip: TIP.into(), gate_key: "k".into() };
    let out = s.apply("sp-a", &ev(BeadState::Submitted, 1, gate, 2_000), &req("g")).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    let out = s.apply("sp-a", &ev(BeadState::Certified, 2, BeadEventKind::Deliver, 2_300), &req("d")).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    warm(&s, 3_000, 5_000);
    let t = s.loop_transitions(3_000, 5_000).unwrap();
    assert_eq!(t.stays, [(2_300, 300)], "{t:?}");
    assert!(s.loop_transitions(3_000, 600).unwrap().stays.is_empty(), "left before the window");
}

fn landed_at(s: &Store<MemBackend>, id: &str, from: BeadState, kind: BeadEventKind, at: i64) {
    let out = s.apply(id, &ev(from, version(s, id), kind, at), &req(&format!("land-{id}"))).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
}

#[test_id(tdiqmf9)]
#[test]
fn a_hand_landing_counts_as_the_repos_newest_delivered() {
    let s = fresh();
    work(&s, "sp-h", "harness", 2_000, Some(2_500));
    landed_at(&s, "sp-h", BeadState::Submitted, BeadEventKind::ContentOnBase { proof: "ungated:concierge:r:evidence=x".into() }, 3_000);
    let d = dwell(&s, &["--now", "4000", "--window", "100000"]);
    assert_eq!(d["last"]["harness"]["Delivered"], 3_000, "{d}");
}

#[test_id(tvogh9d)]
#[test]
fn a_settle_landed_certified_bead_counts_as_delivered() {
    let s = fresh();
    s.create("sp-s", Some("harness"), Some(TIP), 1_000, &req("c")).unwrap();
    let gate = BeadEventKind::GatePass { tip: TIP.into(), gate_key: "k".into() };
    let out = s.apply("sp-s", &ev(BeadState::Submitted, 1, gate, 2_000), &req("g")).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
    landed_at(&s, "sp-s", BeadState::Certified, BeadEventKind::ContentOnBase { proof: "settle-landed:x".into() }, 3_100);
    let d = dwell(&s, &["--now", "4000", "--window", "100000"]);
    assert_eq!(d["last"]["harness"]["Delivered"], 3_100, "{d}");
}

#[test_id(tu3ems3)]
#[test]
fn the_newest_arrival_in_landed_wins_across_round_and_hand_landings() {
    let s = fresh();
    s.create("sp-r", Some("harness"), Some(TIP), 1_000, &req("cr")).unwrap();
    let gate = BeadEventKind::GatePass { tip: TIP.into(), gate_key: "k".into() };
    s.apply("sp-r", &ev(BeadState::Submitted, 1, gate, 2_000), &req("gr")).unwrap();
    s.apply("sp-r", &ev(BeadState::Certified, 2, BeadEventKind::Deliver, 2_300), &req("dr")).unwrap();
    landed_at(&s, "sp-r", BeadState::InDelivery, BeadEventKind::Delivered { merge_sha: TIP.into(), proof: "p".into() }, 2_800);
    work(&s, "sp-h", "harness", 2_000, Some(2_500));
    landed_at(&s, "sp-h", BeadState::Submitted, BeadEventKind::ContentOnBase { proof: "ungated:x".into() }, 3_000);
    let d = dwell(&s, &["--now", "4000", "--window", "100000"]);
    assert_eq!(d["last"]["harness"]["Delivered"], 3_000, "{d}");
}

#[test_id(tm67okx)]
#[test]
fn a_correction_or_a_non_landing_is_not_a_delivery() {
    let mut idx = super::super::LastIndex::default();
    let mut repo = |_: &str| Ok(Some("harness".to_string()));
    idx.fold_states("sp-z", 50, ("SUBMITTED", "LANDED"), &mut repo).unwrap();
    idx.fold_states("sp-a", 100, ("DROPPED", "LANDED"), &mut repo).unwrap();
    idx.fold_states("sp-a", 200, ("LANDED", "LANDED"), &mut repo).unwrap();
    idx.fold_states("sp-b", 300, ("READY", "WORKING"), &mut repo).unwrap();
    assert_eq!(idx.last["harness"]["Delivered"], 50, "only the real arrival counts: {:?}", idx.last);
}

#[test_id(tce989z)]
#[test]
fn the_first_dwell_on_a_store_many_chunks_deep_reads_no_more_than_a_chunk_and_says_it_is_short_and_warming_then_completes_it() {
    let z = sized(CHUNKED);
    let (s, now) = (&z.store, z.now);
    s.contract().backend().events_read();
    let (code, out) = verbs::run(s, &["dwell".into(), "--window".into(), "86400".into(), "--now".into(), now.to_string()]);
    assert_eq!(code, 0, "{out}");
    let d: Value = serde_json::from_str(&out).unwrap();
    let read = s.contract().backend().events_read();
    assert!(read <= s.dwell_chunk_revs as usize, "a cold request read {read} events keys");
    assert_eq!(d["complete"], false, "{d}");
    assert!(d["horizon"].as_i64().unwrap() >= now - 86_400, "{d}");
    warming_in_calls(&z);
}

fn warming_in_calls(z: &Sized) {
    let (s, now) = (&z.store, z.now);
    let mark = now - 3_600;
    let mut calls = 0;
    while !s.warm_dwell(now, 86_400, s.dwell_chunk_revs as usize * 10).unwrap().0 {
        calls += 1;
        assert!(calls < 200, "never covered the day");
    }
    assert!(calls > 1, "the budget should have split the read");
    let d = s.dwell(now, 86_400, Some(mark)).unwrap();
    assert!(d.complete, "{d:?}");
    assert_eq!(d.last, z.last);
    let got: std::collections::BTreeMap<String, Vec<i64>> = d.landed_after.iter().map(|(r, ls)| (r.clone(), ls.iter().map(|l| l.at).collect())).collect();
    let want: std::collections::BTreeMap<String, Vec<i64>> = z
        .delivered
        .iter()
        .map(|(r, ts)| (r.clone(), ts.iter().copied().filter(|t| *t > mark).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>()))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    assert_eq!(got, want);
}

#[test_id(t29v6rq)]
#[test]
fn loop_transitions_on_a_fresh_store_reads_nothing_back_and_is_short_until_warmed() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    let t = s.loop_transitions(4_000, 100_000).unwrap();
    assert_eq!((t.scanned, t.complete), (0, false), "{t:?}");
    warm(&s, 4_000, 100_000);
    let t = s.loop_transitions(4_000, 100_000).unwrap();
    assert_eq!((t.scanned, t.complete), (0, true), "{t:?}");
}

#[test_id(tntey77)]
#[test]
fn a_request_folds_a_bounded_tail_and_answers_short_until_the_sweep_catches_up() {
    const BOUND: u64 = 20;
    let mut s = fresh();
    s.dwell_forward_revs = BOUND;
    warm(&s, 4_000, 100);
    for i in 0..BOUND as usize {
        work(&s, &format!("sp-{i}"), "harness", 2_000 + i as i64, None);
    }
    assert!(s.contract().head().unwrap() > BOUND, "the writes must outrun the bound");
    let d = s.dwell(4_000, 100, None).unwrap();
    assert!(!d.complete && d.scanned > 0 && d.scanned <= BOUND as usize, "complete {} scanned {}", d.complete, d.scanned);
    assert_eq!(d.horizon, 4_000, "an index behind the head claims no coverage");
    warm(&s, 4_000, 100);
    assert!(s.dwell(4_000, 100, None).unwrap().complete);
}
