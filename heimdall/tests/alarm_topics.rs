// covers: heimdall/src/pass.rs heimdall/src/alarm.rs
//! Each alert heimdall has firing, as one retained value on `alarm/<key>`, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::sync::Arc;

use heimdall::classify::{Observation, RestartAlert, Thresholds};
use heimdall::conditions::{Condition, Conditions, R};
use heimdall::pass::{self, PassInput, PassPaths, PassReport};
use heimdall::state::State;
use inbox_rules::Bus;
use ports::VirtualClock;
use serde_json::{json, Value};
use spira_sub::core::{Core, Saved};
use testkit::broker::{Broker, Subscriber};

const T0: i64 = 1_800_000_000;
const UNIT: &str = "a.service";
const RESTARTS: &str = "alarm/restart-loop:a.service";
const STREAK: &str = "alarm/gate-red-streak-spira";

/// A store that takes every write and holds nothing.
struct Store;
impl Conditions for Store {
    fn list(&self) -> R<Vec<Condition>> {
        Ok(Vec::new())
    }
    fn set(&self, _: &str, _: &str, _: &str, _: &str, _: bool) -> R<()> {
        Ok(())
    }
    fn clear(&self, _: &str, _: &str, _: &str) -> R<()> {
        Ok(())
    }
}

struct Rig {
    dir: testkit::TempDir,
    bus: Bus,
    state: State,
    now: i64,
    /// A restart loop's delta, which is part of its title.
    restarts: Option<i64>,
    gate: Vec<Value>,
    gate_unread: bool,
}

impl Rig {
    fn new(broker: &Broker) -> Rig {
        Rig { dir: testkit::TempDir::new("heimdall-alarm"), bus: Bus::At(broker.socket().to_path_buf()), state: State::default(), now: T0, restarts: None, gate: Vec::new(), gate_unread: false }
    }

    fn pass(&mut self) -> PassReport {
        self.now += 120;
        let restart_alerts = self.restarts.map(|delta| RestartAlert { unit: UNIT.into(), current: delta, baseline: 0, delta, window: 3600 }).into_iter().collect();
        let obs = Observation {
            sentinel_log_readable: true,
            sentinel_timer: "active".into(),
            strands: json!({}),
            restart_alerts,
            thresholds: Thresholds { pass_stale: 1_000_000, starve_passes: 5, mirror_stale: 90_000, ghost_stale: 1800, restart_threshold: 3, restart_window: 3600, red_streak: 4, noverdict_streak: 4 },
            gate_runs: if self.gate_unread { Vec::new() } else { vec![("spira".into(), self.gate.clone())] },
            gate_runs_unread: self.gate_unread,
            ..Observation::default()
        };
        let inp = PassInput { obs, confirm: 2, clear_n: 2, refresh: 3600, tz: "UTC".into(), render_time: |e, _| format!("@{e}"), report_only: false };
        let paths = PassPaths {
            fallback: self.dir.join("fallback"),
            heartbeat: self.dir.join("heimdall.status"),
            inbox: self.dir.join("inbox.log"),
            inbox_cursor: self.dir.join("inbox.cursor"),
            inbox_bus: self.bus.clone(),
            operator_mail: Err("no mail in this rig".into()),
        };
        pass::run(&inp, &Store, &mut self.state, &paths, self.now)
    }
}

fn gate_rows(status: &str, reason: &str) -> Vec<Value> {
    ["a", "b", "c", "d"].iter().map(|b| json!({ "bead": b, "status": status, "reason": reason, "red_unit": "u", "red_line": "l" })).collect()
}

fn alarms(broker: &Broker) -> Vec<(String, Value)> {
    let mut held: Vec<(String, Value)> = broker.retained("alarm/#").into_iter().map(|(t, v)| (t, serde_json::from_str(&v).unwrap())).collect();
    held.sort_by(|a, b| a.0.cmp(&b.0));
    held
}

#[test_id(tabwjbu)]
#[test]
fn an_alert_that_keeps_firing_is_one_retained_value_a_subscriber_hears_once_until_it_passes() {
    let broker = Broker::up();
    let mut raw = Subscriber::to(broker.socket(), &["alarm/#"]);
    let mut subscriber = Core::new(Arc::new(VirtualClock::new(0)), Saved::default());
    let mut r = Rig::new(&broker);

    for delta in [5, 6, 7] {
        r.restarts = Some(delta);
        r.pass();
    }

    let since = T0 + 120;
    let value = json!({ "text": format!("{UNIT} restarted 5 time(s) in the last 60m"), "since": since, "at": since });
    assert_eq!(alarms(&broker), vec![(RESTARTS.to_string(), value)]);
    subscriber.add("alarm/+", "", false, true).unwrap();
    let mut heard = Vec::new();
    for _ in 0..3 {
        let got = raw.recv();
        heard.extend(subscriber.message(&got.topic, &got.payload, true, true).lines);
    }
    assert_eq!(heard.len(), 1, "{heard:?}");

    r.restarts = None;
    r.pass();

    assert_eq!(alarms(&broker), vec![]);
    let cleared = raw.recv();
    assert_eq!((cleared.topic.as_str(), cleared.payload.len()), (RESTARTS, 0), "nothing but the clear followed the three");
}

#[test_id(tg1fciw)]
#[test]
fn two_alerts_are_two_topics_and_each_clears_on_its_own() {
    let broker = Broker::up();
    let mut r = Rig::new(&broker);
    r.restarts = Some(5);
    r.pass();
    r.gate = gate_rows("FAIL", "branch-red");
    r.pass();

    let held = alarms(&broker);
    let topics: Vec<&str> = held.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(topics, [STREAK, RESTARTS], "a key's / does not split its topic");
    assert_eq!((held[0].1["since"].as_i64(), held[1].1["since"].as_i64()), (Some(T0 + 240), Some(T0 + 120)));

    r.restarts = None;
    r.pass();
    assert_eq!(alarms(&broker).iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), [STREAK]);

    r.restarts = Some(5);
    r.pass();
    assert_eq!(alarms(&broker)[1].1["since"], json!(T0 + 480), "an alert that returns first fired when it returned");
}

#[test_id(tejkjox)]
#[test]
fn with_the_broker_stopped_the_pass_completes_and_still_pages_the_inbox() {
    let mut broker = Broker::up();
    let mut r = Rig::new(&broker);
    broker.stop();
    r.gate = gate_rows("NO_VERDICT", "fence-silent");

    r.pass();
    let rep = r.pass();

    assert_eq!(rep.acted, 1, "the condition is raised: {:?}", rep.err);
    assert!(!rep.fallback_written);
    let inbox = std::fs::read_to_string(r.dir.join("inbox.log")).unwrap();
    assert!(inbox.contains("[heimdall]") && inbox.contains("reason=fence-silent"), "{inbox}");
    let heartbeat = std::fs::read_to_string(r.dir.join("heimdall.status")).unwrap();
    assert!(heartbeat.contains("SP_HEIMDALL_KEYS='gate-noverdict-streak/spira'"), "{heartbeat}");
    assert!(rep.err.iter().any(|l| l.contains("alarm topics")), "{:?}", rep.err);
}

#[test_id(t1b8rvo)]
#[test]
fn a_clear_the_broker_never_took_is_sent_again() {
    let mut broker = Broker::up();
    let mut r = Rig::new(&broker);
    r.restarts = Some(5);
    r.pass();
    broker.stop();
    r.restarts = None;
    r.pass();
    broker.restart();
    let mut raw = Subscriber::to(broker.socket(), &["alarm/#"]);

    r.pass();

    let got = raw.recv();
    assert_eq!((got.topic.as_str(), got.payload.len()), (RESTARTS, 0));
}

#[test_id(tcq7hgp)]
#[test]
fn unread_gate_rows_do_not_clear_a_streak_alarm_even_before_it_is_raised() {
    let broker = Broker::up();
    let mut r = Rig::new(&broker);
    r.gate = gate_rows("FAIL", "branch-red");
    r.pass();
    let raised = alarms(&broker);
    assert_eq!(raised.len(), 1, "{raised:?}");

    r.gate_unread = true;
    r.pass();
    r.pass();
    assert_eq!(alarms(&broker), raised);

    r.gate_unread = false;
    r.gate = gate_rows("PASS", "");
    r.pass();
    assert_eq!(alarms(&broker), vec![]);
}
