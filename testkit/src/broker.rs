//! A throwaway MQTT broker: the PATH `mosquitto`, running the shipped config on a socket in its
//! own [`TempDir`]. A missing `mosquitto` panics; a test that needs one never skips.

use std::collections::VecDeque;
use std::io::ErrorKind;
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
/// How often a wait on the broker looks at its process.
const POLL: Duration = Duration::from_millis(50);
/// Marks the end of what [`Broker::retained`] reads.
const END_TOPIC: &str = "testkit/end";

/// One `mosquitto` process running a config the caller placed; killed when dropped.
pub struct Process(ChildGuard);

impl Process {
    /// Starts it in `dir` and returns once it reports ready (`sd_notify`), by which time its
    /// listener accepts. Panics as soon as it exits without reporting, naming its exit status;
    /// its own stderr is inherited, so the reason is in the test's output.
    pub fn start(dir: &Path, conf: &Path) -> Process {
        let notify_path = dir.join("n");
        let _ = std::fs::remove_file(&notify_path);
        let notify = UnixDatagram::bind(&notify_path).expect("bind the readiness socket");
        notify.set_read_timeout(Some(POLL)).expect("bound each readiness read");
        let mut command = Command::new("mosquitto");
        command.arg("-c").arg(conf).current_dir(dir).env("NOTIFY_SOCKET", &notify_path).stdin(Stdio::null());
        let mut process = Process(ChildGuard::spawn(&mut command));
        let mut buf = [0u8; 64];
        let deadline = std::time::Instant::now() + FAILSAFE;
        let n = loop {
            let error = match notify.recv(&mut buf) {
                Ok(n) => break n,
                Err(error) => error,
            };
            if let Some(status) = (process.0).0.try_wait().expect("poll mosquitto") {
                panic!("mosquitto exited before it reported ready: {status}");
            }
            let timed_out = matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut);
            assert!(timed_out && std::time::Instant::now() < deadline, "mosquitto never reported ready: {error}");
        };
        assert_eq!(&buf[..n], b"READY=1", "mosquitto's first notification");
        process
    }

    /// SIGTERM and a clean exit, which is when a persisting broker writes its database.
    ///
    /// The signal is repeated until it exits: mosquitto reports ready before its main loop sets
    /// its run flag, so a SIGTERM landing in between is overwritten and lost.
    pub fn stop_clean(mut self) {
        let deadline = std::time::Instant::now() + FAILSAFE;
        let status = loop {
            if let Some(status) = (self.0).0.try_wait().expect("poll mosquitto") {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "mosquitto ignored SIGTERM for {FAILSAFE:?}");
            // SAFETY: a plain signal(2) to a child this value has not reaped, so the pid is its own.
            assert_eq!(unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) }, 0, "signal mosquitto");
            std::thread::sleep(POLL);
        };
        assert!(status.success(), "mosquitto did not exit cleanly: {status}");
    }
}

/// A subscriber that owes nothing to the library that publishes.
pub struct Subscriber {
    link: Link,
    early: VecDeque<Delivered>,
}

enum Link {
    V3(rumqttc::Client, Box<rumqttc::Connection>),
    /// The client is held because dropping it ends the connection.
    V5(#[allow(dead_code)] rumqttc::v5::Client, Box<rumqttc::v5::Connection>),
}

/// One delivered message.
#[derive(Debug)]
pub struct Delivered {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: bool,
    /// Empty over MQTT 3.1.1, which has none.
    pub user_properties: Vec<(String, String)>,
}

impl From<rumqttc::Publish> for Delivered {
    fn from(p: rumqttc::Publish) -> Delivered {
        Delivered { topic: p.topic, payload: p.payload.to_vec(), retain: p.retain, user_properties: Vec::new() }
    }
}

impl From<rumqttc::v5::mqttbytes::v5::Publish> for Delivered {
    fn from(p: rumqttc::v5::mqttbytes::v5::Publish) -> Delivered {
        let topic = String::from_utf8_lossy(&p.topic).into_owned();
        let user_properties = p.properties.map(|p| p.user_properties).unwrap_or_default();
        Delivered { topic, payload: p.payload.to_vec(), retain: p.retain, user_properties }
    }
}

fn client_id() -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!("sub-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

impl Subscriber {
    /// Subscribes to `filters` over MQTT 3.1.1 and returns once the broker has acknowledged
    /// them all, so nothing published afterwards is missed.
    pub fn to(socket: &Path, filters: &[&str]) -> Subscriber {
        use rumqttc::{Client, Event, MqttOptions, Packet, QoS, Transport};
        let mut options = MqttOptions::new(client_id(), socket.to_str().expect("a UTF-8 socket path"), 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(Duration::ZERO);
        let (client, mut connection) = Client::new(options, 64);
        for f in filters {
            client.subscribe(*f, QoS::AtLeastOnce).expect("queue the subscription");
        }
        let mut acked = 0;
        let mut early = VecDeque::new();
        while acked < filters.len() {
            match connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => acked += 1,
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => early.push_back(p.into()),
                Ok(Ok(_)) => {}
                other => panic!("the broker never acknowledged the subscription: {other:?}"),
            }
        }
        Subscriber { link: Link::V3(client, Box::new(connection)), early }
    }

    /// [`to`](Subscriber::to) over MQTT 5, so each message arrives with its user properties.
    pub fn to_v5(socket: &Path, filters: &[&str]) -> Subscriber {
        use rumqttc::v5::mqttbytes::v5::Packet;
        use rumqttc::v5::mqttbytes::QoS;
        use rumqttc::v5::{Client, Event, MqttOptions};
        let mut options = MqttOptions::new(client_id(), socket.to_str().expect("a UTF-8 socket path"), 0);
        options.set_transport(rumqttc::Transport::Unix);
        let (client, mut connection) = Client::new(options, 64);
        for f in filters {
            client.subscribe(*f, QoS::AtLeastOnce).expect("queue the subscription");
        }
        let mut acked = 0;
        let mut early = VecDeque::new();
        while acked < filters.len() {
            match connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => acked += 1,
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => early.push_back(p.into()),
                Ok(Ok(_)) => {}
                other => panic!("the broker never acknowledged the subscription: {other:?}"),
            }
        }
        Subscriber { link: Link::V5(client, Box::new(connection)), early }
    }

    /// The next message; panics when none arrives.
    pub fn recv(&mut self) -> Delivered {
        use rumqttc::v5::mqttbytes::v5::Packet as Packet5;
        use rumqttc::v5::Event as Event5;
        use rumqttc::{Event, Packet};
        if let Some(delivered) = self.early.pop_front() {
            return delivered;
        }
        loop {
            match &mut self.link {
                Link::V3(_, connection) => match connection.recv_timeout(FAILSAFE) {
                    Ok(Ok(Event::Incoming(Packet::Publish(p)))) => return p.into(),
                    Ok(Ok(_)) => {}
                    other => panic!("no message arrived: {other:?}"),
                },
                Link::V5(_, connection) => match connection.recv_timeout(FAILSAFE) {
                    Ok(Ok(Event5::Incoming(Packet5::Publish(p)))) => return p.into(),
                    Ok(Ok(_)) => {}
                    other => panic!("no message arrived: {other:?}"),
                },
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

    /// What a subscriber arriving now is handed for `filter`: each retained `(topic, payload)`.
    pub fn retained(&self, filter: &str) -> Vec<(String, String)> {
        use rumqttc::QoS;
        let mut subscriber = Subscriber::to(&self.socket, &[filter]);
        let Link::V3(client, _) = &subscriber.link else { unreachable!("Subscriber::to speaks 3.1.1") };
        // The broker works through one connection in order, so what `filter` holds is sent
        // ahead of a marker published and subscribed to after its acknowledgement.
        client.publish(END_TOPIC, QoS::AtLeastOnce, true, "end").expect("queue the end marker");
        client.subscribe(END_TOPIC, QoS::AtLeastOnce).expect("queue the end subscription");
        let mut held = Vec::new();
        loop {
            let got = subscriber.recv();
            if got.topic == END_TOPIC {
                return held;
            }
            held.push((got.topic, String::from_utf8_lossy(&got.payload).into_owned()));
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
