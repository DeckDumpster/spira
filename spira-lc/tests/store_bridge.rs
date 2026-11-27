// covers: spira-lc/src/control/bridge.rs spira-lc/src/control/store.rs spira-lc/src/serve.rs
//! `spira-lc serve`'s store bridge against a real broker. A running server, because what is
//! under test is the process's own publisher, its reconnect and its stderr.

#[macro_use]
extern crate spira_test_id;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use lifecycle::bead::{BeadState, LifecycleRow};
use runtime::ports::VirtualClock;
use serde_json::{json, Value};
use state_store::{Config, Contract, Object, Ports, Request, Txn};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::broker::{Broker, Subscriber};

/// Bounds each wait on the server or the broker, so a broken one fails instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const PINNED_NOW: i64 = 1_800_000_000;
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const PUBLISHER: &str = "publisher/lc-serve";

struct Server {
    child: Child,
    socket: PathBuf,
    stderr: Receiver<String>,
    _dir: testkit::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// A server on a new store, which `seed` writes to first.
    fn start(broker: &Broker, metrics: &str, seed: impl FnOnce(&Path)) -> Server {
        let dir = testkit::TempDir::new("spira-lc-store-bridge");
        let run_dir = dir.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let store = dir.join("control.db");
        drop(SqliteBackend::open(&store, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap());
        seed(&store);
        let socket = dir.join("lc.sock");
        let (d, r, s, sock) = (dir.display().to_string(), run_dir.display().to_string(), store.display().to_string(), socket.display().to_string());
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[
                ("SPIRA_WORKSPACES", &d),
                ("SPIRA_RUN", &r),
                ("SPIRA_LC_STORE_PATH", &s),
                ("SPIRA_LC_SOCKET", &sock),
                ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"),
                ("SPIRA_BROKER_SOCKET", broker.socket().to_str().unwrap()),
                ("SPIRA_METRICS_ENDPOINT", metrics),
            ],
        );
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let mut child = Command::new(testkit::bin_exe("spira-lc"))
            .arg("serve")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &toml)
            .env("SPIRA_HOME", &home)
            .env("SPIRA_NOW", PINNED_NOW.to_string())
            .env("SPIRA_RELEASE", dir.join("release"))
            .env("HOME", dir.join("home"))
            .stderr(Stdio::piped())
            .spawn()
            .expect("spira-lc serve runs");
        let lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let (tx, stderr) = channel();
        std::thread::spawn(move || lines.map_while(Result::ok).for_each(|l| drop(tx.send(l))));
        let server = Server { child, socket, stderr, _dir: dir };
        server.wait_line(" ready after ");
        server
    }

    fn wait_line(&self, needle: &str) -> String {
        let mut said = Vec::new();
        loop {
            match self.stderr.recv_timeout(FAILSAFE) {
                Ok(line) if line.contains(needle) => return line,
                Ok(line) => said.push(line),
                Err(e) => panic!("the server never said {needle:?} ({e}): {}", said.join("\n")),
            }
        }
    }

    fn call(&self, words: &[&str]) -> (i32, String) {
        let argv: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        let reply = spira_store::lc_call::exchange_once(spira_store::lc_call::connect(&self.socket).unwrap(), &argv, None).expect("serve answers");
        (reply.code, reply.text())
    }

    fn create(&self, id: &str) {
        let (code, out) = self.call(&["create-bead", id, "--ready"]);
        assert_eq!(code, 0, "{out}");
    }

    fn event(&self, id: &str, expect: &str, version: u64, actor: &str, kind: Value) -> (i32, String) {
        let (version, kind) = (version.to_string(), kind.to_string());
        self.call(&["event", "bead", id, "--expect", expect, "--version", &version, "--actor", actor, "--kind", &kind])
    }

    /// A builder's claim of a READY bead born at version 0.
    fn claim(&self, id: &str) {
        let kind = json!({"Claim": {"holder": "dwarf-1", "lease_until": PINNED_NOW + 3_600, "persona": "builder"}});
        let (code, out) = self.event(id, "READY", 0, "dwarf-1", kind);
        assert_eq!(code, 0, "{out}");
    }
}

/// A DROPPED record, written straight to the store: the verb that drops a bead reads the
/// content store, which this fixture has none of.
fn seed_dropped(store: &Path, id: &str) {
    let backend = SqliteBackend::open(store, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
    let ports = Ports::virtual_clock(&VirtualClock::new(0));
    let contract = Contract::open(backend, ports, Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let mut row = LifecycleRow::filed(id);
    row.state = BeadState::Dropped;
    let mut record = serde_json::to_value(row).unwrap();
    record["updated_at"] = json!(PINNED_NOW);
    let put = Txn::new().put(&format!("lifecycle/{id}"), Object::new(record));
    assert!(contract.txn(&put, &Request::new("seed", "test", "seed")).unwrap().succeeded);
}

fn next(sub: &mut Subscriber) -> (String, Value) {
    let d = sub.recv();
    (d.topic, serde_json::from_slice(&d.payload).unwrap())
}

/// The latest value of each topic, read until every one of `wanted` has arrived.
fn until(sub: &mut Subscriber, wanted: &[&str]) -> BTreeMap<String, Value> {
    let mut got = BTreeMap::new();
    while !wanted.iter().all(|w| got.contains_key(*w)) {
        let (topic, value) = next(sub);
        got.insert(topic, value);
    }
    got
}

/// A server whose bridge the broker has seen come up.
fn bridged(broker: &Broker) -> Server {
    watched(Server::start(broker, "", |_| {}), broker)
}

fn watched(server: Server, broker: &Broker) -> Server {
    wait_up(broker);
    server
}

fn wait_up(broker: &Broker) {
    let mut watch = Subscriber::to(broker.socket(), &[PUBLISHER]);
    while next(&mut watch).1["state"] != "up" {}
}

#[test_id(tbdarbf)]
#[test]
fn a_claim_is_published_as_the_beads_state_and_its_session_count() {
    let broker = Broker::up();
    let server = bridged(&broker);
    let mut watching = Subscriber::to(broker.socket(), &["bead/+/state", "bead/+/sessions"]);
    server.create("sp-bra");
    server.claim("sp-bra");

    let got = until(&mut watching, &["bead/sp-bra/state", "bead/sp-bra/sessions"]);
    assert_eq!(got["bead/sp-bra/state"], json!({"state": "WORKING", "from": "READY", "actor": "dwarf-1", "at": PINNED_NOW}));
    assert_eq!(got["bead/sp-bra/sessions"], json!({"sessions": 1, "holder": "dwarf-1", "at": PINNED_NOW}));

    let mut late = Subscriber::to(broker.socket(), &["bead/sp-bra/state"]);
    assert_eq!(next(&mut late).1["state"], "WORKING", "the state is retained for a later subscriber");
}

#[test_id(tnchd8g)]
#[test]
fn a_refused_transition_publishes_nothing() {
    let broker = Broker::up();
    let server = bridged(&broker);
    server.create("sp-bra");
    server.create("sp-brb");
    let mut watching = Subscriber::to(broker.socket(), &["bead/+/state", "bead/+/sessions"]);
    let (code, out) = server.event("sp-bra", "READY", 0, "reaper", json!("HolderDead"));
    assert_eq!(code, 3, "the event is refused: {out}");
    server.claim("sp-brb");

    // One thread publishes in commit order, so anything of the refusal would come first.
    assert_eq!(next(&mut watching).0, "bead/sp-brb/state");
    assert_eq!(next(&mut watching).0, "bead/sp-brb/sessions");
}

#[test_id(t3o6rgu)]
#[test]
fn a_restarted_broker_is_handed_every_open_beads_state_again_and_no_terminal_one() {
    let mut broker = Broker::up();
    let sink = testkit::otlp::Sink::start();
    let server = watched(Server::start(&broker, &sink.addr, |store| seed_dropped(store, "sp-brgone")), &broker);
    let mut before = Subscriber::to(broker.socket(), &["bead/+/state", "bead/+/sessions"]);
    server.create("sp-bropen");
    server.claim("sp-bropen");
    let (code, out) = server.event("sp-bropen", "WORKING", 1, "dwarf-1", json!({"Submit": {"tip": TIP}}));
    assert_eq!(code, 0, "{out}");
    let mut held = until(&mut before, &["bead/sp-bropen/state", "bead/sp-bropen/sessions"]);
    while held["bead/sp-bropen/state"]["state"] != "SUBMITTED" {
        let (topic, value) = next(&mut before);
        held.insert(topic, value);
    }
    let (code, out) = server.call(&["show", "sp-brgone"]);
    assert!(code == 0 && out.contains("DROPPED"), "the terminal record is there to be left out: {code} {out}");

    broker.restart();
    let mut after = Subscriber::to(broker.socket(), &["bead/+/state", "bead/+/sessions"]);
    let mut again = until(&mut after, &["bead/sp-bropen/state", "bead/sp-bropen/sessions"]);
    // A later transition is published behind the whole republish, so it marks its end.
    server.create("sp-brlast");
    server.claim("sp-brlast");
    again.extend(until(&mut after, &["bead/sp-brlast/state"]));

    assert_eq!(again["bead/sp-bropen/state"], held["bead/sp-bropen/state"]);
    assert_eq!(again["bead/sp-bropen/sessions"], held["bead/sp-bropen/sessions"]);
    assert!(!again.contains_key("bead/sp-brgone/state"), "a terminal bead was republished: {again:?}");

    let mut late = Subscriber::to(broker.socket(), &["bead/sp-bropen/state"]);
    assert_eq!(next(&mut late).1, held["bead/sp-bropen/state"]);
    sink.expect(&["spira_lc_republished", "spira_lc_republish_seconds"], FAILSAFE);
}

#[test_id(t6vqm9u)]
#[test]
fn a_transition_with_the_broker_down_is_applied_and_the_failed_publish_is_logged() {
    let mut broker = Broker::up();
    let server = bridged(&broker);
    server.create("sp-bra");
    broker.stop();
    server.wait_line("lost the broker");

    server.claim("sp-bra");

    let line = server.wait_line("publish failed (bead/sp-bra/state)");
    assert!(line.contains("spira-pub[lc-serve]"), "{line}");
    let (code, out) = server.call(&["show", "sp-bra"]);
    assert!(code == 0 && out.contains("WORKING"), "{code} {out}");
}
