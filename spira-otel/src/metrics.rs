//! Fire-and-forget OTLP metrics pushed straight to a Prometheus-compatible store (VictoriaMetrics
//! `/opentelemetry/v1/metrics`), protobuf over plain HTTP. Every phase has a deadline; a failed
//! push is counted in `push_failed()`, never returned, and never touches the producer's durable record.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{
    metric::Data, number_data_point::Value as Num, AggregationTemporality, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

static PUSH_FAILED: AtomicU64 = AtomicU64::new(0);

/// Seconds-scale bucket bounds shared by every duration histogram.
pub const SECONDS_BOUNDS: [f64; 12] = [1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0];

pub const PATH: &str = "/opentelemetry/v1/metrics";

/// Pushes pending metrics lost to a connect, write, read or status error since this process started.
pub fn push_failed() -> u64 {
    PUSH_FAILED.load(Ordering::Relaxed)
}

pub enum Point<'a> {
    /// A monotonic counter incremented by `value`.
    Count { name: &'a str, value: u64 },
    /// One observation of a seconds histogram.
    Seconds { name: &'a str, value: f64 },
}

pub struct MetricsSink {
    endpoint: Option<String>,
    service: String,
    deadline: Duration,
}

impl MetricsSink {
    /// `endpoint` is `http://host:port` (or `host:port`); empty or `None` disables the sink.
    pub fn new(endpoint: Option<&str>, service: &str, deadline: Duration) -> MetricsSink {
        let endpoint = endpoint.map(str::trim).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches("http://").trim_end_matches('/').to_string());
        MetricsSink { endpoint, service: service.to_string(), deadline }
    }

    /// Pushes `points`, all carrying `labels`; a failure is counted, never returned.
    pub fn push(&self, points: &[Point], labels: &[(&str, &str)]) {
        if self.try_push(points, labels).is_err() {
            PUSH_FAILED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn try_push(&self, points: &[Point], labels: &[(&str, &str)]) -> std::io::Result<()> {
        let Some(addr) = &self.endpoint else { return Ok(()) };
        post(addr, &self.encode(points, labels, SystemTime::now()).encode_to_vec(), self.deadline)
    }

    pub fn encode(&self, points: &[Point], labels: &[(&str, &str)], now: SystemTime) -> ExportMetricsServiceRequest {
        let t = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
        let attributes: Vec<KeyValue> = labels.iter().map(|(k, v)| kv(k, v)).collect();
        let metrics = points
            .iter()
            .map(|p| match p {
                Point::Count { name, value } => Metric {
                    name: (*name).into(),
                    data: Some(Data::Sum(Sum {
                        data_points: vec![NumberDataPoint { attributes: attributes.clone(), start_time_unix_nano: t, time_unix_nano: t, value: Some(Num::AsInt(*value as i64)), ..Default::default() }],
                        aggregation_temporality: AggregationTemporality::Delta as i32,
                        is_monotonic: true,
                    })),
                    ..Default::default()
                },
                Point::Seconds { name, value } => {
                    let mut buckets = vec![0u64; SECONDS_BOUNDS.len() + 1];
                    buckets[SECONDS_BOUNDS.iter().position(|b| value <= b).unwrap_or(SECONDS_BOUNDS.len())] = 1;
                    Metric {
                        name: (*name).into(),
                        unit: "s".into(),
                        data: Some(Data::Histogram(Histogram {
                            data_points: vec![HistogramDataPoint {
                                attributes: attributes.clone(),
                                start_time_unix_nano: t,
                                time_unix_nano: t,
                                count: 1,
                                sum: Some(*value),
                                bucket_counts: buckets,
                                explicit_bounds: SECONDS_BOUNDS.to_vec(),
                                min: Some(*value),
                                max: Some(*value),
                                ..Default::default()
                            }],
                            aggregation_temporality: AggregationTemporality::Delta as i32,
                        })),
                        ..Default::default()
                    }
                }
            })
            .collect();
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource { attributes: vec![kv("service.name", &self.service)], ..Default::default() }),
                scope_metrics: vec![ScopeMetrics { scope: Some(InstrumentationScope { name: "spira-otel".into(), ..Default::default() }), metrics, ..Default::default() }],
                ..Default::default()
            }],
        }
    }
}

fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }), ..Default::default() }
}

fn post(addr: &str, body: &[u8], deadline: Duration) -> std::io::Result<()> {
    let end = Instant::now() + deadline;
    let left = || end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline"));
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other("no address"))?;
    let mut s = TcpStream::connect_timeout(&target, left()?)?;
    s.set_write_timeout(Some(left()?))?;
    let head = format!("POST {PATH} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.set_read_timeout(Some(left()?))?;
    let mut status = [0u8; 12];
    s.read_exact(&mut status)?;
    match &status[9..12] {
        b"200" | b"202" | b"204" => Ok(()),
        other => Err(std::io::Error::other(format!("status {}", String::from_utf8_lossy(other)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn sink(addr: &str) -> MetricsSink {
        MetricsSink::new(Some(addr), "spira-tsd", Duration::from_millis(300))
    }

    fn serve(reply: &'static str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = c.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                if buf.len() >= i + 4 + len {
                    break;
                }
            }
            c.write_all(reply.as_bytes()).unwrap();
            buf
        });
        (addr, h)
    }

    const LABELS: [(&str, &str); 2] = [("repo", "r"), ("instance_type", "c7a.4xlarge")];

    #[test]
    fn a_histogram_and_a_counter_encode_with_their_labels_and_round_trip() {
        let s = sink("127.0.0.1:1");
        let req = s.encode(&[Point::Seconds { name: "cert_pass_boot_seconds", value: 20.0 }, Point::Count { name: "cert_pass_verdicts_total", value: 1 }], &LABELS, UNIX_EPOCH + Duration::from_secs(5));
        let back = ExportMetricsServiceRequest::decode(req.encode_to_vec().as_slice()).unwrap();
        let m = &back.resource_metrics[0].scope_metrics[0].metrics;
        let Some(Data::Histogram(h)) = &m[0].data else { panic!("histogram") };
        let p = &h.data_points[0];
        assert_eq!((p.count, p.sum), (1, Some(20.0)));
        assert_eq!(p.bucket_counts.iter().sum::<u64>(), 1);
        assert_eq!(p.bucket_counts[2], 0);
        assert_eq!(p.bucket_counts[3], 1, "20s falls in the (15,30] bucket");
        assert_eq!(p.attributes.len(), 2);
        assert_eq!(p.attributes[1].key, "instance_type");
        let Some(Data::Sum(c)) = &m[1].data else { panic!("sum") };
        assert!(c.is_monotonic);
        assert_eq!(c.data_points[0].value, Some(Num::AsInt(1)));
        assert_eq!(c.data_points[0].time_unix_nano, 5_000_000_000);
    }

    #[test]
    fn a_value_past_the_last_bound_lands_in_the_overflow_bucket() {
        let req = sink("x:1").encode(&[Point::Seconds { name: "n", value: 99_999.0 }], &[], UNIX_EPOCH);
        let Some(Data::Histogram(h)) = &req.resource_metrics[0].scope_metrics[0].metrics[0].data else { panic!() };
        assert_eq!(*h.data_points[0].bucket_counts.last().unwrap(), 1);
    }

    #[test]
    fn a_push_posts_protobuf_to_the_otlp_path() {
        let (addr, h) = serve("HTTP/1.1 204 No Content\r\n\r\n");
        sink(&addr).try_push(&[Point::Count { name: "c", value: 2 }], &LABELS).unwrap();
        let got = h.join().unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("POST /opentelemetry/v1/metrics HTTP/1.1"), "{text}");
        assert!(text.contains("application/x-protobuf"));
        let i = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(ExportMetricsServiceRequest::decode(&got[i..]).is_ok());
    }

    #[test]
    fn a_refused_connection_and_a_server_error_are_counted_not_raised() {
        let before = push_failed();
        sink("127.0.0.1:1").push(&[Point::Count { name: "c", value: 1 }], &[]);
        let (addr, h) = serve("HTTP/1.1 500 Internal Server Error\r\n\r\n");
        sink(&addr).push(&[Point::Count { name: "c", value: 1 }], &[]);
        h.join().unwrap();
        assert!(push_failed() >= before + 2);
    }

    #[test]
    fn a_server_that_never_answers_is_cut_off_at_the_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let before = push_failed();
        sink(&addr).push(&[Point::Count { name: "c", value: 1 }], &[]);
        assert!(push_failed() > before);
        drop(l);
    }

    #[test]
    fn an_unset_endpoint_pushes_nothing_and_counts_nothing() {
        let before = push_failed();
        MetricsSink::new(None, "s", Duration::from_millis(50)).push(&[Point::Count { name: "c", value: 1 }], &[]);
        MetricsSink::new(Some("  "), "s", Duration::from_millis(50)).push(&[Point::Count { name: "c", value: 1 }], &[]);
        assert_eq!(push_failed(), before);
    }
}
