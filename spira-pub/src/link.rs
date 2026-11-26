//! What every long-lived client of the broker shares: how one connection is made, and how a
//! thread sleeps between brokers.

use std::io::ErrorKind;
use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use rumqttc::{Client, Connection, ConnectionError, Event, LastWill, MqttOptions, NetworkOptions, Packet, QoS, Transport};

use crate::{CONNECT_DEADLINE, MAX_PAYLOAD};

const MAX_PACKET: usize = MAX_PAYLOAD + (4 << 10);
/// Between connects while a broker that has just made its socket is not yet listening on it.
pub const LISTEN_PAUSE: Duration = Duration::from_millis(5);

/// Why a connection was not made or ended: the failure's text, and whether nothing was listening.
pub struct Lost {
    pub why: String,
    pub refused: bool,
}

impl Lost {
    fn from(e: ConnectionError) -> Lost {
        let refused = matches!(&e, ConnectionError::Io(io) if matches!(io.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound));
        Lost { why: e.to_string(), refused }
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
    fn once(&self, within: Duration) -> Result<(Client, Connection), Lost> {
        let socket = self.socket.to_str().ok_or_else(|| Lost { why: format!("socket path {} is not UTF-8", self.socket.display()), refused: false })?;
        let mut options = MqttOptions::new(self.client_id, socket, 0);
        options.set_transport(Transport::Unix);
        // Liveness is the socket: a dead client's closes, and the broker publishes its will.
        options.set_keep_alive(Duration::ZERO);
        options.set_max_packet_size(MAX_PACKET, MAX_PACKET);
        options.set_last_will(LastWill::new(self.will_topic.clone(), gone().to_string(), QoS::AtLeastOnce, true));
        let (client, mut connection) = Client::new(options, self.capacity);
        let mut network = NetworkOptions::new();
        network.set_connection_timeout(CONNECT_DEADLINE.as_secs());
        connection.eventloop.set_network_options(network);
        match connection.recv_timeout(within) {
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => Ok((client, connection)),
            Ok(Ok(other)) => Err(Lost { why: format!("expected CONNACK, got {other:?}"), refused: false }),
            Ok(Err(e)) => Err(Lost::from(e)),
            Err(e) => Err(Lost { why: format!("no CONNACK: {e:?}"), refused: false }),
        }
    }

    /// A connection within [`CONNECT_DEADLINE`]. `announced` says the broker's socket has just
    /// appeared, so a refusal is waited out until the deadline.
    pub fn connect(&self, announced: bool) -> Result<(Client, Connection), Lost> {
        // Real time, not an injected clock: this bounds socket I/O, which virtual time never ends.
        let started = Instant::now();
        loop {
            match self.once(CONNECT_DEADLINE.saturating_sub(started.elapsed())) {
                Err(lost) if announced && lost.refused && started.elapsed() < CONNECT_DEADLINE => std::thread::sleep(LISTEN_PAUSE),
                other => return other,
            }
        }
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
