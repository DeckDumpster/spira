//! Where the pass speaks: log lines on stdout in lib.sh `log`'s shape, and movements —
//! which also cross the seam to the sentinel through the mailbox (landing.progress).
//!
//! ONLY MOVEMENTS CROSS THE SEAM. `progress` gates the sentinel's judgement tier; a line that
//! moved nothing (a "gated and held") is a log line and stays one.

use crate::util::{append_line, iso_utc, unix_now};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counted here so the SIGTERM path can write an honest status without reaching into the
/// pass's own stack.
pub static BRANCHES: AtomicU64 = AtomicU64::new(0);
pub static MOVED: AtomicU64 = AtomicU64::new(0);

pub struct Reporter {
    /// Tests: messages (without timestamp) instead of stdout.
    capture: Option<RefCell<Vec<String>>>,
    mailbox: Option<PathBuf>,
    moved: Cell<u64>,
    branches: Cell<u64>,
}

impl Reporter {
    pub fn stdout(mailbox: Option<PathBuf>) -> Reporter {
        Reporter { capture: None, mailbox, moved: Cell::new(0), branches: Cell::new(0) }
    }
    pub fn capture(mailbox: Option<PathBuf>) -> Reporter {
        Reporter { capture: Some(RefCell::new(Vec::new())), mailbox, moved: Cell::new(0), branches: Cell::new(0) }
    }

    pub fn moved(&self) -> u64 {
        self.moved.get()
    }
    pub fn branches(&self) -> u64 {
        self.branches.get()
    }
    /// Every spira/* ref seen, landable or not: the positive control's evidence.
    pub fn add_branches(&self, n: u64) {
        self.branches.set(self.branches.get() + n);
        BRANCHES.fetch_add(n, Ordering::SeqCst);
    }

    pub fn log(&self, msg: &str) {
        match &self.capture {
            Some(c) => c.borrow_mut().push(msg.to_string()),
            None => self.emit(&format!("{} spira: {}", iso_utc(unix_now()), msg)),
        }
    }

    /// A line some child already formatted (lib.sh's own `log`): passed through as is.
    pub fn raw(&self, line: &str) {
        match &self.capture {
            Some(c) => c.borrow_mut().push(line.to_string()),
            None => self.emit(line),
        }
    }

    /// A movement of the DAG: logged, counted and appended to the mailbox as the bare
    /// message (the sentinel adds its own prefix and timestamp when it counts it).
    pub fn progress(&self, msg: &str) {
        MOVED.fetch_add(1, Ordering::SeqCst);
        self.moved.set(self.moved.get() + 1);
        self.log(msg);
        if let Some(m) = &self.mailbox {
            append_line(m, msg);
        }
    }

    fn emit(&self, line: &str) {
        let mut so = std::io::stdout().lock();
        let _ = writeln!(so, "{line}");
        let _ = so.flush();
    }

    /// Tests: everything logged so far.
    pub fn lines(&self) -> Vec<String> {
        self.capture.as_ref().map(|c| c.borrow().clone()).unwrap_or_default()
    }
}

/// The signal path's view of the counters (the pass's own stack is not reachable there).
pub fn global_counts() -> (u64, u64) {
    (BRANCHES.load(Ordering::SeqCst), MOVED.load(Ordering::SeqCst))
}
