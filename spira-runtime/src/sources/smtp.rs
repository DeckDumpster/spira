//! The real `submission` source (`tasks::g::submission`): `spira-submission`'s implicit-TLS SMTP
//! listener inside the scheduler. Each session runs `spira_submission::session` on its own thread
//! against [`EventSink`]: an accepted message is delivered in-process (`mail::sendmail::deliver`,
//! no `mail sendmail` child), then submitted as `submission.accepted` with a reply channel, and
//! the client hears `250` only once the event is journaled, `451` otherwise; every refusal is
//! submitted as `submission.refused`. Replaces `spira-submission-prod.service`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use runtime::host::Submit;
use spira_submission::smtp::{Envelope, Sink};
use tokio::sync::mpsc;
use tokio::sync::oneshot::error::TryRecvError;

use crate::tasks::g::submission::{submit_id, Accepted, Refused, STREAM};

/// How long the listener sleeps between accept attempts when no client is waiting, and how
/// often a dormant source looks whether its channel closed.
const IDLE: Duration = Duration::from_millis(200);

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
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) // clock-safe: stamps a wire event's id on a source thread, not the loop
}

/// The sink one session answers through.
pub struct EventSink {
    pub tx: mpsc::Sender<Submit>,
    pub delivery: Option<Delivery>,
    /// How long the client waits for its event to be journaled before it hears `451`.
    pub reply_within: Duration,
}

impl EventSink {
    fn journaled(&self, s: Submit, mut rx: tokio::sync::oneshot::Receiver<Result<(), String>>) -> Result<(), String> {
        self.tx.blocking_send(s).map_err(|_| "the scheduler is not taking submissions".to_string())?;
        let until = Instant::now() + self.reply_within; // clock-safe: a wire deadline on the session's own thread
        loop {
            match rx.try_recv() {
                Ok(r) => return r,
                Err(TryRecvError::Closed) => return Err("the scheduler dropped the submission".into()),
                Err(TryRecvError::Empty) if Instant::now() >= until => return Err("the scheduler did not journal the submission in time".into()),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)), // clock-safe: waits on the session's own thread for the loop's answer
            }
        }
    }
}

impl Sink for EventSink {
    /// In-process delivery; a message whose id is already in a mailbox is not delivered twice (a
    /// client retrying after a `451` whose delivery had landed).
    fn deliver(&self, message: &[u8]) -> Result<(), String> {
        let d = self.delivery.as_ref().ok_or("no mail environment configured")?;
        let raw = String::from_utf8_lossy(message);
        let mid = mail::message::header_ci_before_blank(&raw, "message-id");
        if mail::sendmail::find_message_by_id(&d.mail_root, &mid).is_some() {
            return Ok(());
        }
        mail::sendmail::deliver(&d.run_dir, &d.home, &d.mail_root, d.mute, &raw, None).map(|_| ())
    }

    fn accept(&self, env: &Envelope, message: &[u8]) -> Result<(), String> {
        self.deliver(message)?;
        let a = Accepted {
            peer: env.peer.into(),
            user: env.user.into(),
            from: env.from.into(),
            to: env.to.to_vec(),
            message_id: env.message_id.into(),
            in_reply_to: Some(env.in_reply_to.to_string()).filter(|s| !s.is_empty()),
        };
        let (s, rx) = Submit::with_reply(&submit_id(Some(env.message_id), env.peer, wall_ms()), STREAM, a.event_kind());
        self.journaled(s, rx)
    }

    fn refused(&self, peer: &str, user: &str, from: &str, reason: &str) {
        let r = Refused { peer: peer.into(), user: user.into(), from: from.into(), reason: reason.into() };
        let _ = self.tx.blocking_send(Submit::new(&submit_id(None, peer, wall_ms()), STREAM, r.event_kind()));
    }
}

/// Parks until `tx` closes: the source registered but not configured.
fn dormant(tx: &mpsc::Sender<Submit>) {
    while !tx.is_closed() {
        std::thread::sleep(IDLE); // clock-safe: a dormant source's own thread, waiting for shutdown
    }
}

/// Runs the listener for `cfg` until `tx` closes; `None` (the box is not configured, as
/// `spira_submission::Config::from_spira` says) is dormant, never a failure. A listener that
/// cannot bind is said once and the source parks dormant.
pub fn run(cfg: Option<spira_submission::Config>, delivery: Option<Delivery>, tx: mpsc::Sender<Submit>) {
    let Some(cfg) = cfg.filter(|c| c.cert.is_file() && c.key.is_file()) else { return dormant(&tx) };
    if tx.is_closed() {
        return;
    }
    let listener = match TcpListener::bind(cfg.socket()).and_then(|l| l.set_nonblocking(true).map(|_| l)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("spira-runtime: source submission: bind {}: {e}", cfg.socket());
            return dormant(&tx);
        }
    };
    let cfg = Arc::new(cfg);
    while !tx.is_closed() {
        match listener.accept() {
            Ok((tcp, _)) => {
                let _ = tcp.set_nonblocking(false);
                let (cfg, sink) = (cfg.clone(), EventSink { tx: tx.clone(), delivery: delivery.clone(), reply_within: spira_submission::IO_TIMEOUT });
                std::thread::spawn(move || spira_submission::session(tcp, &cfg, &sink));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(IDLE), // clock-safe: the listener's own thread between accepts
            Err(e) => {
                eprintln!("spira-runtime: source submission: accept: {e}");
                std::thread::sleep(IDLE); // clock-safe: backs off a failing accept on the listener's own thread
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(tx: mpsc::Sender<Submit>, mail_root: &std::path::Path, run: &std::path::Path) -> EventSink {
        EventSink { tx, delivery: Some(Delivery { run_dir: run.into(), home: run.into(), mail_root: mail_root.into(), mute: true }), reply_within: Duration::from_secs(2) }
    }

    fn envelope<'a>(to: &'a [String]) -> Envelope<'a> {
        Envelope { peer: "10.0.0.2:5555", user: "ryan", from: "me@example.org", to, message_id: "<m1@x>", in_reply_to: "" }
    }

    #[test]
    fn an_accepted_message_is_delivered_then_answered_once_the_event_is_journaled() {
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
        assert_eq!(s.accept(&envelope(&to), b"Message-ID: <m1@x>\nFrom: me@example.org\n\nhello\n"), Ok(()));
        let (id, body) = loop_side.join().unwrap();
        assert_eq!(id, "submission/<m1@x>");
        assert_eq!((body.to, body.in_reply_to), (to, None));
        assert!(mail::sendmail::find_message_by_id(&mail_root, "<m1@x>").is_some(), "delivered in-process");
    }

    #[test]
    fn a_submission_the_loop_refuses_is_a_451_and_a_dropped_one_too() {
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
        assert_eq!(s.accept(&envelope(&to), b"Message-ID: <m1@x>\n\nhello\n"), Err("refused: stream quarantined".into()));
        let again = s.accept(&envelope(&to), b"Message-ID: <m1@x>\n\nhello\n");
        assert!(again.is_err(), "{again:?}");
        loop_side.join().unwrap();
        assert!(mail::sendmail::find_message_by_id(&mail_root, "<m1@x>").is_some(), "the retry did not deliver a second copy, and the first is kept");
    }

    #[test]
    fn a_refusal_is_submitted_with_its_reason() {
        let (tx, mut rx) = mpsc::channel::<Submit>(4);
        EventSink { tx, delivery: None, reply_within: Duration::ZERO }.refused("10.0.0.3:6666", "", "x@y", "relay denied");
        let s = rx.blocking_recv().unwrap();
        let r: Refused = serde_json::from_str(&s.kind.body).unwrap();
        assert_eq!((s.stream.as_str(), r.reason.as_str(), r.peer.as_str()), (STREAM, "relay denied", "10.0.0.3:6666"));
        assert!(s.id.starts_with("submission/10.0.0.3:6666+"), "{}", s.id);
    }

    #[test]
    fn an_unconfigured_listener_is_dormant_until_its_channel_closes() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        run(None, None, tx);
    }
}
