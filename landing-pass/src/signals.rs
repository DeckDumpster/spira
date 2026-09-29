//! SIGTERM/SIGINT for the gated pass. systemd's RuntimeMaxSec and `landing-pass halt` both
//! stop a pass with SIGTERM, and the status file must be written on that path too — a worker
//! that only reports when it finishes cleanly is a worker whose silence means nothing.
//!
//! TERM and INT are blocked in every thread and received by one thread with `sigwait`, which
//! forwards TERM to the child the pass is waiting on (a gate, a seam call), writes the status
//! with rc 143, removes the run record and exits. Children get the default mask back
//! (`util::command`).

use crate::model::StatusFile;
use crate::records::Files;
use crate::report::global_counts;
use crate::util::{unix_now, CURRENT_CHILD};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

fn term_set() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        set
    }
}

/// Block TERM/INT in this (still single-threaded) process. Call before any thread exists.
pub fn block() {
    let set = term_set();
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Start the receiving thread. On a signal: forward TERM to the current child, write the
/// status (rc 143), drop the run record and the container registry, exit 143.
pub fn install(run: PathBuf) {
    std::thread::spawn(move || {
        let set = term_set();
        let mut sig: libc::c_int = 0;
        loop {
            let rc = unsafe { libc::sigwait(&set, &mut sig) };
            if rc == 0 {
                break;
            }
        }
        let child = CURRENT_CHILD.load(Ordering::SeqCst);
        if child > 0 {
            unsafe {
                // The child leads its own process group (util::command): TERM the group.
                libc::kill(-child, libc::SIGTERM);
            }
        }
        let files = Files::new(&run);
        let (branches, moved) = global_counts();
        files.write_status(&StatusFile { at: unix_now(), rc: 143, branches, moved });
        files.clear_run();
        std::process::exit(143);
    });
}
