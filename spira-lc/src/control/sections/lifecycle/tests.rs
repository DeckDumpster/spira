use std::cell::{Cell, RefCell};

use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState};
use runtime::ports::{Clock, VirtualClock};
use serde_json::{json, Value};
use spira_otel::metrics::MetricsSink;
use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};

use super::{round_events, run, LoopEvent, Outside, RunDir, StopKind};

use crate::control::sections::Call;
use crate::control::store::Store;

const FLOOR_MS: u64 = 1_000;
/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600;
const MIN: u64 = 60;
const H: u64 = 3600;
const DAY: u64 = 86_400;

#[derive(Default)]
struct Loop {
    stops: RefCell<Vec<LoopEvent>>,
    rounds: RefCell<Vec<LoopEvent>>,
    broken: Cell<bool>,
    rounds_broken: Cell<bool>,
    no_releases: Cell<bool>,
}

impl Outside for Loop {
    fn stops(&self) -> Result<Vec<LoopEvent>, String> {
        if self.broken.get() {
            return Err("the stop family is unreadable".into());
        }
        Ok(self.stops.borrow().clone())
    }

    fn records_releases(&self) -> bool {
        !self.no_releases.get()
    }

    fn rounds(&self, since: u64) -> Result<Vec<LoopEvent>, String> {
        if self.rounds_broken.get() {
            return Err("cert-pass is unreadable".into());
        }
        Ok(self.rounds.borrow().iter().filter(|e| at(e) >= since).cloned().collect())
    }
}

fn at(e: &LoopEvent) -> u64 {
    match e {
        LoopEvent::Landed { at, .. } | LoopEvent::RedAttributed { at, .. } | LoopEvent::Eject { at, .. } | LoopEvent::Stop { at, .. } | LoopEvent::OverCapPass { at, .. } => *at,
    }
}

struct Rig<B: Backend> {
    store: Store<B>,
    clock: VirtualClock,
    n: Cell<u32>,
    sink: MetricsSink,
    outside: Loop,
}

fn rig() -> Rig<MemBackend> {
    rig_on(MemBackend::new())
}

fn rig_on<B: Backend>(backend: B) -> Rig<B> {
    let clock = VirtualClock::new(T0 * 1000);
    let c = Contract::open(backend, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Rig {
        store: Store::new(c, FLOOR_MS),
        clock,
        n: Cell::new(0),
        sink: MetricsSink::new(None, "test", std::time::Duration::from_millis(1)),
        outside: Loop::default(),
    }
}

impl<B: Backend> Rig<B> {
    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }

    fn req(&self, what: &str) -> Request {
        self.n.set(self.n.get() + 1);
        Request::new(&format!("t/{}/{what}", self.n.get()), "test", what)
    }

    fn call_with(&self, sink: &MetricsSink, argv: &[&str], body: Option<&Value>) -> (i32, Value) {
        self.n.set(self.n.get() + 1);
        let request = format!("t/{}", self.n.get());
        let mut args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        if let Some(b) = body {
            args.push("--json".into());
            args.push(b.to_string());
        }
        let call = Call { now_ms: self.clock.now_ms(), request: &request, metrics: sink };
        let (code, out) = if argv[0] == super::VERB {
            run(&self.store, &args, &call, &self.outside)
        } else {
            crate::control::sections::run(self.store.contract(), &args, &call)
        };
        (code, serde_json::from_str(&out).unwrap_or_else(|e| panic!("{argv:?} answered non-JSON {out:?}: {e}")))
    }

    fn call(&self, argv: &[&str], body: Option<&Value>) -> (i32, Value) {
        self.call_with(&self.sink, argv, body)
    }

    fn publish(&self) -> Value {
        let (code, v) = self.call(&["publish-section", "lifecycle"], None);
        assert_eq!(code, 0, "{v}");
        v
    }

    fn publish_from(&self, outside: &dyn Outside) -> Value {
        self.n.set(self.n.get() + 1);
        let request = format!("t/{}", self.n.get());
        let args: Vec<String> = ["publish-section", "lifecycle"].map(String::from).to_vec();
        let call = Call { now_ms: self.clock.now_ms(), request: &request, metrics: &self.sink };
        let (code, out) = run(&self.store, &args, &call, outside);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn section(&self) -> Value {
        let (code, v) = self.call(&["section", "get", "Lifecycle/all"], None);
        assert_eq!(code, 0, "{v}");
        v
    }

    fn status(&self) -> Value {
        self.section()["section"]["status"].clone()
    }

    fn event(&self, id: &str, expect: BeadState, version: u64, kind: BeadEventKind, at: u64) {
        let ev = BeadEvent { expect, version, kind, actor: "test".into(), at: Some(at as i64) };
        let out = self.store.apply(id, &ev, &self.req("apply")).unwrap();
        assert!(matches!(out, crate::control::store::Outcome::Applied { .. }), "{id}: {out:?}");
    }

    /// A bead certified at `certified`, landed at `landed`, through the machine's own events.
    fn land(&self, id: &str, repo: &str, certified: u64, landed: u64) {
        self.store.create(id, Some(repo), Some("t1"), (certified - 1) as i64, &self.req("create")).unwrap();
        self.event(id, BeadState::Submitted, 1, BeadEventKind::GatePass { tip: "t1".into(), gate_key: "k".into() }, certified);
        self.event(id, BeadState::Certified, 2, BeadEventKind::Deliver, landed - 1);
        self.event(id, BeadState::InDelivery, 3, BeadEventKind::Delivered { merge_sha: "m".into(), proof: "p".into() }, landed);
    }

    fn advance(&self, secs: u64) {
        self.clock.advance(secs * 1000);
    }
}

fn value(v: &Value) -> f64 {
    v["value"].as_f64().unwrap_or_else(|| panic!("not a value: {v}"))
}

fn below_floor(status: &Value) -> Value {
    status["conditions"].as_array().unwrap().iter().find(|c| c["type"] == "BelowFloor").unwrap().clone()
}

#[test]
fn the_section_is_a_term_published_through_admission_with_goodput_from_landed_records() {
    let r = rig();
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H - 20 * MIN, now - 2 * H);
    r.land("b-2", "harness", now - 30 * MIN - 10 * MIN, now - 30 * MIN);
    r.land("b-3", "site", now - 5 * H, now - 4 * H);
    r.land("old", "site", now - 3 * DAY, now - 3 * DAY + MIN);
    let out = r.publish();
    assert_eq!(out["published"], true, "{out}");

    let got = r.section();
    let s = &got["section"];
    assert_eq!((s["metadata"]["owner"].as_str(), s["metadata"]["target_latency"].as_str(), s["metadata"]["max_age"].as_str()), (Some("spira-lc"), Some("5m"), Some("15m")));
    assert!(s.get("actions").is_none(), "a term declares no actions: {s}");
    assert_eq!(s["flow"]["consumes"], json!(["round-controller/landed"]));
    assert_eq!(got["freshness"]["state"], "Fresh");

    let st = &s["status"];
    assert_eq!(value(&st["landed_24h"]), 3.0);
    assert_eq!(value(&st["landed_1h"]), 1.0);
    assert_eq!(value(&st["goodput_ratio_24h"]), 1.0);
    assert_eq!(value(&st["goodput_rate_24h"]), 3.0);
    assert_eq!(value(&st["goodput_rate_1h"]), 24.0);
    assert_eq!(value(&st["repos"]["harness"]["landed_24h"]), 2.0);
    assert_eq!(value(&st["repos"]["site"]["landed_24h"]), 1.0);
    assert_eq!(st["repos"]["site"]["goodput_ratio_1h"]["unobservable"], "no landings in window");
    let dwell = value(&st["certified_dwell_p95_secs"]);
    assert!(dwell > 20.0 * MIN as f64 && dwell < H as f64, "the stays were 20 min, 10 min and 1 h: {dwell}");
    assert_eq!(below_floor(st)["status"], "Unknown", "no spec yet: {st}");
    assert!(s.get("distance").is_none(), "no spec names a floor, so there is no distance: {s}");
}

#[test]
fn a_stay_in_certified_is_measured_from_its_entry_and_one_with_no_recorded_entry_is_not() {
    use crate::control::feed::Change;
    use crate::control::records::Audit;
    let audit = |from: &str, to: &str, at: i64, applied: bool| Change {
        rev: 0,
        audit: Audit { machine: "bead".into(), lc_key: "b".into(), event: "e".into(), expect: from.into(), from_state: from.into(), to_state: to.into(), applied, refusal: None, evidence: Value::Null, actor: "t".into(), at },
    };
    let h = [
        audit("CERTIFIED", "IN_DELIVERY", 50, true),
        audit("SUBMITTED", "CERTIFIED", 100, true),
        audit("CERTIFIED", "CERTIFIED", 150, true),
        audit("CERTIFIED", "IN_DELIVERY", 170, false),
        audit("CERTIFIED", "IN_DELIVERY", 400, true),
    ];
    assert_eq!(super::certified_stays(&h), vec![(400, 300)]);
}

#[test]
fn it_publishes_on_change_at_most_once_a_minute_with_a_five_minute_keepalive() {
    let r = rig();
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H, now - H - 30 * MIN);
    assert_eq!(r.publish()["published"], true);
    let first = r.section()["section"]["metadata"]["resourceVersion"].clone();

    r.advance(30);
    let out = r.publish();
    assert_eq!(out["published"], false, "{out}");
    r.advance(MIN);
    assert_eq!(r.publish()["published"], false, "nothing changed and the keepalive is not due");
    assert_eq!(r.section()["section"]["metadata"]["resourceVersion"], first);

    r.advance(4 * MIN);
    assert_eq!(r.publish()["published"], true, "the keepalive");
    let n = r.now();
    r.land("b-2", "harness", n - 10 * MIN, n - 5 * MIN);
    r.advance(MIN);
    assert_eq!(r.publish()["published"], true, "a changed status publishes a minute later");
    assert_eq!(value(&r.status()["landed_24h"]), 2.0);
}

#[test]
fn when_the_publisher_stops_the_section_reads_stale_with_its_reason() {
    let r = rig();
    r.publish();
    r.advance(15 * MIN);
    assert_eq!(r.section()["freshness"]["state"], "Fresh");
    r.advance(MIN);
    let f = r.section()["freshness"].clone();
    assert_eq!(f["state"], "Stale", "{f}");
    assert!(f["reason"].as_str().unwrap().contains("max_age"), "{f}");
}

#[test]
fn an_unknown_section_or_an_unreadable_input_cannot_tell_and_writes_nothing() {
    let r = rig();
    let head = r.store.contract().head().unwrap();
    let (code, v) = r.call(&["publish-section", "alarms"], None);
    assert_eq!(code, 2, "{v}");
    r.outside.broken.set(true);
    let (code, v) = r.call(&["publish-section", "lifecycle"], None);
    assert_eq!(code, 2, "{v}");
    assert!(v["cannot_tell"].as_str().unwrap().contains("stop family"), "{v}");
    r.outside.broken.set(false);
    r.outside.rounds_broken.set(true);
    let (code, v) = r.call(&["publish-section", "lifecycle"], None);
    assert_eq!(code, 2, "{v}");
    assert!(v["cannot_tell"].as_str().unwrap().contains("cert-pass"), "{v}");
    assert_eq!(r.store.contract().head().unwrap(), head);
}

#[test]
fn the_distance_reads_only_terms_with_a_value_and_a_positive_target() {
    use crate::goodput::Measure;
    let v = Measure::Value(1.0);
    assert_eq!(super::distance(&v, Some(0.0), &Measure::WarmingUp, Some(1800.0)), None);
    let d = super::distance(&Measure::WarmingUp, Some(150.0), &v, Some(1800.0)).unwrap();
    assert_eq!(d.expr, "max(0, certified_dwell_p95_secs / 1800 - 1)");
    assert!(d.close < d.open);
    assert_eq!(super::distance(&v, None, &v, None), None);
}

fn pass(batch: &str, members: &[&str], cause: &str, ts: &str) -> Value {
    json!({ "ts": ts, "family": "cert-pass", "repo": "harness", "batch": batch, "pass": "1", "members": members,
            "instance_type": "c7i.2xlarge", "vcpus": 8, "where": "ec2", "capacity_wait_s": 60, "boot_s": 0,
            "build_s": 540, "suites_s": 600, "verdict": "x", "cause": cause, "detail": "" })
}

fn attribution(round: &str, owner: &str, ts: &str) -> Value {
    json!({ "ts": ts, "family": "round-attribution", "repo": "harness", "round": round, "iteration": "1",
            "suite": "test-a.sh", "outcome": "owner", "owner": owner })
}

#[test]
fn rounds_are_read_from_typed_pass_and_attribution_rows() {
    let passes = vec![
        pass("r-1", &["a", "b"], "red-attributed", "2026-10-31T10:00:00Z"),
        pass("r-2", &["a", "c"], "red-attributed", "2026-10-31T12:00:00Z"),
        pass("r-3", &[], "over-cap", "2026-10-31T13:00:00Z"),
        json!({ "ts": "2026-10-31T13:00:00Z", "family": "cert-pass", "cause": "unheard-of" }),
    ];
    let attributions = vec![
        attribution("r-1", "a", "2026-10-31T10:00:00Z"),
        attribution("r-1", "a", "2026-10-31T10:01:00Z"),
        attribution("r-2", "a", "2026-10-31T12:00:00Z"),
        attribution("r-2", "", "2026-10-31T12:00:00Z"),
    ];
    let all = round_events(&passes, &attributions, 0);
    let ejects: Vec<(String, u32)> = all.iter().filter_map(|e| match e {
        LoopEvent::Eject { batch, repeat_n, .. } => Some((batch.clone(), *repeat_n)),
        _ => None,
    }).collect();
    assert_eq!(ejects, vec![("r-1".to_string(), 1), ("r-2".to_string(), 2)], "one eject per round, numbered across rounds");
    assert!(all.iter().any(|e| matches!(e, LoopEvent::RedAttributed { batch, innocent, wall_secs: 1200, .. } if batch == "r-2" && innocent == &["c".to_string()])), "{all:?}");
    assert!(all.iter().any(|e| matches!(e, LoopEvent::OverCapPass { wall_secs: 1200, .. })));

    let since = spira_desired_state::section::parse_rfc3339("2026-10-31T11:00:00Z").unwrap();
    let late = round_events(&passes, &attributions, since);
    assert!(late.iter().any(|e| matches!(e, LoopEvent::Eject { repeat_n: 2, .. })), "the count reaches back past `since`: {late:?}");
    assert!(!late.iter().any(|e| matches!(e, LoopEvent::Eject { repeat_n: 1, .. })));
}

#[test]
fn the_run_directory_reads_the_halt_stamp_and_missing_families_as_empty() {
    let tmp = testkit::TempDir::new("lifecycle-section-rundir");
    let dir = RunDir(tmp.path().to_path_buf());
    assert!(dir.stops().unwrap().is_empty());
    assert!(dir.rounds(0).unwrap().is_empty(), "no family file yet is no rows");
    std::fs::write(tmp.join("world.halted"), "").unwrap();
    let stops = dir.stops().unwrap();
    assert!(matches!(stops.as_slice(), [LoopEvent::Stop { kind: StopKind::Halt, bead: None, .. }]), "{stops:?}");
    std::fs::create_dir_all(tmp.join("tsd")).unwrap();
    let rows = [pass("r-9", &[], "over-cap", "2026-10-31T13:00:00Z").to_string()].join("\n");
    std::fs::write(tmp.join("tsd").join("cert-pass.jsonl"), rows).unwrap();
    assert_eq!(dir.rounds(0).unwrap().len(), 1);
}

fn write_rows(dir: &std::path::Path, family: &str, rows: &[Value]) {
    std::fs::create_dir_all(dir.join("tsd")).unwrap();
    let text: Vec<String> = rows.iter().map(Value::to_string).collect();
    std::fs::write(dir.join("tsd").join(format!("{family}.jsonl")), text.join("\n")).unwrap();
}

fn stop_row(kind: &str, at: u64, repo: Option<&str>, bead: Option<&str>) -> Value {
    json!({
        "ts": tsd::iso_utc(at), "host": "h", "family": "loop-stop",
        "kind": kind, "reason": "operator", "actor": "world", "repo": repo, "bead": bead, "detail": "",
    })
}

fn land_row(at: u64, bead: &str, commit: &str, reverts: Option<&str>) -> Value {
    json!({ "ts": tsd::iso_utc(at), "host": "h", "family": "land", "repo": "harness", "bead": bead, "commit": commit, "reverts": reverts })
}

#[test]
fn a_halt_that_was_released_is_excluded_from_the_trailing_day_by_the_typed_stop_records_alone() {
    let r = rig();
    let now = r.now();
    for (i, ago) in [3 * H, 4 * H, 5 * H, 6 * H].into_iter().enumerate() {
        r.land(&format!("in-{i}"), "harness", now - ago - MIN, now - ago);
    }
    r.land("before-halt", "harness", now - 28 * H - MIN, now - 28 * H);
    let tmp = testkit::TempDir::new("lifecycle-halt-released");
    let dir = RunDir(tmp.path().to_path_buf());
    assert!(dir.records_releases(), "the run directory records releases of the world");
    r.publish_from(&dir);
    let unrecorded = value(&r.status()["goodput_rate_24h"]);
    assert_eq!(unrecorded, 4.0, "with no stop record the halt reads as running time and the old landing is out of the day");

    r.advance(2 * MIN);
    write_rows(tmp.path(), "loop-stop", &[stop_row("halt", now - 12 * H, None, None), stop_row("release", now - 7 * H, None, None)]);
    r.publish_from(&dir);
    let st = r.status();
    assert_eq!(value(&st["goodput_rate_24h"]), 5.0, "five hours of halt are not counted, so the day reaches back to the old landing: {st}");
    assert_eq!(value(&st["landed_24h"]), 5.0);
}

#[test]
fn a_revert_whose_landing_names_the_reverted_commit_charges_the_reverted_bead() {
    let r = rig();
    let now = r.now();
    r.land("bad", "harness", now - 3 * H - MIN, now - 3 * H);
    r.land("undo", "harness", now - 2 * H - MIN, now - 2 * H);
    let tmp = testkit::TempDir::new("lifecycle-reverts");
    write_rows(tmp.path(), "land", &[land_row(now - 3 * H, "bad", "abcdef0123456789", None), land_row(now - 2 * H, "undo", "0123456789abcdef", Some("abcdef0"))]);
    r.publish_from(&RunDir(tmp.path().to_path_buf()));
    let harmful = r.status()["harmful"].clone();
    let beads: Vec<&str> = harmful.as_array().unwrap().iter().map(|h| h["bead"].as_str().unwrap()).collect();
    assert_eq!(beads, vec!["bad"], "{harmful}");
    assert_eq!(harmful[0]["evidence"][0]["signal"], "revert");
}

#[test]
fn a_revert_subject_without_the_typed_field_charges_nothing() {
    let r = rig();
    let now = r.now();
    r.land("bad", "harness", now - 3 * H - MIN, now - 3 * H);
    r.land("undo", "harness", now - 2 * H - MIN, now - 2 * H);
    let tmp = testkit::TempDir::new("lifecycle-revert-prose");
    let mut prose = land_row(now - 2 * H, "undo", "0123456789abcdef", None);
    prose["subject"] = json!("Revert \"bad\" (abcdef0123456789)");
    write_rows(tmp.path(), "land", &[land_row(now - 3 * H, "bad", "abcdef0123456789", None), prose]);
    r.publish_from(&RunDir(tmp.path().to_path_buf()));
    assert!(r.status().get("harmful").is_none(), "{}", r.status());
}

#[test]
fn the_typed_stop_rows_replace_the_halt_stamp_and_a_pause_names_its_repo_and_bead() {
    let tmp = testkit::TempDir::new("lifecycle-stop-rows");
    let dir = RunDir(tmp.path().to_path_buf());
    write_rows(tmp.path(), "loop-stop", &[stop_row("halt", 100, None, None), stop_row("release", 200, None, None), stop_row("pause", 300, Some("harness"), Some("b-9")), json!({"ts": tsd::iso_utc(400), "family": "loop-stop", "kind": "halt", "reason": "because"})]);
    let stops = dir.stops().unwrap();
    let kinds: Vec<(StopKind, u64)> = stops.iter().map(|e| match e { LoopEvent::Stop { kind, at, .. } => (*kind, *at), _ => panic!() }).collect();
    assert_eq!(kinds, vec![(StopKind::Halt, 100), (StopKind::Release, 200), (StopKind::Pause, 300)], "a reason outside the closed set is no stop: {stops:?}");
    assert!(matches!(&stops[2], LoopEvent::Stop { repo: Some(r), bead: Some(b), .. } if r == "harness" && b == "b-9"));
}

/// Each read a backend served, by kind, prefix and limit.
#[derive(Default)]
struct Counting {
    inner: MemBackend,
    reads: std::sync::Mutex<Vec<(&'static str, String, usize)>>,
}

impl Counting {
    fn note(&self, kind: &'static str, key: &str, limit: usize) {
        self.reads.lock().unwrap().push((kind, key.to_string(), limit));
    }
    fn take(&self) -> Vec<(&'static str, String, usize)> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl Backend for Counting {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
        self.note("get", key, 1);
        self.inner.get(key, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range", prefix, limit);
        self.inner.range(prefix, after, limit, at)
    }
    fn range_in_state(&self, prefix: &str, state: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range_in_state", &format!("{prefix}{state}"), limit);
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
}

#[test]
fn publish_reads_no_record_outside_its_window() {
    let r = rig_on(Counting::default());
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H - 20 * MIN, now - 2 * H);
    r.land("b-2", "harness", now - 40 * MIN, now - 30 * MIN);
    r.land("b-3", "site", now - 5 * H, now - 4 * H);
    r.land("old", "site", now - 3 * DAY, now - 3 * DAY + MIN);
    r.store.contract().backend().take();
    assert_eq!(r.publish()["published"], true);
    assert_eq!(value(&r.status()["landed_24h"]), 3.0);
    let reads = r.store.contract().backend().take();
    let under = |p: &str| reads.iter().filter(|(k, key, _)| *k == "range" && key.starts_with(p)).count();
    for prefix in [crate::control::keys::LIFECYCLE, crate::control::keys::READINESS, state_store::LOCKS_PREFIX, crate::control::keys::HISTORY] {
        assert_eq!(under(prefix), 0, "a range under {prefix}: {reads:?}");
    }
    let states: Vec<_> = reads.iter().filter(|(k, _, _)| *k == "range_in_state").collect();
    assert!(states.len() == 1 && states[0].1.to_lowercase().contains("certified"), "{states:?}");
}

#[test]
fn a_landing_by_reclassify_counts() {
    let r = rig();
    let now = r.now();
    r.store.create("rc", Some("harness"), None, (now - 2 * H) as i64, &r.req("create")).unwrap();
    let kind = BeadEventKind::Reclassify { state: BeadState::Landed, rule: "terminal-landing-line".into() };
    let ev = BeadEvent { expect: BeadState::Ready, version: 0, kind, actor: "classifier".into(), at: Some((now - H) as i64) };
    let out = r.store.apply("rc", &ev, &r.req("reclassify")).unwrap();
    assert!(matches!(out, crate::control::store::Outcome::Applied { .. }), "{out:?}");
    r.publish();
    assert_eq!(value(&r.status()["landed_24h"]), 1.0);
}

#[test]
fn an_index_short_of_the_window_is_unobservable_not_zero() {
    let r = rig();
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H - 20 * MIN, now - 2 * H);
    r.land("b-2", "harness", now - 40 * MIN, now - 30 * MIN);
    crate::control::dwell::SCAN_CAP.with(|c| c.set(Some(0)));
    r.publish();
    crate::control::dwell::SCAN_CAP.with(|c| c.set(None));
    let st = r.status();
    let why = st["landed_24h"]["unobservable"].as_str().unwrap_or_else(|| panic!("not unobservable: {st}"));
    assert!(why.contains("reaches back only to"), "{why}");
    assert_eq!(below_floor(&st)["status"], "Unknown");
    r.advance(2 * MIN);
    assert_eq!(r.publish()["published"], true);
    assert_eq!(value(&r.status()["landed_24h"]), 2.0);
}
