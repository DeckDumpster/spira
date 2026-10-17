//! One OTLP span per call, appended as one `ExportTraceServiceRequest` JSON line to
//! `<dir>/<service>.<UTC date>.jsonl` — the Collector file-exporter format, so an
//! `otlpjsonfile` receiver can ship the files unchanged. Emitting never fails or blocks the
//! caller beyond one short append: an error is counted and dropped.

pub mod fixture;
pub mod metrics;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, InstrumentationScope, KeyValue};
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
    dir: PathBuf,
    service: String,
    keep_days: u32,
}

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
}

/// Spans lost to a write error since this process started.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

impl Sink {
    pub fn new(dir: impl Into<PathBuf>, service: &str, keep_days: u32) -> Sink {
        Sink { dir: dir.into(), service: service.to_string(), keep_days }
    }

    /// Appends one span; a failure is counted, never returned.
    pub fn emit(&self, call: &Call) {
        if self.write(call, SystemTime::now()).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn write(&self, call: &Call, now: SystemTime) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(&self.request(call)).map_err(std::io::Error::other)?;
        line.push(b'\n');
        fs::create_dir_all(&self.dir)?;
        let day = utc_date(now);
        let path = self.path(&day);
        let first_today = !path.exists();
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        append_locked(&mut f, &line)?;
        if first_today {
            self.prune(&day);
        }
        Ok(())
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
        ];
        if failed {
            attributes.push(KeyValue { key: "spira.exit_code".into(), value: Some(AnyValue { value: Some(Value::IntValue(i64::from(c.exit_code))) }), ..Default::default() });
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

fn kv_str(k: &str, v: &str) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }), ..Default::default() }
}

fn kv_int(k: &str, v: u64) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::IntValue(v as i64)) }), ..Default::default() }
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

fn append_locked(f: &mut File, line: &[u8]) -> std::io::Result<()> {
    // SAFETY: the descriptor is open for the lifetime of `f`; the lock drops with the file.
    if unsafe { flock(f.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
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
        Call { method: "list", caller: "work/list", request_size: 40, response_size: 439_002, exit_code: code, start, end: start + Duration::from_millis(12), queue_wait: Duration::ZERO }
    }

    fn tmp(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(name)
    }

    fn attr<'a>(span: &'a Span, k: &str) -> Option<&'a Value> {
        span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.as_ref()?.value.as_ref())
    }

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

    #[test]
    fn a_write_after_utc_midnight_opens_the_next_days_file_and_old_files_go() {
        let dir = tmp("rotate");
        fs::create_dir_all(&dir).unwrap();
        for d in ["2023-11-01", "2023-11-10", "2023-11-11", "2023-11-13"] {
            fs::write(dir.join(format!("spira-lc.{d}.jsonl")), "x\n").unwrap();
        }
        fs::write(dir.join("other.2023-11-01.jsonl"), "x\n").unwrap();
        let sink = Sink::new(dir.path(), "spira-lc", 3);
        let before = UNIX_EPOCH + Duration::from_secs(1_700_006_399 + 86_400 * 0);
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

    #[test]
    fn an_unwritable_sink_drops_and_counts_without_failing() {
        let dir = tmp("blocked");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "x").unwrap();
        let sink = Sink::new(file.join("under-a-file"), "spira-lc", 14);
        let before = dropped();
        sink.emit(&call(0));
        assert_eq!(dropped(), before + 1);
    }

    #[test]
    fn per_span_overhead_is_far_under_a_millisecond() {
        let dir = tmp("overhead");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        sink.emit(&call(0));
        let n = 500;
        let t = std::time::Instant::now();
        for _ in 0..n {
            sink.emit(&call(0));
        }
        let per = t.elapsed() / n;
        eprintln!("per-span overhead: {per:?}");
        assert!(per < Duration::from_millis(1), "{per:?} per span");
    }

    #[test]
    fn the_checked_in_fixture_still_deserializes() {
        let text = include_str!("../tests/fixtures/spira-lc.ndjson");
        assert!(text.lines().count() >= 3);
        for l in text.lines() {
            let r: ExportTraceServiceRequest = serde_json::from_str(l).unwrap();
            assert_eq!(r.resource_spans[0].scope_spans[0].spans.len(), 1);
        }
    }

    #[test]
    #[ignore = "rewrites tests/fixtures/spira-lc.ndjson from freshly emitted spans"]
    fn regenerate_fixture() {
        let dir = tmp("fixture");
        let sink = Sink::new(dir.path(), "spira-lc", 14);
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let rows = [("show", "work/show", 61, 1_204, 0, 8), ("list", "work/list", 33, 439_002, 0, 212), ("show", "gate-run", 58, 96, 1, 3), ("event", "lifecycle/event", 412, 0, 0, 41)];
        for (i, (m, who, rq, rs, code, ms)) in rows.into_iter().enumerate() {
            let start = t0 + Duration::from_secs(i as u64);
            let c = Call { method: m, caller: who, request_size: rq, response_size: rs, exit_code: code, start, end: start + Duration::from_millis(ms), queue_wait: Duration::ZERO };
            sink.write(&c, start).unwrap();
        }
        fs::copy(dir.join("spira-lc.2023-11-14.jsonl"), concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/spira-lc.ndjson")).unwrap();
    }

    #[test]
    fn civil_dates_round_trip() {
        for t in [0u64, 86_400 * 59, 86_400 * 60, 1_700_000_000, 4_102_444_800] {
            let d = utc_date(UNIX_EPOCH + Duration::from_secs(t));
            assert_eq!(day_number(&d), Some((t / 86_400) as i64), "{d}");
        }
    }
}
