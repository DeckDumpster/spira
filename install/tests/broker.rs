// covers: systemd/mosquitto.conf install/src/broker.rs testkit/src/broker.rs
//! The rendered broker config against the real `mosquitto` on PATH.
#[macro_use]
extern crate spira_test_id;

use install::values::HostValues;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use testkit::broker::Process;

/// Bounds each wait on the broker so a broken one fails the test instead of hanging it.
const FAILSAFE: Duration = Duration::from_secs(30);

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

    let broker = Process::start(&dir, &conf);
    assert_eq!(connect_with_keepalive_zero(&socket), [0x20, 0x02, 0x00, 0x00], "CONNACK, return code 0");
    broker.stop_clean();
    assert!(!dir.join("mosquitto.db").exists(), "persistence is off");

    // Positive control: with persistence on, this same stop writes the database here.
    let persisting = std::fs::read_to_string(&conf).unwrap().replace("persistence false", "persistence true");
    std::fs::write(&conf, persisting).unwrap();
    Process::start(&dir, &conf).stop_clean();
    assert!(dir.join("mosquitto.db").exists(), "the check above could have found a database");
}

#[test_id(tn4c89y)]
#[test]
fn a_broker_that_exits_before_ready_fails_the_start_at_once_naming_its_exit() {
    let dir = testkit::TempDir::new("mq");
    let (conf, _) = placed_config(&dir);

    // Positive control: the unmodified config starts, so the failure below is the directive's.
    Process::start(&dir, &conf).stop_clean();

    let mut refused = std::fs::read_to_string(&conf).unwrap();
    refused.push_str("\nno_such_option true\n");
    std::fs::write(&conf, refused).unwrap();
    let panic = std::panic::catch_unwind(|| drop(Process::start(&dir, &conf))).expect_err("mosquitto refuses the directive");
    let message = panic.downcast_ref::<String>().expect("a formatted panic message");
    assert!(message.contains("exited before it reported ready: exit status:"), "{message}");
}
