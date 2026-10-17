//! A fixture OTLP metrics sink for tests: accepts pushes on a local port and decodes them.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::metrics::v1::{metric::Data, number_data_point::Value as Num};
use prost::Message;

/// One data point as received.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub service: String,
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

impl Seen {
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

pub struct Sink {
    addr: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl Sink {
    pub fn start() -> Sink {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let (s2, st2) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            while !st2.load(Ordering::Relaxed) {
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                if let Some(body) = read_request(&mut c) {
                    if let Ok(req) = ExportMetricsServiceRequest::decode(body.as_slice()) {
                        s2.lock().unwrap().extend(flatten(&req));
                    }
                }
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        Sink { addr, seen, stop }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Runs `exe args` in a minimal environment whose config (`toml`) points the metrics endpoint
    /// at this sink, then returns what was pushed. `extra_env` is added to that environment.
    pub fn run(&self, exe: &str, args: &[&str], toml: &std::path::Path, home: &str, extra_env: &[(&str, &str)]) -> Vec<Seen> {
        // batch-job: runs the binary under test to completion
        std::process::Command::new(exe)
            .args(args)
            .env_clear()
            .env("SPIRA_TOML", toml)
            .env("SPIRA_HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .envs(extra_env.iter().copied())
            .output()
            .unwrap();
        self.received(Duration::from_secs(3))
    }

    /// Everything received so far, waiting up to `wait` for at least one point.
    pub fn received(&self, wait: Duration) -> Vec<Seen> {
        let end = std::time::Instant::now() + wait;
        while self.seen.lock().unwrap().is_empty() && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read_request(c: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = c.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
        let len: usize = head.split("content-length: ").nth(1)?.split("\r\n").next()?.parse().ok()?;
        if buf.len() >= i + 4 + len {
            return Some(buf[i + 4..i + 4 + len].to_vec());
        }
    }
}

fn flatten(req: &ExportMetricsServiceRequest) -> Vec<Seen> {
    let mut out = Vec::new();
    for rm in &req.resource_metrics {
        let service = rm
            .resource
            .iter()
            .flat_map(|r| &r.attributes)
            .find(|a| a.key == "service.name")
            .and_then(|a| a.value.as_ref())
            .and_then(|v| if let Some(Value::StringValue(s)) = &v.value { Some(s.clone()) } else { None })
            .unwrap_or_default();
        for m in rm.scope_metrics.iter().flat_map(|s| &s.metrics) {
            let attrs = |a: &[opentelemetry_proto::tonic::common::v1::KeyValue]| {
                a.iter().filter_map(|kv| Some((kv.key.clone(), if let Some(Value::StringValue(s)) = &kv.value.as_ref()?.value { s.clone() } else { String::new() }))).collect::<Vec<_>>()
            };
            let num = |p: &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint| match p.value {
                Some(Num::AsInt(i)) => i as f64,
                Some(Num::AsDouble(d)) => d,
                None => 0.0,
            };
            match &m.data {
                Some(Data::Sum(s)) => out.extend(s.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: num(p) })),
                Some(Data::Gauge(g)) => out.extend(g.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: num(p) })),
                Some(Data::Histogram(h)) => out.extend(h.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: p.sum.unwrap_or(0.0) })),
                _ => {}
            }
        }
    }
    out
}
