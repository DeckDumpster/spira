// covers: spira-lc/src/control/topic_tap.rs spira-lc/src/control/verbs.rs spira-lc/src/serve.rs
//! `spira-lc serve`'s topic tap against a real broker. A running server, because what is under
//! test is the process's own subscription and what it leaves in the store across a restart.

#[macro_use]
extern crate spira_test_id;

use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::broker::Broker;

/// Bounds each wait on the server or the broker, so a broken one fails instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const PINNED_NOW: i64 = 1_800_000_000;
const SUBSCRIBED: &str = "topic tap subscribed";

/// The directory a server and its restart share.
struct Home(testkit::TempDir);

impl Home {
    fn new() -> Home {
        let dir = testkit::TempDir::new("spira-lc-topic-tap");
        std::fs::create_dir_all(dir.join("run")).unwrap();
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        drop(SqliteBackend::open(&dir.join("control.db"), sqlite()).unwrap());
        Home(dir)
    }
}

fn sqlite() -> Options {
    Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }
}

struct Server {
    child: Child,
    socket: PathBuf,
    stderr: Receiver<String>,
    said: RefCell<Vec<String>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// A server on `home`'s store whose clock is pinned to `now`.
    fn start(home: &Home, broker: &Broker, now: i64) -> Server {
        let dir: &Path = home.0.path();
        let socket = dir.join("lc.sock");
        let _ = std::fs::remove_file(&socket);
        let (d, r, s, sock) =
            (dir.display().to_string(), dir.join("run").display().to_string(), dir.join("control.db").display().to_string(), socket.display().to_string());
        let toml = spira_config::process::fixture_toml(
            dir,
            &[
                ("SPIRA_WORKSPACES", &d),
                ("SPIRA_RUN", &r),
                ("SPIRA_LC_STORE_PATH", &s),
                ("SPIRA_LC_SOCKET", &sock),
                ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"),
                ("SPIRA_BROKER_SOCKET", broker.socket().to_str().unwrap()),
                ("SPIRA_METRICS_ENDPOINT", ""),
            ],
        );
        let harness = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let mut child = Command::new(testkit::bin_exe("spira-lc"))
            .arg("serve")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &toml)
            .env("SPIRA_HOME", &harness)
            .env("SPIRA_NOW", now.to_string())
            .env("SPIRA_RELEASE", dir.join("release"))
            .env("HOME", dir.join("home"))
            .stderr(Stdio::piped())
            .spawn()
            .expect("spira-lc serve runs");
        let lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let (tx, stderr) = channel();
        std::thread::spawn(move || lines.map_while(Result::ok).for_each(|l| drop(tx.send(l))));
        let server = Server { child, socket, stderr, said: RefCell::default() };
        server.wait_said(" ready after ", 1);
        server
    }

    /// Waits until `times` lines of the server's stderr have held `needle`.
    fn wait_said(&self, needle: &str, times: usize) {
        let mut said = self.said.borrow_mut();
        while said.iter().filter(|l| l.contains(needle)).count() < times {
            match self.stderr.recv_timeout(FAILSAFE) {
                Ok(line) => said.push(line),
                Err(e) => panic!("the server never said {needle:?} {times} time(s) ({e}): {}", said.join("\n")),
            }
        }
    }

    fn call(&self, words: &[&str]) -> (i32, String) {
        let argv: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        let reply = spira_store::lc_call::exchange_once(spira_store::lc_call::connect(&self.socket).unwrap(), &argv, None).expect("serve answers");
        (reply.code, reply.text())
    }

    /// One `topics` answer.
    fn topics(&self, flags: &[&str]) -> Value {
        let (code, out) = self.call(&[&["topics"], flags].concat());
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    /// The list once `enough` holds of it, read again at each write the store's own `watch`
    /// reports under `topics/`.
    fn listed(&self, enough: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let mut from = 0;
        loop {
            let rows = self.topics(&[])["topics"].as_array().unwrap().clone();
            if enough(&rows) {
                return rows;
            }
            let wait = FAILSAFE.as_millis().to_string();
            let (code, out) = self.call(&["watch", "topics/", "--from-rev", &from.to_string(), "--wait-ms", &wait]);
            assert_eq!(code, 0, "{out}");
            let woke: Value = serde_json::from_str(&out).unwrap();
            // `list_rev` answers a revision already compacted away; the list is read again after it.
            let next = woke["through"].as_u64().or_else(|| woke["list_rev"].as_u64());
            from = next.unwrap_or_else(|| panic!("no write brought what was waited for ({out}): {rows:?}"));
        }
    }
}

fn row<'a>(rows: &'a [Value], family: &str) -> Option<&'a Value> {
    let mut found = rows.iter().filter(|r| r["family"] == family);
    let first = found.next();
    assert!(found.next().is_none(), "two rows for {family}: {rows:?}");
    first
}

/// A producer that owes nothing to spira-pub: MQTT 3.1.1, no user properties.
fn publish(broker: &Broker, messages: &[(&str, bool)]) {
    use rumqttc::{Client, Event, MqttOptions, Packet, QoS, Transport};
    let mut options = MqttOptions::new("bypass", broker.socket().to_str().unwrap(), 0);
    options.set_transport(Transport::Unix);
    options.set_keep_alive(Duration::ZERO);
    let (client, mut connection) = Client::new(options, 64);
    for (topic, retain) in messages {
        client.publish(*topic, QoS::AtLeastOnce, *retain, "{}").unwrap();
    }
    let mut acked = 0;
    while acked < messages.len() {
        match connection.recv_timeout(FAILSAFE) {
            Ok(Ok(Event::Incoming(Packet::PubAck(_)))) => acked += 1,
            Ok(Ok(_)) => {}
            other => panic!("the broker never acknowledged: {other:?}"),
        }
    }
}

/// A producer that stamps its name as an MQTT 5 user property.
fn publish_stamped(broker: &Broker, topic: &str, properties: &[(&str, &str)]) {
    use rumqttc::v5::mqttbytes::v5::{Packet, PublishProperties};
    use rumqttc::v5::mqttbytes::QoS;
    use rumqttc::v5::{Client, Event, MqttOptions};
    let mut options = MqttOptions::new("stamped", broker.socket().to_str().unwrap(), 0);
    options.set_transport(rumqttc::Transport::Unix);
    let (client, mut connection) = Client::new(options, 64);
    let user_properties = properties.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let stamp = PublishProperties { user_properties, ..PublishProperties::default() };
    client.publish_with_properties(topic.to_string(), QoS::AtLeastOnce, false, "{}", stamp).unwrap();
    loop {
        match connection.recv_timeout(FAILSAFE) {
            Ok(Ok(Event::Incoming(Packet::PubAck(_)))) => return,
            Ok(Ok(_)) => {}
            other => panic!("the broker never acknowledged: {other:?}"),
        }
    }
}

#[test_id(t41n6s3)]
#[test]
fn live_publishes_are_listed_once_per_family_and_survive_a_restart() {
    let broker = Broker::up();
    publish(&broker, &[("heldbefore/sp-x", true)]);
    let home = Home::new();
    let server = Server::start(&home, &broker, PINNED_NOW);
    assert_eq!(server.topics(&["--limit", "10000"])["next"], Value::Null);
    server.wait_said(SUBSCRIBED, 1);

    publish(&broker, &[("bead/sp-a/state", true), ("bead/sp-b/state", true), ("nosuch/sp-c", false)]);
    let rows = server.listed(|r| row(r, "bead/+/state").is_some() && row(r, "nosuch/+").is_some());
    for (family, retained) in [("bead/+/state", true), ("nosuch/+", false)] {
        let got = row(&rows, family).unwrap();
        assert_eq!((got["retained"].as_bool(), got["mixed"].as_bool()), (Some(retained), Some(false)), "{got}");
        assert!(got["first_seen"].as_u64().unwrap() <= got["last_seen"].as_u64().unwrap(), "{got}");
        assert_eq!(got["first_seen"], json!(PINNED_NOW * 1_000), "{got}");
        assert_eq!((&got["publishers"], &got["resident"]), (&json!([]), &json!([])), "{got}");
    }
    assert!(row(&rows, "heldbefore/+").is_none(), "a value retained before the tap subscribed was counted: {rows:?}");

    publish(&broker, &[("nosuch/sp-c", true)]);
    let rows = server.listed(|r| row(r, "nosuch/+").is_some_and(|n| n["mixed"] == true));
    assert_eq!(row(&rows, "nosuch/+").unwrap()["retained"], true);

    let mut walked = Vec::new();
    let mut page = server.topics(&["--limit", "1"]);
    loop {
        assert_eq!(page["topics"].as_array().unwrap().len(), 1, "{page}");
        walked.push(page["topics"][0]["family"].as_str().unwrap().to_string());
        let Some(next) = page["next"].as_str() else { break };
        assert_eq!(next, walked.last().unwrap());
        page = server.topics(&["--limit", "1", "--after", next]);
    }
    let whole: Vec<String> = server.topics(&[])["topics"].as_array().unwrap().iter().map(|r| r["family"].as_str().unwrap().to_string()).collect();
    assert_eq!(walked, whole);
    assert!(walked.len() >= 2, "{walked:?}");

    drop(server);
    let server = Server::start(&home, &broker, PINNED_NOW + 100);
    let kept = server.topics(&[])["topics"].as_array().unwrap().clone();
    assert_eq!(row(&kept, "bead/+/state"), row(&rows, "bead/+/state"));
    server.wait_said(SUBSCRIBED, 1);
    publish(&broker, &[("nosuch/sp-d", false)]);
    let rows = server.listed(|r| row(r, "nosuch/+").is_some_and(|n| n["last_seen"] == json!((PINNED_NOW + 100) * 1_000)));
    let got = row(&rows, "nosuch/+").unwrap();
    assert_eq!((&got["first_seen"], &got["mixed"], &got["retained"]), (&json!(PINNED_NOW * 1_000), &json!(true), &json!(false)), "{got}");
}

#[test_id(tj49bx3)]
#[test]
fn the_bridges_publishes_and_stamped_names_are_recorded_across_a_broker_restart() {
    let mut broker = Broker::up();
    let home = Home::new();
    let server = Server::start(&home, &broker, PINNED_NOW);
    assert_eq!(server.topics(&[])["next"], Value::Null);
    server.wait_said(SUBSCRIBED, 1);

    let (code, out) = server.call(&["create-bead", "sp-tap", "--ready"]);
    assert_eq!(code, 0, "{out}");
    let claim = json!({"Claim": {"holder": "dwarf-1", "lease_until": PINNED_NOW + 3_600, "persona": "builder"}}).to_string();
    let (code, out) = server.call(&["event", "bead", "sp-tap", "--expect", "READY", "--version", "0", "--actor", "dwarf-1", "--kind", &claim]);
    assert_eq!(code, 0, "{out}");
    let rows = server.listed(|r| row(r, "bead/+/state").is_some() && row(r, "bead/+/sessions").is_some());
    assert_eq!(row(&rows, "bead/+/sessions").unwrap()["retained"], true);

    broker.restart();
    server.wait_said(SUBSCRIBED, 2);
    publish_stamped(&broker, "stamped/sp-a", &[("spira-publisher", "zeta")]);
    publish_stamped(&broker, "stamped/sp-b", &[("spira-publisher", "alpha"), ("spira-resident", "true")]);
    let rows = server.listed(|r| row(r, "stamped/+").is_some_and(|s| s["publishers"] == json!(["alpha", "zeta"])));
    assert_eq!(row(&rows, "stamped/+").unwrap()["resident"], json!(["alpha"]));
}
