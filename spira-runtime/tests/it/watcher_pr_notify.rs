use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::ports::{SeededRng, VirtualClock};
use runtime::taskrig::TaskRig;
use spira_pub::{Options, Publisher};
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::pr_notify::*;
use spira_runtime::pr_topics::PrTopics;
use spira_runtime::watchers::INBOX_APPEND;
use testkit::broker::Broker;

/// The effect's real runner, publishing to the broker at `socket`.
fn topics(socket: &std::path::Path) -> Arc<PrTopics> {
    let options = Options::new(socket, Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(1)));
    Arc::new(PrTopics::new(Publisher::with("pr-notify", options).unwrap()))
}

fn registration() -> runtime::Registration {
    PrNotify { interval_ms: 90_000, deploy_giveup_secs: 100 }.registration()
}

fn forge(obs: RepoObs) -> Snapshot {
    Snapshot { repos: vec![("r".into(), obs)] }
}

fn pr_seven_open() -> Snapshot {
    forge(RepoObs { open: Some(vec![OpenPr { number: 7, status: "pending".into(), title: "t7".into(), fail: String::new() }]), ..RepoObs::default() })
}

fn pr_seven_merged() -> Snapshot {
    let finals = [(7, Final { state: "MERGED".into(), sha: "0123456789abcdef".into(), beads: vec!["- sp-aaa one".into()] })].into();
    forge(RepoObs { open: Some(vec![]), finals, ..RepoObs::default() })
}

fn pr_tracked_then_merged() -> TaskRig {
    rig_publishing_through(|_| Ok("unpublished".into()))
}

fn rig_publishing_through(publish: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> TaskRig {
    let world = Arc::new(Mutex::new(pr_seven_open()));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into())).double(OPERATOR_NOTE, |n| Ok(n.into())).double(PUBLISH, publish);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = pr_seven_merged();
    r.clock.advance(1);
    r
}

#[test_id(t6xehpl)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&pr_tracked_then_merged(), NAME);
    assert!(rows.iter().any(|k| k == "repos/r"), "{rows:?}");
}

#[test_id(t52kgxc)]
#[test]
fn a_boot_reports_the_merge_of_a_pr_tracked_before_it() {
    kit::assert_boot_matches_an_unstopped_run(pr_tracked_then_merged, registration, STREAM, EventKind::new(TICK, ""));

    let broker = Broker::up();
    let out = topics(broker.socket());
    let publisher = out.clone();
    let mut r = rig_publishing_through(move |input| publisher.publish(input));
    r.reopen(registration()).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    out.flush(FLUSH).unwrap();
    assert_eq!(retained_state(&broker, "pr/r/7"), ("MERGED".to_string(), "0123456789abcdef".to_string()));
}

const FLUSH: std::time::Duration = std::time::Duration::from_secs(30);

fn retained_state(broker: &Broker, topic: &str) -> (String, String) {
    let held = broker.retained(topic);
    let said: serde_json::Value = serde_json::from_str(&held.first().unwrap_or_else(|| panic!("nothing retained on {topic}")).1).unwrap();
    (said["state"].as_str().unwrap().into(), said["sha"].as_str().unwrap().into())
}

#[test_id(te1u2fw)]
#[test]
fn a_restarted_notifier_republishes_every_tracked_pr_to_a_broker_that_kept_nothing() {
    let carry = serde_json::to_string(&decide(&PrNotify { interval_ms: 1, deploy_giveup_secs: 100 }, &Carry::default(), &pr_seven_open(), 50).0).unwrap();
    let broker = Broker::up();
    assert!(broker.retained("pr/#").is_empty());
    let restarted = topics(broker.socket());
    restarted.track(&carry);
    restarted.flush(FLUSH).unwrap();
    assert_eq!(retained_state(&broker, "pr/r/7").0, "OPENED");
}

#[test_id(t1bu1wm)]
#[test]
fn with_the_broker_stopped_the_notifier_asks_for_what_it_asked_for_before() {
    let mut broker = Broker::up();
    broker.stop();
    let out = topics(broker.socket());
    let publisher = out.clone();
    let asked = |mut r: TaskRig| {
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        assert!(r.refused().is_empty(), "{:?}", r.refused());
        r.requested().iter().map(|q| (q.name.clone(), q.input.clone(), q.at)).collect::<Vec<_>>()
    };
    let down = asked(rig_publishing_through(move |input| publisher.publish(input)));
    assert!(down.iter().any(|(name, line, _)| name == INBOX_APPEND && line.ends_with("MERGED #7 t7 [r]")), "{down:?}");
    assert_eq!(down, asked(pr_tracked_then_merged()));
}
