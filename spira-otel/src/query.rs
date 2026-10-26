//! Reads back from the Prometheus-compatible store the metrics sink pushes to (VictoriaMetrics
//! `/api/v1/export`), over plain HTTP. Every failure is a named [`QueryError`]: a reader built on
//! this refuses, it never reports zero for a store it could not read.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub const DEADLINE: Duration = Duration::from_secs(5);
const MAX_BODY: usize = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    NotConfigured,
    Unreachable(String),
    Status(u16, String),
    Malformed(String),
    NoSeries(String),
}

impl QueryError {
    /// The stable name a caller or a test matches on.
    pub fn name(&self) -> &'static str {
        match self {
            QueryError::NotConfigured => "metrics-store-not-configured",
            QueryError::Unreachable(_) => "metrics-store-unreachable",
            QueryError::Status(..) => "metrics-store-refused",
            QueryError::Malformed(_) => "metrics-store-malformed",
            QueryError::NoSeries(_) => "metrics-store-no-series",
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::NotConfigured => write!(f, "{}: SPIRA_METRICS_ENDPOINT is empty, so there is no store to read; refusing to report", self.name()),
            QueryError::Unreachable(e) => write!(f, "{}: {e}; refusing to report", self.name()),
            QueryError::Status(code, body) => write!(f, "{}: status {code}: {body}", self.name()),
            QueryError::Malformed(e) => write!(f, "{}: {e}", self.name()),
            QueryError::NoSeries(what) => write!(f, "{}: {what}; refusing to report an empty window as all-clear", self.name()),
        }
    }
}

impl std::error::Error for QueryError {}

/// One series from `/api/v1/export`: its labels (including `__name__`) and `(ms, value)` samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub labels: BTreeMap<String, String>,
    pub samples: Vec<(i64, f64)>,
}

pub struct Store {
    addr: String,
    deadline: Duration,
}

impl Store {
    /// `endpoint` is `http://host:port` or `host:port`; empty or `None` is [`QueryError::NotConfigured`].
    pub fn new(endpoint: Option<&str>) -> Result<Store, QueryError> {
        let addr = endpoint.map(str::trim).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches("http://").trim_end_matches('/').to_string());
        addr.map(|addr| Store { addr, deadline: DEADLINE }).ok_or(QueryError::NotConfigured)
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Store {
        self.deadline = deadline;
        self
    }

    /// Every sample of the series matching `selector` from `start_secs` (unix) on.
    pub fn export(&self, selector: &str, start_secs: u64) -> Result<Vec<Exported>, QueryError> {
        let path = format!("/api/v1/export?match[]={}&start={start_secs}", encode(selector));
        let body = self.get(&path)?;
        parse_export(&body)
    }

    /// [`Store::export`] bounded above by `end_secs`, so a long window can be read in pieces that each fit a deadline.
    pub fn export_between(&self, selector: &str, start_secs: u64, end_secs: u64) -> Result<Vec<Exported>, QueryError> {
        let path = format!("/api/v1/export?match[]={}&start={start_secs}&end={end_secs}", encode(selector));
        let body = self.get(&path)?;
        parse_export(&body)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, QueryError> {
        let unreachable = |what: &str, e: std::io::Error| QueryError::Unreachable(format!("{} {what}: {e}", self.addr));
        let end = Instant::now() + self.deadline;
        let left = |what: &str| {
            end.checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| QueryError::Unreachable(format!("{} {what}: deadline of {:?} passed", self.addr, self.deadline)))
        };
        let target = self.addr.to_socket_addrs().map_err(|e| unreachable("resolve", e))?.next().ok_or_else(|| QueryError::Unreachable(format!("{}: no address", self.addr)))?;
        let mut s = TcpStream::connect_timeout(&target, left("connect")?).map_err(|e| unreachable("connect", e))?;
        s.set_write_timeout(Some(left("write")?)).map_err(|e| unreachable("write", e))?;
        // HTTP/1.0 so the store answers un-chunked and ends the body by closing.
        let head = format!("GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", self.addr);
        s.write_all(head.as_bytes()).map_err(|e| unreachable("write", e))?;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 64 << 10];
        loop {
            s.set_read_timeout(Some(left("read")?)).map_err(|e| unreachable("read", e))?;
            match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.len() > MAX_BODY {
                        return Err(QueryError::Malformed(format!("response over {MAX_BODY} bytes")));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(unreachable("read", e)),
            }
        }
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| QueryError::Malformed("response has no header end".into()))?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let code: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| QueryError::Malformed(format!("bad status line {:?}", head.lines().next().unwrap_or(""))))?;
        let body = raw[split + 4..].to_vec();
        if code != 200 {
            return Err(QueryError::Status(code, String::from_utf8_lossy(&body).chars().take(200).collect()));
        }
        Ok(body)
    }
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[derive(serde::Deserialize)]
struct Line {
    metric: BTreeMap<String, String>,
    values: Vec<Option<f64>>,
    timestamps: Vec<i64>,
}

fn parse_export(body: &[u8]) -> Result<Vec<Exported>, QueryError> {
    let text = std::str::from_utf8(body).map_err(|e| QueryError::Malformed(format!("export is not utf-8: {e}")))?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let l: Line = serde_json::from_str(line).map_err(|e| QueryError::Malformed(format!("export line: {e}")))?;
        if l.values.len() != l.timestamps.len() {
            return Err(QueryError::Malformed("export values and timestamps differ in length".into()));
        }
        let samples = l.timestamps.into_iter().zip(l.values).filter_map(|(t, v)| Some((t, v?))).collect();
        out.push(Exported { labels: l.metric, samples });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve(reply: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = c.write_all(&reply);
        });
        addr
    }

    #[test]
    fn an_unset_endpoint_is_refused_by_name() {
        for e in [None, Some(""), Some("  ")] {
            assert_eq!(Store::new(e).err().map(|e| e.name()), Some("metrics-store-not-configured"));
        }
    }

    #[test]
    fn a_stopped_store_is_unreachable_not_empty() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let err = Store::new(Some(&addr)).unwrap().export("{a=\"b\"}", 0).unwrap_err();
        assert_eq!(err.name(), "metrics-store-unreachable", "{err}");
    }

    #[test]
    fn a_server_that_never_answers_is_cut_off_at_the_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let err = Store::new(Some(&addr)).unwrap().with_deadline(Duration::from_millis(200)).export("x", 0).unwrap_err();
        assert_eq!(err.name(), "metrics-store-unreachable");
        drop(l);
    }

    #[test]
    fn a_non_200_and_a_garbled_body_are_named() {
        let addr = serve(b"HTTP/1.0 503 Service Unavailable\r\n\r\nbusy".to_vec());
        assert_eq!(Store::new(Some(&addr)).unwrap().export("x", 0).unwrap_err(), QueryError::Status(503, "busy".into()));
        let addr = serve(b"HTTP/1.0 200 OK\r\n\r\nnot json\n".to_vec());
        assert_eq!(Store::new(Some(&addr)).unwrap().export("x", 0).unwrap_err().name(), "metrics-store-malformed");
    }

    #[test]
    fn an_export_body_parses_into_series() {
        let body = b"{\"metric\":{\"__name__\":\"m\",\"k\":\"v\"},\"values\":[1,2.5],\"timestamps\":[1000,2000]}\n";
        let addr = serve([b"HTTP/1.0 200 OK\r\n\r\n".as_slice(), body].concat());
        let got = Store::new(Some(&addr)).unwrap().export("m", 0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].labels["k"], "v");
        assert_eq!(got[0].samples, vec![(1000, 1.0), (2000, 2.5)]);
    }

    #[test]
    fn a_selector_is_percent_encoded() {
        assert_eq!(encode("{a=\"b\"}"), "%7Ba%3D%22b%22%7D");
    }
}
