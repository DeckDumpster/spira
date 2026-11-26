//! A throwaway MQTT broker: the PATH `mosquitto`, running the shipped config on a socket in its
//! own [`TempDir`]. A missing `mosquitto` panics; a test that needs one never skips.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::{ChildGuard, TempDir};

/// The config install renders for `spira-broker.service`.
const TEMPLATE: &str = include_str!("../../systemd/mosquitto.conf");
const SOCKET_SLOT: &str = "@SPIRA_BROKER_SOCKET@";
/// Bounds each wait on the broker, so a broken one fails the test instead of hanging it.
const FAILSAFE: Duration = Duration::from_secs(30);
/// Marks the end of what [`Broker::retained`] reads.
const END_TOPIC: &str = "testkit/end";

/// One `mosquitto` process running a config the caller placed; killed when dropped.
pub struct Process(ChildGuard);

impl Process {
    /// Starts it in `dir` and returns once it reports ready (`sd_notify`), by which time its
    /// listener accepts.
    pub fn start(dir: &Path, conf: &Path) -> Process {
        let notify_path = dir.join("n");
        let _ = std::fs::remove_file(&notify_path);
        let notify = UnixDatagram::bind(&notify_path).expect("bind the readiness socket");
        notify.set_read_timeout(Some(FAILSAFE)).expect("bound the readiness wait");
        let mut command = Command::new("mosquitto");
        command.arg("-c").arg(conf).current_dir(dir).env("NOTIFY_SOCKET", &notify_path).stdin(Stdio::null());
        let process = Process(ChildGuard::spawn(&mut command));
        let mut buf = [0u8; 64];
        let n = notify.recv(&mut buf).expect("mosquitto never reported ready");
        assert_eq!(&buf[..n], b"READY=1", "mosquitto's first notification");
        process
    }

    /// SIGTERM and a clean exit, which is when a persisting broker writes its database.
    pub fn stop_clean(mut self) {
        // SAFETY: a plain signal(2) to a child this value has not reaped, so the pid is its own.
        assert_eq!(unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) }, 0, "signal mosquitto");
        assert!((self.0).0.wait().expect("wait for mosquitto").success(), "mosquitto did not exit cleanly");
    }
}

pub struct Broker {
    child: Option<Process>,
    socket: PathBuf,
    dir: TempDir,
}

impl Broker {
    /// A running broker: its listener accepts by the time this returns.
    pub fn up() -> Broker {
        let dir = TempDir::new("mq");
        let socket = dir.join("s");
        assert!(TEMPLATE.contains(SOCKET_SLOT), "the shipped broker config names no {SOCKET_SLOT}");
        std::fs::write(dir.join("mosquitto.conf"), TEMPLATE.replace(SOCKET_SLOT, &socket.display().to_string()))
            .expect("write the broker config");
        let mut broker = Broker { child: None, socket, dir };
        broker.start();
        broker
    }

    /// The socket it listens on, the same one across [`restart`](Broker::restart).
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// What a subscriber arriving now is handed for `filter`: each retained `(topic, payload)`.
    pub fn retained(&self, filter: &str) -> Vec<(String, String)> {
        let mut publish = Command::new("mosquitto_pub");
        publish.arg("--unix").arg(&self.socket).args(["-t", END_TOPIC, "-m", "end", "-r", "-q", "1"]);
        assert!(publish.status().expect("run mosquitto_pub").success(), "mosquitto_pub failed");
        // The broker sends retained values in the order of the subscription's filters, so
        // the end marker follows everything `filter` holds.
        let mut subscribe = Command::new("mosquitto_sub");
        subscribe.arg("--unix").arg(&self.socket).args(["-v", "-q", "1", "-t", filter, "-t", END_TOPIC]);
        subscribe.args(["-W", &FAILSAFE.as_secs().to_string()]).stdin(Stdio::null()).stdout(Stdio::piped());
        let mut child = ChildGuard::spawn(&mut subscribe);
        let out = BufReader::new((child.0).stdout.take().expect("piped stdout"));
        let mut held = Vec::new();
        for line in out.lines() {
            let line = line.expect("read mosquitto_sub");
            let (topic, payload) = line.split_once(' ').expect("mosquitto_sub -v prints topic then payload");
            if topic == END_TOPIC {
                return held;
            }
            held.push((topic.to_string(), payload.to_string()));
        }
        panic!("mosquitto_sub ended before the end marker");
    }

    fn start(&mut self) {
        assert!(self.child.is_none(), "the broker is already running");
        self.child = Some(Process::start(&self.dir, &self.dir.join("mosquitto.conf")));
    }

    /// Kills the broker, which keeps nothing: every retained value is gone with it.
    pub fn stop(&mut self) {
        self.child = None;
    }

    /// A new broker process on the same socket, holding no state.
    pub fn restart(&mut self) {
        self.stop();
        self.start();
    }
}
