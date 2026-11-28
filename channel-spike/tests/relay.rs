// covers: channel-spike/src/lib.rs
//! A real broker and a real `spira-sub run`, relayed to a fake MCP client over a pipe pair.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use ports::{SeededRng, VirtualClock};
use serde_json::{json, Value};
use spira_pub::{Class, Options, Publisher};
use spira_sub::protocol::{Reply, Request};
use spira_test_id::test_id;
use testkit::broker::{Broker, Subscriber};
use testkit::{ChildGuard, TempDir};

/// Bounds each wait on a pipe or a child so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const NAME: &str = "wake-spike";

struct World {
    dir: TempDir,
    env: Vec<(&'static str, String)>,
}

impl World {
    fn new(broker: &Path) -> World {
        let dir = TempDir::new("chan");
        let control = dir.join("c").display().to_string();
        let declare = [("SPIRA_BROKER_SOCKET", broker.to_str().unwrap()), ("SPIRA_SUB_CONTROL_DIR", control.as_str())];
        let mut env = spira_config::process::fixture_metrics_env(&dir, "", &declare);
        env.push(("SPIRA_LC_BIN", stand_in_store(&dir)));
        World { dir, env }
    }

    fn socket(&self) -> PathBuf {
        spira_sub::run::socket_path(&self.dir.join("c"), NAME)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(testkit::bin_exe("spira-sub"));
        command.args(args).args(["--as", NAME]).env_clear().envs(self.env.clone()).stdin(Stdio::null());
        command
    }

    fn ok(&self, args: &[&str]) {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// `run`, returned once its subscriber topic reads up.
    fn run(&self, broker: &Path) -> ChildGuard {
        let mut watching = Subscriber::to(broker, &[&format!("subscriber/{NAME}")]);
        let run = ChildGuard::spawn(&mut self.command(&["run"]));
        while !serde_json::from_slice::<Value>(&watching.recv().payload).is_ok_and(|v| v["state"] == "up") {}
        run
    }

    /// A plain `follow`, returned once it says it is attached.
    fn follow(&self) -> (ChildGuard, Receiver<String>) {
        let mut command = self.command(&["follow"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = ChildGuard::spawn(&mut command);
        let (stdout, stderr) = child.pipes();
        let notes = lines(stderr.unwrap());
        while !notes.recv_timeout(FAILSAFE).expect("follow never attached").contains("following") {}
        (child, lines(stdout.unwrap()))
    }

    /// Returns once `run` counts `want` readers, or once the relay has returned and so will
    /// never be one; no delivery is replayed to a reader that attaches after it.
    fn readers(&self, want: usize, relay_gone: impl Fn() -> bool + Send + 'static) {
        let (socket, want) = (self.socket(), format!("readers {want}"));
        let (counted, count) = channel();
        std::thread::spawn(move || loop {
            let screen = match spira_sub::client::call(&socket, &Request::List) {
                Ok(Reply::Done { out, .. }) => out,
                other => panic!("list: {other:?}"),
            };
            if screen.contains(&want) || relay_gone() {
                return counted.send(());
            }
            std::thread::yield_now();
        });
        count.recv_timeout(FAILSAFE).expect("no notification arrived: the relay attached no follow");
    }
}

/// A `spira-lc` whose `topics` answers a record naming the one family the test publishes.
fn stand_in_store(dir: &Path) -> String {
    let record = dir.join("record");
    std::fs::write(&record, format!("{}\n", json!({"topics": [{"family": "bead/+/state", "resident": []}], "next": null}))).unwrap();
    let bin = dir.join("spira-lc");
    testkit::write_exe(&bin, &format!("#!/bin/sh\nread -r answer < '{}'\nprintf '%s\\n' \"$answer\"\n", record.display()));
    bin.display().to_string()
}

fn lines(from: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || BufReader::new(from).lines().map_while(Result::ok).try_for_each(|l| tx.send(l).ok()));
    rx
}

#[test_id(tc9n2i6)]
#[test]
fn a_delivery_reaches_the_mcp_client_as_one_channel_notification_with_the_follow_line() {
    let broker = Broker::up();
    let world = World::new(broker.socket());
    let _run = world.run(broker.socket());
    let (_follow, followed) = world.follow();
    let (relay_in, mut client_out) = std::io::pipe().unwrap();
    let (client_in, relay_out) = std::io::pipe().unwrap();
    let socket = world.socket();
    let relay = Arc::new(std::thread::spawn(move || channel_spike::relay(&socket, BufReader::new(relay_in), relay_out)));
    let from_relay = lines(client_in);
    let next = || -> Value {
        let line = from_relay.recv_timeout(FAILSAFE).expect("no notification arrived");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON on the client's stdout: {e}: {line:?}"))
    };

    let initialize = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}}});
    writeln!(client_out, "{initialize}").unwrap();
    let reply = next();
    assert_eq!(reply["id"], 0, "{reply}");
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25", "{reply}");
    assert_eq!(reply["result"]["capabilities"]["experimental"]["claude/channel"], json!({}), "{reply}");
    // A relay that has already gone is judged by the missing notification, not by this write.
    let _ = writeln!(client_out, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let relaying = relay.clone();
    world.readers(2, move || relaying.is_finished());
    world.ok(&["add", "bead/+/state", "--note", "n"]);
    let options = Options::new(broker.socket(), Arc::new(VirtualClock::new(7_000)), Box::new(SeededRng::new(1)));
    let publisher = Publisher::with("t", options).unwrap();
    publisher.publish(Class::State, "bead/sp-a/state", &json!({"state": "LANDED"})).unwrap();
    publisher.flush(FAILSAFE).unwrap();

    let notification = next();
    let printed = followed.recv_timeout(FAILSAFE).expect("the plain follow printed no line");
    assert_eq!(notification["jsonrpc"], "2.0", "{notification}");
    assert_eq!(notification["method"], "notifications/claude/channel", "{notification}");
    assert_eq!(notification.get("id"), None, "{notification}");
    assert_eq!(notification["params"]["content"], printed.as_str(), "{notification}");
    assert_eq!(notification["params"]["meta"], json!({"seq": "1"}), "{notification}");
    for key in notification["params"]["meta"].as_object().unwrap().keys() {
        assert!(!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "meta key {key:?}");
    }
}
