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

fn version(s: &Store<MemBackend>, id: &str) -> u64 {
    s.record(id).unwrap().unwrap().value.row.version
}

fn create(s: &Store<MemBackend>, id: &str, repo: &str, at: i64) {
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

fn dwell(s: &Store<MemBackend>, argv: &[&str]) -> Value {
    let mut args = vec!["dwell".to_string()];
    args.extend(argv.iter().map(|a| a.to_string()));
    let (code, out) = verbs::run(s, &args);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

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

#[test]
fn landed_after_is_the_oldest_delivery_newer_than_the_mark() {
    let mut idx = super::LastIndex::default();
    let mut repo = |_: &str| Ok(Some("harness".to_string()));
    for at in [90, 120, 150, 110] {
        idx.fold("Delivered", "sp-a", at, &mut repo).unwrap();
    }
    idx.fold("Renew", "sp-a", 500, &mut repo).unwrap();
    assert!(!idx.last["harness"].contains_key("Renew"), "only the four kinds are kept");
    assert_eq!(idx.landed_after(100, 0).get("harness"), Some(&110));
    assert_eq!(idx.last["harness"]["Delivered"], 150);
}

#[test]
fn a_bad_flag_cannot_tell() {
    let s = fresh();
    let (code, out) = verbs::run(&s, &["dwell".into(), "--window".into(), "soon".into()]);
    assert_eq!(code, crate::CANNOT_TELL, "{out}");
}

/// Production's size: ~13k records (400 waiting) and ~300k audited txns, `recent` of them one a
/// second inside the last day (more than [`super::MAX_SCANNED`]), over the 600 beads in flight.
/// Written straight through the backend so the fixture builds in seconds. Returns the store, the
/// clock it is read at, and the newest time per repo and kind (and every Delivered) at or after
/// `now - 86_400`, as the audits say.
struct Sized {
    store: Store<state_store_sqlite::SqliteBackend>,
    now: i64,
    waiting: u64,
    last: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>>,
    delivered: std::collections::BTreeMap<String, Vec<i64>>,
    _dir: testkit::TempDir,
}

fn production_sized(recent: u64) -> Sized {
    use state_store::{events_key, Backend, Object, Rev, Write};
    use state_store_sqlite::{Options, SqliteBackend};
    let dir = testkit::TempDir::new("spira-lc-dwell-size");
    let b = SqliteBackend::open(&dir.path().join("control.db"), Options::default()).unwrap();
    let now: i64 = 2_000_000_000;
    let put = |key: String, data: Value, main: u64| Write::Put { key, value: Object::new(data).encode(), create_rev: Rev::new(main, 0), prev: None };
    let audit = |id: &str, event: &str, at: i64| serde_json::json!({ "refused": false, "evidence": { "then": [{ "machine": "bead", "lc_key": id, "event": event, "expect": "READY", "from_state": "READY", "to_state": "READY", "applied": true, "refusal": null, "evidence": null, "actor": "a", "at": at }] } });
    let (records, waiting, txns) = (13_000u64, 400u64, 300_000u64);
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
            let rec = serde_json::json!({ "bead_id": id, "state": state, "tip": null, "gate_key": null, "holder": null, "lease_until": null, "holds": [], "reason": null, "version": 1, "since": at, "repo": repo, "updated_at": at });
            batch.push(put(keys::lifecycle(&id), rec, main));
            repo_of.insert(id.clone(), repo.to_string());
        }
        seen.push((id, event, at));
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
    let c = Contract::open(b, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Sized { store: Store::new(c, FLOOR_MS), now, waiting, last, delivered, _dir: dir }
}

/// Run by hand, release as lc-serve runs:
/// `cargo test -p spira-lc --bin spira-lc --release dwell_ -- --ignored --nocapture`
#[test]
#[ignore]
fn dwell_over_a_day_on_a_production_sized_store_answers_from_the_index_inside_200ms_and_matches_the_audits() {
    let z = production_sized(80_000);
    let (s, now) = (&z.store, z.now);
    let mark = now - 3_600;
    let mut complete = false;
    for _ in 0..10 {
        complete = s.dwell(now, 86_400, Some(mark)).unwrap().complete;
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
        let landed: std::collections::BTreeMap<String, i64> = z.delivered.iter().filter_map(|(r, ts)| Some((r.clone(), *ts.iter().filter(|t| **t > mark).min()?))).collect();
        assert_eq!(d.landed_after, landed);
    }
    times.sort();
    eprintln!("dwell --window 86400: median {:?}, max {:?}", times[2], times[4]);
    assert!(times[2] < std::time::Duration::from_millis(200), "{times:?}");
}

#[test]
fn a_warm_dwell_reads_only_the_revisions_committed_since_the_last_answer() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    let first = s.dwell(4_000, 100_000, None).unwrap();
    assert!(first.scanned > 0);
    assert_eq!(s.dwell(4_000, 100_000, None).unwrap().scanned, 0, "nothing new");
    work(&s, "sp-b", "harness", 3_000, None);
    let d = s.dwell(4_000, 100_000, None).unwrap();
    assert_eq!(d.last["harness"]["Claim"], 3_000, "{d:?}");
    assert!(d.scanned > 0 && d.scanned < first.scanned + 8, "{d:?}");
}

#[test]
fn a_narrower_window_does_not_report_what_a_wider_one_indexed() {
    let s = fresh();
    work(&s, "sp-a", "harness", 2_000, Some(2_500));
    let wide = s.dwell(4_000, 100_000, None).unwrap();
    assert_eq!(wide.last["harness"]["Submit"], 2_500);
    let narrow = s.dwell(4_000, 100, None).unwrap();
    assert!(narrow.last.get("harness").is_none_or(|k| !k.contains_key("Submit")), "{narrow:?}");
    assert!(narrow.complete);
}

#[test]
fn a_refused_or_other_machines_audit_is_not_a_bead_event_but_still_dates_the_scan() {
    let a = serde_json::json!({ "machine": "bead", "lc_key": "sp-a", "event": "Submit", "applied": false, "at": 7 });
    assert_eq!(super::bead_audit(&a), (None, Some(7)));
    let b = serde_json::json!({ "machine": "delivery", "lc_key": "sp-a", "event": "Delivered", "applied": true, "at": 8 });
    assert_eq!(super::bead_audit(&b), (None, Some(8)));
    let c = serde_json::json!({ "machine": "bead", "lc_key": "sp-a", "event": "Submit", "applied": true, "at": 9 });
    assert_eq!(super::bead_audit(&c), (Some(("Submit", "sp-a")), Some(9)));
}
