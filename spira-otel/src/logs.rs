//! One OTLP log record per unit of work, appended as one `ExportLogsServiceRequest` JSON line to
//! `<dir>/<service>.logs.<UTC date>.jsonl` beside the span files, so the same `otlpjsonfile`
//! receiver ships it. The attributes are an open set the caller supplies; nothing here names a key.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, ArrayValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs, SeverityNumber};
use opentelemetry_proto::tonic::resource::v1::Resource;

static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Records lost to a write error since this process started.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

static QUEUE_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Records refused by a full or closed `LogQueue` since this process started.
pub fn queue_dropped() -> u64 {
    QUEUE_DROPPED.load(Ordering::Relaxed)
}

/// An OTLP attribute value: a scalar or an array of one scalar type.
#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Strs(Vec<String>),
    Ints(Vec<i64>),
    Floats(Vec<f64>),
    Bools(Vec<bool>),
}

fn any(v: Value) -> AnyValue {
    AnyValue { value: Some(v) }
}

fn array<T>(items: &[T], f: impl Fn(&T) -> Value) -> Value {
    Value::ArrayValue(ArrayValue { values: items.iter().map(|i| any(f(i))).collect() })
}

impl Attr {
    fn value(&self) -> Value {
        match self {
            Attr::Str(s) => Value::StringValue(s.clone()),
            Attr::Int(i) => Value::IntValue(*i),
            Attr::Float(f) => Value::DoubleValue(*f),
            Attr::Bool(b) => Value::BoolValue(*b),
            Attr::Strs(v) => array(v, |s| Value::StringValue(s.clone())),
            Attr::Ints(v) => array(v, |i| Value::IntValue(*i)),
            Attr::Floats(v) => array(v, |f| Value::DoubleValue(*f)),
            Attr::Bools(v) => array(v, |b| Value::BoolValue(*b)),
        }
    }
}

pub struct LogSink {
    dir: PathBuf,
    service: String,
}

impl LogSink {
    pub fn new(dir: impl Into<PathBuf>, service: &str) -> LogSink {
        LogSink { dir: dir.into(), service: service.to_string() }
    }

    /// Moves the writes to one thread behind a queue of `cap` records.
    pub fn queue(self, cap: usize) -> LogQueue {
        self.queue_then(cap, |_, _| {})
    }

    /// As `queue`, and `then` runs on the writer thread after each record is written.
    pub fn queue_then(self, cap: usize, then: impl Fn(&str, &[(String, Attr)]) + Send + 'static) -> LogQueue {
        let (tx, rx) = sync_channel::<(String, Vec<(String, Attr)>)>(cap);
        let spawned = std::thread::Builder::new().name("otel-log-writer".into()).spawn(move || {
            while let Ok((body, mut attrs)) = rx.recv() {
                let lost = queue_dropped() + dropped();
                attrs.push(("telemetry.dropped".into(), Attr::Int(i64::try_from(lost).unwrap_or(i64::MAX))));
                self.emit(&body, &attrs);
                then(&body, &attrs);
            }
        });
        if spawned.is_err() {
            QUEUE_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        LogQueue { tx }
    }

    /// Appends one record; a failure is counted, never returned.
    pub fn emit(&self, body: &str, attrs: &[(String, Attr)]) {
        if self.write(body, attrs, SystemTime::now()).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn write(&self, body: &str, attrs: &[(String, Attr)], now: SystemTime) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(&self.request(body, attrs, now)).map_err(std::io::Error::other)?;
        line.push(b'\n');
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{}.logs.{}.jsonl", self.service, crate::utc_date(now)));
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        crate::append_locked(&mut f, &line)
    }

    pub fn request(&self, body: &str, attrs: &[(String, Attr)], now: SystemTime) -> ExportLogsServiceRequest {
        let t = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
        let record = LogRecord {
            time_unix_nano: t,
            observed_time_unix_nano: t,
            severity_number: SeverityNumber::Info as i32,
            body: Some(any(Value::StringValue(body.into()))),
            attributes: attrs.iter().map(|(k, v)| KeyValue { key: k.clone(), value: Some(any(v.value())) }).collect(),
            ..Default::default()
        };
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource { attributes: vec![crate::kv_str("service.name", &self.service)], ..Default::default() }),
                scope_logs: vec![ScopeLogs { scope: Some(InstrumentationScope { name: "spira-otel".into(), ..Default::default() }), log_records: vec![record], ..Default::default() }],
                ..Default::default()
            }],
        }
    }
}

/// A `LogSink` behind a bounded queue: `emit` never blocks, and a record it cannot take is counted.
pub struct LogQueue {
    tx: SyncSender<(String, Vec<(String, Attr)>)>,
}

impl LogQueue {
    pub fn emit(&self, body: &str, attrs: &[(String, Attr)]) {
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.tx.try_send((body.into(), attrs.to_vec())) {
            QUEUE_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Reads back every record a sink wrote into `dir` for `service`: the fixture collector of a test.
pub fn read_all(dir: &std::path::Path, service: &str) -> Vec<Vec<(String, serde_json::Value)>> {
    let prefix = format!("{service}.logs.");
    let mut out = vec![];
    let Ok(entries) = fs::read_dir(dir) else { return out };
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))).collect();
    names.sort();
    for p in names {
        for line in fs::read_to_string(p).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            for rl in v["resourceLogs"].as_array().into_iter().flatten() {
                for sl in rl["scopeLogs"].as_array().into_iter().flatten() {
                    for r in sl["logRecords"].as_array().into_iter().flatten() {
                        out.push(
                            r["attributes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|a| (a["key"].as_str().unwrap_or_default().to_string(), a["value"].clone()))
                                .collect(),
                        );
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_carries_every_attribute_it_was_given_with_its_type() {
        let d = testkit::TempDir::new("otel-logs");
        let s = LogSink::new(d.path(), "svc");
        s.write("dispatch", &[("a.b".into(), Attr::Int(3)), ("c".into(), Attr::Strs(vec!["x".into()])), ("e".into(), Attr::Bool(true))], SystemTime::now()).unwrap();
        let r = read_all(d.path(), "svc");
        assert_eq!(r.len(), 1);
        let get = |k: &str| r[0].iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("a.b")["intValue"], "3");
        assert_eq!(get("c")["arrayValue"]["values"][0]["stringValue"], "x");
        assert_eq!(get("e")["boolValue"], true);
        assert!(read_all(d.path(), "other").is_empty(), "positive control: another service reads nothing");
    }
}
