//! VictoriaLogs, both ways: a best-effort shipper for event records and a LogsQL count reader.
//! Shipping never blocks and never retries; reading names every failure, so a count that could
//! not be read is an error, never zero.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

const INSERT_PATH: &str = "/insert/jsonline?_stream_fields=stream,kind_tag&_time_field=at&_msg_field=kind_tag";
const QUERY_PATH: &str = "/select/logsql/query";
const DEADLINE: Duration = Duration::from_secs(3);

/// Records the shipper queues before it drops.
pub const QUEUE: usize = 4096;
/// Records one POST carries at most.
const BATCH: usize = 256;

/// The first bead id (`sp-` and a slug, with any `.n` child suffix) in `text`, if any.
pub fn bead_id(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(p) = text[i..].find("sp-") {
        let at = i + p;
        let start_ok = at == 0 || !b[at - 1].is_ascii_alphanumeric();
        let mut end = at + 3;
        while end < b.len() && (b[end].is_ascii_lowercase() || b[end].is_ascii_digit()) {
            end += 1;
        }
        if start_ok && end > at + 3 {
            while end + 1 < b.len() && b[end] == b'.' && b[end + 1].is_ascii_digit() {
                end += 1;
                while end < b.len() && b[end].is_ascii_digit() {
                    end += 1;
                }
            }
            return Some(text[at..end].to_string());
        }
        i = at + 3;
    }
    None
}

/// What the shipper has lost: a full queue drops, a failed POST counts, and neither is retried.
#[derive(Clone, Debug, Default)]
pub struct ShipCounts {
    dropped: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
}

impl ShipCounts {
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct Shipper {
    tx: SyncSender<String>,
    counts: ShipCounts,
}

impl Shipper {
    /// `url` is `http://host:port`; the one thread posts until every clone of the shipper is gone.
    pub fn start(url: &str) -> Shipper {
        Self::with_capacity(url, QUEUE)
    }

    pub fn with_capacity(url: &str, capacity: usize) -> Shipper {
        let (tx, rx) = sync_channel(capacity);
        let counts = ShipCounts::default();
        let (addr, failed) = (host_port(url), counts.failed.clone());
        let spawned = std::thread::Builder::new().name("vlogs-ship".into()).spawn(move || post_loop(&addr, rx, &failed));
        drop(spawned);
        Shipper { tx, counts }
    }

    /// Queue one record; a full queue drops it and counts.
    pub fn ship(&self, record: &Value) {
        let line = record.to_string();
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.tx.try_send(line) {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn counts(&self) -> ShipCounts {
        self.counts.clone()
    }
}

fn host_port(url: &str) -> String {
    url.trim().trim_start_matches("http://").trim_end_matches('/').to_string()
}

fn post_loop(addr: &str, rx: Receiver<String>, failed: &AtomicU64) {
    while let Ok(first) = rx.recv() {
        let mut body = first;
        for _ in 1..BATCH {
            match rx.try_recv() {
                Ok(line) => {
                    body.push('\n');
                    body.push_str(&line);
                }
                Err(_) => break,
            }
        }
        if post(addr, &body).is_err() {
            failed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn connect(addr: &str, end: Instant) -> std::io::Result<TcpStream> {
    let left = || end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline passed"));
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))?;
    let s = TcpStream::connect_timeout(&target, left()?)?;
    s.set_write_timeout(Some(left()?))?;
    s.set_read_timeout(Some(left()?))?;
    Ok(s)
}

fn post(addr: &str, body: &str) -> std::io::Result<()> {
    let mut s = connect(addr, Instant::now() + DEADLINE)?;
    write!(s, "POST {INSERT_PATH} HTTP/1.0\r\nHost: {addr}\r\nContent-Type: application/stream+json\r\nContent-Length: {}\r\n\r\n{body}", body.len())?;
    let mut head = Vec::new();
    s.read_to_end(&mut head)?;
    let code: u16 = String::from_utf8_lossy(&head).split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("status {code}")))
    }
}

/// The stream and kind a lifecycle transition is shipped under; the counts read them back.
pub const TRANSITION_STREAM: &str = "lifecycle";
pub const TRANSITION_KIND: &str = "transition";

/// One lifecycle transition as shipped: `at` in epoch seconds.
pub fn transition_record(machine: &str, event: &str, from: &str, to: &str, bead_id: &str, at: u64, actor: &str) -> Value {
    serde_json::json!({
        "stream": TRANSITION_STREAM, "kind_tag": TRANSITION_KIND,
        "machine": machine, "event": event, "from": from, "to": to, "bead_id": bead_id, "at": at, "actor": actor,
    })
}

/// The LogsQL filter for every transition within the last `window_secs`.
pub fn transitions_window(window_secs: u64) -> String {
    format!("_time:{window_secs}s kind_tag:{TRANSITION_KIND}")
}

/// The LogsQL filter for transitions of `machine` within the last `window_secs`, optionally into state `to`.
pub fn transitions_filter(machine: &str, to: Option<&str>, window_secs: u64) -> String {
    let quote = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
    let mut q = format!("_time:{window_secs}s kind_tag:{TRANSITION_KIND} machine:{}", quote(machine));
    if let Some(to) = to {
        q.push_str(&format!(" to:{}", quote(to)));
    }
    q
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogsError {
    NotConfigured,
    Unreachable(String),
    Status(u16, String),
    Malformed(String),
}

impl std::fmt::Display for LogsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogsError::NotConfigured => write!(f, "victorialogs-not-configured: SPIRA_VICTORIALOGS_URL is empty"),
            LogsError::Unreachable(e) => write!(f, "victorialogs-unreachable: {e}"),
            LogsError::Status(c, b) => write!(f, "victorialogs-refused: status {c}: {b}"),
            LogsError::Malformed(e) => write!(f, "victorialogs-malformed: {e}"),
        }
    }
}

impl std::error::Error for LogsError {}

fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// One group of a `stats by` count: its field values and the count.
pub type Group = (BTreeMap<String, String>, u64);

/// The one LogsQL reader: `filter` is the part of a query before the `| stats`.
pub struct Logs {
    addr: String,
}

impl Logs {
    pub fn new(url: &str) -> Result<Logs, LogsError> {
        let addr = host_port(url);
        if addr.is_empty() {
            return Err(LogsError::NotConfigured);
        }
        Ok(Logs { addr })
    }

    /// How many records match `filter`.
    pub fn count(&self, filter: &str) -> Result<u64, LogsError> {
        let rows = self.rows(&format!("{filter} | stats count() as n"))?;
        match rows.as_slice() {
            [] => Ok(0),
            [row] => number(row, "n"),
            _ => Err(LogsError::Malformed("a count answered more than one row".into())),
        }
    }

    /// How many records match `filter`, per value of the field `by`.
    pub fn count_by(&self, filter: &str, by: &str) -> Result<BTreeMap<String, u64>, LogsError> {
        let mut out = BTreeMap::new();
        for row in self.rows(&format!("{filter} | stats by ({by}) count() as n"))? {
            let key = row.get(by).and_then(Value::as_str).unwrap_or("").to_string();
            out.insert(key, number(&row, "n")?);
        }
        Ok(out)
    }

    /// How many records match `filter`, per combination of the fields `by`; a record lacking a field counts under "".
    pub fn count_groups(&self, filter: &str, by: &[&str]) -> Result<Vec<Group>, LogsError> {
        let mut out = vec![];
        for row in self.rows(&format!("{filter} | stats by ({}) count() as n", by.join(", ")))? {
            let keys = by.iter().map(|k| (k.to_string(), row.get(*k).and_then(Value::as_str).unwrap_or("").to_string())).collect();
            out.push((keys, number(&row, "n")?));
        }
        Ok(out)
    }

    /// The newest `at` among records matching `filter`, in the unit it was shipped in.
    pub fn newest_at(&self, filter: &str) -> Result<Option<u64>, LogsError> {
        let rows = self.rows(&format!("{filter} | stats max(at) as newest"))?;
        match rows.first() {
            Some(row) if row.get("newest").and_then(Value::as_str).is_some_and(|s| !s.is_empty() && s != "NaN") => number(row, "newest").map(Some),
            _ => Ok(None),
        }
    }

    fn rows(&self, query: &str) -> Result<Vec<Value>, LogsError> {
        let body = self.get(&format!("{QUERY_PATH}?query={}", encode(query)))?;
        let text = std::str::from_utf8(&body).map_err(|e| LogsError::Malformed(format!("not utf-8: {e}")))?;
        text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| LogsError::Malformed(format!("row: {e}")))).collect()
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, LogsError> {
        let down = |what: &str, e: std::io::Error| LogsError::Unreachable(format!("{} {what}: {e}", self.addr));
        let mut s = connect(&self.addr, Instant::now() + DEADLINE).map_err(|e| down("connect", e))?;
        write!(s, "GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", self.addr).map_err(|e| down("write", e))?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).map_err(|e| down("read", e))?;
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| LogsError::Malformed("response has no header end".into()))?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let code: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| LogsError::Malformed(format!("bad status line {:?}", head.lines().next().unwrap_or(""))))?;
        let body = raw[split + 4..].to_vec();
        if code != 200 {
            return Err(LogsError::Status(code, String::from_utf8_lossy(&body).chars().take(200).collect()));
        }
        Ok(body)
    }
}

fn number(row: &Value, key: &str) -> Result<u64, LogsError> {
    let v = row.get(key).ok_or_else(|| LogsError::Malformed(format!("row has no {key}")))?;
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or_else(|| LogsError::Malformed(format!("{key} is not a count: {v}")))
}

/// A stand-in VictoriaLogs for tests: records every insert body and answers selects from a script.
pub mod fake {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    pub struct Script {
        pub status: u16,
        pub body: String,
        /// When set, the connection is held open and never answered.
        pub hang: bool,
    }

    pub struct FakeLogs {
        addr: String,
        inserts: Arc<Mutex<Vec<(String, String)>>>,
        queries: Arc<Mutex<Vec<String>>>,
        answers: Arc<Mutex<Vec<(String, Script)>>>,
        stop: Arc<AtomicBool>,
    }

    impl FakeLogs {
        pub fn start() -> FakeLogs {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            let addr = l.local_addr().unwrap().to_string();
            let f = FakeLogs { addr, inserts: Arc::default(), queries: Arc::default(), answers: Arc::default(), stop: Arc::default() };
            let (ins, qs, ans, stop) = (f.inserts.clone(), f.queries.clone(), f.answers.clone(), f.stop.clone());
            std::thread::spawn(move || {
                let mut held = vec![];
                while !stop.load(Ordering::Relaxed) {
                    let Ok((mut c, _)) = l.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    c.set_nonblocking(false).unwrap();
                    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let Some((target, body)) = read_request(&mut c) else { continue };
                    let script = if target.starts_with("/insert") {
                        ins.lock().unwrap().push((target, body));
                        Script { status: 200, ..Script::default() }
                    } else {
                        let query = decode(target.split("query=").nth(1).unwrap_or(""));
                        qs.lock().unwrap().push(query.clone());
                        ans.lock().unwrap().iter().find(|(sub, _)| query.contains(sub.as_str())).map(|(_, s)| s.clone()).unwrap_or(Script { status: 200, ..Script::default() })
                    };
                    if script.hang {
                        held.push(c);
                        continue;
                    }
                    let _ = write!(c, "HTTP/1.0 {} X\r\nContent-Length: {}\r\n\r\n{}", script.status, script.body.len(), script.body);
                }
            });
            f
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// A query containing `fragment` is answered with `script`; the first match wins.
        pub fn answer(&self, fragment: &str, script: Script) {
            self.answers.lock().unwrap().push((fragment.into(), script));
        }

        /// Every JSON line inserted so far, parsed.
        pub fn records(&self) -> Vec<Value> {
            self.inserts.lock().unwrap().iter().flat_map(|(_, b)| b.lines().filter_map(|l| serde_json::from_str(l).ok())).collect()
        }

        /// The request targets (path and query) of every insert.
        pub fn insert_targets(&self) -> Vec<String> {
            self.inserts.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
        }

        pub fn queries(&self) -> Vec<String> {
            self.queries.lock().unwrap().clone()
        }

        /// Records inserted, waiting up to `wait` for at least `n`.
        pub fn wait_records(&self, n: usize, wait: Duration) -> Vec<Value> {
            let end = Instant::now() + wait;
            while self.records().len() < n && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.records()
        }
    }

    impl Drop for FakeLogs {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 3 <= b.len() {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn read_request(c: &mut TcpStream) -> Option<(String, String)> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = c.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&buf[..i]).into_owned();
            let target = head.split_whitespace().nth(1)?.to_string();
            let len: usize = head.to_lowercase().split("content-length: ").nth(1).and_then(|r| r.split("\r\n").next()).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            if buf.len() >= i + 4 + len {
                return Some((target, String::from_utf8_lossy(&buf[i + 4..i + 4 + len]).into_owned()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeLogs, Script};
    use super::*;
    use serde_json::json;

    #[test]
    fn a_shipped_record_reaches_the_listener_on_the_insert_path_with_the_stream_fields() {
        let f = FakeLogs::start();
        Shipper::start(&f.url()).ship(&json!({"stream": "s", "kind_tag": "k", "at": 1}));
        assert_eq!(f.wait_records(1, Duration::from_secs(3)), vec![json!({"stream": "s", "kind_tag": "k", "at": 1})]);
        assert_eq!(f.insert_targets(), vec![INSERT_PATH.to_string()]);
    }

    #[test]
    fn a_full_queue_drops_and_counts_without_blocking() {
        let hung = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Shipper::with_capacity(&format!("http://{}", hung.local_addr().unwrap()), 2);
        let began = Instant::now();
        for i in 0..50 {
            s.ship(&json!({"i": i}));
        }
        assert!(began.elapsed() < Duration::from_secs(1), "{:?}", began.elapsed());
        assert!(s.counts().dropped() >= 40, "dropped {}", s.counts().dropped());
    }

    use std::net::TcpListener;

    #[test]
    fn a_failed_post_is_counted_and_not_retried() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let s = Shipper::start(&url);
        s.ship(&json!({"i": 1}));
        let end = Instant::now() + Duration::from_secs(3);
        while s.counts().failed() == 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.counts().failed(), 1);
        assert_eq!(s.counts().dropped(), 0);
    }

    #[test]
    fn the_bead_id_is_the_first_sp_id_with_its_child_suffix() {
        assert_eq!(bead_id("watch.sp-neicv8.x").as_deref(), Some("sp-neicv8"));
        assert_eq!(bead_id("see sp-94ptu6.2 and sp-abc").as_deref(), Some("sp-94ptu6.2"));
        assert_eq!(bead_id("wasp-xyz then sp-q1").as_deref(), Some("sp-q1"));
        assert_eq!(bead_id("no id, just sp- and spiral"), None);
    }

    #[test]
    fn a_count_reads_the_stats_row_and_an_unreadable_store_is_an_error_not_zero() {
        let f = FakeLogs::start();
        f.answer("stats count()", Script { status: 200, body: "{\"n\":\"7\"}\n".into(), hang: false });
        let logs = Logs::new(&f.url()).unwrap();
        assert_eq!(logs.count("_time:24h to:CLOSED").unwrap(), 7);
        assert!(f.queries()[0].starts_with("_time:24h to:CLOSED | stats count() as n"));
        let down = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", down.local_addr().unwrap());
        drop(down);
        assert!(matches!(Logs::new(&url).unwrap().count("x"), Err(LogsError::Unreachable(_))));
        assert_eq!(Logs::new("").err(), Some(LogsError::NotConfigured));
    }

    #[test]
    fn a_transition_filter_quotes_its_values_and_a_record_carries_the_fields_it_filters_on() {
        assert_eq!(transitions_filter("bead", Some("CLOSED"), 86_400), "_time:86400s kind_tag:transition machine:\"bead\" to:\"CLOSED\"");
        assert_eq!(transitions_filter("a\"b", None, 60), "_time:60s kind_tag:transition machine:\"a\\\"b\"");
        let r = transition_record("bead", "Claim", "READY", "WORKING", "sp-a", 7, "dwarf-x");
        for (k, v) in [("machine", "bead"), ("from", "READY"), ("to", "WORKING"), ("bead_id", "sp-a"), ("actor", "dwarf-x"), ("kind_tag", "transition")] {
            assert_eq!(r[k], v, "{k}");
        }
        assert_eq!(r["at"], 7);
    }

    #[test]
    fn a_refusal_and_a_garbled_row_are_named() {
        let f = FakeLogs::start();
        f.answer("bad", Script { status: 400, body: "syntax".into(), hang: false });
        f.answer("garbled", Script { status: 200, body: "{\"n\":\"x\"}\n".into(), hang: false });
        let logs = Logs::new(&f.url()).unwrap();
        assert_eq!(logs.count("bad"), Err(LogsError::Status(400, "syntax".into())));
        assert!(matches!(logs.count("garbled"), Err(LogsError::Malformed(_))));
    }
}
