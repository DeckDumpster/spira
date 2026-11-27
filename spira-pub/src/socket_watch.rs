//! The broker's own signal that it is back: it creates its socket file anew each time it starts.
//! Waiting for that is what lets an idle publisher reconnect with no timer and no retry loop.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use inotify::{EventMask, Inotify, WatchMask};

pub struct SocketWatch {
    inotify: Option<Inotify>,
    dir: PathBuf,
    file: OsString,
    armed: bool,
}

impl SocketWatch {
    pub fn new(socket: &Path) -> SocketWatch {
        let dir = socket.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        SocketWatch {
            inotify: Inotify::init().ok(),
            dir: dir.to_path_buf(),
            file: socket.file_name().unwrap_or_default().to_os_string(),
            armed: false,
        }
    }

    /// Starts the watch if it is not running; before a connect, so a broker that comes up
    /// during or after it is not missed. `Err` leaves reconnection to the next publish.
    pub fn arm(&mut self) -> Result<(), String> {
        if self.armed {
            return Ok(());
        }
        let inotify = self.inotify.as_mut().ok_or("inotify is unavailable")?;
        inotify.watches().add(&self.dir, WatchMask::CREATE).map_err(|e| format!("cannot watch {}: {e}", self.dir.display()))?;
        self.armed = true;
        Ok(())
    }

    pub fn fd(&self) -> Option<RawFd> {
        self.inotify.as_ref().filter(|_| self.armed).map(|i| i.as_raw_fd())
    }

    /// Consumes every queued event; true when the socket may have been created since the last
    /// drain (a lost event counts, since it may have been that one).
    pub fn drain(&mut self) -> bool {
        let Some(inotify) = self.inotify.as_mut() else { return false };
        let mut created = false;
        let mut buf = [0u8; 4096];
        loop {
            match inotify.read_events(&mut buf) {
                Ok(events) => {
                    let mut any = false;
                    for e in events {
                        any = true;
                        created |= e.mask.contains(EventMask::Q_OVERFLOW) || e.name == Some(self.file.as_os_str());
                    }
                    if !any {
                        return created;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return created,
            }
        }
    }
}
