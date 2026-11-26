//! Sleeping until the kernel has something to say, with no timer unless one is asked for.

use std::os::fd::RawFd;
use std::time::Duration;

/// Until `fd` is readable or `timeout` passes; with neither, for good. An interrupted call only
/// returns early, and every caller looks at its state again.
pub(crate) fn readable(fd: Option<RawFd>, timeout: Option<Duration>) {
    let mut fds = [libc::pollfd { fd: fd.unwrap_or(-1), events: libc::POLLIN, revents: 0 }];
    let ms = timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
    // SAFETY: `fds` is a live array of the length passed.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
}
