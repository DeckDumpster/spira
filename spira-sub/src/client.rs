//! The command side of the control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use spira_pub::link::{sleep_until_readable, LISTEN_PAUSE};
use spira_pub::{SocketWatch, CONNECT_DEADLINE};

use crate::protocol::{Delivery, Reply, Request};
use crate::render;

/// The most a command waits for its reply: past the longest the client waits for the broker.
const REPLY_DEADLINE: Duration = Duration::from_secs(5);

fn open(socket: &Path, request: &Request) -> std::io::Result<(Reply, BufReader<UnixStream>)> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(REPLY_DEADLINE))?;
    writeln!(stream, "{}", serde_json::to_string(request)?)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok((serde_json::from_str(&line)?, reader))
}

/// One command and its reply.
pub fn call(socket: &Path, request: &Request) -> Result<Reply, String> {
    open(socket, request)
        .map(|(reply, _)| reply)
        .map_err(|e| format!("no answer on {}: {e}; is `spira-sub run` up?", socket.display()))
}

/// A `receive` stream: deliveries, each of them to be acked once it has reached whom it is for.
pub struct Receiving {
    stream: BufReader<UnixStream>,
}

/// Attaches to a `run` that is up as the reader that acks. An earlier such reader is cut off.
pub fn receive(socket: &Path) -> Result<Receiving, String> {
    match open(socket, &Request::Receive) {
        Ok((Reply::Done { .. }, stream)) => {
            stream.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
            Ok(Receiving { stream })
        }
        Ok((other, _)) => Err(format!("receive was refused: {other:?}")),
        Err(e) => Err(format!("no answer on {}: {e}; is `spira-sub run` up?", socket.display())),
    }
}

impl Receiving {
    /// The next delivery, waited for; `None` once the stream has no more.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Delivery> {
        let mut line = String::new();
        if !read_whole_line(&mut self.stream, &mut line) {
            return None;
        }
        serde_json::from_str(&line).ok()
    }

    /// Says the delivery `seq` has arrived; only this spends it.
    pub fn ack(&mut self, seq: u64) -> std::io::Result<()> {
        let line = serde_json::to_string(&Request::Ack { seq })? + "\n";
        self.stream.get_mut().write_all(line.as_bytes())
    }
}

/// Appends one whole line to `into`; false once the stream has no more.
fn read_whole_line(from: &mut impl BufRead, into: &mut String) -> bool {
    matches!(from.read_line(into), Ok(n) if n > 0 && into.ends_with('\n'))
}

/// Copies deliveries to `out` until killed, each in one write. A `run` that is not there yet,
/// or restarts, is waited for on its socket's directory. Returns once `out` is closed.
pub fn follow(socket: &Path, out: &mut impl Write, note: &mut impl FnMut(&str)) -> Result<(), String> {
    // It may be asked before any `run` has made the directory it waits in.
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut watch = SocketWatch::new(socket);
    let mut announced = false;
    let mut waiting = false;
    loop {
        watch.arm()?;
        watch.drain();
        let started = Instant::now();
        let attached = loop {
            match open(socket, &Request::Follow) {
                // The socket file appears a moment before it accepts.
                Err(e) if announced && e.kind() == std::io::ErrorKind::ConnectionRefused && started.elapsed() < CONNECT_DEADLINE => {
                    std::thread::sleep(LISTEN_PAUSE)
                }
                other => break other,
            }
        };
        match attached {
            Ok((Reply::Done { .. }, mut reader)) => {
                note("following");
                reader.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
                let mut delivery = String::new();
                loop {
                    delivery.clear();
                    if !read_whole_line(&mut reader, &mut delivery) {
                        break;
                    }
                    let following = render::batch_size(&delivery).unwrap_or(0);
                    if !(0..following).all(|_| read_whole_line(&mut reader, &mut delivery)) {
                        break;
                    }
                    if out.write_all(delivery.as_bytes()).and_then(|()| out.flush()).is_err() {
                        return Ok(());
                    }
                }
                note("the client went away; waiting for it");
                waiting = true;
                announced = false;
            }
            Ok((other, _)) => return Err(format!("follow was refused: {other:?}")),
            Err(e) => {
                if !waiting {
                    note(&format!("no `spira-sub run` on {} ({e}); waiting for it", socket.display()));
                    waiting = true;
                }
                loop {
                    sleep_until_readable([watch.fd(), None], None);
                    if watch.drain() {
                        break;
                    }
                }
                announced = true;
            }
        }
    }
}
