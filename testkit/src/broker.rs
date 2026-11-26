//! A throwaway MQTT broker: the PATH `mosquitto`, running the shipped config on a socket in its
//! own [`TempDir`]. A missing `mosquitto` panics; a test that needs one never skips.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rumqttc::{Client, Connection, Event, MqttOptions, Packet, Publish, QoS, Transport};

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
        let mut subscriber = Subscriber::open(&self.socket);
        subscriber.client.publish(END_TOPIC, QoS::AtLeastOnce, true, "end").expect("queue the end marker");
        // The broker sends retained values in the order the filters were subscribed, so the
        // end marker follows everything `filter` holds.
        subscriber.add(&[filter, END_TOPIC]);
        let mut held = Vec::new();
        loop {
            let p = subscriber.next();
            if p.topic == END_TOPIC {
                return held;
            }
            held.push((p.topic, String::from_utf8_lossy(&p.payload).into_owned()));
        }
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

/// A subscriber that owes nothing to the library under test.
pub struct Subscriber {
    client: Client,
    connection: Connection,
}

impl Subscriber {
    fn open(socket: &Path) -> Subscriber {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = format!("testkit-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let mut options = MqttOptions::new(id, socket.to_str().expect("a UTF-8 socket path"), 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(Duration::ZERO);
        let (client, connection) = Client::new(options, 32);
        Subscriber { client, connection }
    }

    /// Subscribes to `filters`, in order.
    pub fn subscribe(socket: &Path, filters: &[&str]) -> Subscriber {
        let subscriber = Subscriber::open(socket);
        subscriber.add(filters);
        subscriber
    }

    fn add(&self, filters: &[&str]) {
        for f in filters {
            self.client.subscribe(*f, QoS::AtLeastOnce).expect("queue a subscription");
        }
    }

    /// The next message delivered.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Publish {
        loop {
            match self.connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => return p,
                Ok(Ok(_)) => {}
                other => panic!("no message arrived: {other:?}"),
            }
        }
    }
}
