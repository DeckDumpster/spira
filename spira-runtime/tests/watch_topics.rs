// covers: spira-runtime/src/watch_topics.rs spira-runtime/src/watchers/claim_count.rs spira-runtime/src/tasks/e/watch_ami_stale.rs
//! The watch rows' topics on a real broker: each row's handler on the task rig, its
//! `watch.publish` run by the service's own effect runner.
#[macro_use]
extern crate spira_test_id;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::event::{EffectSpec, EventKind, Registration};
use runtime::host::EffectRunner;
use runtime::ports::{SeededRng, VirtualClock};
use runtime::taskrig::TaskRig;
use serde_json::{json, Value};
use spira_pub::{Options, Publisher};
use spira_runtime::tasks::e::watch_ami_stale as ami;
use spira_runtime::watch_topics::WatchTopics;
use spira_runtime::watchers::claim_count as claims;
use spira_runtime::watchers::{INBOX_APPEND, WATCH_PUBLISH};
use spira_runtime::Effects;
use testkit::broker::{Broker, Delivered, Subscriber};

/// Bounds each wait on the broker so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);

/// The service's effect runner, its watch publisher on the broker at `socket`.
fn service(broker: &Broker) -> Arc<Effects> {
    let fx = Effects::default();
    let options = Options::new(broker.socket(), Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(1)));
    assert!(fx.watch_topics.set(Some(WatchTopics::new(Publisher::with("spira-runtime", options).unwrap()))).is_ok());
    Arc::new(fx)
}

fn flush(fx: &Effects) {
    fx.watch_topics.get().unwrap().as_ref().unwrap().flush(FAILSAFE).unwrap();
}

/// One row on the rig, in a world of the test's: `world` is the snapshot its observe answers.
struct Row {
    rig: TaskRig,
    world: Arc<Mutex<String>>,
    fx: Arc<Mutex<Arc<Effects>>>,
    name: &'static str,
    cadence_ms: u64,
}

impl Row {
    #[allow(clippy::too_many_arguments)]
    fn boot(name: &'static str, observe: &'static str, cadence_ms: u64, registration: Registration, boot: (&str, &str, EventKind), quiet: String, fx: Arc<Effects>) -> Row {
        let (world, fx) = (Arc::new(Mutex::new(quiet)), Arc::new(Mutex::new(fx)));
        let (seen, runner) = (world.clone(), fx.clone());
        let mut rig = TaskRig::new(1, registration)
            .unwrap()
            .double(observe, move |_| Ok(seen.lock().unwrap().clone()))
            .double(INBOX_APPEND, |l| Ok(l.into()))
            .double(WATCH_PUBLISH, move |input| runner.lock().unwrap().run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", input, 10_000)));
        rig.tick(boot.1, boot.2).unwrap();
        Row { rig, world, fx, name, cadence_ms }
    }

    fn pass(&mut self, world: &str) {
        *self.world.lock().unwrap() = world.to_string();
        self.rig.advance(self.cadence_ms).unwrap();
        flush(&self.fx.lock().unwrap());
    }

    fn filter(&self) -> String {
        format!("watch/{}/#", self.name)
    }
}

fn ami_world(verdict: ami::Verdict) -> String {
    serde_json::to_string(&ami::Snapshot { spill: "ec2".into(), verdict }).unwrap()
}

fn stale() -> String {
    ami_world(ami::Verdict::Stale { line: "round AMI ami-0abc is stale".into() })
}

/// The row's boot reads the world before the test has said anything: it reads stale, the one
/// verdict that arms the next pass.
fn ami_row(fx: Arc<Effects>) -> Row {
    let config = ami::Config { spill: "ec2".into(), inbox: "q/inbox".into() };
    Row::boot(ami::INBOX_TAG, ami::OBSERVE, ami::CADENCE_MS, ami::WatchAmiStale::new(config).registration(), ami::WatchAmiStale::boot(), stale(), fx)
}

fn claim_world(rows: &[(&str, u32)]) -> String {
    let rows = rows.iter().map(|(id, n)| claims::Claims { id: id.to_string(), n: *n, ends: vec![] }).collect();
    serde_json::to_string(&claims::Snapshot { rows }).unwrap()
}

fn claim_row(fx: Arc<Effects>) -> Row {
    let task = claims::ClaimCount { interval_ms: 60_000, every: 4 };
    Row::boot(claims::NAME, claims::OBSERVE, 60_000, task.registration(), claims::ClaimCount::boot(), claim_world(&[]), fx)
}

fn payload(held: &str) -> Value {
    serde_json::from_str(held).unwrap_or_else(|e| panic!("{held:?}: {e}"))
}

fn is_clear(d: &Delivered, topic: &str) -> bool {
    d.topic == topic && d.payload.is_empty()
}

/// Three passes over a holding condition, then the pass where it stops.
fn holds_once_then_clears(mut row: Row, broker: &Broker, mut watching: Subscriber, held: &str, gone: &str, topic: &str, text_has: &str, level: &str) {
    for _ in 0..3 {
        row.pass(held);
    }
    let retained = broker.retained(&row.filter());
    let [(at_topic, value)] = retained.as_slice() else { panic!("one retained value on {}: {retained:?}", row.filter()) };
    assert_eq!(at_topic, topic);
    let value = payload(value);
    assert!(value["text"].as_str().unwrap().contains(text_has) && !value["text"].as_str().unwrap().contains("[watch:"), "{value}");
    assert_eq!(value["level"], level);
    assert!(value["since"].is_u64() && value["at"].is_u64(), "{value}");
    assert_eq!(value.as_object().unwrap().len(), 4, "{value}");
    let first = watching.recv();
    assert_eq!((first.topic.as_str(), payload(&String::from_utf8_lossy(&first.payload))), (topic, value));

    row.pass(gone);
    // The next delivery is the clear, so the three holding passes reached the subscriber once.
    let next = watching.recv();
    assert!(is_clear(&next, topic), "{next:?}");
    assert_eq!(broker.retained(&row.filter()), vec![]);
}

#[test_id(tcr36vf)]
#[test]
fn a_stale_image_is_one_retained_value_while_it_holds_and_none_once_it_reads_fresh() {
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = ami_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &stale(), &ami_world(ami::Verdict::Fresh), "watch/round-ami-stale/all", "ami-0abc is stale; EC2 rounds refuse", "P2");
}

#[test_id(tdcpf0f)]
#[test]
fn an_over_claimed_item_is_one_retained_value_while_it_is_live_and_none_once_it_leaves() {
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = claim_row(service(&broker));
    let held = claim_world(&[("sp-a", 5), ("sp-under", 3)]);
    holds_once_then_clears(row, &broker, watching, &held, &claim_world(&[("sp-under", 3)]), "watch/claim-count/sp-a", "CLAIMS sp-a: claimed by a builder 5 times", "P2");
}

/// Restarts the broker, which keeps nothing, and requires every value `row` held to come back.
fn comes_back_after_a_broker_restart(row: &Row, broker: &mut Broker, subjects: usize) {
    let mut before = broker.retained(&row.filter());
    before.sort();
    assert_eq!(before.len(), subjects, "{before:?}");
    broker.restart();
    // Waits for the reconnect's republish: retained or live, each value is delivered.
    let mut watching = Subscriber::to(broker.socket(), &[&row.filter()]);
    let mut back = std::collections::BTreeMap::new();
    while back.len() < subjects {
        let d = watching.recv();
        back.insert(d.topic, String::from_utf8_lossy(&d.payload).into_owned());
    }
    assert_eq!(back.into_iter().collect::<Vec<_>>(), before);
}

#[test_id(tlhr7s6)]
#[test]
fn a_restarted_broker_is_given_the_stale_image_again() {
    let mut broker = Broker::up();
    let mut row = ami_row(service(&broker));
    row.pass(&stale());
    comes_back_after_a_broker_restart(&row, &mut broker, 1);
}

#[test_id(t6ojzvn)]
#[test]
fn a_restarted_broker_is_given_every_held_claim_and_no_cleared_one() {
    let mut broker = Broker::up();
    let mut row = claim_row(service(&broker));
    row.pass(&claim_world(&[("sp-a", 4), ("sp-b", 8), ("sp-gone", 4)]));
    row.pass(&claim_world(&[("sp-a", 4), ("sp-b", 8)]));
    comes_back_after_a_broker_restart(&row, &mut broker, 2);
}

/// A new process holds nothing until an observe hands it what each row last published.
#[test_id(t21q195)]
#[test]
fn a_restarted_runtime_says_its_carried_claims_again_to_a_broker_that_kept_nothing() {
    let mut broker = Broker::up();
    let mut row = claim_row(service(&broker));
    let held = claim_world(&[("sp-a", 4)]);
    row.pass(&held);
    let before = broker.retained(&row.filter());
    assert_eq!(before.len(), 1, "{before:?}");

    broker.restart();
    let fx = service(&broker);
    let mut watching = Subscriber::to(broker.socket(), &["watch/#"]);
    row.rig.reopen(claims::ClaimCount { interval_ms: 60_000, every: 4 }.registration()).unwrap();
    let mark = row.rig.requested().len();
    let (_, stream, kind) = claims::ClaimCount::boot();
    row.rig.tick(stream, kind).unwrap();
    let asked = row.rig.requested()[mark..].iter().find(|q| q.name == claims::OBSERVE).expect("the boot observes").input.clone();
    // The observe's own answer needs a store; what it was asked with is published either way.
    let _ = fx.run("o", &EffectSpec::probe(claims::OBSERVE, asked, 1000));
    flush(&fx);

    let d = watching.recv();
    assert_eq!(vec![(d.topic, String::from_utf8_lossy(&d.payload).into_owned())], before);
}

#[test_id(t83rse6)]
#[test]
fn a_publish_with_no_broker_configured_says_so_and_is_no_failure() {
    let fx = Effects::default();
    assert!(fx.watch_topics.set(None).is_ok());
    let input = json!({"row": "claim-count", "set": {}, "clear": ["sp-a"]}).to_string();
    assert_eq!(fx.run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", input, 1000)), Ok("not published: no broker configured".to_string()));
}
