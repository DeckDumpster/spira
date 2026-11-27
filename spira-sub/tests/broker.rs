// covers: spira-sub/src/run.rs spira-sub/src/client.rs spira-sub/src/main.rs
//! The built `spira-sub` against a real broker: `run`, the commands and `follow`.
#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use ports::{SeededRng, VirtualClock};
use serde_json::{json, Value};
use spira_pub::{Options, Publisher};
use testkit::broker::{Broker, Process, Subscriber};
use testkit::{ChildGuard, TempDir};

/// Bounds each wait on the broker or a child so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);

/// The environment of the built binary: a fixture config naming `broker` and a control
/// directory of this test's own.
struct World {
    dir: TempDir,
    env: Vec<(&'static str, String)>,
}

impl World {
    fn new(broker: &Path) -> World {
        World::pushing_to("", broker)
    }

    fn pushing_to(metrics: &str, broker: &Path) -> World {
        let dir = TempDir::new("sub");
        let control = dir.join("c").display().to_string();
        let declare = [("SPIRA_BROKER_SOCKET", broker.to_str().unwrap()), ("SPIRA_SUB_CONTROL_DIR", control.as_str())];
        let env = spira_config::process::fixture_metrics_env(&dir, metrics, &declare);
        World { dir, env }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(testkit::bin_exe("spira-sub"));
        command.args(args).env_clear().envs(self.env.clone()).stdin(Stdio::null());
        command
    }

    fn call(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let out = self.command(args).output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    }

    /// A command that must succeed with the broker's confirmation; its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.call(args);
        assert!(code == Some(0) && err.is_empty(), "{args:?}: {code:?} {out} {err}");
        out
    }

    fn set_file(&self) -> PathBuf {
        self.dir.join("c/concierge.subs")
    }

    /// `run`, returned once `subscriber/concierge` reads up.
    fn run(&self, broker: &Path) -> ChildGuard {
        let mut watching = Subscriber::to(broker, &["subscriber/concierge"]);
        let run = ChildGuard::spawn(&mut self.command(&["run", "--as", "concierge"]));
        until(&mut watching, "up");
        run
    }

    /// `follow`, returned once it says it is attached.
    fn follow(&self) -> Follow {
        let mut command = self.command(&["follow"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = ChildGuard::spawn(&mut command);
        let (stdout, stderr) = child.pipes();
        let (lines, notes) = (pipe(stdout.unwrap()), pipe(stderr.unwrap()));
        let follow = Follow { _child: child, lines, notes };
        follow.noted("following");
        follow
    }
}

fn pipe(from: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || BufReader::new(from).lines().map_while(Result::ok).try_for_each(|l| tx.send(l).ok()));
    rx
}

struct Follow {
    _child: ChildGuard,
    lines: Receiver<String>,
    notes: Receiver<String>,
}

impl Follow {
    fn next(&self) -> String {
        self.lines.recv_timeout(FAILSAFE).expect("no delivery line arrived")
    }

    fn noted(&self, what: &str) {
        loop {
            let note = self.notes.recv_timeout(FAILSAFE).unwrap_or_else(|e| panic!("follow never said {what:?}: {e}"));
            if note.contains(what) {
                return;
            }
        }
    }
}

/// Reads `watching` until `subscriber/concierge` says `state`.
fn until(watching: &mut Subscriber, state: &str) {
    while !serde_json::from_slice::<Value>(&watching.recv().payload).is_ok_and(|v| v["state"] == state) {}
}

fn publisher(name: &str, socket: &Path) -> Publisher {
    let options = Options::new(socket, Arc::new(VirtualClock::new(7_000)), Box::new(SeededRng::new(1)));
    Publisher::with(name, options).unwrap()
}

fn publish(p: &Publisher, topic: &str, value: Value) {
    p.publish(spira_pub::Class::State, topic, &value).unwrap();
    p.flush(FAILSAFE).unwrap();
}

/// The shipped broker config, also logging each client's connect to `log`.
fn logging_broker(dir: &Path) -> (Process, PathBuf, PathBuf) {
    let (socket, log, conf) = (dir.join("s"), dir.join("log"), dir.join("mosquitto.conf"));
    let shipped = include_str!("../../systemd/mosquitto.conf");
    for slot in ["connection_messages false", "log_dest stderr", "@SPIRA_BROKER_SOCKET@"] {
        assert!(shipped.contains(slot), "the shipped config no longer says {slot}");
    }
    let text = shipped
        .replace("@SPIRA_BROKER_SOCKET@", socket.to_str().unwrap())
        .replace("connection_messages false", "connection_messages true\nlog_type notice\nlog_type information")
        .replace("log_dest stderr", &format!("log_dest file {}", log.display()));
    std::fs::write(&conf, text).unwrap();
    (Process::start(dir, &conf), socket, log)
}

#[test_id(ti5vlho)]
#[test]
fn add_and_drop_change_what_arrives_over_one_connection_that_never_reconnects() {
    let dir = TempDir::new("mq-log");
    let (broker, socket, log) = logging_broker(&dir);
    let world = World::new(&socket);
    let run = world.run(&socket);
    let follow = world.follow();
    let bridge = publisher("t", &socket);

    assert_eq!(world.ok(&["add", "release/active"]), "s1");
    assert_eq!(world.ok(&["add", "bead/+/state", "--note", "landed?"]), "s2");
    publish(&bridge, "bead/sp-a/state", json!({"state": "LANDED"}));
    assert_eq!(follow.next(), "[sub:s2] bead/sp-a/state state=LANDED — landed?");

    world.ok(&["drop", "s2"]);
    publish(&bridge, "bead/sp-a/state", json!({"state": "DONE"}));
    // The marker follows the dropped topic from one publisher, so anything for s2 would be first.
    publish(&bridge, "release/active", json!({"release": "r1"}));
    assert_eq!(follow.next(), "[sub:s1] release/active release=r1");

    let screen = world.ok(&["list"]);
    assert!(screen.contains("s1 release/active") && !screen.contains("s2 "), "{screen}");

    drop(run);
    drop(bridge);
    broker.stop_clean();
    let logged = std::fs::read_to_string(&log).unwrap();
    let connects: Vec<&str> = logged.lines().filter(|l| l.contains("New client connected") && l.contains("spira-sub")).collect();
    assert!(connects.len() == 1 && connects[0].contains(" as spira-sub-concierge "), "{logged}");
}

#[test_id(tj49f9m)]
#[test]
fn a_subscription_made_after_a_state_publish_is_handed_the_retained_value_at_once() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r1"}));

    world.ok(&["add", "release/active", "--once", "--note", "activated?"]);

    assert_eq!(follow.next(), "[sub:s1] release/active release=r1 — activated?");
    assert!(world.ok(&["list"]).contains("no subscriptions"), "--once is spent");
}

#[test_id(t0fjsoq)]
#[test]
fn a_filter_matching_no_family_and_an_unknown_id_are_refused_with_exit_2() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let (code, _, err) = world.call(&["add", "nosuch/#"]);
    assert!(code == Some(2) && err.contains("nosuch/#"), "{code:?} {err}");
    let (code, _, err) = world.call(&["list"]);
    assert!(code == Some(1) && err.contains("spira-sub run"), "{code:?} {err}");
    let _run = world.run(broker.socket());
    let (code, _, err) = world.call(&["drop", "s9"]);
    assert!(code == Some(2) && err.contains("s9"), "{code:?} {err}");
}

#[test_id(tezmsxw)]
#[test]
fn a_killed_client_reads_gone_and_a_restarted_one_resubscribes_from_its_set_file() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let run = world.run(broker.socket());
    world.ok(&["add", "release/active", "--note", "kept"]);
    let mut watching = Subscriber::to(broker.socket(), &["subscriber/concierge"]);

    // Keep-alive is 0, so only the closed socket can tell the broker.
    drop(run);
    until(&mut watching, "gone");
    let saved = std::fs::read_to_string(world.set_file()).unwrap();
    assert!(saved.contains("release/active"), "{saved}");

    let _again = world.run(broker.socket());
    let follow = world.follow();
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r2"}));

    assert_eq!(follow.next(), "[sub:s1] release/active release=r2 — kept");
    assert!(!std::fs::read_to_string(world.set_file()).unwrap().contains("r2"), "the set file holds no message");
}

#[test_id(t4yf7hu)]
#[test]
fn a_reattached_follow_and_a_restarted_run_deliver_nothing_already_delivered() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let run = world.run(broker.socket());
    let first = world.follow();
    world.ok(&["add", "release/active"]);
    world.ok(&["add", "gate/#"]);
    let p = publisher("t", broker.socket());
    publish(&p, "release/active", json!({"release": "r1"}));
    assert_eq!(first.next(), "[sub:s1] release/active release=r1");
    drop(first);

    let again = world.follow();
    publish(&p, "gate/r/sp-a", json!({"rc": 0}));
    assert_eq!(again.next(), "[sub:s2] gate/r/sp-a rc=0");

    drop(run);
    again.noted("went away");
    let _restarted = world.run(broker.socket());
    again.noted("following");
    // Acknowledged after the restart's own subscribes, so r1 has reached `run` by now.
    world.ok(&["add", "round/#"]);
    publish(&p, "gate/r/sp-b", json!({"rc": 0}));
    assert_eq!(again.next(), "[sub:s2] gate/r/sp-b rc=0", "the same stream carries on");
}

#[test_id(tb93383)]
#[test]
fn after_a_broker_restart_the_client_reconnects_and_holds_the_same_state_values() {
    let mut broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    let bridge = publisher("t", broker.socket());
    bridge.on_connect(|again| again.publish(spira_pub::Class::State, "release/active", &json!({"release": "r1"})).unwrap());
    world.ok(&["add", "release/active"]);
    world.ok(&["add", "gate/#"]);
    bridge.connect().unwrap();
    assert_eq!(follow.next(), "[sub:s1] release/active release=r1");

    broker.restart();
    until(&mut Subscriber::to(broker.socket(), &["subscriber/concierge"]), "up");
    // The republished value is the one already delivered, so the next line is the new topic.
    let gate = publisher("g", broker.socket());
    publish(&gate, "gate/r/sp-a", json!({"rc": 0}));
    assert_eq!(follow.next(), "[sub:s2] gate/r/sp-a rc=0");

    let newcomer = world.follow();
    publish(&gate, "gate/r/sp-b", json!({"rc": 0}));
    assert_eq!(newcomer.next(), "[sub:s2] gate/r/sp-b rc=0");
}

#[test_id(tp8m1pd)]
#[test]
fn a_topic_cleared_while_run_was_away_is_news_when_it_is_said_again() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let run = world.run(broker.socket());
    let first = world.follow();
    world.ok(&["add", "bead/+/state"]);
    world.ok(&["add", "release/active"]);
    let p = publisher("t", broker.socket());
    for (bead, state) in [("sp-a", "A"), ("sp-b", "B")] {
        publish(&p, &format!("bead/{bead}/state"), json!({"state": state}));
        assert_eq!(first.next(), format!("[sub:s1] bead/{bead}/state state={state}"));
    }
    drop(first);
    let mut watching = Subscriber::to(broker.socket(), &["subscriber/concierge"]);
    drop(run);
    until(&mut watching, "gone");
    let saved = std::fs::read_to_string(world.set_file()).unwrap();
    assert!(saved.contains("bead/sp-a/state"), "positive control: {saved}");
    p.clear("bead/sp-a/state").unwrap();
    p.flush(FAILSAFE).unwrap();

    let _again = world.run(broker.socket());
    // Acknowledged after the restart's `up`, whose echo has therefore reached `run` by now.
    world.ok(&["add", "round/#"]);
    let saved = std::fs::read_to_string(world.set_file()).unwrap();
    assert!(!saved.contains("bead/sp-a/state") && saved.contains("bead/sp-b/state"), "{saved}");
    let follow = world.follow();
    publish(&p, "bead/sp-a/state", json!({"state": "A"}));
    assert_eq!(follow.next(), "[sub:s1] bead/sp-a/state state=A");
    publish(&p, "bead/sp-b/state", json!({"state": "B"}));
    publish(&p, "release/active", json!({"release": "r1"}));
    assert_eq!(follow.next(), "[sub:s2] release/active release=r1");
}

/// Waits out one batch of `topic`, so what is published next falls early in a fresh interval.
fn at_the_start_of_an_interval(follow: &Follow, p: &Publisher, topic: &str, value: Value) {
    publish(p, topic, value);
    assert!(follow.next().starts_with("[batch] n=1 since "));
    follow.next();
}

#[test_id(tbhes68)]
#[test]
fn a_batched_subscription_arrives_as_one_block_behind_what_is_delivered_at_once() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    world.ok(&["add", "bead/+/state", "--batch", "2s"]);
    world.ok(&["add", "release/active"]);
    let p = publisher("t", broker.socket());
    at_the_start_of_an_interval(&follow, &p, "bead/sp-0/state", json!({"state": "WORKING"}));

    for n in 1..=10 {
        publish(&p, "bead/sp-a/state", json!({"state": format!("S{n}")}));
    }
    publish(&p, "release/active", json!({"release": "r1"}));

    assert_eq!(follow.next(), "[sub:s2] release/active release=r1");
    let header = follow.next();
    assert!(header.starts_with("[batch] n=1 since "), "{header}");
    assert_eq!(follow.next(), "[sub:s1] bead/sp-a/state state=S10");
    publish(&p, "release/active", json!({"release": "r2"}));
    assert_eq!(follow.next(), "[sub:s2] release/active release=r2");
}

/// Passes on each write as it was made.
struct Writes(Sender<String>);

impl std::io::Write for Writes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.send(String::from_utf8_lossy(bytes).into_owned()).map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test_id(tuc4a3j)]
#[test]
fn follow_writes_a_whole_batch_in_one_write() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let (writes, written) = channel();
    let (notes, noted) = channel();
    let socket = world.dir.join("c/concierge.sock");
    std::thread::spawn(move || spira_sub::client::follow(&socket, &mut Writes(writes), &mut |note| drop(notes.send(note.to_string()))));
    assert_eq!(noted.recv_timeout(FAILSAFE).unwrap(), "following");
    let p = publisher("t", broker.socket());
    publish(&p, "bead/sp-a/state", json!({"state": "A"}));
    publish(&p, "bead/sp-b/state", json!({"state": "B"}));

    world.ok(&["add", "bead/+/state", "--batch", "1s"]);

    let mut batch = written.recv_timeout(FAILSAFE).unwrap();
    // The two retained values may straddle a boundary and make two batches of one.
    if batch.starts_with("[batch] n=1 ") {
        assert_eq!(batch.lines().count(), 2, "{batch:?}");
        batch = written.recv_timeout(FAILSAFE).unwrap();
        assert!(batch.starts_with("[batch] n=1 ") && batch.lines().count() == 2, "{batch:?}");
    } else {
        assert!(batch.starts_with("[batch] n=2 ") && batch.lines().count() == 3, "{batch:?}");
    }
    assert!(batch.ends_with('\n'), "{batch:?}");
}

#[test_id(tissyg2)]
#[test]
fn set_moves_one_subscription_between_batched_and_at_once_under_the_same_id() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    assert_eq!(world.ok(&["add", "release/active"]), "s1");
    assert_eq!(world.ok(&["set", "s1", "--batch", "2s"]), "");
    let screen = world.ok(&["list"]);
    assert!(screen.contains("s1 release/active") && screen.contains("batch 2s") && !screen.contains("s2 "), "{screen}");
    let p = publisher("t", broker.socket());
    at_the_start_of_an_interval(&follow, &p, "release/active", json!({"release": "r0"}));

    for release in ["r1", "r2", "r3"] {
        publish(&p, "release/active", json!({"release": release}));
    }
    let header = follow.next();
    assert!(header.starts_with("[batch] n=1 since "), "{header}");
    assert_eq!(follow.next(), "[sub:s1] release/active release=r3");

    world.ok(&["set", "s1", "--now"]);
    publish(&p, "release/active", json!({"release": "r4"}));
    assert_eq!(follow.next(), "[sub:s1] release/active release=r4");
    let screen = world.ok(&["list"]);
    assert!(screen.contains("s1 release/active") && !screen.contains("batch") && !screen.contains("s2 "), "{screen}");
}

#[test_id(tjpzv51)]
#[test]
fn a_resident_publisher_going_reaches_its_familys_subscriptions() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    world.ok(&["add", "bead/+/state"]);
    let bridge = publisher("lc-serve", broker.socket());
    publish(&bridge, "bead/sp-a/state", json!({"state": "WORKING"}));
    assert_eq!(follow.next(), "[sub:s1] bead/sp-a/state state=WORKING");

    drop(bridge);

    assert_eq!(follow.next(), "[sub:s1] publisher/lc-serve state=gone");
}

#[test_id(t2x9ze1)]
#[test]
fn an_at_timer_set_in_the_past_arrives_on_follow() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();

    world.ok(&["add", "--at", "2000-01-01T00:00:00Z", "--note", "look again"]);

    assert_eq!(follow.next(), "[sub:s1] at 2000-01-01T00:00:00Z — look again");
}

#[test_id(t32stn8)]
#[test]
fn follow_started_before_any_run_waits_for_it_and_then_streams() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let mut command = world.command(&["follow"]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ChildGuard::spawn(&mut command);
    let (stdout, stderr) = child.pipes();
    let follow = Follow { _child: child, lines: pipe(stdout.unwrap()), notes: pipe(stderr.unwrap()) };
    follow.noted("waiting for it");

    let _run = world.run(broker.socket());
    follow.noted("following");
    world.ok(&["add", "release/active"]);
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r1"}));

    assert_eq!(follow.next(), "[sub:s1] release/active release=r1");
}

#[test_id(tbp2ghh)]
#[test]
fn an_at_timer_ahead_of_now_is_woken_for_and_delivered() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    let soon = strand::timefmt::utc_stamp(i64::try_from(spira_config::vtime::wall_epoch()).unwrap() + 2);

    world.ok(&["add", "--at", &soon, "--note", "now"]);

    assert_eq!(follow.next(), format!("[sub:s1] at {soon} — now"));
    assert!(world.ok(&["list"]).contains("no subscriptions"));
}

#[test_id(tcxfl3k)]
#[test]
fn overlapping_filters_each_deliver_a_chatter_message_once() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    world.ok(&["add", "progress/+"]);
    world.ok(&["add", "progress/#"]);
    world.ok(&["add", "release/active"]);
    let p = publisher("t", broker.socket());

    publish(&p, "progress/sp-a", json!({"note": "compiling"}));
    publish(&p, "release/active", json!({"release": "r1"}));

    let seen: Vec<String> = std::iter::repeat_with(|| follow.next()).take_while(|l| !l.contains("release/active")).collect();
    assert_eq!(seen, ["[sub:s1] progress/sp-a note=compiling", "[sub:s2] progress/sp-a note=compiling"]);
}

#[test_id(tgjz3s7)]
#[test]
fn deliveries_subscriptions_and_control_commands_reach_the_metrics_store() {
    let broker = Broker::up();
    let sink = testkit::otlp::Sink::start();
    let world = World::pushing_to(&sink.addr, broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    world.ok(&["add", "release/active"]);
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r1"}));
    follow.next();

    sink.expect(&["spira_sub_deliveries", "spira_sub_subscriptions", "spira_sub_control_commands"], FAILSAFE);
}

#[test_id(tsbnmw0)]
#[test]
fn a_change_the_set_file_cannot_hold_fails_and_says_what_is_live() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    // A directory where the set file goes: no write can replace it.
    std::fs::create_dir_all(world.set_file().join("in-the-way")).unwrap();

    let (code, _, err) = world.call(&["add", "release/active"]);
    assert!(code == Some(1) && err.contains("s1 is live but not saved"), "{code:?} {err}");
    let (code, _, err) = world.call(&["add", "--at", "2999-01-01T00:00:00Z", "--note", "n"]);
    assert!(code == Some(1) && err.contains("s2 is set but not saved"), "{code:?} {err}");
    let (code, _, err) = world.call(&["drop", "s1"]);
    assert!(code == Some(1) && err.contains("s1 is dropped but not saved"), "{code:?} {err}");
}
