//! The `margin.changed` source: a long-poll held open on the reader's `GET /api/changes?after=&wait=`,
//! each answer with changes one memory-only event, a failing reader retried on a capped backoff.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use margin_client::{Changes, ChangesRequest, HttpMargin};
use runtime::host::Submit;
use runtime::sources::{on_pool, Running, Source};
use runtime::{EventKind, MARGIN_CHANGED, MARGIN_STREAM, MARGIN_UNREACHABLE};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub const NAME: &str = "margin.changed";
pub const HOLD_SECS: u64 = 30;
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 8 << 20;

/// The body of a `margin.unreachable` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unreachable {
    pub since_ms: u64,
    pub error: String,
}

/// Where the sequence a restart resumes from is kept, and the marks that move it.
pub fn dir(run_dir: &Path) -> PathBuf {
    run_dir.join("margin")
}

fn resume_file(run_dir: &Path) -> PathBuf {
    dir(run_dir).join("through")
}

fn done_dir(run_dir: &Path) -> PathBuf {
    dir(run_dir).join("done")
}

/// The highest sequence whose deliveries all succeeded; zero before any.
pub fn resume_point(run_dir: &Path) -> u64 {
    std::fs::read_to_string(resume_file(run_dir)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// Marks `seq` delivered and moves the resume point as far as `batch` (the actionable sequences of one
/// answer, in order) allows: to just before its first undelivered one, or to `through` when every one is done.
pub fn record_done(run_dir: &Path, seq: u64, batch: &[u64], through: u64) -> Result<(), String> {
    let done = done_dir(run_dir);
    std::fs::create_dir_all(&done).map_err(|e| format!("{}: {e}", done.display()))?;
    std::fs::write(done.join(seq.to_string()), "").map_err(|e| format!("{}: {e}", done.display()))?;
    let at = resume_point(run_dir);
    let floor = batch.iter().find(|s| **s > at && !done.join(s.to_string()).exists()).map_or(through, |first| first.saturating_sub(1));
    if floor <= at {
        return Ok(());
    }
    let (tmp, to) = (dir(run_dir).join("through.tmp"), resume_file(run_dir));
    std::fs::write(&tmp, floor.to_string()).and_then(|()| std::fs::rename(&tmp, &to)).map_err(|e| format!("{}: {e}", to.display()))?;
    for s in batch.iter().filter(|s| **s <= floor) {
        let _ = std::fs::remove_file(done.join(s.to_string()));
    }
    Ok(())
}

pub struct MarginChanged {
    /// `None`: the reader is not configured, and the source parks.
    pub client: Option<HttpMargin>,
    pub run_dir: PathBuf,
    pub hold_secs: u64,
    pub backoff_first: Duration,
    pub backoff_cap: Duration,
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64) // clock-safe: stamps an event on a source thread, not the loop
}

impl MarginChanged {
    pub fn configured(cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let run_dir = PathBuf::from(cfg("SPIRA_RUN")?);
        let client = match cfg(margin_client::URL_KEY)?.trim() {
            "" => None,
            _ => Some(HttpMargin::configured(cfg)?),
        };
        Ok(MarginChanged { client, run_dir, hold_secs: HOLD_SECS, backoff_first: BACKOFF_FIRST, backoff_cap: BACKOFF_CAP })
    }
}

impl Source for MarginChanged {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(self.serve(tx))
    }
}

/// Where a request goes: scheme, the name the certificate must carry, and the addresses it resolved to.
#[derive(Debug)]
struct Target {
    tls: bool,
    host: String,
    addrs: Vec<SocketAddr>,
    path: String,
}

struct Prepared {
    request: ChangesRequest,
    target: Target,
}

fn target_of(url: &str) -> Result<Target, String> {
    let (tls, rest, default_port) = if let Some(r) = url.strip_prefix("https://") {
        (true, r, 443)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r, 80)
    } else {
        return Err(format!("{}: {url} is not an http:// or https:// address", margin_client::URL_KEY));
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (h, p.parse::<u16>().map_err(|_| format!("{}: {url} has no usable port", margin_client::URL_KEY))?),
        _ => (authority, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    let addrs: Vec<SocketAddr> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("the reader at {url}: cannot resolve {host}: {e}"))?.collect();
    Ok(Target { tls, host, addrs, path: path.to_string() })
}

fn prepare(client: &HttpMargin, after: u64, hold: u64) -> Result<Prepared, String> {
    let request = client.changes_request(after, hold)?;
    let target = target_of(&request.url)?;
    Ok(Prepared { request, target })
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map(|b| b.with_root_certificates(roots).with_no_client_auth());
            Arc::new(config.expect("ring provides the default protocol versions"))
        })
        .clone()
}

/// One request on one connection of its own, the whole exchange inside the request's timeout.
async fn exchange(p: &Prepared) -> Result<(u16, String), String> {
    let t = &p.target;
    let whole = async {
        let tcp = tokio::net::TcpStream::connect(t.addrs.as_slice()).await.map_err(|e| format!("connect: {e}"))?;
        if t.tls {
            let name = rustls::pki_types::ServerName::try_from(t.host.clone()).map_err(|e| format!("{}: not a name a certificate can carry: {e}", t.host))?;
            let tls = tokio_rustls::TlsConnector::from(tls_config()).connect(name, tcp).await.map_err(|e| format!("tls: {e}"))?;
            send(p, hyper_util::rt::TokioIo::new(tls)).await
        } else {
            send(p, hyper_util::rt::TokioIo::new(tcp)).await
        }
    };
    tokio::time::timeout(p.request.timeout, whole).await.unwrap_or_else(|_| Err("timed out".to_string()))
}

async fn send<I>(p: &Prepared, io: I) -> Result<(u16, String), String>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io).await.map_err(|e| e.to_string())?;
    let host = if p.target.host.contains(':') { format!("[{}]", p.target.host) } else { p.target.host.clone() };
    let req = hyper::Request::get(&p.target.path)
        .header(hyper::header::HOST, host)
        .header(hyper::header::AUTHORIZATION, format!("Bearer {}", p.request.token))
        .header(hyper::header::ACCEPT, ChangesRequest::ACCEPT)
        .header(hyper::header::CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|e| e.to_string())?;
    let work = async {
        let resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = Limited::new(resp.into_body(), MAX_BODY).collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    };
    tokio::pin!(work, conn);
    tokio::select! {
        biased;
        r = &mut work => r,
        c = &mut conn => match c {
            Err(e) => Err(e.to_string()),
            Ok(()) => work.await,
        },
    }
}

async fn fetch(client: &HttpMargin, after: u64, hold: u64) -> Result<Changes, String> {
    let c = client.clone();
    let p = on_pool("margin request", Arc::new(move || prepare(&c, after, hold))).await.map_err(|e| e.to_string())??;
    match exchange(&p).await {
        Ok((status, body)) => p.request.reply(status, &body),
        Err(why) => Err(p.request.silent(&why)),
    }
}

impl MarginChanged {
    async fn serve(self, tx: mpsc::Sender<Submit>) {
        let Some(client) = &self.client else {
            tx.closed().await;
            return;
        };
        let dir = self.run_dir.clone();
        let Ok(mut after) = on_pool("margin resume", Arc::new(move || resume_point(&dir))).await else {
            let _ = on_pool("source", Arc::new(|| eprintln!("runtime: {NAME}: the resume point could not be read; the source ends"))).await;
            return;
        };
        let mut failing: Option<Duration> = None;
        let mut heard = false;
        loop {
            let hold = if heard && failing.is_none() { self.hold_secs } else { 0 };
            let got = tokio::select! {
                _ = tx.closed() => return,
                got = fetch(client, after, hold) => got,
            };
            match got {
                Ok(answer) => {
                    let said = !heard || failing.is_some();
                    heard = true;
                    failing = None;
                    after = after.max(answer.through);
                    if answer.changes.is_empty() && !said {
                        continue;
                    }
                    let body = serde_json::to_string(&answer).unwrap_or_default();
                    let id = if answer.changes.is_empty() { format!("{MARGIN_CHANGED}/{}/{}", answer.through, wall_ms()) } else { format!("{MARGIN_CHANGED}/{}", answer.through) };
                    if tx.send(Submit::new(&id, MARGIN_STREAM, EventKind::new(MARGIN_CHANGED, &body))).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let wait = match failing {
                        None => {
                            let body = serde_json::to_string(&Unreachable { since_ms: wall_ms(), error }).unwrap_or_default();
                            let id = format!("{MARGIN_UNREACHABLE}/{after}/{}", wall_ms());
                            if tx.send(Submit::new(&id, MARGIN_STREAM, EventKind::new(MARGIN_UNREACHABLE, &body))).await.is_err() {
                                return;
                            }
                            self.backoff_first
                        }
                        Some(last) => (last * 2).min(self.backoff_cap),
                    };
                    failing = Some(wait);
                    tokio::select! {
                        _ = tx.closed() => return,
                        () = tokio::time::sleep(wait) => {} // clock-safe: the retry wait on the source's own task
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::*;
    use runtime::sources::Sources;

    const FIRST: Duration = Duration::from_millis(120);
    const CAP: Duration = Duration::from_millis(240);

    type Seen = Arc<Mutex<Vec<(Instant, String)>>>;

    /// Answers each request with the next scripted `(status, body)`, then stops accepting; returns the base URL and what it saw.
    fn serve(script: Vec<(u16, &'static str)>) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in script {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or_default().to_string();
                log.lock().unwrap().push((Instant::now(), line));
                write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            std::thread::sleep(Duration::from_secs(30)); // clock-safe: keeps the listener bound while the test reads
        });
        (base, seen)
    }

    fn source(base: &str, dir: &testkit::TempDir) -> Box<MarginChanged> {
        let token = dir.path().join("token");
        std::fs::write(&token, "tok").unwrap();
        let client = HttpMargin { base: base.into(), token_file: token.to_str().unwrap().into(), timeout: Duration::from_millis(400) };
        Box::new(MarginChanged { client: Some(client), run_dir: dir.path().into(), hold_secs: 0, backoff_first: FIRST, backoff_cap: CAP })
    }

    fn start(s: Box<MarginChanged>) -> mpsc::Receiver<Submit> {
        let (tx, rx) = mpsc::channel(8);
        let mut sources = Sources::default();
        sources.register(s).unwrap();
        sources.start(tx);
        rx
    }

    #[test_id(tk4pitq)]
    #[tokio::test(flavor = "current_thread")]
    async fn two_answers_with_changes_submit_two_events_in_order_and_the_first_ask_resumes_from_the_recorded_point() {
        let dir = testkit::TempDir::new("margin-source-two");
        std::fs::create_dir_all(super::dir(dir.path())).unwrap();
        std::fs::write(resume_file(dir.path()), "7\n").unwrap();
        let (base, seen) = serve(vec![(200, r#"{"last_seq":9,"changes":[{"seq":8,"kind":"published","collection_id":"c1"}]}"#), (200, r#"{"last_seq":12,"changes":[{"seq":12,"kind":"published","collection_id":"c2"}]}"#)]);
        let mut rx = start(source(&base, &dir));
        let (a, b) = (rx.recv().await.unwrap(), rx.recv().await.unwrap());
        assert_eq!((a.stream.as_str(), a.kind.tag.as_str(), b.kind.tag.as_str()), (MARGIN_STREAM, MARGIN_CHANGED, MARGIN_CHANGED));
        assert!(a.kind.body.contains(r#""through":9"#) && b.kind.body.contains(r#""through":12"#), "{} / {}", a.kind.body, b.kind.body);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked[0].contains("after=7"), "{asked:?}");
        assert!(asked[1].contains("after=9"), "{asked:?}");
    }

    #[test_id(t1srdqm)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_reader_that_refuses_is_said_once_and_asked_again_no_sooner_than_the_backoff() {
        let dir = testkit::TempDir::new("margin-source-down");
        let (base, seen) = serve(vec![(500, "no"), (500, "no"), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#)]);
        let mut rx = start(source(&base, &dir));
        let down = rx.recv().await.unwrap();
        let back = rx.recv().await.unwrap();
        assert_eq!((down.kind.tag.as_str(), back.kind.tag.as_str()), (MARGIN_UNREACHABLE, MARGIN_CHANGED), "three refusals are one unreachable, and the first good answer is said so the task can reset");
        let u: Unreachable = serde_json::from_str(&down.kind.body).unwrap();
        assert!(u.error.contains("HTTP 500") && u.since_ms > 0, "{u:?}");
        let times: Vec<Instant> = seen.lock().unwrap().iter().map(|(t, _)| *t).collect();
        let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[0] >= FIRST && gaps[1] >= CAP && gaps[2] >= CAP, "{gaps:?}");
    }

    #[test_id(tvo7x8m)]
    #[tokio::test(flavor = "current_thread")]
    async fn two_outages_at_one_through_give_recoveries_with_distinct_ids() {
        let dir = testkit::TempDir::new("margin-source-twice");
        let (base, _) = serve(vec![(200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#)]);
        let mut rx = start(source(&base, &dir));
        let mut tags: Vec<(String, String)> = Vec::new();
        for _ in 0..5 {
            let s = rx.recv().await.unwrap();
            tags.push((s.id.clone(), s.kind.tag.to_string()));
        }
        let recoveries: Vec<&String> = tags.iter().filter(|(_, t)| t == MARGIN_CHANGED).map(|(i, _)| i).collect();
        assert_eq!(recoveries.len(), 3, "{tags:?}");
        assert!(recoveries[0] != recoveries[1] && recoveries[1] != recoveries[2] && recoveries[0] != recoveries[2], "{recoveries:?}");
    }

    #[test_id(t4mq567)]
    #[tokio::test(flavor = "current_thread")]
    async fn an_empty_first_answer_is_said_and_retries_ask_without_waiting_though_the_hold_is_set() {
        let dir = testkit::TempDir::new("margin-source-first");
        let (base, seen) = serve(vec![(200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#), (200, r#"{"last_seq":5,"changes":[{"seq":5,"kind":"published","collection_id":"c"}]}"#)]);
        let mut s = source(&base, &dir);
        s.hold_secs = 7;
        let mut rx = start(s);
        let first = rx.recv().await.unwrap();
        assert_eq!(first.kind.tag.as_str(), MARGIN_CHANGED, "an empty first answer is the proof the feed is up");
        assert_eq!(rx.recv().await.unwrap().kind.tag.as_str(), MARGIN_UNREACHABLE);
        assert_eq!(rx.recv().await.unwrap().kind.tag.as_str(), MARGIN_CHANGED);
        assert_eq!(rx.recv().await.unwrap().kind.tag.as_str(), MARGIN_CHANGED);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked[0].contains("wait=0") && asked[1].contains("wait=7") && asked[2].contains("wait=0") && asked[3].contains("wait=7"), "{asked:?}");
    }

    #[test_id(tmx57w7)]
    #[tokio::test(flavor = "current_thread")]
    async fn after_refusals_a_change_arrives_and_is_asked_with_the_same_after() {
        let dir = testkit::TempDir::new("margin-source-same-after");
        let (base, seen) = serve(vec![(500, "no"), (500, "no"), (200, r#"{"last_seq":3,"changes":[{"seq":3,"kind":"published","collection_id":"c"}]}"#)]);
        let mut s = source(&base, &dir);
        s.hold_secs = 7;
        let mut rx = start(s);
        let (down, back) = (rx.recv().await.unwrap(), rx.recv().await.unwrap());
        assert_eq!((down.kind.tag.as_str(), back.kind.tag.as_str()), (MARGIN_UNREACHABLE, MARGIN_CHANGED));
        assert!(back.kind.body.contains(r#""through":3"#), "{}", back.kind.body);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked.iter().all(|l| l.contains("after=0")), "{asked:?}");
    }

    #[test_id(tqhabfo)]
    #[test]
    fn the_resume_point_moves_only_to_just_before_the_first_undelivered_change_of_an_answer() {
        let dir = testkit::TempDir::new("margin-resume");
        assert_eq!(resume_point(dir.path()), 0);
        let batch = [3, 5, 8];
        record_done(dir.path(), 5, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 2, "everything under 3 is delivered or needs nothing; 3 is not delivered yet");
        record_done(dir.path(), 3, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 7, "8 is the first not delivered");
        record_done(dir.path(), 8, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 9);
        assert!(!done_dir(dir.path()).join("5").exists(), "a mark under the resume point is dropped");
    }

    /// The comm of every thread of this process, as the kernel names it (cut to 15 bytes).
    fn thread_comms() -> Vec<String> {
        std::fs::read_dir(std::path::Path::new(system_paths::PROC_SELF).join("task")).unwrap().filter_map(|t| std::fs::read_to_string(t.unwrap().path().join("comm")).ok()).map(|c| c.trim().to_string()).collect()
    }

    const SOURCE_COMM: &str = "source-margin.c";

    #[test_id(tb2n9xk)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_the_reader_holds_costs_no_source_thread_and_leaves_the_loop_running() {
        let planted = {
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let t = std::thread::Builder::new().name("source-margin.changed".into()).spawn(move || {
                let _ = stop_rx.recv();
            }).unwrap();
            while !thread_comms().iter().any(|c| c == SOURCE_COMM) {
                tokio::task::yield_now().await;
            }
            drop(stop_tx);
            t.join().unwrap();
            true
        };
        assert!(planted, "the probe sees a thread named as the old source's was");
        assert!(!thread_comms().iter().any(|c| c == SOURCE_COMM), "the planted thread is gone");

        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf).unwrap();
            let _ = held_tx.send(());
            let _ = release_rx.recv();
        });
        let dir = testkit::TempDir::new("margin-source-held");
        let mut s = source(&base, &dir);
        s.client.as_mut().unwrap().timeout = Duration::from_secs(60);
        let _rx = start(s);
        held_rx.await.unwrap();
        assert_eq!(tokio::spawn(async { 7 }).await.unwrap(), 7, "another task runs on the loop while the reader holds its answer");
        assert!(!thread_comms().iter().any(|c| c == SOURCE_COMM), "{:?}", thread_comms());
        drop(release_tx);
        holder.join().unwrap();
    }

    #[test_id(tr5c8zq)]
    #[tokio::test(flavor = "current_thread")]
    async fn an_https_reader_with_an_untrusted_certificate_is_said_once_naming_the_certificate_and_never_the_token() {
        let fixtures = testkit::manifest_dir().join("../spira-submission/tests/fixtures");
        let read = |f: &str| std::io::BufReader::new(std::fs::File::open(fixtures.join(f)).unwrap());
        let certs = rustls_pemfile::certs(&mut read("test-cert.pem")).collect::<Result<Vec<_>, _>>().unwrap();
        let key = rustls_pemfile::private_key(&mut read("test-key.pem")).unwrap().unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_no_client_auth().with_single_cert(certs, key).unwrap());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("https://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let mut s = stream.unwrap();
                let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
                let _ = conn.complete_io(&mut s);
            }
        });
        let dir = testkit::TempDir::new("margin-source-untrusted");
        let token = dir.path().join("token");
        std::fs::write(&token, "s3cret-bearer").unwrap();
        let client = HttpMargin { base, token_file: token.to_str().unwrap().into(), timeout: Duration::from_secs(5) };
        let mut rx = start(Box::new(MarginChanged { client: Some(client), run_dir: dir.path().into(), hold_secs: 0, backoff_first: FIRST, backoff_cap: CAP }));
        let down = rx.recv().await.unwrap();
        assert_eq!(down.kind.tag.as_str(), MARGIN_UNREACHABLE);
        let u: Unreachable = serde_json::from_str(&down.kind.body).unwrap();
        assert!(u.error.contains("invalid peer certificate") && !u.error.contains("s3cret-bearer"), "{}", u.error);
    }

    #[test_id(t0q3m1c)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_base_that_is_not_http_or_https_is_said_naming_the_config_key() {
        let dir = testkit::TempDir::new("margin-source-scheme");
        let mut rx = start(source("ftp://127.0.0.1:1", &dir));
        let down = rx.recv().await.unwrap();
        let u: Unreachable = serde_json::from_str(&down.kind.body).unwrap();
        assert!(u.error.contains(margin_client::URL_KEY), "{}", u.error);
    }

    #[test_id(tn8v2ud)]
    #[test]
    fn a_base_is_split_into_scheme_host_port_and_path() {
        let t = target_of("http://127.0.0.1:8081/api/changes?after=1&wait=0").unwrap();
        assert_eq!((t.tls, t.host.as_str(), t.addrs.iter().map(|a| a.port()).collect::<Vec<_>>(), t.path.as_str()), (false, "127.0.0.1", vec![8081], "/api/changes?after=1&wait=0")); // not a path: a request target
        let t = target_of("https://127.0.0.1/api/changes").unwrap();
        assert_eq!((t.tls, t.addrs[0].port()), (true, 443), "https defaults to 443");
        let t = target_of("http://[::1]:9/api/changes").unwrap();
        assert_eq!((t.host.as_str(), t.addrs[0].port()), ("::1", 9), "a bracketed address loses its brackets");
        assert!(target_of("http://127.0.0.1:notaport/x").unwrap_err().contains(margin_client::URL_KEY));
    }

    struct Echo;
    impl runtime::Handler for Echo {
        fn interests(&self) -> &[runtime::EventKindTag] {
            &[MARGIN_CHANGED, MARGIN_UNREACHABLE]
        }
        fn handle(&self, ctx: &runtime::Ctx, _: &runtime::Projection, ev: &runtime::Event) -> Vec<runtime::Command> {
            vec![runtime::Command::Emit(EventKind::new("margin.seen", &format!("{}@{}", ev.kind.tag, ctx.now())))]
        }
    }

    struct NoEffects;
    impl runtime::host::EffectRunner for NoEffects {
        fn run(&self, _: &str, _: &runtime::EffectSpec) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn virtual_run() -> Vec<String> {
        use runtime::ports::{Clock, VirtualClock};
        let (j, clock): (Arc<dyn runtime::Journal>, _) = (runtime::MemJournal::new(), VirtualClock::new(0));
        let engine = runtime::Engine::open(j.clone(), Arc::new(clock.clone()), vec![runtime::Registration::new("seen", Echo)], 3).unwrap();
        let script = vec![
            (900, Submit::new("margin.changed/9", MARGIN_STREAM, EventKind::new(MARGIN_CHANGED, "{}"))),
            (250, Submit::new("margin.unreachable/0/1", MARGIN_STREAM, EventKind::new(MARGIN_UNREACHABLE, "{}"))),
        ];
        let mut sources = Sources::default();
        sources.register(Box::new(runtime::sources::ScriptedSource::new(NAME, Arc::new(clock.clone()), script))).unwrap();
        let mut v = runtime::host::VirtualDriver::new(engine, clock.clone(), Arc::new(NoEffects)).with_sources(sources).unwrap();
        v.run_until(2_000).unwrap();
        assert_eq!(clock.now_ms(), 2_000);
        runtime::conformance::trace(&*j).into_iter().filter(|t| t.2 == "margin.seen").map(|t| format!("{}@{}", t.3, t.4)).collect()
    }

    #[test_id(tj9d4wp)]
    #[test]
    fn a_scripted_margin_source_delivers_at_its_virtual_times_the_same_in_two_runs_and_the_real_one_is_refused() {
        let first = virtual_run();
        assert_eq!(first, ["margin.unreachable@250@250", "margin.changed@900@900"]);
        assert_eq!(first, virtual_run());

        let dir = testkit::TempDir::new("margin-source-refused");
        let mut real = Sources::default();
        real.register(source("http://127.0.0.1:1", &dir)).unwrap();
        let err = real.into_scripted().err().expect("a source that reads the real world is refused under virtual time");
        assert!(err.to_string().contains(NAME), "{err}");
    }
}
