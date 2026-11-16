//! A runtime control socket that answers every request with exit 0: world stop journals a
//! scheduler halt there, and a world runs no scheduler. Serves until the phase file says `done`.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn serve(sock: &Path, phase_file: &Path, limit: Duration) -> Result<(), String> {
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let end = Instant::now() + limit;
    while Instant::now() < end && std::fs::read_to_string(phase_file).map(|p| p.trim() != "done").unwrap_or(true) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                let _ = runtime::socket::answer_one(stream, &|_| Ok(String::new()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = std::fs::remove_file(sock);
    Ok(())
}
