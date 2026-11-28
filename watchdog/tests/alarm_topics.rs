// covers: watchdog/src/alarm.rs watchdog/src/lib.rs
//! The scheduler watchdog's `alarm/watchdog`, against a real broker with a fake service manager.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
#[allow(dead_code)]
mod support;

use std::cell::RefCell;
use std::sync::Arc;

use runtime::ports::{Clock, VirtualClock};
use runtime::units::{ScriptedUnits, UnitFacts, UnitState};
use serde_json::{json, Value};
use testkit::broker::{Broker, Subscriber};
use watchdog::{watch, Config, Deps, InboxPager, MarkerSource, Outcome, Reading, StatusSource};

const UNIT: &str = "spira-runtime.service";
const START_MS: u64 = 1_000_000;

struct Status(RefCell<Reading>);
impl StatusSource for Status {
    fn read(&self) -> Reading {
        self.0.borrow().clone()
    }
}
struct NoMarker;
impl MarkerSource for NoMarker {
    fn in_flight_since(&self) -> Option<u64> {
        None
    }
}

struct Rig {
    cfg: Config,
    clock: VirtualClock,
    units: ScriptedUnits,
    status: Status,
    pager: InboxPager,
    /// The configuration the alarm's publisher is built from.
    broker: String,
}

impl Rig {
    /// An active scheduler whose last turn is older than the bound.
    fn wedged(dir: &testkit::TempDir, broker: &Broker) -> Rig {
        let cfg = Config::from_lookup(
            |k| Ok(match k {
                watchdog::KEY_BOUND => "90".to_string(),
                watchdog::KEY_UNIT => UNIT.to_string(),
                watchdog::KEY_RUN => dir.path().display().to_string(),
                other => dir.join(other).display().to_string(),
            }),
            Some(dir.path().display().to_string()),
        )
        .unwrap();
        let clock = VirtualClock::new(START_MS);
        let units = ScriptedUnits::new(Arc::new(clock.clone()));
        units.install(UNIT, UnitFacts { state: UnitState::Active, main_pid: Some(7), exit: None, since_ms: Some(0) });
        let status = Status(RefCell::new(Reading::Turn { at_ms: START_MS - 91_000 }));
        let pager = InboxPager(support::sink(dir, broker, "spira-watchdog"));
        Rig { cfg, clock, units, status, pager, broker: broker.socket().display().to_string() }
    }

    fn pass(&self) -> Outcome {
        let alarm = watchdog::publisher(&|k| match k {
            "SPIRA_BROKER_SOCKET" => Ok(self.broker.clone()),
            "SPIRA_RUN" => Ok(self.cfg.incident.parent().unwrap().display().to_string()),
            "SPIRA_SEED" => Ok("1".into()),
            other => Err(format!("{other} is not declared")),
        })
        .expect("the configuration names a broker");
        let deps = Deps { clock: &self.clock, units: &self.units, status: &self.status, marker: &NoMarker, pager: &self.pager };
        watch(&self.cfg, &deps, Some(&alarm)).unwrap()
    }

    fn pass_after(&self, ms: u64) -> Outcome {
        self.clock.advance(ms);
        self.pass()
    }
}

fn alarms(broker: &Broker) -> Vec<(String, Value)> {
    broker.retained("alarm/#").into_iter().map(|(t, v)| (t, serde_json::from_str(&v).unwrap())).collect()
}

#[test_id(tzc2uco)]
#[test]
fn a_wedged_scheduler_is_one_unchanged_alarm_until_a_healthy_pass_clears_it() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("watchdog-alarm"));
    let r = Rig::wedged(&d, &broker);

    assert!(r.pass().scheduler_down());

    let raised = vec![("alarm/watchdog".to_string(), json!({ "text": "last turn 91s ago, bound 90s", "since": 1000, "at": 1000 }))];
    assert_eq!(alarms(&broker), raised);

    assert!(r.pass_after(60_000).scheduler_down());
    assert_eq!(alarms(&broker), raised, "the value does not move while the check keeps failing");

    *r.status.0.borrow_mut() = Reading::Turn { at_ms: r.clock.now_ms() };
    assert_eq!(r.pass(), Outcome::Recovered { since_ms: START_MS });
    assert_eq!(alarms(&broker), vec![]);
}

#[test_id(tueftqb)]
#[test]
fn with_the_broker_stopped_the_passes_complete_and_still_page_the_inbox() {
    let (mut broker, d) = (Broker::up(), testkit::TempDir::new("watchdog-alarm-nobroker"));
    let r = Rig::wedged(&d, &broker);
    broker.stop();

    let outcomes = [r.pass(), r.pass_after(60_000), r.pass_after(120_000)];

    assert!(matches!(outcomes[2], Outcome::Down { page: Some(Ok(())), paged: true, .. }), "{}", outcomes[2]);
    let inbox = std::fs::read_to_string(&r.pager.0.inbox).unwrap();
    assert!(inbox.contains(&format!("scheduler {UNIT} is DOWN")), "{inbox}");
}

#[test_id(tyc7em8)]
#[test]
fn a_clear_the_broker_missed_is_sent_by_a_later_healthy_pass() {
    let (mut broker, d) = (Broker::up(), testkit::TempDir::new("watchdog-alarm-missed-clear"));
    let r = Rig::wedged(&d, &broker);
    r.pass();
    broker.stop();
    *r.status.0.borrow_mut() = Reading::Turn { at_ms: r.clock.now_ms() };
    assert_eq!(r.pass(), Outcome::Recovered { since_ms: START_MS });
    broker.restart();
    let mut raw = Subscriber::to(broker.socket(), &["alarm/#"]);

    assert_eq!(r.pass(), Outcome::Healthy);

    loop {
        let got = raw.recv();
        assert_eq!(got.topic, "alarm/watchdog");
        if got.payload.is_empty() {
            break;
        }
    }
}
