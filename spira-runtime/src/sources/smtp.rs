//! The real `submission` source (`tasks::g::submission`): `spira-submission`'s implicit-TLS SMTP
//! listener on the scheduler's loop. The listener and each connection are tasks of one `JoinSet`:
//! `tokio-rustls` over `TcpStream`, every read, write and handshake step bounded by
//! `IO_TIMEOUT`, the protocol the pure `spira_submission::smtp::Session`. What blocks (the
//! certificate, the password file, in-process delivery, log lines) goes to the offload pool. An
//! accepted message is delivered, then submitted as `submission.accepted` with a reply channel,
//! and the client hears `250` only once the event is journaled, `451` otherwise; every refusal is
//! submitted as `submission.refused`. A full channel suspends the session whose submission
//! waits, never the loop.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::sources::on_pool;
use spira_submission::smtp::{Envelope, Session, Step};
use spira_submission::IO_TIMEOUT;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

use crate::tasks::g::submission::{submit_id, Accepted, Refused, STREAM};

/// How long a failing accept backs off.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(200);

/// Where an accepted message is delivered: the `mail` crate's own environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub run_dir: PathBuf,
    pub home: PathBuf,
    pub mail_root: PathBuf,
    pub mute: bool,
}

impl Delivery {
    pub fn of(env: &mail::env::Env) -> Self {
        Delivery { run_dir: env.run_dir.clone(), home: env.home.clone(), mail_root: env.mail_root.clone(), mute: env.mute }
    }

    /// In-process delivery; a message whose id is already in a mailbox is not delivered twice (a
    /// client retrying after a `451` whose delivery had landed).
    fn deliver(&self, message: &[u8]) -> Result<(), String> {
        let raw = String::from_utf8_lossy(message);
        let mid = mail::message::header_ci_before_blank(&raw, "message-id");
        if mail::sendmail::find_message_by_id(&self.mail_root, &mid).is_some() {
            return Ok(());
        }
        mail::sendmail::deliver(&self.run_dir, &self.home, &self.mail_root, self.mute, &raw, None).map(|_| ())
    }
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) // clock-safe: stamps a wire event's id, not a decision
}

/// What the sessions answer through.
pub struct EventSink {
    pub tx: mpsc::Sender<Submit>,
    pub delivery: Option<Delivery>,
    /// How long the client waits for its event to be journaled before it hears `451`.
    pub reply_within: Duration,
}

impl EventSink {
    async fn journaled(&self, s: Submit, rx: tokio::sync::oneshot::Receiver<Result<(), String>>) -> Result<(), String> {
        self.tx.send(s).await.map_err(|_| "the scheduler is not taking submissions".to_string())?;
        match timeout(self.reply_within, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the scheduler dropped the submission".into()),
            Err(_) => Err("the scheduler did not journal the submission in time".into()),
        }
    }

    /// Delivers `message`, then submits its event and waits for it to be journaled.
    pub async fn accept(&self, env: &Envelope<'_>, message: Vec<u8>) -> Result<(), String> {
        let delivery = self.delivery.clone().ok_or("no mail environment configured")?;
        on_pool("submission deliver", Arc::new(move || delivery.deliver(&message))).await.map_err(|e| e.to_string())??;
        let a = Accepted {
            peer: env.peer.into(),
            user: env.user.into(),
            from: env.from.into(),
            to: env.to.to_vec(),
            message_id: env.message_id.into(),
            in_reply_to: Some(env.in_reply_to.to_string()).filter(|s| !s.is_empty()),
        };
        let (s, rx) = Submit::with_reply(&submit_id(Some(env.message_id), env.peer, wall_ms()), STREAM, a.event_kind());
        self.journaled(s, rx).await
    }

    pub async fn refused(&self, peer: &str, user: &str, from: &str, reason: &str) {
        let r = Refused { peer: peer.into(), user: user.into(), from: from.into(), reason: reason.into() };
        let _ = self.tx.send(Submit::new(&submit_id(None, peer, wall_ms()), STREAM, r.event_kind())).await;
    }

    async fn attempt(&self, peer: &str, user: &str, from: &str, outcome: &str) {
        let line = spira_submission::smtp::attempt_line(peer, user, from, outcome);
        let _ = on_pool("submission log", Arc::new(move || eprintln!("spira-submission: {line}"))).await;
        if let Some(reason) = spira_submission::smtp::refusal_reason(outcome) {
            self.refused(peer, user, from, reason).await;
        }
    }
}

/// Parks until `stop` or until `tx` closes: the source registered but not configured, or its
/// listener unbound.
async fn dormant(tx: &mpsc::Sender<Submit>, stop: impl Future<Output = ()>) {
    tokio::select! {
        _ = stop => {}
        _ = tx.closed() => {}
    }
}

async fn say(what: String) {
    let _ = on_pool("source", Arc::new(move || eprintln!("spira-runtime: source submission: {what}"))).await;
}

/// Serves `cfg` until `tx` closes or `stop` resolves (a reload); `None` (the box is not
/// configured, as `spira_submission::Config::from_spira` says) is dormant, never a failure. A
/// listener that cannot bind is said once and the source parks dormant. The listener is dropped
/// on return, so the next round can bind the same address; on `stop` the open sessions go on to
/// their end, on a closed channel they are cut.
pub async fn run(cfg: Option<spira_submission::Config>, delivery: Option<Delivery>, tx: mpsc::Sender<Submit>, stop: impl Future<Output = ()>) {
    tokio::pin!(stop);
    let Some(cfg) = cfg else { return dormant(&tx, stop).await };
    let listener = match TcpListener::bind(cfg.socket()).await {
        Ok(l) => l,
        Err(e) => {
            say(format!("bind {}: {e}", cfg.socket())).await;
            return dormant(&tx, stop).await;
        }
    };
    let cfg = Arc::new(cfg);
    let sink = Arc::new(EventSink { tx: tx.clone(), delivery, reply_within: IO_TIMEOUT });
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => {
                drop(listener);
                tokio::spawn(async move { while sessions.join_next().await.is_some() {} });
                return;
            }
            _ = tx.closed() => return,
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    sessions.spawn(connection(tcp, cfg.clone(), sink.clone()));
                }
                Err(e) => {
                    say(format!("accept: {e}")).await;
                    tokio::time::sleep(ACCEPT_BACKOFF).await; // clock-safe: backs off a failing accept on the source's own task
                }
            },
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
}

/// One connection, start to finish: the TLS handshake with the certificate read now, then the
/// SMTP session against `sink`.
async fn connection(tcp: TcpStream, cfg: Arc<spira_submission::Config>, sink: Arc<EventSink>) {
    let peer = tcp.peer_addr().map_or("unknown".to_string(), |a| a.to_string());
    let (c, k) = (cfg.cert.clone(), cfg.key.clone());
    let tls_cfg = match on_pool("submission tls", Arc::new(move || spira_submission::server_config(&c, &k))).await.map_err(|e| e.to_string()).and_then(|r| r) {
        Ok(c) => c,
        Err(e) => return sink.attempt(&peer, "", "", &format!("tls: refused: {e}")).await,
    };
    let mut tls = match timeout(IO_TIMEOUT, TlsAcceptor::from(tls_cfg).accept(tcp)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return sink.attempt(&peer, "", "", &format!("tls: refused: handshake failed: {e}")).await,
        Err(_) => return sink.attempt(&peer, "", "", "tls: refused: handshake failed: timed out").await,
    };
    let policy = cfg.policy(peer.clone());
    let mut session = Session::new(policy.clone());
    let mut steps: std::collections::VecDeque<Step> = session.start().into();
    'conn: loop {
        while let Some(step) = steps.pop_front() {
            match step {
                Step::Send(text) => {
                    let wrote = timeout(IO_TIMEOUT, async { tls.write_all(text.as_bytes()).await?; tls.flush().await }).await;
                    if !matches!(wrote, Ok(Ok(()))) {
                        break 'conn;
                    }
                }
                Step::Attempt { user, from, outcome } => sink.attempt(&peer, &user, &from, &outcome).await,
                Step::Verify { password } => {
                    let p = policy.clone();
                    let ok = on_pool("submission credential", Arc::new({
                        let password = password.clone();
                        move || spira_submission::smtp::verify_credential(&p, &password)
                    }))
                    .await
                    .unwrap_or(false);
                    steps.extend(session.verified(ok));
                }
                Step::Accept { envelope, message } => steps.extend(session.accepted(sink.accept(&envelope.as_envelope(), message).await)),
                Step::Close => break 'conn,
            }
        }
        let mut chunk = [0u8; 4096];
        match timeout(IO_TIMEOUT, tls.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => steps.extend(session.feed(&chunk[..n])),
            _ => break,
        }
    }
    let _ = timeout(IO_TIMEOUT, tls.shutdown()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::g::submission::Accepted;

    fn sink(tx: mpsc::Sender<Submit>, mail_root: &std::path::Path, run: &std::path::Path) -> EventSink {
        EventSink { tx, delivery: Some(Delivery { run_dir: run.into(), home: run.into(), mail_root: mail_root.into(), mute: true }), reply_within: Duration::from_secs(3600) }
    }

    fn envelope<'a>(to: &'a [String]) -> Envelope<'a> {
        Envelope { peer: "10.0.0.2:5555", user: "ryan", from: "me@example.org", to, message_id: "<m1@x>", in_reply_to: "" }
    }

    #[test_id(t0ndsz1)]
    #[tokio::test]
    async fn an_accepted_message_is_delivered_then_answered_once_the_event_is_journaled() {
        let dir = testkit::TempDir::new("submission-sink");
        let (mail_root, run) = (dir.path().join("mail"), dir.path().join("run"));
        std::fs::create_dir_all(&run).unwrap();
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        let loop_side = std::thread::spawn(move || {
            let s = rx.blocking_recv().unwrap();
            let body: Accepted = serde_json::from_str(&s.kind.body).unwrap();
            let _ = s.reply.unwrap().send(Ok(()));
            (s.id, body)
        });
        let to = vec!["concierge@spira".to_string()];
        let s = sink(tx, &mail_root, &run);
        assert_eq!(s.accept(&envelope(&to), b"Message-ID: <m1@x>\nFrom: me@example.org\n\nhello\n".to_vec()).await, Ok(()));
        let (id, body) = loop_side.join().unwrap();
        assert_eq!(id, "submission/<m1@x>");
        assert_eq!((body.to, body.in_reply_to), (to, None));
        assert!(mail::sendmail::find_message_by_id(&mail_root, "<m1@x>").is_some(), "delivered in-process");
    }

    #[test_id(tyai4mj)]
    #[tokio::test]
    async fn a_submission_the_loop_refuses_is_a_451_and_a_dropped_one_too() {
        let dir = testkit::TempDir::new("submission-refuse");
        let (mail_root, run) = (dir.path().join("mail"), dir.path().join("run"));
        std::fs::create_dir_all(&run).unwrap();
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        let loop_side = std::thread::spawn(move || {
            let s = rx.blocking_recv().unwrap();
            let _ = s.reply.unwrap().send(Err("refused: stream quarantined".into()));
            drop(rx.blocking_recv());
        });
        let to = vec!["concierge@spira".to_string()];
        let s = sink(tx, &mail_root, &run);
        let msg = || b"Message-ID: <m1@x>\n\nhello\n".to_vec();
        assert_eq!(s.accept(&envelope(&to), msg()).await, Err("refused: stream quarantined".into()));
        let again = s.accept(&envelope(&to), msg()).await;
        assert!(again.is_err(), "{again:?}");
        loop_side.join().unwrap();
        assert!(mail::sendmail::find_message_by_id(&mail_root, "<m1@x>").is_some(), "the retry did not deliver a second copy, and the first is kept");
    }

    #[test_id(tnjzxn2)]
    #[tokio::test]
    async fn a_refusal_is_submitted_with_its_reason() {
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        EventSink { tx, delivery: None, reply_within: Duration::ZERO }.refused("10.0.0.3:6666", "", "x@y", "relay denied").await;
        let s = rx.recv().await.unwrap();
        let r: Refused = serde_json::from_str(&s.kind.body).unwrap();
        assert_eq!((s.stream.as_str(), r.reason.as_str(), r.peer.as_str()), (STREAM, "relay denied", "10.0.0.3:6666"));
        assert!(s.id.starts_with("submission/10.0.0.3:6666+"), "{}", s.id);
    }

    #[test_id(ti36rq1)]
    #[tokio::test]
    async fn an_unconfigured_listener_is_dormant_until_its_channel_closes() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        run(None, None, tx, std::future::pending()).await;
    }

    #[test_id(t3vz3ke)]
    #[tokio::test]
    async fn an_unanswered_submission_is_a_451_after_the_wait_and_the_loop_is_not_held() {
        let dir = testkit::TempDir::new("submission-timeout");
        let (mail_root, run) = (dir.path().join("mail"), dir.path().join("run"));
        std::fs::create_dir_all(&run).unwrap();
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        let mut s = sink(tx, &mail_root, &run);
        s.reply_within = Duration::from_millis(20);
        let to = vec!["concierge@spira".to_string()];
        let other = tokio::spawn(async { 1 + 1 });
        let got = s.accept(&envelope(&to), b"Message-ID: <m9@x>\n\nhi\n".to_vec()).await;
        assert_eq!(got, Err("the scheduler did not journal the submission in time".into()));
        assert_eq!(other.await.unwrap(), 2, "another task ran meanwhile");
        assert!(rx.try_recv().is_ok(), "the event was submitted");
    }

    struct Client {
        tls: tokio_rustls::client::TlsStream<TcpStream>,
        buf: Vec<u8>,
    }

    impl Client {
        async fn connect(addr: std::net::SocketAddr) -> Client {
            let tcp = loop {
                match TcpStream::connect(addr).await {
                    Ok(t) => break t,
                    Err(_) => tokio::task::yield_now().await,
                }
            };
            let name = tokio_rustls::rustls::pki_types::ServerName::IpAddress(addr.ip().into());
            let tls = tokio_rustls::TlsConnector::from(spira_submission::probe_config()).connect(name, tcp).await.unwrap();
            Client { tls, buf: Vec::new() }
        }

        async fn line(&mut self) -> String {
            timeout(Duration::from_secs(30), async {
                loop {
                    if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                        let l: Vec<u8> = self.buf.drain(..=i).collect();
                        return String::from_utf8_lossy(&l).trim_end().to_string();
                    }
                    let mut chunk = [0u8; 1024];
                    let n = self.tls.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "the server closed the connection");
                    self.buf.extend_from_slice(&chunk[..n]);
                }
            })
            .await
            .expect("the server answered")
        }

        async fn say(&mut self, text: &str) {
            self.tls.write_all(format!("{text}\r\n").as_bytes()).await.unwrap();
        }
    }

    struct Box_ {
        dir: testkit::TempDir,
        cfg: spira_submission::Config,
        delivery: Delivery,
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn fixture(name: &str) -> PathBuf {
        testkit::manifest_dir().join("../spira-submission/tests/fixtures").join(name)
    }

    fn setup(name: &str) -> Box_ {
        use std::os::unix::fs::PermissionsExt;
        let dir = testkit::TempDir::new(name);
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "hunter2\n").unwrap();
        std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o600)).unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let cfg = spira_submission::Config {
            addr: "127.0.0.1".parse().unwrap(),
            port: free_port(),
            cert: fixture("test-cert.pem"),
            key: fixture("test-key.pem"),
            password_file: pw,
            user: "ryan".into(),
            from: "me@example.org".into(),
            rcpt_domain: "spira".into(),
            sendmail: vec![],
        };
        let delivery = Delivery { run_dir: run.clone(), home: run, mail_root: dir.path().join("mail"), mute: true };
        Box_ { dir, cfg, delivery }
    }

    fn plain(user: &str, pass: &str) -> String {
        let raw = format!("\0{user}\0{pass}");
        let t = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for ch in raw.as_bytes().chunks(3) {
            let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                out.push(if i <= ch.len() { t[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }

    async fn submit_message(c: &mut Client, id: &str) {
        c.line().await;
        c.say(&format!("AUTH PLAIN {}", plain("ryan", "hunter2"))).await;
        assert!(c.line().await.starts_with("235"));
        c.say("MAIL FROM:<me@example.org>").await;
        assert!(c.line().await.starts_with("250"));
        c.say("RCPT TO:<concierge@spira>").await;
        assert!(c.line().await.starts_with("250"));
        c.say("DATA").await;
        assert!(c.line().await.starts_with("354"));
        c.say(&format!("Message-ID: {id}\r\nFrom: me@example.org\r\n\r\nhello\r\n.")).await;
    }

    #[test_id(tghal24)]
    #[tokio::test]
    async fn a_tls_session_hears_250_only_after_the_reply_and_451_on_refusal_while_the_loop_runs() {
        let b = setup("submission-tls");
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let addr = b.cfg.socket();
        let server = tokio::spawn(run(Some(b.cfg.clone()), Some(b.delivery.clone()), tx, async { let _ = stop_rx.await; }));
        let mut c = Client::connect(addr).await;
        submit_message(&mut c, "<a1@x>").await;
        let held = rx.recv().await.unwrap();
        assert_eq!(held.id, "submission/<a1@x>");
        let silent = timeout(Duration::from_millis(100), c.line()).await;
        assert!(silent.is_err(), "no answer before the reply: {silent:?}");
        assert_eq!(tokio::spawn(async { 1 + 1 }).await.unwrap(), 2, "another task runs while the client waits");
        held.reply.unwrap().send(Ok(())).unwrap();
        assert!(c.line().await.starts_with("250 2.0.0 queued"));

        c.say("MAIL FROM:<me@example.org>").await;
        assert!(c.line().await.starts_with("250"));
        c.say("RCPT TO:<concierge@spira>").await;
        assert!(c.line().await.starts_with("250"));
        c.say("DATA").await;
        assert!(c.line().await.starts_with("354"));
        c.say("Message-ID: <a2@x>\r\n\r\nagain\r\n.").await;
        rx.recv().await.unwrap().reply.unwrap().send(Err("refused: stream quarantined".into())).unwrap();
        assert!(c.line().await.starts_with("451 4.3.0"));
        c.say("QUIT").await;
        assert!(c.line().await.starts_with("221"));
        let _ = stop_tx.send(());
        server.await.unwrap();
        drop(b.dir);
    }

    #[test_id(tseja1q)]
    #[tokio::test]
    async fn a_reload_rebinds_the_same_address_and_lets_an_open_session_finish() {
        let b = setup("submission-reload");
        let (tx, _rx) = mpsc::channel::<Submit>(4);
        let addr = b.cfg.socket();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let first = tokio::spawn(run(Some(b.cfg.clone()), Some(b.delivery.clone()), tx.clone(), async { let _ = stop_rx.await; }));
        let mut open = Client::connect(addr).await;
        assert!(open.line().await.starts_with("220"));
        stop_tx.send(()).unwrap();
        first.await.unwrap();
        let (stop2_tx, stop2_rx) = tokio::sync::oneshot::channel::<()>();
        let second = tokio::spawn(run(Some(b.cfg.clone()), Some(b.delivery.clone()), tx, async { let _ = stop2_rx.await; }));
        let mut fresh = Client::connect(addr).await;
        assert!(fresh.line().await.starts_with("220"), "the same address is bound again");
        open.say("NOOP").await;
        assert!(open.line().await.starts_with("250"), "the session open at the reload finished its exchange");
        stop2_tx.send(()).unwrap();
        second.await.unwrap();
    }
}
