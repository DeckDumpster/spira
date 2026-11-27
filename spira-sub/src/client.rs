//! The command side of the control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use spira_pub::link::{sleep_until_readable, LISTEN_PAUSE};
use spira_pub::{SocketWatch, CONNECT_DEADLINE};

use crate::protocol::{Reply, Request};

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

/// Copies delivery lines to `out` until killed. A `run` that is not there yet, or restarts, is
/// waited for on its socket's directory, and replays on the new attach. Returns once `out` is
/// closed.
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
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(n) if n > 0 && line.ends_with('\n') => {
                            if out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_err() {
                                return Ok(());
                            }
                        }
                        _ => break,
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
