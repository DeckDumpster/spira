//! The pane's real size, read directly from the kernel — never by shelling out.
//!
//! THE SCAR THIS MODULE EXISTS TO CLOSE. The original read the terminal size by running
//! `stty size` as a child process. `std::process::Command::output()` gives that child a
//! *null* stdin — never this process's own tty — so the child's own `ioctl(TIOCGWINSZ)` (on
//! its stdin, which is what `stty size` inspects) failed every time, in every pane, with
//! "standard input: Inappropriate ioctl for device". `term_size` never saw that failure as
//! anything but "no reading", and fell to its last-resort default on every single tick. On
//! an 81-row live pane the operator actually uses, that made `health loop` render five rows
//! forever. There was no code path in which the old check could ever have succeeded.
//!
//! The fix is to call `ioctl(fd, TIOCGWINSZ, ...)` on THIS process's own fd — no child, no
//! stdin to lose — which is exactly what `stty` itself does one layer down.

use std::os::unix::io::RawFd;

/// `ioctl(fd, TIOCGWINSZ, ...)` on `fd`, validated. `None` when `fd` is not a tty, the
/// ioctl itself fails, or the kernel reports a zero size (a `resize -s 0 0` pty, or one
/// that has never been given a size at all — `winsize` is zero-initialized until a client
/// sets it).
pub fn winsize_of_fd(fd: RawFd) -> Option<(i64, i64)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a plain, caller-owned file descriptor (stdin/stdout/stderr in every
    // real caller); `&mut ws` is a valid, appropriately-sized `winsize` for the lifetime of
    // this call, matching the ioctl's documented contract.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc != 0 {
        return None;
    }
    valid_size(ws.ws_row, ws.ws_col)
}

/// The validation `winsize_of_fd` applies to whatever the kernel handed back, split out so
/// it can be tested without a real tty.
fn valid_size(rows: u16, cols: u16) -> Option<(i64, i64)> {
    if rows == 0 || cols == 0 {
        return None;
    }
    Some((rows as i64, cols as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_row_or_col_is_rejected() {
        assert_eq!(valid_size(0, 168), None);
        assert_eq!(valid_size(81, 0), None);
        assert_eq!(valid_size(0, 0), None);
    }

    #[test]
    fn a_real_reading_passes_through_as_is() {
        assert_eq!(valid_size(81, 168), Some((81, 168)));
        assert_eq!(valid_size(24, 80), Some((24, 80)));
    }

    #[test]
    fn winsize_of_fd_on_a_definitely_non_tty_fd_is_none_not_a_panic() {
        // A plain file is never a tty; -1 is never a valid fd either. Neither must panic —
        // the whole point is that a failed ioctl is indistinguishable from "not a tty" and
        // falls through to the next candidate fd (or the env-var/default fallback) rather
        // than crashing the pane.
        let f = std::fs::File::open("/dev/null").expect("/dev/null must exist");
        use std::os::unix::io::AsRawFd;
        assert_eq!(winsize_of_fd(f.as_raw_fd()), None);
        assert_eq!(winsize_of_fd(-1), None);
    }
}
