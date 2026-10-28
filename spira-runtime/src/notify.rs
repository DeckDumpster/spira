//! sd_notify, sent only from the dispatch loop's own thread: a loop that hangs stops
//! kicking, which is how systemd's watchdog sees a hang and not only a death.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Notifier {
    dest: Option<SocketAddr>,
    sock: UnixDatagram,
    every: Option<Duration>,
    last: Mutex<Option<Instant>>,
}

impl Notifier {
    /// `socket` is `$NOTIFY_SOCKET`, `watchdog_usec` is `$WATCHDOG_USEC`; both are set by systemd.
    pub fn new(socket: Option<&str>, watchdog_usec: Option<u64>) -> std::io::Result<Notifier> {
        let dest = match socket.filter(|s| !s.is_empty()) {
            Some(s) => Some(match s.strip_prefix('@') {
                Some(name) => SocketAddr::from_abstract_name(name.as_bytes())?,
                None => SocketAddr::from_pathname(s)?,
            }),
            None => None,
        };
        let every = watchdog_usec.filter(|u| *u > 0).map(|u| Duration::from_micros(u / 3));
        Ok(Notifier { dest, sock: UnixDatagram::unbound()?, every, last: Mutex::new(None) })
    }

    pub fn from_env() -> std::io::Result<Notifier> {
        Notifier::new(std::env::var("NOTIFY_SOCKET").ok().as_deref(), std::env::var("WATCHDOG_USEC").ok().and_then(|v| v.parse().ok()))
    }

    /// Longest the loop may go without a turn: a third of the watchdog period.
    pub fn max_sleep(&self) -> Option<Duration> {
        self.every
    }

    fn send(&self, msg: &str) {
        if let Some(d) = &self.dest {
            let _ = self.sock.send_to_addr(msg.as_bytes(), d);
        }
    }

    pub fn ready(&self) {
        self.send("READY=1");
    }

    /// `WATCHDOG=1`, at most once per `max_sleep`; cheap enough to call after every event.
    pub fn kick(&self) {
        let Some(every) = self.every else { return };
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_none_or(|t| t.elapsed() >= every / 2) {
            *last = Some(Instant::now());
            self.send("WATCHDOG=1");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kicks_arrive_on_the_socket_and_are_rate_limited() {
        let d = testkit::TempDir::new("notify");
        let path = d.path().join("n.sock");
        let rx = UnixDatagram::bind(&path).unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let n = Notifier::new(path.to_str(), Some(3_000_000)).unwrap();
        n.ready();
        n.kick();
        n.kick();
        let mut buf = [0u8; 32];
        let got = |rx: &UnixDatagram, buf: &mut [u8]| {
            let n = rx.recv(buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        };
        assert_eq!(got(&rx, &mut buf), "READY=1");
        assert_eq!(got(&rx, &mut buf), "WATCHDOG=1");
        rx.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        assert!(rx.recv(&mut buf).is_err(), "the second kick inside the window is dropped");
        assert_eq!(n.max_sleep(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn no_socket_means_no_send_and_no_watchdog_means_no_kick() {
        let n = Notifier::new(None, None).unwrap();
        n.ready();
        n.kick();
        assert_eq!(n.max_sleep(), None);
    }
}
