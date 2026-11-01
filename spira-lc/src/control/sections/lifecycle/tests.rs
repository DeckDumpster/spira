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
    let clock = VirtualClock::new(T0 * 1000);
    let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
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
            crate::control::sections::run(&self.store, &args, &call)
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

    fn section(&self) -> Value {
        let (code, v) = self.call(&["section", "get", "Lifecycle/all"], None);
        assert_eq!(code, 0, "{v}");
        v
    }

    fn status(&self) -> Value {
        self.section()["section"]["status"].clone()
    }

    fn floor(&self, floor: f64) {
        let doc = json!({
            "apiVersion": "spira/v1", "kind": "Lifecycle", "metadata": { "name": "all" },
            "spec": {
                "goodput_rate_floor": { "value": floor, "statute": "law-throughput-floor" },
                "certified_dwell_p95_secs": { "value": 1800.0, "statute": "law-throughput-floor" },
            },
        });
        let (code, v) = self.call(&["section", "apply-spec", "--actor", "compose"], Some(&doc));
        assert_eq!(code, 0, "{v}");
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
fn below_the_floor_opens_below_floor_naming_the_rate_and_the_distance_reads_the_floor() {
    let r = rig();
    r.floor(150.0);
    let now = r.now();
    for i in 0..10 {
        r.land(&format!("b-{i}"), "harness", now - (i + 1) * H - MIN, now - (i + 1) * H);
    }
    r.publish();
    let s = r.section()["section"].clone();
    let c = below_floor(&s["status"]);
    assert_eq!(c["status"], "True", "{c}");
    assert!(c["reason"].as_str().unwrap().contains("10.0/day"), "{c}");
    assert_eq!(s["status"]["observedGeneration"], 1, "caught up with the spec");
    assert_eq!(s["distance"]["fn"], "max(0, 1 - goodput_rate_24h / 150) + max(0, certified_dwell_p95_secs / 1800 - 1)");
    assert!(s.get("actions").is_none());

    r.floor(5.0);
    r.advance(MIN);
    r.publish();
    assert_eq!(below_floor(&r.status())["status"], "False", "at or above the floor closes it");

    r.outside.no_releases.set(true);
    r.advance(MIN);
    r.publish();
    let s = r.section()["section"].clone();
    let c = below_floor(&s["status"]);
    assert_eq!(c["status"], "Unknown", "with no recorded release a halt cannot be excluded: {c}");
    assert!(c["reason"].as_str().unwrap().contains("release"), "{c}");
    assert_eq!(s["distance"]["fn"], "max(0, certified_dwell_p95_secs / 1800 - 1)", "nor does the rate rank");
}

#[test]
fn every_windowed_event_names_its_repo_and_a_record_with_none_is_counted_but_not_listed() {
    let r = rig();
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H, now - H - 30 * MIN);
    r.store.create("nameless", None, Some("t1"), (now - 3 * H) as i64, &r.req("create")).unwrap();
    r.event("nameless", BeadState::Submitted, 1, BeadEventKind::GatePass { tip: "t1".into(), gate_key: "k".into() }, now - 3 * H);
    r.event("nameless", BeadState::Certified, 2, BeadEventKind::Deliver, now - 2 * H);
    r.event("nameless", BeadState::InDelivery, 3, BeadEventKind::Delivered { merge_sha: "m".into(), proof: "p".into() }, now - 2 * H + 1);
    r.outside.stops.borrow_mut().push(LoopEvent::Stop { kind: StopKind::Pause, repo: Some("paused".into()), bead: None, at: now - H, reason: String::new() });
    r.outside.rounds.borrow_mut().extend([
        LoopEvent::OverCapPass { repo: "slow".into(), at: now - H, wall_secs: 60 },
        LoopEvent::Eject { bead: "b-1".into(), batch: "round-1".into(), at: now - H, repeat_n: 1 },
        LoopEvent::Eject { bead: "stranger".into(), batch: "round-1".into(), at: now - H, repeat_n: 1 },
    ]);
    r.publish();
    let st = r.status();
    let repos: Vec<&str> = st["repos"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(repos, ["harness", "paused", "slow"], "{st}");
    assert_eq!(value(&st["landed_24h"]), 2.0);
    assert!(st["certified_dwell_p95_secs"].get("value").is_some(), "{st}");
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
fn after_a_release_from_a_30h_halt_the_24h_fields_warm_up_and_below_floor_stays_closed_until_a_day_has_run() {
    let r = rig();
    r.floor(150.0);
    let now = r.now();
    r.land("before", "harness", now - 40 * H, now - 31 * H - 1);
    r.outside.stops.borrow_mut().extend([
        LoopEvent::Stop { kind: StopKind::Halt, repo: None, bead: None, at: now - 31 * H, reason: String::new() },
        LoopEvent::Stop { kind: StopKind::Release, repo: None, bead: None, at: now - H, reason: String::new() },
    ]);
    r.publish();
    let st = r.status();
    assert_eq!(st["goodput_rate_24h"], "WarmingUp", "{st}");
    assert_eq!(st["landed_24h"], "WarmingUp");
    assert_eq!(st["certified_dwell_p95_secs"], "WarmingUp");
    assert!(st["goodput_rate_1h"].get("value").is_some(), "the hour is already past the release: {st}");
    assert_eq!(st["warming_up_until"], json!(spira_desired_state::store::format_rfc3339(now + 23 * H)));
    let c = below_floor(&st);
    assert_eq!(c["status"], "False");
    assert!(c["reason"].as_str().unwrap().contains("warming up"), "{c}");
    assert!(r.section()["section"].get("distance").is_none(), "a warming field is never a term");

    r.advance(23 * H - MIN);
    r.publish();
    assert_eq!(below_floor(&r.status())["status"], "False", "a minute short of a day of running time");
    r.advance(MIN);
    r.publish();
    let st = r.status();
    assert_eq!(value(&st["goodput_rate_24h"]), 0.0);
    assert!(st.get("warming_up_until").is_none());
    assert_eq!(below_floor(&st)["status"], "True", "{st}");
}

#[test]
fn a_harmful_bead_is_listed_with_its_cost_and_typed_evidence() {
    let r = rig();
    let now = r.now();
    for i in 0..4 {
        r.land(&format!("b-{i}"), "harness", now - (i + 2) * H, now - (i + 1) * H);
    }
    r.outside.rounds.borrow_mut().push(LoopEvent::RedAttributed { bead: "b-0".into(), batch: "round-1".into(), innocent: vec!["b-1".into()], at: now - 3 * H, wall_secs: 30 * MIN });
    r.publish();
    let st = r.status();
    let h = &st["harmful"][0];
    assert_eq!((h["bead"].as_str(), h["repo"].as_str(), h["cost_minutes"].as_f64()), (Some("b-0"), Some("harness"), Some(30.0)), "{st}");
    assert_eq!(h["evidence"][0]["signal"], "red-with-innocents");
    assert_eq!(h["evidence"][0]["batch"], "round-1");
    assert!((value(&st["goodput_ratio_24h"]) - (0.75 - 30.0 / 1440.0)).abs() < 1e-9);
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

#[test]
fn goodput_is_sent_as_otlp_gauges_per_repo_and_window() {
    let r = rig();
    let now = r.now();
    r.land("b-1", "harness", now - 2 * H, now - 30 * MIN);
    let ((), got) = spira_otel::metrics::fixture::capture(|sink| {
        assert_eq!(r.call_with(sink, &["publish-section", "lifecycle"], None).0, 0);
    });
    let find = |name: &str, repo: &str, window: &str| got.iter().any(|s| s.name == name && s.label("repo") == Some(repo) && s.label("window") == Some(window));
    for name in ["spira_goodput_ratio", "spira_goodput_rate", "spira_goodput_landed"] {
        assert!(find(name, "all", "24h") && find(name, "harness", "1h"), "{name}: {got:?}");
    }
    assert!(got.iter().any(|s| s.name == crate::control::sections::WRITES && s.label("verb") == Some("publish-section")), "{got:?}");
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
