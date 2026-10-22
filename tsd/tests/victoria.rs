//! A family row pushed to a real VictoriaMetrics container reads back as the same row, and the stand-in store (spira_otel::fixture::FakeStore) gives the identical answers.
//! Skips when no container runtime can start the image (set `SPIRA_VM_IMAGE` to override it).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use tsd::store::{push_row, Reader};

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
    let name = format!("tsd-vm-{}", std::process::id());
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

fn row(ts: &str, fields: &[(&str, Value)]) -> String {
    let fields: Vec<(String, Value)> = fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    tsd::build_row(ts, "h1", "bead-stage", &fields).unwrap()
}

#[test]
fn rows_pushed_to_a_real_store_read_back_as_the_same_rows() {
    let Some((_vm, addr)) = start() else {
        eprintln!("skipped: no container runtime for {IMAGE}");
        return;
    };
    scenario(&addr);
}

#[test]
fn the_stand_in_store_gives_the_same_answers_as_the_real_one() {
    let fake = spira_otel::fixture::FakeStore::start();
    scenario(fake.addr());
}

fn scenario(addr: &str) {
    let addr = addr.to_string();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let t = |back: u64| tsd::iso_utc(now - back);
    let sent = vec![
        row(&t(300), &[("seq", 1.into()), ("machine", "bead".into()), ("key", "sp-a".into()), ("to_state", "READY".into()), ("applied", true.into())]),
        row(&t(200), &[("seq", 2.into()), ("machine", "bead".into()), ("key", "sp-a".into()), ("to_state", "WORKING".into()), ("applied", true.into()), ("instance", "prod".into())]),
        row(&t(100), &[("seq", 3.into()), ("machine", "bead".into()), ("key", "sp-a".into()), ("to_state", "LANDED".into()), ("applied", false.into()), ("secs", 2.5.into())]),
    ];
    for line in &sent {
        push_row(Some(&addr), "bead-stage", line).unwrap();
    }
    let reader = Reader::new(Some(&addr)).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut got = Vec::new();
    while Instant::now() < end {
        got = reader.rows_or_empty("bead-stage", now - 3600).unwrap();
        if got.len() == sent.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let want: Vec<Value> = sent.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(got, want, "labels the store added would show here");
    assert!(reader.rows("sentinel-phase", now - 3600).unwrap_err().name() == "metrics-store-no-series");
}
