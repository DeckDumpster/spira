// covers: spira-sub/src/run.rs spira-sub/src/client.rs spira-sub/src/main.rs
//! The built `spira-sub` against a real broker: `run`, the commands and `follow`.
#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use ports::{SeededRng, VirtualClock};
use rumqttc::{Client, Connection, Event, MqttOptions, Packet, QoS, Transport};
use serde_json::{json, Value};
use spira_pub::{Options, Publisher};
use testkit::broker::{Broker, Process};
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
        let dir = TempDir::new("sub");
        let control = dir.join("c").display().to_string();
        let declare = [("SPIRA_BROKER_SOCKET", broker.to_str().unwrap()), ("SPIRA_SUB_CONTROL_DIR", control.as_str())];
        let env = spira_config::process::fixture_metrics_env(&dir, "", &declare);
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
        let mut watching = Raw::subscribe(broker, "subscriber/concierge");
        let run = ChildGuard::spawn(&mut self.command(&["run", "--as", "concierge"]));
        watching.until(|v| v["state"] == "up");
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

/// A subscriber that owes nothing to what is under test.
struct Raw {
    _client: Client,
    connection: Connection,
}

impl Raw {
    fn subscribe(socket: &Path, filter: &str) -> Raw {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = format!("raw-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut options = MqttOptions::new(id, socket.to_str().unwrap(), 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(Duration::ZERO);
        let (client, connection) = Client::new(options, 32);
        client.subscribe(filter, QoS::AtLeastOnce).unwrap();
        let mut raw = Raw { _client: client, connection };
        while !matches!(raw.event(), Event::Incoming(Packet::SubAck(_))) {}
        raw
    }

    fn event(&mut self) -> Event {
        match self.connection.recv_timeout(FAILSAFE) {
            Ok(Ok(event)) => event,
            other => panic!("the watching client heard nothing: {other:?}"),
        }
    }

    fn until(&mut self, want: impl Fn(&Value) -> bool) {
        loop {
            if let Event::Incoming(Packet::Publish(p)) = self.event() {
                if serde_json::from_slice(&p.payload).is_ok_and(|v| want(&v)) {
                    return;
                }
            }
        }
    }
}

fn publisher(name: &str, socket: &Path) -> Publisher {
    let options = Options::new(socket, Arc::new(VirtualClock::new(7_000)), Box::new(SeededRng::new(1)));
    Publisher::with(name, options).unwrap()
}

fn publish(p: &Publisher, topic: &str, value: Value) {
    p.publish(topic, &value).unwrap();
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
    let mut watching = Raw::subscribe(broker.socket(), "subscriber/concierge");

    // Keep-alive is 0, so only the closed socket can tell the broker.
    drop(run);
    watching.until(|v| v["state"] == "gone");
    let saved = std::fs::read_to_string(world.set_file()).unwrap();
    assert!(saved.contains("release/active"), "{saved}");

    let _again = world.run(broker.socket());
    let follow = world.follow();
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r2"}));

    assert_eq!(follow.next(), "[sub:s1] release/active release=r2 — kept");
    assert!(!std::fs::read_to_string(world.set_file()).unwrap().contains("r2"), "the set file holds no message");
}

#[test_id(t2mtm0z)]
#[test]
fn a_reattached_follow_is_replayed_the_current_values_and_waits_out_a_restart() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let run = world.run(broker.socket());
    let first = world.follow();
    world.ok(&["add", "release/active"]);
    publish(&publisher("t", broker.socket()), "release/active", json!({"release": "r1"}));
    assert_eq!(first.next(), "[sub:s1] release/active release=r1");
    drop(first);

    let again = world.follow();
    assert_eq!(again.next(), "[sub:s1] release/active release=r1");

    drop(run);
    again.noted("went away");
    let _restarted = world.run(broker.socket());
    again.noted("following");
    assert_eq!(again.next(), "[sub:s1] release/active release=r1", "the same stream carries on");
}

#[test_id(tb93383)]
#[test]
fn after_a_broker_restart_the_client_reconnects_and_holds_the_same_state_values() {
    let mut broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let follow = world.follow();
    let bridge = publisher("t", broker.socket());
    bridge.on_connect(|again| again.publish("release/active", &json!({"release": "r1"})).unwrap());
    world.ok(&["add", "release/active"]);
    world.ok(&["add", "gate/#"]);
    bridge.connect().unwrap();
    assert_eq!(follow.next(), "[sub:s1] release/active release=r1");

    broker.restart();
    Raw::subscribe(broker.socket(), "subscriber/concierge").until(|v| v["state"] == "up");
    // The republished value is the one already delivered, so the next line is the new topic.
    let gate = publisher("g", broker.socket());
    publish(&gate, "gate/r/sp-a", json!({"rc": 0}));
    assert_eq!(follow.next(), "[sub:s2] gate/r/sp-a rc=0");

    let replayed = world.follow();
    assert_eq!([replayed.next(), replayed.next()], ["[sub:s1] release/active release=r1", "[sub:s2] gate/r/sp-a rc=0"]);
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
