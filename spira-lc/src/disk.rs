//! The server's file calls outside SQLite: the process's `Fs`, and `LogWriter`, which takes
//! best-effort log lines off the request path. A request pushes a line and goes on; one job at
//! a time on the offload pool appends what has queued.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use ports::{Fs, RealFs};
use runtime::offload::Pool;

/// The most line bytes held for the writer; a line past it is dropped and counted.
pub const MAX_QUEUED_BYTES: usize = 8 << 20;
/// What the shutdown flush waits for a blocked append.
pub const FLUSH_BOUND: Duration = Duration::from_secs(5);
pub const LOG_OVERFLOW: &str = "spira_lc_log_overflow";

struct Line {
    path: PathBuf,
    text: String,
}

#[derive(Default)]
struct State {
    queue: Vec<Line>,
    bytes: usize,
    flying: bool,
    overflow: u64,
}

struct Shared {
    fs: Arc<dyn Fs>,
    state: Mutex<State>,
    idle: Condvar,
}

pub struct LogWriter {
    shared: Arc<Shared>,
    pool: Arc<Pool>,
}

fn held(s: &Mutex<State>) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// Appends each run of lines for one file with one call. A failure is reported and the lines
/// are dropped: nothing waits on them.
fn write_batch(fs: &dyn Fs, batch: Vec<Line>) {
    let mut i = 0;
    while i < batch.len() {
        let n = batch[i..].iter().take_while(|l| l.path == batch[i].path).count();
        let text: String = batch[i..i + n].iter().map(|l| l.text.as_str()).collect();
        if let Err(e) = fs.append(&batch[i].path, text.as_bytes()) {
            eprintln!("spira-lc: {}: {e}", batch[i].path.display());
        }
        i += n;
    }
}

impl LogWriter {
    pub fn new(fs: Arc<dyn Fs>, pool: Arc<Pool>) -> LogWriter {
        LogWriter { shared: Arc::new(Shared { fs, state: Mutex::new(State::default()), idle: Condvar::new() }), pool }
    }

    /// Queues `text` for `path`; it is appended by the pool, not by the caller.
    pub fn push(&self, path: PathBuf, text: String) {
        {
            let mut s = held(&self.shared.state);
            if s.bytes + text.len() > MAX_QUEUED_BYTES {
                s.overflow += 1;
                return;
            }
            s.bytes += text.len();
            s.queue.push(Line { path, text });
            if s.flying {
                return;
            }
            s.flying = true;
        }
        let shared = self.shared.clone();
        // A pool that refuses leaves the batch queued for the next push or the flush.
        if self.pool.offload("lc.log", None, move || Self::append_until_empty(&shared)).is_err() {
            held(&self.shared.state).flying = false;
            self.shared.idle.notify_all();
        }
    }

    fn append_until_empty(shared: &Shared) {
        loop {
            let batch = {
                let mut s = held(&shared.state);
                if s.queue.is_empty() {
                    s.flying = false;
                    shared.idle.notify_all();
                    return;
                }
                s.bytes = 0;
                std::mem::take(&mut s.queue)
            };
            write_batch(shared.fs.as_ref(), batch);
        }
    }

    /// Lines dropped for want of room since this writer was made.
    pub fn overflow(&self) -> u64 {
        held(&self.shared.state).overflow
    }

    /// Waits for the job in flight up to `bound`, then writes what is left on this thread.
    /// A stopping process must not wait on a file nobody will ever read.
    pub fn flush(&self, bound: Duration) {
        let s = held(&self.shared.state);
        let (mut s, wait) = self.shared.idle.wait_timeout_while(s, bound, |s| s.flying).unwrap_or_else(|e| e.into_inner());
        if wait.timed_out() {
            return;
        }
        let batch = std::mem::take(&mut s.queue);
        s.bytes = 0;
        drop(s);
        write_batch(self.shared.fs.as_ref(), batch);
    }
}

static WRITER: OnceLock<LogWriter> = OnceLock::new();

thread_local! {
    static LOCAL: RefCell<Option<Arc<LogWriter>>> = const { RefCell::new(None) };
}

/// Makes `writer` the process's. The first install stands.
pub fn install(writer: LogWriter) {
    let _ = WRITER.set(writer);
}

/// Runs `f` with `writer` taking this thread's log lines, whatever the process has installed.
#[cfg(test)]
pub fn with_writer<R>(writer: Arc<LogWriter>, f: impl FnOnce() -> R) -> R {
    let before = LOCAL.with(|l| l.replace(Some(writer)));
    let out = f();
    LOCAL.with(|l| *l.borrow_mut() = before);
    out
}

/// The process's disk: what `install` was given, else the real one.
pub fn fs() -> Arc<dyn Fs> {
    WRITER.get().map_or_else(|| Arc::new(RealFs) as Arc<dyn Fs>, |w| w.shared.fs.clone())
}

/// One best-effort log line for `path`: queued where a writer is installed, else appended here.
pub fn log_line(path: &Path, text: String) {
    if let Some(w) = LOCAL.with(|l| l.borrow().clone()) {
        return w.push(path.to_path_buf(), text);
    }
    match WRITER.get() {
        Some(w) => w.push(path.to_path_buf(), text),
        None => {
            let _ = RealFs.append(path, text.as_bytes());
        }
    }
}

/// Writes what the installed writer still holds.
pub fn flush() {
    if let Some(w) = WRITER.get() {
        w.flush(FLUSH_BOUND);
    }
}

fn overflow_series(total: u64, pushed: u64, at: std::time::SystemTime) -> spira_otel::metrics::Series {
    spira_otel::metrics::Series { name: LOG_OVERFLOW.into(), value: total.saturating_sub(pushed) as f64, labels: Vec::new(), at }
}

/// Pushes the lines dropped since the last push, 0 included, so a quiet writer is told apart
/// from a missing one; `pushed` advances only when the push succeeds.
pub fn push_overflow(sink: &spira_otel::metrics::MetricsSink, pushed: &mut u64) {
    let Some(w) = WRITER.get() else { return };
    let total = w.overflow();
    if sink.try_push_series(&[overflow_series(total, *pushed, std::time::SystemTime::now())]).is_ok() {
        *pushed = total;
    }
}

/// A file's text through the process's `Fs`.
pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    let bytes = fs().read(path.as_ref())?;
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A file's bytes through the process's `Fs`.
pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    fs().read(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::{SimFs, VirtualClock};
    use runtime::offload::SimJobs;

    fn sim(cap: usize) -> (Arc<SimFs>, Arc<SimJobs>, LogWriter) {
        let clock = VirtualClock::new(0);
        let fs = SimFs::new(1, Default::default(), clock.clone());
        fs.create_dir(Path::new("/run")).unwrap();
        let jobs = SimJobs::new(clock, fs.clone(), cap);
        let writer = LogWriter::new(fs.clone(), Pool::over(jobs.clone()));
        (fs, jobs, writer)
    }

    fn log() -> PathBuf {
        PathBuf::from("/run/x.log")
    }

    #[test_id(ts6g1pl)]
    #[test]
    fn a_line_past_the_queue_bound_is_counted_and_dropped_and_the_rest_are_written() {
        let (fs, _jobs, w) = sim(0);
        w.push(log(), "a".repeat(5 << 20));
        w.push(log(), "b".repeat(5 << 20));
        w.push(log(), "c\n".into());
        assert_eq!(w.overflow(), 1);
        w.flush(Duration::from_secs(1));
        let written = fs.peek(&log()).unwrap();
        assert_eq!(written.len(), (5 << 20) + 2);
        assert!(written.ends_with(b"c\n") && !written.contains(&b'b'));
    }

    #[test_id(tofqldd)]
    #[test]
    fn a_batch_the_pool_refused_stays_queued_and_is_appended_by_a_later_push() {
        let (fs, jobs, w) = sim(1);
        w.push(log(), "one\n".into());
        w.push(log(), "two\n".into());
        assert_eq!(fs.peek(&log()).unwrap(), b"one\n", "the pool was full: two waits");
        jobs.run_all();
        w.push(log(), "three\n".into());
        assert_eq!(fs.peek(&log()).unwrap(), b"one\ntwo\nthree\n");
    }

    #[test_id(t90wbe5)]
    #[test]
    fn flush_writes_the_lines_the_pool_never_took() {
        let (fs, _jobs, w) = sim(0);
        w.push(log(), "one\n".into());
        w.push(PathBuf::from("/run/y.log"), "other\n".into());
        w.push(log(), "two\n".into());
        assert!(fs.peek(&log()).is_none());
        w.flush(Duration::from_secs(1));
        assert_eq!(fs.peek(&log()).unwrap(), b"one\ntwo\n");
        assert_eq!(fs.peek(Path::new("/run/y.log")).unwrap(), b"other\n");
    }

    #[test_id(tladi4c)]
    #[test]
    fn a_push_never_waits_on_a_blocked_disk_and_the_flush_gives_up_after_its_bound() {
        let (fs, _jobs, w) = sim(8);
        fs.hold_next(1);
        let writer = Arc::new(w);
        let pushed = {
            let w = writer.clone();
            std::thread::spawn(move || w.push(log(), "stuck\n".into()))
        };
        fs.wait_blocked(1);
        writer.push(log(), "next\n".into());
        writer.flush(Duration::from_millis(1));
        assert!(fs.peek(&log()).is_none(), "the append in flight holds the file");
        fs.release();
        pushed.join().unwrap();
        writer.flush(Duration::from_secs(5));
        assert_eq!(fs.peek(&log()).unwrap(), b"stuck\nnext\n");
    }
}
