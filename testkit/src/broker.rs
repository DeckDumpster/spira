//! A throwaway MQTT broker: the PATH `mosquitto`, running the shipped config on a socket in its
//! own [`TempDir`]. A missing `mosquitto` panics; a test that needs one never skips.

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

/// A subscriber that owes nothing to the library that publishes.
pub struct Subscriber {
    _client: rumqttc::Client,
    connection: rumqttc::Connection,
}

/// One delivered message.
#[derive(Debug)]
pub struct Delivered {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: bool,
}

impl Subscriber {
    /// Subscribes to `filters` and returns once the broker has acknowledged them all, so
    /// nothing published afterwards is missed.
    pub fn to(socket: &Path, filters: &[&str]) -> Subscriber {
        use rumqttc::{Client, Event, MqttOptions, Packet, QoS, Transport};
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = format!("sub-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut options = MqttOptions::new(id, socket.to_str().expect("a UTF-8 socket path"), 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(Duration::ZERO);
        let (client, mut connection) = Client::new(options, 64);
        for f in filters {
            client.subscribe(*f, QoS::AtLeastOnce).expect("queue the subscription");
        }
        let mut acked = 0;
        while acked < filters.len() {
            match connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => acked += 1,
                // A retained value ahead of a later acknowledgement would be lost here.
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => panic!("delivered before the subscriptions stood: {}", p.topic),
                Ok(Ok(_)) => {}
                other => panic!("the broker never acknowledged the subscription: {other:?}"),
            }
        }
        Subscriber { _client: client, connection }
    }

    /// The next message; panics when none arrives.
    pub fn next(&mut self) -> Delivered {
        use rumqttc::{Event, Packet};
        loop {
            match self.connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                    return Delivered { topic: p.topic, payload: p.payload.to_vec(), retain: p.retain }
                }
                Ok(Ok(_)) => {}
                other => panic!("no message arrived: {other:?}"),
            }
        }
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
