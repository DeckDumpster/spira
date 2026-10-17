//! Pushes through `MetricsSink` to a real VictoriaMetrics container and queries the series back.
//! Skips when no container runtime can start the image (set `SPIRA_VM_IMAGE` to override it).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use spira_otel::metrics::{MetricsSink, Point};

const IMAGE: &str = "docker.io/victoriametrics/victoria-metrics:v1.102.1";

struct Vm(String);

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = Command::new("podman").args(["rm", "-f", &self.0]).output();
    }
}

fn http(addr: &str, req: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn start() -> Option<(Vm, String)> {
    let image = std::env::var("SPIRA_VM_IMAGE").unwrap_or_else(|_| IMAGE.into());
    let name = format!("spira-otel-vm-{}", std::process::id());
    let out = Command::new("podman").args(["run", "-d", "--rm", "--name", &name, "-p", "127.0.0.1::8428", &image, "-retentionPeriod=1d", "-search.latencyOffset=0s"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let vm = Vm(name.clone());
    let port = Command::new("podman").args(["port", &name, "8428"]).output().ok()?;
    let addr = format!("127.0.0.1:{}", String::from_utf8_lossy(&port.stdout).trim().rsplit(':').next()?);
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        if TcpStream::connect(&addr).is_ok() && http(&addr, "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").contains("OK") {
            return Some((vm, addr));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

fn query(addr: &str, q: &str) -> String {
    let enc: String = q.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    http(addr, &format!("GET /api/v1/query?query={enc} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"))
}

#[test]
fn pushed_points_are_queryable_series() {
    let Some((_vm, addr)) = start() else {
        eprintln!("skipped: no container runtime for {IMAGE}");
        return;
    };
    let sink = MetricsSink::new(Some(&addr), "spira-test", Duration::from_secs(5));
    let labels = [("verb", "show")];
    sink.try_push(&[Point::Count { name: "spira_bead_store_calls", value: 1 }, Point::Seconds { name: "spira_bead_store_call_seconds", value: 0.25 }], &labels).unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let (mut calls, mut secs) = (String::new(), String::new());
    while Instant::now() < end {
        calls = query(&addr, "sum_over_time(spira_bead_store_calls{verb=\"show\"}[1h])");
        secs = query(&addr, "quantile_over_time(0.5, spira_bead_store_call_seconds{verb=\"show\"}[1h])");
        if calls.contains("\"1\"") && secs.contains("0.25") {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("series never appeared:\ncalls={calls}\nsecs={secs}\nmetrics={}", http(&addr, "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").lines().filter(|l| l.contains("rows_dropped")).collect::<Vec<_>>().join("\n"));
}
