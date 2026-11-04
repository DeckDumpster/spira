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

    /// Dormant, until `tx` closes, when no address is configured; otherwise the listener, with
    /// the rest of its config (certificate, key, password, sender, recipient domain) and the mail
    /// environment it delivers into read once as it starts.
    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) {
        let configured = !self.addr.is_empty() && self.port != 0;
        let cfg = if configured { spira_submission::Config::from_spira().unwrap_or_else(|e| { eprintln!("spira-runtime: source {NAME}: {e}"); None }) } else { None };
        let delivery = cfg.as_ref().and_then(|_| mail::env::Env::load().inspect_err(|e| eprintln!("spira-runtime: source {NAME}: mail: {e}")).ok()).map(|e| super::sources::smtp::Delivery::of(&e));
        super::sources::smtp::run(cfg, delivery, tx)
    }
}
