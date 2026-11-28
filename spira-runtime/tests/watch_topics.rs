// covers: spira-runtime/src/watch_topics.rs spira-runtime/src/watchers/claim_count.rs spira-runtime/src/tasks/e/watch_ami_stale.rs spira-runtime/src/tasks/e/watch_throughput.rs spira-runtime/src/tasks/e/watch_transitions.rs spira-runtime/src/watchers/bead_sweep.rs spira-runtime/src/watchers/supersede_duty.rs spira-runtime/src/tasks/d/watch_perf.rs
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
use spira_runtime::tasks::d::watch_perf as perf;
use spira_runtime::tasks::e::watch_ami_stale as ami;
use spira_runtime::tasks::e::watch_throughput as throughput;
use spira_runtime::tasks::e::watch_transitions as transitions;
use spira_runtime::watch_topics::WatchTopics;
use spira_runtime::watchers::bead_sweep as sweep;
use spira_runtime::watchers::claim_count as claims;
use spira_runtime::watchers::supersede_duty as supersede;
use spira_runtime::watchers::{INBOX_APPEND, WATCH_PUBLISH};
use spira_runtime::Effects;
use testkit::broker::{Broker, Delivered, Subscriber};

/// Bounds each wait on the broker so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);

/// The service's effect runner, its watch publisher on the broker at `socket`.
fn service(broker: &Broker) -> Arc<Effects> {
    let run_dir = broker.socket().parent().unwrap().to_path_buf();
    let fx = Effects { world: spira_runtime::world::World { run_dir, ..Default::default() }, ..Default::default() };
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
            .double(transitions::LOG_APPEND, |l| Ok(l.into()))
            .double(throughput::FILE, |_| Ok("filed".into()))
            // The other writes of a row that has any; its conditions are what is asserted here.
            .double(supersede::ACT, |_| Ok(String::new()))
            .double(perf::LOG, |l| Ok(l.into()))
            .double(perf::SLOW_SAMPLE, |l| Ok(l.into()))
            .double(perf::METRICS, |_| Ok("{}".into()))
            .double(WATCH_PUBLISH, move |input| runner.lock().unwrap().run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", input, 10_000)));
        rig.clock.advance_to(START_S * 1000);
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

fn sweep_world(rows: &[&str]) -> String {
    let rows = rows.iter().map(|id| sweep::Row { id: id.to_string(), state: "READY".into(), holds: vec!["poison".into()], ..Default::default() }).collect();
    serde_json::to_string(&sweep::Snapshot { rows, ..Default::default() }).unwrap()
}

fn sweep_row(fx: Arc<Effects>) -> Row {
    let task = sweep::BeadSweep { interval_ms: 60_000, ask_label: "ask".into(), home_repo: "spira".into(), hold_min: 30, rework_min: 5, cert_min: 5, wait_min: 45, batch_min: 60, stop_review_secs: 3600 };
    Row::boot(sweep::NAME, sweep::OBSERVE, 60_000, task.registration(), sweep::BeadSweep::boot(), sweep_world(&[]), fx)
}

fn perf_world(lc_list_ms: u64) -> String {
    let timings = vec![perf::Timing { name: "lc-list".into(), micros: lc_list_ms * 1000, millis: lc_list_ms, ..Default::default() }];
    serde_json::to_string(&perf::Snapshot { timings, ..Default::default() }).unwrap()
}

fn perf_row(fx: Arc<Effects>) -> Row {
    let config = perf::Config { budget_ms: 500, interval_secs: 300, remind_secs: 3600, inbox: "q/inbox".into(), metrics_endpoint: "127.0.0.1:9".into(), run: "r".into() };
    Row::boot(perf::INBOX_TAG, perf::OBSERVE, perf::CADENCE_MS, perf::WatchPerf::new(config).registration(), perf::WatchPerf::boot(), perf_world(40), fx)
}

fn supersede_world(requested: bool) -> String {
    let reading = supersede::Reading::Request { succ: "sp-new".into(), own: "HELD".into(), succ_state: "LANDED".into() };
    let held = requested.then(|| supersede::Held { id: "sp-old".into(), version: "3".into(), reading });
    serde_json::to_string(&supersede::Snapshot { held: held.into_iter().collect() }).unwrap()
}

fn supersede_row(fx: Arc<Effects>) -> Row {
    let task = supersede::SupersedeDuty { interval_ms: 60_000 };
    Row::boot(supersede::NAME, supersede::OBSERVE, 60_000, task.registration(), supersede::SupersedeDuty::boot(), supersede_world(false), fx)
}

fn payload(held: &str) -> Value {
    serde_json::from_str(held).unwrap_or_else(|e| panic!("{held:?}: {e}"))
}

fn is_clear(d: &Delivered, topic: &str) -> bool {
    d.topic == topic && d.payload.is_empty()
}

/// The epoch second every row's rig starts at: the flow rows judge ages against it.
const START_S: u64 = 10 * 86_400;

fn transitions_world(unsubmitted: bool) -> String {
    let bead = |id: &str| transitions::Bead { id: id.into(), repo: "spira".into(), state: "WORKING".into(), since: START_S - 600, ..Default::default() };
    let beads = if unsubmitted { vec![bead("sp-w1"), bead("sp-w2")] } else { vec![] };
    let last = [("spira".to_string(), [("Submit".to_string(), START_S - 3 * 3_600)].into())].into();
    serde_json::to_string(&transitions::Snapshot { readable: true, horizon: START_S - 6 * 3_600, beads, last, ..Default::default() }).unwrap()
}

fn transitions_config() -> transitions::Config {
    transitions::Config { period_s: 300, working_min: 2, submit_bound_s: 5_400, cut_bound_s: 2_700, pr_bound_s: 900, gate_timeout_s: 2_700, certify_par: 2, deliver_bound_s: 3_600, activate_bound_s: 900, ask_bound_s: 3_600, release_repo: "spira".into(), heartbeat_bound_s: 120, task_bound_s: 600, round_max_open: 1, releases: "rel".into(), repo_map: "map".into(), activation_bound_s: 75, run: "run".into() }
}

fn transitions_row(fx: Arc<Effects>) -> Row {
    Row::boot("transitions", transitions::OBSERVE, 300_000, transitions::WatchTransitions::new(transitions_config()).registration(), transitions::WatchTransitions::boot(), transitions_world(false), fx)
}

fn throughput_world(stalled: bool) -> String {
    let waiting = throughput::Waiting { bead: "sp-x".into(), since: Some(START_S - 3_600) };
    let landing = throughput::RepoLanding { repo: "spira".into(), hourly: vec![0; 24], last_landing: START_S - 3_600, waiting: 1, oldest: Some(waiting), ..Default::default() };
    serde_json::to_string(&throughput::Snapshot { readable: true, landings: if stalled { vec![landing] } else { vec![] }, ..Default::default() }).unwrap()
}

fn throughput_config() -> throughput::Config {
    throughput::Config { stall_s: 600, abandon_count: 5, abandon_window_s: 7_200, ratio_min_pct: 40, ratio_min_runs: 6, passed_grace_s: 300, activation_bound_s: 75 }
}

fn throughput_row(fx: Arc<Effects>) -> Row {
    Row::boot("throughput", throughput::OBSERVE, throughput::CADENCE_MS, throughput::WatchThroughput::new(throughput_config()).registration(), throughput::WatchThroughput::boot(), throughput_world(false), fx)
}

/// What the one retained value of a holding condition reads as.
struct Said {
    topic: &'static str,
    level: &'static str,
    text_has: &'static str,
}

/// Three passes over a holding condition, then the pass where it stops.
fn holds_once_then_clears(mut row: Row, broker: &Broker, mut watching: Subscriber, held: &str, gone: &str, said: Said) {
    let Said { topic, level, text_has } = said;
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
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = ami_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &stale(), &ami_world(ami::Verdict::Fresh), Said { topic: "watch/round-ami-stale/all", level: "P2", text_has: "ami-0abc is stale; EC2 rounds refuse" });
}

#[test_id(tdcpf0f)]
#[test]
fn an_over_claimed_item_is_one_retained_value_while_it_is_live_and_none_once_it_leaves() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = claim_row(service(&broker));
    let held = claim_world(&[("sp-a", 5), ("sp-under", 3)]);
    holds_once_then_clears(row, &broker, watching, &held, &claim_world(&[("sp-under", 3)]), Said { topic: "watch/claim-count/sp-a", level: "P2", text_has: "CLAIMS sp-a: claimed by a builder 5 times" });
}

#[test_id(tgytcv9)]
#[test]
fn a_flow_stall_is_one_retained_value_for_its_repo_while_it_holds_and_none_once_it_clears() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = transitions_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &transitions_world(true), &transitions_world(false), Said { topic: "watch/transitions/spira", level: "P0", text_has: "P0 STALL WORKING→SUBMITTED (spira)" });
}

#[test_id(tz70y6b)]
#[test]
fn a_landing_stall_is_one_retained_value_for_its_repo_while_it_holds_and_none_once_it_lands() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = throughput_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &throughput_world(true), &throughput_world(false), Said { topic: "watch/throughput/spira", level: "P0", text_has: "P0 spira: landing stalled" });
}

#[test_id(t7uj3b4)]
#[test]
fn a_suspect_is_one_retained_value_while_the_sweep_finds_it_and_none_once_it_does_not() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = sweep_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &sweep_world(&["sp-a"]), &sweep_world(&[]), Said { topic: "watch/bead-sweep/sp-a", level: "P2", text_has: "SUSPECT sp-a READY POISONED" });
}

#[test_id(t3y07dl)]
#[test]
fn a_slow_probe_is_one_retained_value_while_it_is_slow_and_none_once_it_is_not() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = perf_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &perf_world(900), &perf_world(40), Said { topic: "watch/perf-watch/lc-list", level: "P2", text_has: "PERF SLOW lc-list 900ms" });
}

#[test_id(twjn1qc)]
#[test]
fn a_supersede_request_is_one_retained_value_while_it_stands_and_none_once_it_is_gone() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let row = supersede_row(service(&broker));
    holds_once_then_clears(row, &broker, watching, &supersede_world(true), &supersede_world(false), Said { topic: "watch/supersede-duty/sp-old", level: "P2", text_has: "SUPERSEDE REQUEST sp-old: successor sp-new" });
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
    // The whole republish is queued at once, so a flush covers anything still behind these.
    flush(&row.fx.lock().unwrap());
    let mut after = broker.retained(&row.filter());
    after.sort();
    assert_eq!(after, before);
}

#[test_id(tlhr7s6)]
#[test]
fn a_restarted_broker_is_given_the_stale_image_again() {
    let _env = testkit::env_read();
    let mut broker = Broker::up();
    let mut row = ami_row(service(&broker));
    row.pass(&stale());
    comes_back_after_a_broker_restart(&row, &mut broker, 1);
}

#[test_id(t6ojzvn)]
#[test]
fn a_restarted_broker_is_given_every_held_claim_and_no_cleared_one() {
    let _env = testkit::env_read();
    let mut broker = Broker::up();
    let mut row = claim_row(service(&broker));
    row.pass(&claim_world(&[("sp-a", 4), ("sp-b", 8), ("sp-gone", 4)]));
    row.pass(&claim_world(&[("sp-a", 4), ("sp-b", 8)]));
    comes_back_after_a_broker_restart(&row, &mut broker, 2);
}

/// Drops the runner `row` has been publishing through, publisher and held values with it, and
/// puts a new one in its place: only that one can say anything afterwards.
fn replace_the_process(row: &Row, broker: &Broker) -> Arc<Effects> {
    let fx = service(broker);
    let old = std::mem::replace(&mut *row.fx.lock().unwrap(), fx.clone());
    assert_eq!(Arc::strong_count(&old), 1, "something else still holds the old process");
    fx
}

/// A new process holds nothing until an observe hands it what each row last published.
#[test_id(t21q195)]
#[test]
fn a_restarted_runtime_says_its_carried_claims_again_to_a_broker_that_kept_nothing() {
    let _env = testkit::env_read();
    let mut broker = Broker::up();
    let mut row = claim_row(service(&broker));
    let held = claim_world(&[("sp-a", 4)]);
    row.pass(&held);
    let before = broker.retained(&row.filter());
    assert_eq!(before.len(), 1, "{before:?}");

    let fx = replace_the_process(&row, &broker);
    broker.restart();
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

/// The same, for the image row's own observe runner. Spill is not on EC2 in its configuration,
/// so the observe behind the tracking asks AWS nothing.
#[test_id(tfirjgp)]
#[test]
fn a_restarted_runtime_says_the_carried_stale_image_again_to_a_broker_that_kept_nothing() {
    let dir = testkit::TempDir::new("watch-ami-spill");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_ROUND_VM_SPILL", "local")]);
    let _env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    let mut broker = Broker::up();
    let mut row = ami_row(service(&broker));
    row.pass(&stale());
    let before = broker.retained(&row.filter());
    assert_eq!(before.len(), 1, "{before:?}");

    let fx = replace_the_process(&row, &broker);
    broker.restart();
    let mut watching = Subscriber::to(broker.socket(), &["watch/#"]);
    let mark = row.rig.requested().len();
    row.rig.advance(ami::CADENCE_MS).unwrap();
    let asked = row.rig.requested()[mark..].iter().find(|q| q.name == ami::OBSERVE).expect("a stale image is asked about again").input.clone();
    let answered = fx.run("o", &EffectSpec::probe(ami::OBSERVE, asked, 1000)).expect("the observe answers");
    assert!(answered.contains("local"), "{answered}");
    flush(&fx);

    let d = watching.recv();
    assert_eq!(vec![(d.topic, String::from_utf8_lossy(&d.payload).into_owned())], before);
}

/// What each flow row's boot after a restart asks its observe, run by the service's own runner:
/// the observe's own answer needs a store, but what it was asked with is published either way.
fn says_its_carried_value_again(mut row: Row, mut broker: Broker, held: &str, observe: &str, registration: Registration, boot: (&str, &str, EventKind)) {
    row.pass(held);
    let before = broker.retained(&row.filter());
    assert_eq!(before.len(), 1, "{before:?}");

    let fx = replace_the_process(&row, &broker);
    broker.restart();
    let mut watching = Subscriber::to(broker.socket(), &["watch/#"]);
    row.rig.reopen(registration).unwrap();
    let mark = row.rig.requested().len();
    row.rig.tick(boot.1, boot.2).unwrap();
    let asked = row.rig.requested()[mark..].iter().find(|q| q.name == observe).expect("the boot observes").input.clone();
    let _ = fx.run("o", &EffectSpec::probe(observe, asked, 1000));
    flush(&fx);

    let d = watching.recv();
    assert_eq!(vec![(d.topic, String::from_utf8_lossy(&d.payload).into_owned())], before);
}

#[test_id(txzcfv6)]
#[test]
fn a_restarted_runtime_says_its_carried_flow_stall_again_to_a_broker_that_kept_nothing() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let row = transitions_row(service(&broker));
    let registration = transitions::WatchTransitions::new(transitions_config()).registration();
    says_its_carried_value_again(row, broker, &transitions_world(true), transitions::OBSERVE, registration, transitions::WatchTransitions::boot());
}

#[test_id(t9089zn)]
#[test]
fn a_restarted_runtime_says_its_carried_landing_stall_again_to_a_broker_that_kept_nothing() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let row = throughput_row(service(&broker));
    let registration = throughput::WatchThroughput::new(throughput_config()).registration();
    says_its_carried_value_again(row, broker, &throughput_world(true), throughput::OBSERVE, registration, throughput::WatchThroughput::boot());
}

/// A broker that keeps a value while this publisher cannot reach it: the publisher's path to it
/// does not exist until the test makes it.
#[test_id(txhrbiy)]
#[test]
fn a_clear_the_broker_could_not_take_is_sent_once_it_can_be_reached() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let direct = Publisher::with("before", Options::new(broker.socket(), Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(2)))).unwrap();
    direct.publish(spira_pub::Class::State, "watch/claim-count/sp-a", &json!({"text": "old"})).unwrap();
    direct.flush(FAILSAFE).unwrap();

    let dir = testkit::TempDir::new("watch-unreached");
    let path = dir.join("s");
    let topics = WatchTopics::new(Publisher::with("spira-runtime", Options::new(&path, Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(1)))).unwrap());
    topics.publish(&json!({"row": "claim-count", "set": {}, "clear": ["sp-a"]}).to_string()).unwrap();
    assert_eq!(broker.retained("watch/#").len(), 1, "the clear had no way to the broker");

    std::os::unix::fs::symlink(broker.socket(), &path).unwrap();
    let value = json!({"level": "P2", "text": "new", "since": 1, "at": 1});
    topics.publish(&json!({"row": "claim-count", "set": {"sp-b": value}, "clear": []}).to_string()).unwrap();
    topics.flush(FAILSAFE).unwrap();

    let held: Vec<String> = broker.retained("watch/#").into_iter().map(|(t, _)| t).collect();
    assert_eq!(held, ["watch/claim-count/sp-b"]);
}

#[test_id(ttx5vdv)]
#[test]
fn a_clear_lost_with_a_stopping_runtime_is_asked_for_again_after_the_restart() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let fx = service(&broker);
    // What the process that is about to stop had put on the broker.
    let held = json!({"row": "claim-count", "set": {"sp-a": {"level": "P2", "text": "x", "since": 1, "at": 1}}, "clear": []}).to_string();
    fx.run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", held, 10_000)).unwrap();
    flush(&fx);

    let world = Arc::new(Mutex::new(claim_world(&[("sp-a", 4)])));
    let (seen, runner) = (world.clone(), fx.clone());
    let registration = || claims::ClaimCount { interval_ms: 60_000, every: 4 }.registration();
    let mut rig = TaskRig::new(1, registration())
        .unwrap()
        .double(claims::OBSERVE, move |_| Ok(seen.lock().unwrap().clone()))
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(WATCH_PUBLISH, move |input| runner.run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", input, 10_000)))
        .hanging(WATCH_PUBLISH);
    let (_, stream, boot) = claims::ClaimCount::boot();
    rig.tick(stream, boot).unwrap();
    *world.lock().unwrap() = claim_world(&[]);
    rig.advance(60_000).unwrap();
    assert_eq!(broker.retained("watch/#").len(), 1, "the clear never ran");

    rig.reopen(registration()).unwrap();
    rig.run_until_idle().unwrap();
    flush(&fx);
    assert_eq!(broker.retained("watch/#"), vec![]);
}

#[test_id(t83rse6)]
#[test]
fn a_publish_with_no_broker_configured_says_so_and_is_no_failure() {
    let _env = testkit::env_read();
    let fx = Effects::default();
    assert!(fx.watch_topics.set(None).is_ok());
    let input = json!({"row": "claim-count", "set": {}, "clear": ["sp-a"]}).to_string();
    assert_eq!(fx.run("k", &EffectSpec::keyed(WATCH_PUBLISH, "k", input, 1000)), Ok("not published: no broker configured".to_string()));
}
