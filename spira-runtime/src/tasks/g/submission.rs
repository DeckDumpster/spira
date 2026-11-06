//! Source `submission`: the operator's mail client submitted a message over SMTP. Replaces
//! `spira-submission` (`spira-submission-prod.service`, `Type=simple`, ExecStartPre
//! `world guard`). Pattern P3. Contract: `wiki/projects/spira/designs/tasks/source-submission.md`.
//! The real listener is `tasks::g::sources::smtp`; under virtual time a `ScriptedSource` stands
//! in, and this source is refused.
//!
//! # Tick
//!
//! No timer. The source owns a bound TLS listener for the life of the scheduler, one thread per
//! connection, 5 s IO timeouts. Dormant, not failing, when unconfigured (an empty
//! `SPIRA_SUBMISSION_ADDR` or any empty cert, key, password or from): no listener, no event. Cert
//! and password are re-read per connection.
//!
//! # Observe
//!
//! Per SMTP session: AUTH PLAIN/LOGIN against the password file, `MAIL FROM` equal to the operator
//! address, `RCPT TO` in the recipient domain, at most 20 recipients, DATA at most 10 MiB; the
//! `message-id` and `in-reply-to` headers. No store read.
//!
//! # Decide
//!
//! `smtp::serve_session` is reused unchanged. A finished DATA phase maps to one event:
//! delivered → [`ACCEPTED`] ([`Accepted`]); delivery failed (451), or auth, sender, relay or size
//! refused → [`REFUSED`] ([`Refused`]). Delivery stays a synchronous library call into the `mail`
//! crate's `sendmail` module on the connection thread (a message id already delivered is not
//! delivered again), and the accepted event is submitted with `Submit::with_reply`: the client hears
//! 250 once the event is journaled, 451 when delivery or the journal refused.
//!
//! # Act
//!
//! The submit, `Submit.id` = `submission/<message_id or peer+ts>` ([`submit_id`]). No handler
//! subscribes this round; the events replace the stderr `attempt(...)` audit line.
//!
//! # Silence
//!
//! No connection, no event. A health probe (banner then QUIT, no DATA) submits nothing.
//!
//! # Replaces
//!
//! The unit and its watchd `extern` row; the `mail sendmail` child per accepted message. Pinned
//! config: [`KEYS`]; port 1025–65535 and a non-wildcard address stay config refusals.
//!
//! # Reverse
//!
//! Release rollback restores the unit, together with re-enabling it; a listener cannot shadow.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use runtime::event::{EventKind, EventKindTag};
use runtime::host::Submit;
use runtime::sources::Source;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// The source's registered name.
pub const NAME: &str = "submission";
/// The stream every submission event lands on.
pub const STREAM: &str = "submission";
pub const ACCEPTED: EventKindTag = "submission.accepted";
pub const REFUSED: EventKindTag = "submission.refused";
/// What the real source reads.
pub const BACKEND: &str = "smtp-submission";

pub const KEYS: &[&str] = &[
    "SPIRA_SUBMISSION_ADDR",
    "SPIRA_SUBMISSION_PORT",
    "SPIRA_SUBMISSION_CERT",
    "SPIRA_SUBMISSION_KEY",
    "SPIRA_SUBMISSION_PASSWORD_FILE",
    "SPIRA_SUBMISSION_USER",
    "SPIRA_SUBMISSION_FROM",
    "SPIRA_SUBMISSION_RCPT_DOMAIN",
    "SPIRA_METRICS_ENDPOINT",
];

/// The body of [`ACCEPTED`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub peer: String,
    pub user: String,
    pub from: String,
    pub to: Vec<String>,
    pub message_id: String,
    pub in_reply_to: Option<String>,
}

/// The body of [`REFUSED`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub peer: String,
    pub user: String,
    pub from: String,
    pub reason: String,
}

/// `submission/<message_id>`, or `submission/<peer>+<ts>` for a session with no message id.
pub fn submit_id(message_id: Option<&str>, peer: &str, ts: u64) -> String {
    match message_id {
        Some(m) if !m.is_empty() => format!("submission/{m}"),
        _ => format!("submission/{peer}+{ts}"),
    }
}

impl Accepted {
    pub fn event_kind(&self) -> EventKind {
        EventKind::new(ACCEPTED, &serde_json::to_string(self).unwrap_or_default())
    }
}

impl Refused {
    pub fn event_kind(&self) -> EventKind {
        EventKind::new(REFUSED, &serde_json::to_string(self).unwrap_or_default())
    }
}

/// Reloads asked of the source, counted. The `submission-provision` task's restart bumps it
/// ([`reload`]); the running source ends its listener and reads its config anew when it moves.
static RELOADS: AtomicU64 = AtomicU64::new(0);

/// How often a dormant source reads its config anew on its own, so a first configuration
/// written by `spira-submission provision` from outside the scheduler is picked up too.
pub const DORMANT_RECHECK: Duration = Duration::from_secs(60);

/// Asks the running `source` to reload. Only this source reloads; any other name is refused.
pub fn reload(source: &str) -> Result<String, String> {
    if source != NAME {
        return Err(format!("no source {source:?} reloads here (only {NAME:?}) — refused"));
    }
    let n = RELOADS.fetch_add(1, Ordering::SeqCst) + 1;
    Ok(format!("reloaded source {NAME} (reload {n})"))
}

/// The reloads asked so far.
pub fn reloads() -> u64 {
    RELOADS.load(Ordering::SeqCst)
}

/// Serves until `closed`: each round reads the config anew (`fresh`) and serves it until the
/// channel closes or a reload is asked (`serve` is handed that stop condition).
pub fn serve_reloading<C>(mut fresh: impl FnMut() -> C, mut serve: impl FnMut(C, &dyn Fn() -> bool), closed: &dyn Fn() -> bool) {
    while !closed() {
        let seen = reloads();
        let cfg = fresh();
        serve(cfg, &|| closed() || reloads() != seen);
    }
}

/// The real source: the SMTP submission listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub addr: String,
    pub port: u16,
}

impl Source for Submission {
    fn name(&self) -> &str {
        NAME
    }

    /// The listener, its config (address, certificate, key, password, sender, recipient domain)
    /// and the mail environment it delivers into read anew on every reload; the certificate, key
    /// and password files themselves are read per connection. Dormant while unconfigured, reading
    /// its config again every [`DORMANT_RECHECK`].
    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) {
        let fresh = || spira_submission::Config::from_spira_fresh().unwrap_or_else(|e| {
            eprintln!("spira-runtime: source {NAME}: {e}");
            None
        });
        let serve = |cfg: Option<spira_submission::Config>, stop: &dyn Fn() -> bool| {
            let usable = cfg.as_ref().is_some_and(|c| c.cert.is_file() && c.key.is_file());
            let delivery = cfg.as_ref().filter(|_| usable).and_then(|_| mail::env::Env::load().inspect_err(|e| eprintln!("spira-runtime: source {NAME}: mail: {e}")).ok()).map(|e| super::sources::smtp::Delivery::of(&e));
            let since = std::time::Instant::now(); // clock-safe: a real source's own thread; refused under virtual time
            super::sources::smtp::run_until(cfg, delivery, &tx, &|| stop() || (!usable && since.elapsed() >= DORMANT_RECHECK))
        };
        serve_reloading(fresh, serve, &|| tx.is_closed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn only_the_submission_source_reloads() {
        let before = reloads();
        assert!(reload("mail.arrived").is_err());
        assert_eq!(reloads(), before, "a refused reload asks nothing");
        assert!(reload(NAME).unwrap().contains(NAME));
        assert!(reloads() > before);
    }

    #[test]
    fn a_reload_ends_the_serve_and_the_next_one_reads_the_config_anew() {
        let reads = Cell::new(0);
        let served = RefCell::new(Vec::new());
        let closed = Cell::new(false);
        serve_reloading(
            || {
                reads.set(reads.get() + 1);
                reads.get()
            },
            |cfg, stop| {
                served.borrow_mut().push(cfg);
                assert!(!stop(), "a fresh serve is not stopped");
                if cfg == 1 {
                    reload(NAME).unwrap();
                    assert!(stop(), "a reload stops the serve");
                } else {
                    closed.set(true);
                    assert!(stop(), "a closed channel stops the serve");
                }
            },
            &|| closed.get(),
        );
        assert_eq!(*served.borrow(), vec![1, 2], "the second serve has the config read after the reload");
    }

    #[test]
    fn the_provision_tasks_restart_is_this_sources_reload_not_a_unit() {
        let before = reloads();
        let r = crate::tasks::a::submission_provision::unit_restart(&crate::Effects::default(), NAME);
        assert!(r.as_deref().is_ok_and(|m| m.contains("reloaded source submission")), "{r:?}");
        assert!(reloads() > before);
    }
}
