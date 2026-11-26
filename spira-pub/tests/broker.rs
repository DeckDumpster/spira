// covers: spira-pub/src/publisher.rs spira-pub/src/socket_watch.rs testkit/src/broker.rs
//! The library and the CLI against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ports::{SeededRng, VirtualClock};
use rumqttc::{Client, Connection, Event, MqttOptions, Packet, Publish, QoS, Transport};
use serde_json::{json, Value};
use spira_pub::{Class, Options, PubError, Publisher};
use testkit::broker::{Broker, Subscriber};

/// Bounds each wait on the broker or a child so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const NOW_MS: u64 = 7_000;
const CHILD_SOCKET: &str = "SPIRA_PUB_TEST_CHILD_SOCKET";
const CHILD_UP: &str = "child-publisher-is-up";

fn next_value(subscriber: &mut Subscriber) -> (String, Value, bool) {
    let p = subscriber.next();
    (p.topic, serde_json::from_slice(&p.payload).unwrap(), p.retain)
}

/// What a subscriber arriving now is handed for `topic`.
fn retained(socket: &Path, topic: &str) -> Value {
    let (got, value, retain) = next_value(&mut Subscriber::subscribe(socket, &[topic]));
    assert!(retain && got == topic, "not a retained {topic}: {got} {value} retain={retain}");
    value
}

#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<String>>>);

impl Logged {
    fn text(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

fn options(socket: &Path, log: &Logged) -> Options {
    let mut o = Options::new(socket, Arc::new(VirtualClock::new(NOW_MS)), Box::new(SeededRng::new(1)));
    let lines = log.0.clone();
    o.log = Arc::new(move |line| lines.lock().unwrap().push(line.to_string()));
    o
}

fn publisher(name: &str, socket: &Path) -> Publisher {
    Publisher::with(name, options(socket, &Logged::default())).unwrap()
}

#[test_id(tulqtgh)]
#[test]
fn a_published_state_is_retained_for_a_later_subscriber_and_chatter_is_not() {
    let broker = Broker::up();
    let p = publisher("t", broker.socket());

    p.publish("progress/sp-a", &json!({"note": "n"})).unwrap();
    p.publish("release/active", &json!({"release": "r1"})).unwrap();
    p.flush(FAILSAFE).unwrap();

    assert_eq!(retained(broker.socket(), "release/active"), json!({"release": "r1"}));
    // Retained messages arrive in subscription order, so a retained progress would be first.
    let (topic, ..) = next_value(&mut Subscriber::subscribe(broker.socket(), &["progress/sp-a", "release/active"]));
    assert_eq!(topic, "release/active", "chatter was retained");
}

#[test_id(tri0yt7)]
#[test]
fn a_publisher_is_up_while_it_lives_and_gone_once_dropped_having_delivered_what_it_queued() {
    let broker = Broker::up();
    let p = publisher("t", broker.socket());
    p.connect().unwrap();
    p.flush(FAILSAFE).unwrap();
    assert_eq!(retained(broker.socket(), "publisher/t"), json!({"state": "up", "since_ms": NOW_MS}));

    p.publish("release/active", &json!({"release": "last words"})).unwrap();
    drop(p);

    assert_eq!(retained(broker.socket(), "publisher/t"), json!({"state": "gone"}));
    assert_eq!(retained(broker.socket(), "release/active"), json!({"release": "last words"}));
}

/// Not a test of its own: the process the next one kills. Without the socket it does nothing.
#[test_id(t9d9osw)]
#[test]
fn child_process_holding_a_publisher() {
    let Some(socket) = std::env::var_os(CHILD_SOCKET) else { return };
    let p = publisher("t", Path::new(&socket));
    p.connect().unwrap();
    p.flush(FAILSAFE).unwrap();
    println!("\n{CHILD_UP}");
    // Ends only when killed, or when the parent is gone and its end of the pipe closes.
    let _ = std::io::stdin().read_line(&mut String::new());
}

#[test_id(t8bvnwj)]
#[test]
fn a_killed_publishers_will_says_gone_with_no_keep_alive_to_wait_out() {
    let broker = Broker::up();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "--nocapture", "--test-threads=1"])
        .arg(child_test_name())
        .env(CHILD_SOCKET, broker.socket())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let up = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l == CHILD_UP);
    assert!(up, "the child never connected");

    let mut watching = Subscriber::subscribe(broker.socket(), &["publisher/t"]);
    assert_eq!(next_value(&mut watching).1["state"], "up");
    child.kill().unwrap();
    child.wait().unwrap();

    // Keep-alive is 0, so the broker has no timer for this client: only its socket says so.
    assert_eq!(next_value(&mut watching).1, json!({"state": "gone"}));
    assert_eq!(retained(broker.socket(), "publisher/t"), json!({"state": "gone"}));
}

/// The child's name as libtest filters on it, test id included.
fn child_test_name() -> String {
    let me = Command::new(std::env::current_exe().unwrap()).arg("--list").output().unwrap();
    String::from_utf8(me.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .find(|l| l.starts_with("child_process_holding_a_publisher"))
        .expect("the child test is listed")
        .to_string()
}

#[test_id(tyghoi5)]
#[test]
fn after_a_broker_restart_on_connect_republishes_and_an_idle_publisher_needs_no_publish() {
    let mut broker = Broker::up();
    let p = publisher("t", broker.socket());
    p.on_connect(|again| again.publish("release/active", &json!({"release": "r1"})).unwrap());
    p.connect().unwrap();
    p.flush(FAILSAFE).unwrap();
    let before = retained(broker.socket(), "release/active");

    broker.restart();

    // The new broker holds nothing, so whatever arrives was republished. Wait for it live or
    // retained, then read it as a fresh subscriber does.
    assert_eq!(next_value(&mut Subscriber::subscribe(broker.socket(), &["release/active"])).1, before);
    assert_eq!(retained(broker.socket(), "release/active"), before);
    assert_eq!(next_value(&mut Subscriber::subscribe(broker.socket(), &["publisher/t"])).1["state"], "up");
}

#[test_id(tbvwg01)]
#[test]
fn with_the_broker_stopped_publish_fails_is_logged_and_the_caller_carries_on() {
    let mut broker = Broker::up();
    let log = Logged::default();
    let p = Publisher::with("t", options(broker.socket(), &log)).unwrap();
    p.publish("release/active", &json!({"release": "r1"})).unwrap();
    p.flush(FAILSAFE).unwrap();

    broker.stop();
    assert!(p.wait_disconnected(FAILSAFE), "the client never saw the broker go");
    let sent = p.publish("release/active", &json!({"release": "r2"}));
    let carried_on = true;

    // Unreachable, not Timeout: the refused connect failed it, not the deadline.
    assert!(matches!(sent, Err(PubError::Unreachable { .. })), "{sent:?}");
    assert!(carried_on);
    assert!(log.text().contains("publish failed (release/active)"), "{}", log.text());
}

#[test_id(t9sumn5)]
#[test]
fn a_broker_that_never_answers_the_connect_fails_the_publish() {
    let dir = testkit::TempDir::new("mute");
    let socket = dir.join("s");
    let _mute = UnixListener::bind(&socket).unwrap();

    let sent = publisher("t", &socket).publish("release/active", &json!({}));

    assert!(matches!(sent, Err(PubError::Timeout | PubError::Unreachable { .. })), "{sent:?}");
}

#[test_id(t2wjlqm)]
#[test]
fn a_full_queue_and_a_refused_topic_fail_at_once_and_are_logged() {
    let broker = Broker::up();
    let log = Logged::default();
    let mut o = options(broker.socket(), &log);
    o.queue = 0;
    let p = Publisher::with("t", o).unwrap();

    let full = p.publish("release/active", &json!({}));
    let undeclared = p.publish("nosuch/topic", &json!({}));
    let wrong = p.publish_as_class(Class::Chatter, "release/active", &json!({}));

    assert!(matches!(full, Err(PubError::QueueFull { .. })), "{full:?}");
    assert!(matches!(undeclared, Err(PubError::UndeclaredFamily { .. })), "{undeclared:?}");
    assert!(matches!(wrong, Err(PubError::WrongClass { .. })), "{wrong:?}");
    assert!(log.text().contains("nosuch/topic"), "{}", log.text());
}

#[test_id(tegnae3)]
#[test]
fn publishes_failures_and_queue_depth_reach_the_metrics_store() {
    let broker = Broker::up();
    let sink = testkit::otlp::Sink::start();
    let mut o = options(broker.socket(), &Logged::default());
    o.metrics = Some(spira_otel::metrics::MetricsSink::for_component(Some(&sink.addr), "spira-pub"));
    let p = Publisher::with("t", o).unwrap();

    p.publish("release/active", &json!({})).unwrap();
    p.publish("nosuch/topic", &json!({})).unwrap_err();
    drop(p);

    sink.expect(&["spira_pub_publishes", "spira_pub_failures", "spira_pub_queue_depth", "undeclared_family"], FAILSAFE);
}

/// The built CLI, configured only by a fixture declaring `socket` as the broker's.
fn cli(dir: &Path, socket: &Path, args: &[&str], stdin: &str) -> (Option<i32>, String) {
    cli_pushing_to("", dir, socket, args, stdin)
}

fn cli_pushing_to(endpoint: &str, dir: &Path, socket: &Path, args: &[&str], stdin: &str) -> (Option<i32>, String) {
    let env = spira_config::process::fixture_metrics_env(dir, endpoint, &[("SPIRA_BROKER_SOCKET", socket.to_str().unwrap())]);
    let mut child = Command::new(testkit::bin_exe("spira-pub"))
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A CLI that refused its arguments may be gone before it reads this.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    let out = child.wait_with_output().unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test_id(tsb2ucc)]
#[test]
fn the_cli_publishes_stdin_to_the_configured_broker_and_exits_by_what_happened() {
    let mut broker = Broker::up();
    let dir = testkit::TempDir::new("cli");
    let send = |topic: &str, stdin: &str| cli(&dir, broker.socket(), &["--as", "queue", "--topic", topic], stdin);

    let (code, err) = send("release/active", r#"{"release":"r9"}"#);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(retained(broker.socket(), "release/active"), json!({"release": "r9"}));
    assert_eq!(retained(broker.socket(), "publisher/queue"), json!({"state": "gone"}));

    let (code, err) = send("nosuch/topic", "{}");
    assert!(code == Some(2) && err.contains("nosuch/topic"), "{code:?} {err}");
    let (code, err) = cli(&dir, broker.socket(), &["--as", "q", "--topic", "release/active", "--class", "chatter"], "{}");
    assert!(code == Some(2) && err.contains("declared state"), "{code:?} {err}");
    let (code, err) = send("release/active", "not json");
    assert!(code == Some(2) && err.contains("not JSON"), "{code:?} {err}");

    broker.stop();
    let (code, err) = cli(&dir, broker.socket(), &["--as", "queue", "--topic", "release/active"], "{}");
    assert!(code == Some(1) && err.contains("unreachable"), "{code:?} {err}");
}

#[test_id(tvdsteq)]
#[test]
fn the_configured_endpoint_receives_the_clis_run_and_the_librarys_series() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("cli-otel");
    let sink = testkit::otlp::Sink::start();

    let (code, err) = cli_pushing_to(&sink.addr, &dir, broker.socket(), &["--as", "queue", "--topic", "release/active"], "{}");

    assert_eq!(code, Some(0), "{err}");
    sink.expect(&["spira_pub_publishes", "spira_pub_queue_depth", "spira_component_runs"], FAILSAFE);
}
