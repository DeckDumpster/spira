// covers: systemd/mosquitto.conf install/src/broker.rs
//! The rendered broker config against the real `mosquitto` on PATH.
#[macro_use]
extern crate spira_test_id;

use install::values::HostValues;
use std::io::{Read, Write};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Bounds each wait on the broker so a broken one fails the test instead of hanging it.
const FAILSAFE: Duration = Duration::from_secs(30);

struct Broker(Child);

impl Broker {
    /// Starts the broker in `dir` and returns once it reports ready (`sd_notify`), by which time
    /// its listener accepts.
    fn start(dir: &Path, conf: &Path) -> Broker {
        let notify_path = dir.join("n");
        let _ = std::fs::remove_file(&notify_path);
        let notify = UnixDatagram::bind(&notify_path).unwrap();
        notify.set_read_timeout(Some(FAILSAFE)).unwrap();
        let child = Command::new("mosquitto")
            .arg("-c")
            .arg(conf)
            .current_dir(dir)
            .env("NOTIFY_SOCKET", &notify_path)
            .stdin(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start mosquitto from PATH, which this test requires: {e}"));
        let broker = Broker(child);
        let mut buf = [0u8; 64];
        let n = notify.recv(&mut buf).expect("mosquitto never reported ready");
        assert_eq!(&buf[..n], b"READY=1");
        broker
    }

    /// A clean stop, which is when a persisting broker writes its database.
    fn stop(mut self) {
        assert!(Command::new("kill").arg(self.0.id().to_string()).status().unwrap().success());
        assert!(self.0.wait().unwrap().success(), "mosquitto did not exit cleanly");
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An MQTT 3.1.1 CONNECT with a clean session and keep-alive 0; returns the 4-byte CONNACK.
fn connect_with_keepalive_zero(socket: &Path) -> [u8; 4] {
    let client_id = b"probe";
    let mut body = vec![0, 4, b'M', b'Q', b'T', b'T', 4, 0x02, 0, 0];
    body.extend((client_id.len() as u16).to_be_bytes());
    body.extend(client_id);
    let mut packet = vec![0x10, body.len() as u8];
    packet.extend(body);
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(FAILSAFE)).unwrap();
    stream.write_all(&packet).unwrap();
    let mut connack = [0u8; 4];
    stream.read_exact(&mut connack).unwrap();
    connack
}

/// The shipped template, placed as install places it, into `dir`.
fn placed_config(dir: &Path) -> (PathBuf, PathBuf) {
    let socket = dir.join("s");
    let host = HostValues { run: dir.display().to_string(), broker_socket: socket.display().to_string(), ..Default::default() };
    let templates = spira_config::resolve::checkout_root_for_process().unwrap().join("systemd");
    assert!(install::broker::place_conf(&templates, &host).unwrap(), "a first placement writes the file");
    (dir.join(install::broker::CONF_FILE), socket)
}

#[test_id(t3naf6o)]
#[test]
fn a_keepalive_zero_client_gets_connack_and_the_broker_leaves_no_database() {
    let dir = testkit::TempDir::new("mq");
    let (conf, socket) = placed_config(&dir);

    let broker = Broker::start(&dir, &conf);
    assert_eq!(connect_with_keepalive_zero(&socket), [0x20, 0x02, 0x00, 0x00], "CONNACK, return code 0");
    broker.stop();
    assert!(!dir.join("mosquitto.db").exists(), "persistence is off");

    // Positive control: with persistence on, this same stop writes the database here.
    let persisting = std::fs::read_to_string(&conf).unwrap().replace("persistence false", "persistence true");
    std::fs::write(&conf, persisting).unwrap();
    Broker::start(&dir, &conf).stop();
    assert!(dir.join("mosquitto.db").exists(), "the check above could have found a database");
}
