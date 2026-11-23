//! One OTLP span per call, appended as one `ExportTraceServiceRequest` JSON line to
//! `<dir>/<service>.<UTC date>.jsonl` — the Collector file-exporter format, so an
//! `otlpjsonfile` receiver can ship the files unchanged. Emitting never fails or blocks the
//! caller beyond one short append: an error is counted and dropped. A `buffered` sink does not
//! append on the caller: it queues, and one thread appends each second's queue.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod fixture;
pub mod logs;
pub mod metrics;
pub mod oneshot;
pub mod query;
pub mod rounds;
pub mod run;
pub mod vlogs;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, ArrayValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{span::SpanKind, status::StatusCode, ResourceSpans, ScopeSpans, Span, Status};

static DROPPED: AtomicU64 = AtomicU64::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

extern "C" {
    fn flock(fd: i32, op: i32) -> i32;
}
const LOCK_EX: i32 = 2;

/// Writes spans for one service into one directory, keeping `keep_days` days of files.
pub struct Sink {
    inner: Arc<Inner>,
    queue: Option<Arc<Queue>>,
}

struct Inner {
    dir: PathBuf,
    service: String,
    keep_days: u32,
    day_max_bytes: u64,
    dropped: AtomicU64,
    warned_days: Mutex<Vec<String>>,
}

/// Serialised spans waiting for the writer thread, each with its emit time.
#[derive(Default)]
struct Pending {
    spans: Vec<(SystemTime, Vec<u8>)>,
    bytes: usize,
    busy: bool,
    flushers: usize,
}

#[derive(Default)]
struct Queue {
    pending: Mutex<Pending>,
    wake: Condvar,
    done: Condvar,
}

/// The most serialised span bytes a buffered sink holds before it drops.
pub const QUEUE_MAX_BYTES: usize = 8 << 20;
const APPEND_EVERY: Duration = Duration::from_secs(1);
const FLUSH_BOUND: Duration = Duration::from_secs(5);

/// What one call looked like; the attribute set of the span built from it.
pub struct Call<'a> {
    pub method: &'a str,
    pub caller: &'a str,
    pub request_size: u64,
    pub response_size: u64,
    pub exit_code: i32,
    pub start: SystemTime,
    pub end: SystemTime,
    pub queue_wait: Duration,
    /// Request received to the worker being handed it.
    pub before_work: Duration,
    /// Handing the worker the request to its answer, or to the cutoff.
    pub answer_wait: Duration,
    /// The answer in hand to the end of the span: reply, settling, counters.
    pub after_answer: Duration,
    /// The answer was the deadline answer.
    pub cut: bool,
    /// Request read to answer written; `None` where no request was read.
    pub answered: Option<Duration>,
    /// The request cutoff that abandoned the call; `None` where it answered in time.
    pub cut_at: Option<Duration>,
    /// Microseconds per store phase the request recorded.
    pub phases: &'a [(&'a str, u64)],
    /// The duration of each store commit, in order.
    pub commits_us: &'a [u64],
    /// SQLite read transactions.
    pub reads: u64,
    pub read_max_us: u64,
    pub reads_ended: u64,
    pub reads_overran: u64,
    /// The store phase a request cut at its deadline was still in.
    pub open_phase: Option<&'a str>,
    /// Dolt SQL statements run.
    pub content_queries: u64,
    /// The longest single Dolt statement, in microseconds, and its shape.
    pub content_query_max_us: u64,
    pub content_query_max_shape: &'a str,
    /// The scheduler task whose effect made the call.
    pub task: Option<&'a str>,
    /// Child processes run.
    pub children: u64,
    /// Basenames of the child programs, in order.
    pub programs: &'a [String],
    /// The request's sub-verb, when it is a plain word.
    pub subverb: Option<&'a str>,
}

/// Spans lost to a write error since this process started.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

impl Sink {
    pub fn new(dir: impl Into<PathBuf>, service: &str, keep_days: u32) -> Sink {
        Sink { inner: Arc::new(Inner::new(dir.into(), service, keep_days, u64::MAX)), queue: None }
    }

    /// A sink whose `emit` only queues: one thread appends the queue once a second, and a day
    /// file at `day_max_bytes` takes no more.
    pub fn buffered(dir: impl Into<PathBuf>, service: &str, keep_days: u32, day_max_bytes: u64) -> Sink {
        let inner = Arc::new(Inner::new(dir.into(), service, keep_days, day_max_bytes));
        let queue = Arc::new(Queue::default());
        let (i, q) = (inner.clone(), queue.clone());
        std::thread::Builder::new().name("otel-writer".into()).spawn(move || writer_loop(&i, &q)).expect("the span writer thread starts");
        Sink { inner, queue: Some(queue) }
    }

    /// Spans this sink lost; `dropped()` counts the process.
    pub fn dropped_here(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Appends one span, or queues it on a buffered sink; a failure is counted, never returned.
    pub fn emit(&self, call: &Call) {
        let Some(queue) = &self.queue else {
            let _ = self.write(call, SystemTime::now());
            return;
        };
        let Ok(line) = self.inner.line(call) else { return self.inner.drop_spans(1) };
        let mut p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        if p.bytes + line.len() > QUEUE_MAX_BYTES {
            drop(p);
            return self.inner.drop_spans(1);
        }
        p.bytes += line.len();
        p.spans.push((SystemTime::now(), line));
        queue.wake.notify_one();
    }

    /// Blocks until everything queued so far has been appended or dropped, or `FLUSH_BOUND`
    /// passes: a day file held locked elsewhere must not hold a stopping process.
    pub fn flush(&self) {
        let Some(queue) = &self.queue else { return };
        let mut p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.flushers += 1;
        queue.wake.notify_one();
        let (mut p, _) = queue.done.wait_timeout_while(p, FLUSH_BOUND, |p| !p.spans.is_empty() || p.busy).unwrap_or_else(|e| e.into_inner());
        p.flushers -= 1;
    }

    pub fn write(&self, call: &Call, now: SystemTime) -> std::io::Result<()> {
        let line = self.inner.line(call).inspect_err(|_| self.inner.drop_spans(1))?;
        self.inner.append_day(&utc_date(now), &[&line])
    }
}

fn writer_loop(inner: &Inner, queue: &Queue) {
    let mut p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        while p.spans.is_empty() {
            p = queue.wake.wait(p).unwrap_or_else(|e| e.into_inner());
        }
        let batch = std::mem::take(&mut p.spans);
        p.bytes = 0;
        p.busy = true;
        drop(p);
        inner.write_batch(&batch);
        p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.busy = false;
        queue.done.notify_all();
        p = queue.wake.wait_timeout_while(p, APPEND_EVERY, |p| p.flushers == 0).unwrap_or_else(|e| e.into_inner()).0;
    }
}

impl Inner {
    fn new(dir: PathBuf, service: &str, keep_days: u32, day_max_bytes: u64) -> Inner {
        Inner { dir, service: service.to_string(), keep_days, day_max_bytes, dropped: AtomicU64::new(0), warned_days: Mutex::new(Vec::new()) }
    }

    fn line(&self, call: &Call) -> std::io::Result<Vec<u8>> {
        let mut line = serde_json::to_vec(&self.request(call)).map_err(std::io::Error::other)?;
        line.push(b'\n');
        Ok(line)
    }

    fn drop_spans(&self, n: u64) {
        DROPPED.fetch_add(n, Ordering::Relaxed);
        self.dropped.fetch_add(n, Ordering::Relaxed);
    }

    /// Appends a batch, one locked write per UTC day of emit time.
    fn write_batch(&self, batch: &[(SystemTime, Vec<u8>)]) {
        let mut i = 0;
        while i < batch.len() {
            let day = utc_date(batch[i].0);
            let n = batch[i..].iter().take_while(|(t, _)| utc_date(*t) == day).count();
            let lines: Vec<&[u8]> = batch[i..i + n].iter().map(|(_, l)| l.as_slice()).collect();
            let _ = self.append_day(&day, &lines);
            i += n;
        }
    }

    /// Appends whole lines to `day`'s file under its lock while the file stays within the day
    /// bound; the rest, or all of them on an error, are dropped and counted.
    fn append_day(&self, day: &str, lines: &[&[u8]]) -> std::io::Result<()> {
        let opened = self.open_locked(day);
        let Ok((mut f, path, first_today)) = opened else {
            self.drop_spans(lines.len() as u64);
            return opened.map(|_| ());
        };
        let mut size = match f.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                self.drop_spans(lines.len() as u64);
                return Err(e);
            }
        };
        let mut out = Vec::new();
        let mut kept = 0u64;
        for l in lines {
            if size + l.len() as u64 > self.day_max_bytes {
                break;
            }
            size += l.len() as u64;
            out.extend_from_slice(l);
            kept += 1;
        }
        let lost = lines.len() as u64 - kept;
        if lost > 0 {
            self.drop_spans(lost);
            let mut warned = self.warned_days.lock().unwrap_or_else(|e| e.into_inner());
            if !warned.iter().any(|d| d == day) {
                warned.push(day.to_string());
                eprintln!("spira-otel: {} is at its {} byte day bound; further spans that day are dropped", path.display(), self.day_max_bytes);
            }
        }
        f.write_all(&out).inspect_err(|_| self.drop_spans(kept))?;
        if first_today {
            self.prune(day);
        }
        Ok(())
    }

    fn open_locked(&self, day: &str) -> std::io::Result<(File, PathBuf, bool)> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(day);
        let first_today = !path.exists();
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        lock(&f)?;
        Ok((f, path, first_today))
    }

    fn path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{}.{day}.jsonl", self.service))
    }

    fn prune(&self, today: &str) {
        let Some(oldest) = day_number(today).map(|d| d - i64::from(self.keep_days) + 1) else { return };
        let prefix = format!("{}.", self.service);
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(day) = name.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".jsonl")) else { continue };
            if day_number(day).is_some_and(|d| d < oldest) {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    fn request(&self, c: &Call) -> ExportTraceServiceRequest {
        let nanos = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
        let failed = c.exit_code != 0;
        let mut attributes = vec![
            kv_str("rpc.system", "spira-lc"),
            kv_str("rpc.method", c.method),
            kv_str("spira.caller", c.caller),
            kv_int("rpc.request.size", c.request_size),
            kv_int("rpc.response.size", c.response_size),
            kv_int("spira.queue_wait_us", c.queue_wait.as_micros() as u64),
            kv_int("spira.serve.before_work_us", c.before_work.as_micros() as u64),
            kv_int("spira.serve.answer_wait_us", c.answer_wait.as_micros() as u64),
            kv_int("spira.serve.after_answer_us", c.after_answer.as_micros() as u64),
            KeyValue { key: "spira.serve.cut".into(), value: Some(AnyValue { value: Some(Value::BoolValue(c.cut)) }) },
        ];
        if let Some(answered) = c.answered {
            attributes.push(kv_int("spira.answer_us", answered.as_micros() as u64));
        }
        if let Some(cut) = c.cut_at {
            attributes.push(kv_int("spira.deadline_cut_ms", cut.as_millis() as u64));
        }
        for (name, us) in c.phases {
            attributes.push(kv_int(&format!("spira.phase.{name}_us"), *us));
        }
        if !c.commits_us.is_empty() {
            attributes.push(kv_int("spira.store.commits", c.commits_us.len() as u64));
            let values = c.commits_us.iter().map(|us| AnyValue { value: Some(Value::IntValue(*us as i64)) }).collect();
            attributes.push(KeyValue { key: "spira.phase.commits_us".into(), value: Some(AnyValue { value: Some(Value::ArrayValue(ArrayValue { values })) }) });
        }
        if c.reads > 0 {
            attributes.push(kv_int("spira.store.reads", c.reads));
            attributes.push(kv_int("spira.store.read_max_us", c.read_max_us));
            attributes.push(kv_int("spira.store.reads_ended", c.reads_ended));
            attributes.push(kv_int("spira.store.reads_overran", c.reads_overran));
        }
        if c.content_queries > 0 {
            attributes.push(kv_int("spira.content.queries", c.content_queries));
            attributes.push(kv_int("spira.content.query_max_us", c.content_query_max_us));
            attributes.push(kv_str("spira.content.query_max_shape", c.content_query_max_shape));
        }
        if let Some(task) = c.task {
            attributes.push(kv_str("spira.task", task));
        }
        if c.children > 0 {
            attributes.push(kv_int("spira.children", c.children));
        }
        if !c.programs.is_empty() {
            attributes.push(kv_str("spira.children.programs", &c.programs.join(",")));
        }
        if let Some(sub) = c.subverb {
            attributes.push(kv_str("spira.subverb", sub));
        }
        if let Some(open) = c.open_phase {
            attributes.push(kv_str("spira.phase.open", open));
        }
        if failed {
            attributes.push(KeyValue { key: "spira.exit_code".into(), value: Some(AnyValue { value: Some(Value::IntValue(i64::from(c.exit_code))) }) });
        }
        let span = Span {
            trace_id: ids(16),
            span_id: ids(8),
            name: c.method.to_string(),
            kind: SpanKind::Server as i32,
            start_time_unix_nano: nanos(c.start),
            end_time_unix_nano: nanos(c.end),
            attributes,
            status: Some(Status { code: if failed { StatusCode::Error } else { StatusCode::Ok } as i32, message: String::new() }),
            ..Default::default()
        };
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource { attributes: vec![kv_str("service.name", &self.service)], ..Default::default() }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope { name: "spira-otel".into(), ..Default::default() }),
                    spans: vec![span],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }
}

pub(crate) fn kv_str(k: &str, v: &str) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }) }
}

fn kv_int(k: &str, v: u64) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::IntValue(v as i64)) }) }
}

/// `n` unique non-zero bytes: time, pid and a counter, mixed — not random, but never repeated.
fn ids(n: usize) -> Vec<u8> {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
    let mut x = t ^ seq.rotate_left(32) ^ (u64::from(std::process::id()) << 16);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend_from_slice(&(z ^ (z >> 31) | 1).to_be_bytes());
    }
    out.truncate(n);
    out[n - 1] |= 1;
    out
}

fn lock(f: &File) -> std::io::Result<()> {
    // SAFETY: the descriptor is open for the lifetime of `f`; the lock drops with the file.
    if unsafe { flock(f.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn append_locked(f: &mut File, line: &[u8]) -> std::io::Result<()> {
    lock(f)?;
    f.write_all(line)
}

/// `YYYY-MM-DD` of a UTC instant.
pub fn utc_date(t: SystemTime) -> String {
    let days = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() / 86_400;
    let (y, m, d) = civil(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn day_number(date: &str) -> Option<i64> {
    let mut p = date.split('-');
    let (y, m, d): (i64, i64, i64) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    if p.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn dir_of(run: &Path) -> PathBuf {
    run.join("otel")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn call(code: i32) -> Call<'static> {
        let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        Call { method: "list", caller: "work/list", request_size: 40, response_size: 439_002, exit_code: code, start, end: start + Duration::from_millis(12), queue_wait: Duration::ZERO, before_work: Duration::ZERO, answer_wait: Duration::ZERO, after_answer: Duration::ZERO, cut: false, answered: None, cut_at: None, phases: &[], commits_us: &[], reads: 0, read_max_us: 0, reads_ended: 0, reads_overran: 0, open_phase: None, content_queries: 0, content_query_max_us: 0, content_query_max_shape: "", task: None, children: 0, programs: &[], subverb: None }
    }

    fn tmp(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(name)
    }

    fn attr<'a>(span: &'a Span, k: &str) -> Option<&'a Value> {
        span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.as_ref()?.value.as_ref())
    }

    #[test_id(txpa66z)]
    #[test]
    fn answer_time_and_cutoff_are_int_attributes_only_when_given() {
        let dir = tmp("answer-cut");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        sink.write(&Call { answered: Some(Duration::from_millis(12)), cut_at: Some(Duration::from_millis(1000)), ..call(0) }, now).unwrap();
        sink.write(&call(0), now).unwrap();
        let text = fs::read_to_string(dir.join("spira-lc.2023-11-14.jsonl")).unwrap();
        let spans: Vec<Span> = text.lines().map(|l| serde_json::from_str::<ExportTraceServiceRequest>(l).unwrap().resource_spans[0].scope_spans[0].spans[0].clone()).collect();
        assert_eq!(attr(&spans[0], "spira.answer_us"), Some(&Value::IntValue(12_000)));
        assert_eq!(attr(&spans[0], "spira.deadline_cut_ms"), Some(&Value::IntValue(1000)));
        assert_eq!(attr(&spans[1], "spira.answer_us"), None);
        assert_eq!(attr(&spans[1], "spira.deadline_cut_ms"), None);
    }

    #[test_id(tal4n2j)]
    #[test]
    fn the_subverb_children_and_queries_are_written_only_when_present() {
        let dir = tmp("subverb");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let programs = ["bd".to_string(), "git".to_string()];
        sink.write(&Call { subverb: Some("note"), children: 2, programs: &programs, content_queries: 3, ..call(0) }, now).unwrap();
        sink.write(&call(0), now).unwrap();
        let text = fs::read_to_string(dir.join("spira-lc.2023-11-14.jsonl")).unwrap();
        let spans: Vec<Span> = text.lines().map(|l| serde_json::from_str::<ExportTraceServiceRequest>(l).unwrap().resource_spans[0].scope_spans[0].spans[0].clone()).collect();
        assert_eq!(attr(&spans[0], "spira.subverb"), Some(&Value::StringValue("note".into())));
        assert_eq!(attr(&spans[0], "spira.children.programs"), Some(&Value::StringValue("bd,git".into())));
        assert_eq!(attr(&spans[0], "spira.children"), Some(&Value::IntValue(2)));
        assert_eq!(attr(&spans[0], "spira.content.queries"), Some(&Value::IntValue(3)));
        for k in ["spira.subverb", "spira.children", "spira.children.programs", "spira.content.queries"] {
            assert_eq!(attr(&spans[1], k), None, "{k}");
        }
    }

    #[test_id(toh8axn)]
    #[test]
    fn the_longest_statement_and_the_task_are_written_only_when_present() {
        let dir = tmp("query-max");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        sink.write(&Call { content_queries: 2, content_query_max_us: 938_132, content_query_max_shape: "SELECT a FROM t WHERE x = ?", task: Some("sentinel"), ..call(0) }, now).unwrap();
        sink.write(&Call { content_query_max_us: 5, content_query_max_shape: "SELECT 1", ..call(0) }, now).unwrap();
        let text = fs::read_to_string(dir.join("spira-lc.2023-11-14.jsonl")).unwrap();
        let spans: Vec<Span> = text.lines().map(|l| serde_json::from_str::<ExportTraceServiceRequest>(l).unwrap().resource_spans[0].scope_spans[0].spans[0].clone()).collect();
        assert_eq!(attr(&spans[0], "spira.content.query_max_us"), Some(&Value::IntValue(938_132)));
        assert_eq!(attr(&spans[0], "spira.content.query_max_shape"), Some(&Value::StringValue("SELECT a FROM t WHERE x = ?".into())));
        assert_eq!(attr(&spans[0], "spira.task"), Some(&Value::StringValue("sentinel".into())));
        for k in ["spira.content.query_max_us", "spira.content.query_max_shape", "spira.task"] {
            assert_eq!(attr(&spans[1], k), None, "{k}");
        }
    }

    #[test_id(tovzjfu)]
    #[test]
    fn the_serve_stages_and_the_cut_are_written_each_under_its_own_name() {
        let dir = tmp("serve-stages");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let stages = Call { before_work: Duration::from_micros(11), answer_wait: Duration::from_micros(22), after_answer: Duration::from_micros(33), cut: true, ..call(0) };
        sink.write(&stages, now).unwrap();
        let text = fs::read_to_string(dir.join("spira-lc.2023-11-14.jsonl")).unwrap();
        let span = serde_json::from_str::<ExportTraceServiceRequest>(text.trim()).unwrap().resource_spans[0].scope_spans[0].spans[0].clone();
        assert_eq!(attr(&span, "spira.serve.before_work_us"), Some(&Value::IntValue(11)));
        assert_eq!(attr(&span, "spira.serve.answer_wait_us"), Some(&Value::IntValue(22)));
        assert_eq!(attr(&span, "spira.serve.after_answer_us"), Some(&Value::IntValue(33)));
        assert_eq!(attr(&span, "spira.serve.cut"), Some(&Value::BoolValue(true)));
    }

    #[test_id(tb7lqpy)]
    #[test]
    fn a_span_is_one_line_that_round_trips_through_the_protos_own_deserializer() {
        let dir = tmp("roundtrip");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        sink.write(&call(0), UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
        sink.write(&call(3), UNIX_EPOCH + Duration::from_secs(1_700_000_001)).unwrap();
        let text = fs::read_to_string(dir.join("spira-lc.2023-11-14.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let raw: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let span = &raw["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["startTimeUnixNano"], "1700000000000000000", "64-bit ints are strings");
        assert_eq!(span["kind"], 2);
        let ok: ExportTraceServiceRequest = serde_json::from_str(lines[0]).unwrap();
        let res = &ok.resource_spans[0];
        assert_eq!(attr_of(&res.resource.as_ref().unwrap().attributes, "service.name"), Some(&Value::StringValue("spira-lc".into())));
        let s = &res.scope_spans[0].spans[0];
        assert_eq!(s.trace_id.len(), 16);
        assert_eq!(s.span_id.len(), 8);
        assert_eq!(s.end_time_unix_nano - s.start_time_unix_nano, 12_000_000);
        assert_eq!(attr(s, "rpc.system"), Some(&Value::StringValue("spira-lc".into())));
        assert_eq!(attr(s, "rpc.method"), Some(&Value::StringValue("list".into())));
        assert_eq!(attr(s, "spira.caller"), Some(&Value::StringValue("work/list".into())));
        assert_eq!(attr(s, "rpc.request.size"), Some(&Value::IntValue(40)));
        assert_eq!(attr(s, "rpc.response.size"), Some(&Value::IntValue(439_002)));
        assert_eq!(attr(s, "spira.exit_code"), None);
        assert_eq!(s.status.as_ref().unwrap().code, StatusCode::Ok as i32);
        let bad: ExportTraceServiceRequest = serde_json::from_str(lines[1]).unwrap();
        let s = &bad.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.status.as_ref().unwrap().code, StatusCode::Error as i32);
        assert_eq!(attr(s, "spira.exit_code"), Some(&Value::IntValue(3)));
    }

    fn attr_of<'a>(attrs: &'a [KeyValue], k: &str) -> Option<&'a Value> {
        attrs.iter().find(|a| a.key == k).and_then(|a| a.value.as_ref()?.value.as_ref())
    }

    #[test_id(t9khpf6)]
    #[test]
    fn a_write_after_utc_midnight_opens_the_next_days_file_and_old_files_go() {
        let dir = tmp("rotate");
        fs::create_dir_all(&dir).unwrap();
        for d in ["2023-11-01", "2023-11-10", "2023-11-11", "2023-11-13"] {
            fs::write(dir.join(format!("spira-lc.{d}.jsonl")), "x\n").unwrap();
        }
        fs::write(dir.join("other.2023-11-01.jsonl"), "x\n").unwrap();
        let sink = Sink::new(dir.path(), "spira-lc", 3);
        let before = UNIX_EPOCH + Duration::from_secs(1_700_006_399);
        let midnight = UNIX_EPOCH + Duration::from_secs(1_700_006_400);
        assert_eq!(utc_date(before), "2023-11-14");
        assert_eq!(utc_date(midnight), "2023-11-15", "1_700_006_400 is 2023-11-15T00:00:00Z");
        sink.write(&call(0), midnight - Duration::from_secs(1)).unwrap();
        sink.write(&call(0), midnight).unwrap();
        assert!(dir.join("spira-lc.2023-11-14.jsonl").exists());
        assert!(dir.join("spira-lc.2023-11-15.jsonl").exists());
        assert!(!dir.join("spira-lc.2023-11-01.jsonl").exists(), "older than 3 days");
        assert!(!dir.join("spira-lc.2023-11-10.jsonl").exists());
        assert!(dir.join("spira-lc.2023-11-13.jsonl").exists(), "within the window");
        assert!(dir.join("other.2023-11-01.jsonl").exists(), "another service's files are not ours");
    }

    #[test_id(t46j4z6)]
    #[test]
    fn an_unwritable_sink_drops_and_counts_without_failing() {
        let dir = tmp("blocked");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "x").unwrap();
        let sink = Sink::new(file.join("under-a-file"), "spira-lc", 14);
        sink.emit(&call(0));
        assert_eq!(sink.dropped_here(), 1);
        assert!(dropped() >= 1);
    }

    fn lines_in(path: &Path) -> usize {
        fs::read_to_string(path).map(|t| t.lines().count()).unwrap_or(0)
    }

    #[test_id(tij1vrt)]
    #[test]
    fn spans_past_the_day_bound_are_dropped_and_counted() {
        let dir = tmp("day-bound");
        let one = Sink::new(dir.path(), "spira-lc", 14).inner.line(&call(0)).unwrap().len() as u64;
        let sink = Sink::buffered(dir.path(), "spira-lc", 14, one * 3);
        for _ in 0..5 {
            sink.emit(&call(0));
        }
        sink.flush();
        let file = dir.join(format!("spira-lc.{}.jsonl", utc_date(SystemTime::now())));
        assert_eq!(lines_in(&file), 3);
        assert_eq!(sink.dropped_here(), 2);
    }

    #[test_id(tg8rnrs)]
    #[test]
    fn a_day_at_its_bound_warns_once_however_many_spans_it_drops() {
        let dir = tmp("warn-once");
        let sink = Sink::buffered(dir.path(), "spira-lc", 14, 0);
        for _ in 0..3 {
            sink.emit(&call(0));
            sink.flush();
        }
        assert_eq!(sink.dropped_here(), 3);
        assert_eq!(sink.inner.warned_days.lock().unwrap().len(), 1);
    }

    #[test_id(tzu0pck)]
    #[test]
    fn past_the_queue_bound_emit_drops_and_counts() {
        let dir = tmp("emit-full");
        let sink = Sink::buffered(dir.path(), "spira-lc", 14, u64::MAX);
        {
            let mut p = sink.queue.as_ref().unwrap().pending.lock().unwrap();
            p.bytes = QUEUE_MAX_BYTES;
        }
        sink.emit(&call(0));
        assert_eq!(sink.dropped_here(), 1);
    }

    #[test_id(tt0a5nt)]
    #[test]
    fn a_batch_spanning_utc_midnight_lands_in_both_days_files() {
        let dir = tmp("midnight");
        let sink = Sink::buffered(dir.path(), "spira-lc", 14, u64::MAX);
        let midnight = UNIX_EPOCH + Duration::from_secs(1_700_006_400);
        let line = sink.inner.line(&call(0)).unwrap();
        sink.inner.write_batch(&[(midnight - Duration::from_secs(1), line.clone()), (midnight, line.clone()), (midnight + Duration::from_secs(1), line)]);
        assert_eq!(lines_in(&dir.join("spira-lc.2023-11-14.jsonl")), 1);
        assert_eq!(lines_in(&dir.join("spira-lc.2023-11-15.jsonl")), 2);
    }

    #[test_id(tt5n0n1)]
    #[test]
    fn a_buffered_sink_over_an_unwritable_directory_drops_and_counts() {
        let dir = tmp("buffered-blocked");
        let file = dir.join("file");
        fs::write(&file, "x").unwrap();
        let sink = Sink::buffered(file.join("under-a-file"), "spira-lc", 14, u64::MAX);
        sink.emit(&call(0));
        sink.emit(&call(0));
        sink.flush();
        assert_eq!(sink.dropped_here(), 2);
    }

    #[test_id(t91nkcq)]
    #[test]
    fn the_checked_in_fixture_still_deserializes() {
        let text = include_str!("../tests/fixtures/spira-lc.ndjson");
        assert!(text.lines().count() >= 3);
        for l in text.lines() {
            let r: ExportTraceServiceRequest = serde_json::from_str(l).unwrap();
            assert_eq!(r.resource_spans[0].scope_spans[0].spans.len(), 1);
        }
    }

    #[test_id(tidl5ug)]
    #[test]
    #[ignore = "rewrites tests/fixtures/spira-lc.ndjson from freshly emitted spans"]
    fn regenerate_fixture() {
        let dir = tmp("fixture");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let rows = [("show", "work/show", 61, 1_204, 0, 8), ("list", "work/list", 33, 439_002, 0, 212), ("show", "gate-run", 58, 96, 1, 3), ("event", "lifecycle/event", 412, 0, 0, 41)];
        for (i, (m, who, rq, rs, code, ms)) in rows.into_iter().enumerate() {
            let start = t0 + Duration::from_secs(i as u64);
            let c = Call { method: m, caller: who, request_size: rq, response_size: rs, exit_code: code, start, end: start + Duration::from_millis(ms), queue_wait: Duration::ZERO, before_work: Duration::ZERO, answer_wait: Duration::ZERO, after_answer: Duration::ZERO, cut: false, answered: None, cut_at: None, phases: &[], commits_us: &[], reads: 0, read_max_us: 0, reads_ended: 0, reads_overran: 0, open_phase: None, content_queries: 0, content_query_max_us: 0, content_query_max_shape: "", task: None, children: 0, programs: &[], subverb: None };
            sink.write(&c, start).unwrap();
        }
        fs::copy(dir.join("spira-lc.2023-11-14.jsonl"), testkit::manifest_dir().join("tests/fixtures/spira-lc.ndjson")).unwrap();
    }

    #[test_id(toec17w)]
    #[test]
    fn civil_dates_round_trip() {
        for t in [0u64, 86_400 * 59, 86_400 * 60, 1_700_000_000, 4_102_444_800] {
            let d = utc_date(UNIX_EPOCH + Duration::from_secs(t));
            assert_eq!(day_number(&d), Some((t / 86_400) as i64), "{d}");
        }
    }
}

/// A process exit status a `main` body can return and the telemetry wrapper can read back,
/// which `std::process::ExitCode` does not allow.
pub mod exit {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ExitCode(u8);

    impl ExitCode {
        pub const SUCCESS: ExitCode = ExitCode(0);
        pub const FAILURE: ExitCode = ExitCode(1);

        pub fn code(self) -> i32 {
            i32::from(self.0)
        }
    }

    impl From<u8> for ExitCode {
        fn from(c: u8) -> ExitCode {
            ExitCode(c)
        }
    }

    pub fn finish(code: i32) -> std::process::ExitCode {
        std::process::ExitCode::from(code.clamp(0, 255) as u8)
    }
}
