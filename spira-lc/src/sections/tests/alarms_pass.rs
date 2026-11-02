//! The alarms pass end to end: `incident::alarms_pass::run` against the control store's in-memory
//! backend through the verbs a deployed pass calls, with stub mail and HTTP ports and the real
//! mailbox fallback, on the rig's virtual clock.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use incident::alarms_pass::{self as ap, Config, DeadMan, FileSwitch, MaildirFallback, Ports, Report};
use reconciler_engine::goodput_pass::{run as goodput, Lc, Outcome, PassConfig};
use reconciler_engine::pass_record::{Mode, NoLever, PassRecord, Refused, PASS_KEY};
use tsd::alarm::{Message, Outcome as Delivery};

use super::*;

struct Dispatch<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.0.call(&argv, None)
    }
}

/// The primary path: answers with `id` (empty prints no id), or fails when `id` is `None`.
struct Mail {
    id: RefCell<Option<String>>,
    n: Cell<u32>,
    sent: RefCell<Vec<Message>>,
    notes: RefCell<Vec<(String, String)>>,
}

impl Mail {
    fn answering(id: Option<&str>) -> Mail {
        Mail { id: RefCell::new(id.map(String::from)), n: Cell::new(0), sent: RefCell::new(vec![]), notes: RefCell::new(vec![]) }
    }

    fn set(&self, id: Option<&str>) {
        *self.id.borrow_mut() = id.map(String::from);
    }
}

impl ap::Primary for Mail {
    fn send(&self, m: &Message) -> Result<String, String> {
        self.sent.borrow_mut().push(m.clone());
        self.n.set(self.n.get() + 1);
        match self.id.borrow().as_deref() {
            None => Err("the primary path is down".into()),
            Some("") => Ok(String::new()),
            Some(id) => Ok(format!("{id}-{}", self.n.get())),
        }
    }

    fn note(&self, ask: &str, text: &str) -> Result<(), String> {
        self.notes.borrow_mut().push((ask.into(), text.into()));
        Ok(())
    }
}

#[derive(Default)]
struct Http {
    calls: RefCell<Vec<String>>,
}

impl ap::Http for Http {
    fn get(&self, url: &str, _deadline: Duration) -> Result<String, String> {
        self.calls.borrow_mut().push(url.into());
        Ok("status 200".into())
    }
}

struct World {
    tmp: testkit::TempDir,
    mail: Mail,
    http: Http,
    timer: Cell<bool>,
    fallback: Cell<bool>,
    url: RefCell<String>,
}

impl World {
    fn new(name: &str, id: Option<&str>) -> World {
        let tmp = testkit::TempDir::new(name);
        std::fs::create_dir_all(tmp.join("run")).unwrap();
        World { tmp, mail: Mail::answering(id), http: Http::default(), timer: Cell::new(false), fallback: Cell::new(true), url: RefCell::new("http://heartbeat.test/ping".into()) }
    }

    fn run_dir(&self) -> PathBuf {
        self.tmp.join("run")
    }

    fn mail_root(&self) -> PathBuf {
        self.tmp.join("mail")
    }

    fn cfg(&self) -> Config {
        Config { run_dir: self.run_dir(), host: "h".into(), deadman_secs: 180, heartbeat_url: self.url.borrow().clone(), fallback: self.fallback.get(), floor_proposal: None }
    }

    fn pass<B: Backend>(&self, r: &Rig<B>) -> Report {
        let run_dir = self.run_dir();
        let timer_on = || self.timer.get();
        let switch = FileSwitch { run_dir: &run_dir, timer_on: &timer_on };
        let fallback = MaildirFallback { mail_root: self.mail_root(), mute: false };
        let lc = Dispatch(r);
        let p = Ports { lc: &lc, primary: &self.mail, fallback: &fallback, http: &self.http, switch: &switch, clock: &r.clock };
        ap::run(&p, &self.cfg()).unwrap()
    }

    fn attempts(&self) -> Vec<tsd::alarm::Attempt> {
        tsd::alarm::read(&self.run_dir())
    }

    fn sent(&self) -> usize {
        self.mail.sent.borrow().len()
    }
}

fn mailbox_files(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("new")).map_or(0, |d| d.count())
}

fn alarms_section<B: Backend>(r: &Rig<B>) -> Value {
    r.ok(&["section", "get", "Alarms/all"], None)
}

fn condition(v: &Value, type_: &str) -> String {
    v["section"]["status"]["conditions"].as_array().unwrap().iter().find(|c| c["type"] == type_).map_or("absent".into(), |c| c["status"].as_str().unwrap().to_string())
}

/// A Probe section with the given open conditions, observed now.
fn probe<B: Backend>(r: &Rig<B>, name: &str, conditions: &[(&str, &str)]) {
    let conds: Vec<Value> = conditions.iter().map(|(t, at)| json!({ "type": t, "status": "True", "reason": "test", "lastTransitionTime": at })).collect();
    let body = json!({
        "apiVersion": "spira/v1",
        "kind": "Probe",
        "metadata": { "name": name, "owner": "probe", "observedAt": spira_desired_state::store::format_rfc3339(r.clock.now_ms() / 1000), "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0, "depth": { "value": 3.0, "window": "level" }, "conditions": conds },
    });
    r.publish(&body);
}

fn record(n: u64, at_ms: u64) -> PassRecord {
    PassRecord {
        n,
        list_rev: 1,
        at: spira_desired_state::store::format_rfc3339(at_ms / 1000),
        holder: "inv-1".into(),
        mode: Mode::Acting,
        goodput: None,
        binding: None,
        gaps: vec![],
        steps: vec![],
        would_step: vec![],
        judged: vec![],
        acks_refused: vec![],
        refused: vec![],
        exhausted: vec![],
        no_lever: vec![],
        ages: Default::default(),
        shadow_kinds: vec![],
    }
}

fn put_record<B: Backend>(r: &Rig<B>, rec: &PassRecord) {
    r.put_knob(PASS_KEY, serde_json::to_value(rec).unwrap());
}

fn nolever(n: u64, at_ms: u64, conditions: &[&str], refused_code: &str) -> PassRecord {
    let mut rec = record(n, at_ms);
    rec.no_lever = vec![NoLever { section: "Probe/a".into(), conditions: conditions.iter().map(|s| s.to_string()).collect(), since: 1 }];
    rec.refused = vec![Refused { section: "Probe/a".into(), action: "drain".into(), code: refused_code.into(), reason: format!("{refused_code} at {n}") }];
    rec
}

#[test]
fn a_failed_page_is_recorded_undelivered_and_a_retry_delivery_intent_resends_it_and_acks_applied() {
    let r = rig();
    let w = World::new("alarms-retry", Some(""));
    w.fallback.set(false);
    probe(&r, "a", &[("Wedged", NOW)]);
    let rep = w.pass(&r);
    assert!(rep.sent.is_empty(), "the primary printed no id: {rep:?}");
    assert_eq!(rep.undelivered_open, 1);
    let a = w.attempts();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].outcome, Delivery::Failed { reason: "the primary path returned no id".into() });
    assert!(a[0].message.is_some(), "a failure keeps what a retry sends");
    let s = alarms_section(&r);
    assert_eq!(s["section"]["status"]["undelivered_open"]["value"], 1.0);
    assert_eq!(condition(&s, "Undelivered"), "True");

    // The goodput pass steps retry-delivery on the Alarms gap; the alarms pass applies it.
    let cfg = PassConfig { holder: "inv-1".into(), timeout_secs: 50, shadow: vec![], halted: false };
    let Outcome::Passed(p) = goodput(&Dispatch(&r), &r.clock, &cfg).unwrap() else { panic!("the goodput pass stood by") };
    let step = p.record.steps.iter().find(|s| s.section == "Alarms/all").unwrap_or_else(|| panic!("no retry step: {:?}", p.record));
    assert_eq!(step.action, "retry-delivery");
    let key = step.intent.clone().unwrap();

    w.mail.set(Some("sp-ask"));
    r.clock.advance(MIN);
    probe(&r, "a", &[("Wedged", NOW)]);
    let rep = w.pass(&r);
    assert_eq!(rep.retried, [(key.clone(), "applied".to_string())]);
    assert_eq!(rep.undelivered_open, 0);
    assert_eq!(w.sent(), 2, "the one alarm, and its one re-send");
    let i = r.store.get(&key).unwrap().unwrap().value.data;
    assert_eq!(i["ack"]["result"], "applied", "{i}");
    assert_eq!(r.knob("knobs/Alarms/all/retry-delivery"), json!(1));
    assert!(matches!(w.attempts().last().unwrap().outcome, Delivery::Delivered { .. }));
    assert_eq!(condition(&alarms_section(&r), "Undelivered"), "False");
}

#[test]
fn a_nolever_entry_pages_once_a_changed_one_is_a_note_and_a_second_condition_type_pages_once() {
    let r = rig();
    let w = World::new("alarms-identity", Some("sp-ask"));
    probe(&r, "a", &[("Deep", NOW)]);
    put_record(&r, &nolever(1, r.clock.now_ms(), &["Deep"], "in-flight"));
    let rep = w.pass(&r);
    assert_eq!(rep.sent, ["page:Probe/a:Deep"]);
    let first = w.mail.sent.borrow()[0].clone();
    assert_eq!(first.mailbox, "concierge", "a kind with no escalation class pages the Concierge");
    for want in ["## Question", "## Default", "drain: refused (in-flight)", "condition: Deep", "docs/runbooks/reconciler.md#deep"] {
        assert!(first.body.contains(want), "{want} missing: {}", first.body);
    }

    r.clock.advance(MIN);
    put_record(&r, &nolever(2, r.clock.now_ms(), &["Deep"], "in-flight"));
    let rep = w.pass(&r);
    assert!(rep.sent.is_empty() && rep.noted.is_empty(), "the same entry next pass sends nothing: {rep:?}");

    r.clock.advance(MIN);
    put_record(&r, &nolever(3, r.clock.now_ms(), &["Deep"], "back-off"));
    let rep = w.pass(&r);
    assert!(rep.sent.is_empty(), "{rep:?}");
    assert_eq!(rep.noted, ["page:Probe/a:Deep"]);
    assert_eq!(w.mail.notes.borrow()[0].0, "sp-ask-1", "the note goes on the open ask");

    r.clock.advance(MIN);
    put_record(&r, &nolever(4, r.clock.now_ms(), &["Deep", "Wide"], "back-off"));
    assert_eq!(w.pass(&r).sent, ["page:Probe/a:Deep,Wide"], "a second condition type on the section pages");
    r.clock.advance(MIN);
    put_record(&r, &nolever(5, r.clock.now_ms(), &["Deep", "Wide"], "back-off"));
    assert!(w.pass(&r).sent.is_empty(), "and only once");
    assert_eq!(w.sent(), 2);
}

#[test]
fn a_page_on_raise_condition_pages_on_raise_without_a_pass_record() {
    let r = rig();
    let w = World::new("alarms-raise", Some("sp-ask"));
    probe(&r, "b", &[("Wedged", NOW)]);
    assert_eq!(w.pass(&r).sent, ["page:Probe/b:Wedged"]);
    r.clock.advance(MIN);
    probe(&r, "b", &[("Wedged", NOW)]);
    assert!(w.pass(&r).sent.is_empty(), "one page per raise");
    r.clock.advance(MIN);
    probe(&r, "b", &[("Wedged", "2026-10-31T19:41:00Z")]);
    assert_eq!(w.pass(&r).sent, ["page:Probe/b:Wedged"], "a new raise pages inside the window");
}

#[test]
fn a_failed_primary_falls_back_to_the_mailbox_and_the_forced_mode_writes_nothing() {
    let r = rig();
    let w = World::new("alarms-fallback", None);
    probe(&r, "b", &[("Wedged", NOW)]);
    let rep = w.pass(&r);
    assert_eq!(rep.sent, ["page:Probe/b:Wedged"]);
    assert_eq!(mailbox_files(&w.mail_root().join("concierge")), 1, "mail_deliver wrote the page into the mailbox");
    let outcomes: Vec<Delivery> = w.attempts().into_iter().map(|a| a.outcome).collect();
    assert_eq!(outcomes, [Delivery::Failed { reason: "the primary path is down".into() }, Delivery::DeliveredFallback]);
    assert_eq!(rep.undelivered_open, 0);

    let r = rig();
    let w = World::new("alarms-forced", None);
    w.fallback.set(false);
    probe(&r, "b", &[("Wedged", NOW)]);
    let rep = w.pass(&r);
    assert!(rep.sent.is_empty());
    assert!(!w.mail_root().exists(), "the forced mode writes nothing");
    assert_eq!(w.attempts().into_iter().map(|a| a.outcome).collect::<Vec<_>>(), [Delivery::Failed { reason: "the primary path is down".into() }]);
    assert_eq!(rep.undelivered_open, 1);
}

fn lifecycle<B: Backend>(r: &Rig<B>, rate: Value, below: bool) {
    let at = spira_desired_state::store::format_rfc3339(r.clock.now_ms() / 1000);
    let rev = r.call(&["section", "get", "Lifecycle/all"], None).1["section"]["metadata"]["resourceVersion"].as_str().map_or("0".to_string(), String::from);
    let conditions = if below { json!([{ "type": "BelowFloor", "status": "True", "reason": "rate under 150", "lastTransitionTime": NOW }]) } else { json!([]) };
    let body = json!({
        "apiVersion": "spira/v1",
        "kind": "Lifecycle",
        "metadata": { "name": "all", "owner": "spira-lc", "observedAt": at, "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 1, "goodput_rate_24h": rate, "conditions": conditions },
    });
    r.ok(&["section", "publish-status", "--actor", "spira-lc", "--expect-rev", &rev], Some(&body));
}

#[test]
fn below_the_floor_is_one_standing_ask_refiled_only_when_the_binding_condition_changes() {
    let r = rig();
    let w = World::new("alarms-floor", Some("sp-ask"));
    r.apply_spec(&json!({ "apiVersion": "spira/v1", "kind": "Lifecycle", "metadata": { "name": "all" }, "spec": { "goodput_rate_floor": { "value": 150.0, "statute": "law-throughput-floor" } } }));
    lifecycle(&r, json!({ "value": 40.0, "window": "24h" }), true);
    let mut rec = record(1, r.clock.now_ms());
    rec.binding = Some("Probe/a".into());
    put_record(&r, &rec);
    let rep = w.pass(&r);
    assert_eq!(rep.floor.as_deref(), Some("Probe/a"));
    let ask = w.mail.sent.borrow()[0].clone();
    assert_eq!((ask.mailbox.as_str(), ask.class.as_deref()), ("operator", Some("policy")));
    for want in ["MOOT-WHEN: incident floor-cleared", "binding condition: Probe/a", "no proposal recorded", "## Class basis", "floor 150"] {
        assert!(ask.body.contains(want), "{want} missing: {}", ask.body);
    }

    for _ in 0..8 {
        r.clock.advance(6 * HOUR);
        lifecycle(&r, json!({ "value": 40.0, "window": "24h" }), true);
        put_record(&r, &rec);
        assert_eq!(w.pass(&r).floor, None, "the same binding condition sends nothing");
    }
    assert_eq!(w.sent(), 1, "48 virtual hours, one ask");

    rec.binding = Some("Probe/b".into());
    put_record(&r, &rec);
    assert_eq!(w.pass(&r).floor.as_deref(), Some("Probe/b"), "a new binding condition refiles");
    r.clock.advance(MIN);
    assert_eq!(w.pass(&r).floor, None);
    assert_eq!(w.sent(), 2);

    assert_eq!(ap::floor_cleared(&Dispatch(&r)), Ok(false), "40 is below the floor");
    lifecycle(&r, json!({ "value": 150.0, "window": "24h" }), false);
    assert_eq!(ap::floor_cleared(&Dispatch(&r)), Ok(true), "a rate at the floor clears it");
    lifecycle(&r, json!("WarmingUp"), false);
    assert_eq!(ap::floor_cleared(&Dispatch(&r)), Ok(false), "a warming rate never clears it");
}

#[test]
fn the_dead_man_pages_once_under_reconciler_and_reads_the_switch_first() {
    let r = rig();
    let w = World::new("alarms-deadman", Some("sp-ask"));
    w.timer.set(true);
    put_record(&r, &record(1, r.clock.now_ms()));
    r.clock.advance(181_000);
    let rep = w.pass(&r);
    assert_eq!(rep.deadman, Some(DeadMan::Stale { age_secs: Some(181) }));
    assert_eq!(rep.sent, ["page:reconciler:ReconcilerPassStale"]);
    assert_eq!(condition(&alarms_section(&r), "ReconcilerPassStale"), "True");
    r.clock.advance(MIN);
    assert!(w.pass(&r).sent.is_empty(), "once");

    let w = World::new("alarms-deadman-halted", Some("sp-ask"));
    w.timer.set(true);
    std::fs::write(w.run_dir().join("world.halted.observability"), "").unwrap();
    let rep = w.pass(&r);
    assert!(matches!(rep.deadman, Some(DeadMan::Waiting(_))) && rep.sent.is_empty(), "{rep:?}");
    assert!(alarms_section(&r)["section"]["status"]["deadman"].as_str().unwrap().starts_with("WAITING"));

    let w = World::new("alarms-deadman-off", Some("sp-ask"));
    let rep = w.pass(&r);
    assert!(matches!(rep.deadman, Some(DeadMan::Waiting(ref why)) if why.contains("reconciler-goodput timer")) && rep.sent.is_empty(), "{rep:?}");

    let w = World::new("alarms-deadman-work-halt", Some("sp-ask"));
    w.timer.set(true);
    std::fs::write(w.run_dir().join("world.halted"), "").unwrap();
    put_record(&r, &record(2, r.clock.now_ms()));
    let rep = w.pass(&r);
    assert_eq!((rep.deadman, rep.sent.len()), (Some(DeadMan::Fresh { age_secs: 0 }), 0), "a work-plane halt with a fresh pass trips nothing");
}

#[test]
fn a_fresh_check_pings_the_heartbeat_once_a_stale_one_never_and_an_empty_url_is_recorded() {
    let r = rig();
    let w = World::new("alarms-heartbeat", Some("sp-ask"));
    w.timer.set(true);
    put_record(&r, &record(1, r.clock.now_ms()));
    let rep = w.pass(&r);
    assert_eq!(rep.heartbeat.as_deref(), Some("status 200"));
    assert_eq!(*w.http.calls.borrow(), ["http://heartbeat.test/ping"]);
    assert_eq!(alarms_section(&r)["section"]["status"]["heartbeat_last"], "status 200");

    r.clock.advance(181_000);
    let rep = w.pass(&r);
    assert_eq!(rep.heartbeat, None);
    assert_eq!(w.http.calls.borrow().len(), 1, "a stale check pings nothing");

    put_record(&r, &record(2, r.clock.now_ms()));
    w.url.borrow_mut().clear();
    let rep = w.pass(&r);
    assert_eq!(rep.heartbeat.as_deref(), Some(ap::NO_HEARTBEAT_URL));
    assert_eq!(w.http.calls.borrow().len(), 1);
    assert_eq!(alarms_section(&r)["section"]["status"]["heartbeat_last"], ap::NO_HEARTBEAT_URL);
}

#[test]
fn when_the_alarms_pass_stops_its_section_reads_stale() {
    let r = rig();
    let w = World::new("alarms-stale", Some("sp-ask"));
    assert!(w.pass(&r).published);
    assert_eq!(alarms_section(&r)["freshness"]["state"], "Fresh");
    r.clock.advance(16 * MIN);
    assert_eq!(alarms_section(&r)["freshness"]["state"], "Stale");
}

#[test]
fn an_exhausted_section_pages_once_under_its_open_conditions() {
    let r = rig();
    let w = World::new("alarms-exhausted", Some("sp-ask"));
    probe(&r, "a", &[("Deep", NOW)]);
    let mut rec = record(1, r.clock.now_ms());
    rec.exhausted = vec!["Probe/a".into(), "Probe/gone".into()];
    put_record(&r, &rec);
    let rep = w.pass(&r);
    assert_eq!(rep.sent, ["page:Probe/a:Deep", "page:Probe/gone:Exhausted"]);
    assert!(w.mail.sent.borrow()[0].body.contains("failed inside its back-off"));
    r.clock.advance(MIN);
    assert!(w.pass(&r).sent.is_empty());
}

#[test]
fn the_probe_proves_the_primary_path_alone_and_its_health_reads_the_latest_probe() {
    let r = rig();
    let w = World::new("alarms-probe", Some("m"));
    let run_dir = w.run_dir();
    let timer_on = || false;
    let switch = FileSwitch { run_dir: &run_dir, timer_on: &timer_on };
    let fallback = MaildirFallback { mail_root: w.mail_root(), mute: false };
    let lc = Dispatch(&r);
    let p = Ports { lc: &lc, primary: &w.mail, fallback: &fallback, http: &w.http, switch: &switch, clock: &r.clock };
    let now = || r.clock.now_ms() / 1000;
    assert!(ap::probe_health(&run_dir, now(), 7200).is_err(), "no probe yet");
    assert_eq!(ap::probe(&p, &w.cfg(), "alarms-probe"), Ok("m-1".to_string()));
    assert_eq!(w.mail.sent.borrow()[0].mailbox, "alarms-probe");
    assert_eq!(ap::probe_health(&run_dir, now(), 7200), Ok(()));
    r.clock.advance(2 * HOUR + 1000);
    assert!(ap::probe_health(&run_dir, now(), 7200).unwrap_err().contains("old"));
    w.mail.set(None);
    assert!(ap::probe(&p, &w.cfg(), "alarms-probe").is_err());
    assert!(!w.mail_root().exists(), "a probe never falls back");
    assert!(ap::probe_health(&run_dir, now(), 7200).unwrap_err().contains("failed"));
}
