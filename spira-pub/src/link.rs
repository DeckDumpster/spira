//! What every long-lived client of the broker shares: how one connection is made, and how a
//! thread sleeps between brokers.

use std::io::ErrorKind;
use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use rumqttc::v5;
use rumqttc::Transport;

use crate::{CONNECT_DEADLINE, MAX_PAYLOAD};

const MAX_PACKET: usize = MAX_PAYLOAD + (4 << 10);
/// rumqttc's MQTT 5 client has no way to turn keep-alive off; this is the longest the wire holds.
const V5_KEEP_ALIVE: Duration = Duration::from_secs(u16::MAX as u64);
/// Between connects while a broker that has just made its socket is not yet listening on it.
pub const LISTEN_PAUSE: Duration = Duration::from_millis(5);

/// Why a connection was not made or ended: the failure's text, and whether nothing was listening.
pub struct Lost {
    pub why: String,
    pub refused: bool,
}

impl Lost {
    fn from_v5(e: v5::ConnectionError) -> Lost {
        let refused = matches!(&e, v5::ConnectionError::Io(io) if nothing_listening(io));
        Lost { why: e.to_string(), refused }
    }
}

/// Whether a connect's I/O error says nothing was listening on the socket.
pub fn nothing_listening(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
}

/// `attempt`, handed the time left of [`CONNECT_DEADLINE`]. `announced` says the broker's socket
/// has just appeared, so a failure `refused` accepts is waited out until the deadline.
pub fn wait_out_refusal<T, E>(announced: bool, mut attempt: impl FnMut(Duration) -> Result<T, E>, refused: impl Fn(&E) -> bool) -> Result<T, E> {
    // Real time, not an injected clock: this bounds socket I/O, which virtual time never ends.
    let started = Instant::now();
    loop {
        match attempt(CONNECT_DEADLINE.saturating_sub(started.elapsed())) {
            Err(e) if announced && refused(&e) && started.elapsed() < CONNECT_DEADLINE => std::thread::sleep(LISTEN_PAUSE),
            other => return other,
        }
    }
}

/// Who is connecting.
pub struct Dial<'a> {
    pub client_id: &'a str,
    pub socket: &'a Path,
    /// The retained topic the broker sets to gone when this client's socket closes.
    pub will_topic: String,
    /// Requests the client holds ahead of the connection.
    pub capacity: usize,
}

impl Dial<'_> {
    fn path(&self) -> Result<&str, Lost> {
        self.socket.to_str().ok_or_else(|| Lost { why: format!("socket path {} is not UTF-8", self.socket.display()), refused: false })
    }

    fn once_v5(&self, within: Duration, will_properties: &[(String, String)]) -> Result<(v5::Client, v5::Connection), Lost> {
        use v5::mqttbytes::v5::{LastWill, LastWillProperties, Packet};
        let mut options = v5::MqttOptions::new(self.client_id, self.path()?, 0);
        options.set_transport(Transport::Unix);
        options.set_keep_alive(V5_KEEP_ALIVE);
        options.set_max_packet_size(Some(MAX_PACKET as u32));
        options.set_connection_timeout(CONNECT_DEADLINE.as_secs());
        let properties = LastWillProperties {
            delay_interval: None,
            payload_format_indicator: None,
            message_expiry_interval: None,
            content_type: None,
            response_topic: None,
            correlation_data: None,
            user_properties: will_properties.to_vec(),
        };
        let will = LastWill::new(self.will_topic.clone(), gone().to_string(), v5::mqttbytes::QoS::AtLeastOnce, true, Some(properties));
        options.set_last_will(will);
        let (client, mut connection) = v5::Client::new(options, self.capacity);
        match connection.recv_timeout(within) {
            Ok(Ok(v5::Event::Incoming(Packet::ConnAck(_)))) => Ok((client, connection)),
            Ok(Ok(other)) => Err(Lost { why: format!("expected CONNACK, got {other:?}"), refused: false }),
            Ok(Err(e)) => Err(Lost::from_v5(e)),
            Err(e) => Err(Lost { why: format!("no CONNACK: {e:?}"), refused: false }),
        }
    }

    /// A connection over MQTT 5 within [`CONNECT_DEADLINE`], with `will_properties` as the last
    /// will's user properties. `announced` says the broker's socket has just appeared, so a
    /// refusal is waited out until the deadline.
    pub fn connect_v5(&self, announced: bool, will_properties: &[(String, String)]) -> Result<(v5::Client, v5::Connection), Lost> {
        wait_out_refusal(announced, |within| self.once_v5(within, will_properties), |lost| lost.refused)
    }
}

/// The value of a publisher or subscriber that is not there.
pub fn gone() -> serde_json::Value {
    serde_json::json!({"state": "gone"})
}

/// Sleeps until one of `fds` is readable or `timeout` passes; with no timeout, for as long as it
/// takes. `None` is ignored. An interrupted call only returns early, and a caller looks at its
/// state again.
pub fn sleep_until_readable(fds: [Option<RawFd>; 2], timeout: Option<Duration>) {
    // poll(2) ignores a negative descriptor.
    let mut fds = fds.map(|fd| libc::pollfd { fd: fd.unwrap_or(-1), events: libc::POLLIN, revents: 0 });
    let ms = timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
    // SAFETY: `fds` is a live array of the length passed.
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
}
