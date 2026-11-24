//! Source `submission`: the operator's mail client submitted a message over SMTP. Replaces
//! `spira-submission` (`spira-submission-prod.service`, `Type=simple`, ExecStartPre
//! `world guard`). Pattern P3. Contract: `wiki/projects/spira/designs/tasks/source-submission.md`.
//! The real listener is `tasks::g::sources::smtp`; under virtual time a `ScriptedSource` stands
//! in, and this source is refused.
//!
//! # Tick
//!
//! No timer. The source owns a bound TLS listener for the life of the scheduler, one task per
//! connection on the scheduler's loop, 5 s IO timeouts. Dormant, not failing, when unconfigured (an empty
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
//! The pure `smtp::Session` holds the protocol. A finished DATA phase maps to one event:
//! delivered → [`ACCEPTED`] ([`Accepted`]); delivery failed (451), or auth, sender, relay or size
//! refused → [`REFUSED`] ([`Refused`]). Delivery is a library call into the `mail`
//! crate's `sendmail` module on the offload pool (a message id already delivered is not
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

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use runtime::event::{EventKind, EventKindTag};
use runtime::host::Submit;
use runtime::sources::{on_pool, Running, Source};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

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
static RELOADS: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// How often a dormant source reads its config anew on its own, so a first configuration
/// written by `spira-submission provision` from outside the scheduler is picked up too.
pub const DORMANT_RECHECK: Duration = Duration::from_secs(60);

/// Asks the running `source` to reload. Only this source reloads; any other name is refused.
pub fn reload(source: &str) -> Result<String, String> {
    if source != NAME {
        return Err(format!("no source {source:?} reloads here (only {NAME:?}) — refused"));
    }
    let mut n = 0;
    RELOADS.send_modify(|count| {
        *count += 1;
        n = *count;
    });
    Ok(format!("reloaded source {NAME} (reload {n})"))
}

/// The reloads asked so far.
pub fn reloads() -> u64 {
    *RELOADS.borrow()
}

/// Serves until `closed`: each round reads the config anew (`fresh`) and serves it until the
/// channel closes or a reload is asked (`serve` is handed the reload watch, which resolves
/// `changed` for a reload asked since this round began).
pub async fn serve_reloading<C, F, G>(mut fresh: impl FnMut() -> F, mut serve: impl FnMut(C, watch::Receiver<u64>) -> G, closed: impl Fn() -> bool)
where
    F: std::future::Future<Output = C>,
    G: std::future::Future<Output = ()>,
{
    while !closed() {
        let reloads = RELOADS.subscribe();
        let cfg = fresh().await;
        serve(cfg, reloads).await;
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
    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(async move {
            let fresh = || async {
                on_pool(NAME, Arc::new(read_config)).await.unwrap_or((None, false, None))
            };
            let serve = |(cfg, usable, delivery): Round, mut reload: watch::Receiver<u64>| {
                let tx = tx.clone();
                async move {
                    let recheck = async {
                        if usable {
                            std::future::pending::<()>().await
                        } else {
                            tokio::time::sleep(DORMANT_RECHECK).await // waits-on: the config a provision run may write from outside the scheduler; cap DORMANT_RECHECK
                        }
                    };
                    let stop = async {
                        tokio::select! {
                            _ = reload.changed() => {}
                            _ = recheck => {}
                        }
                    };
                    super::sources::smtp::run(cfg.filter(|_| usable), delivery, tx, stop).await
                }
            };
            serve_reloading(fresh, serve, || tx.is_closed()).await
        })
    }
}

type Round = (Option<spira_submission::Config>, bool, Option<super::sources::smtp::Delivery>);

/// The config and the mail environment a round serves, read anew: blocking, so on the pool.
fn read_config() -> Round {
    let cfg = spira_submission::Config::from_spira_fresh().unwrap_or_else(|e| {
        eprintln!("spira-runtime: source {NAME}: {e}");
        None
    });
    let usable = cfg.as_ref().is_some_and(|c| c.cert.is_file() && c.key.is_file());
    let delivery = cfg.as_ref().filter(|_| usable).and_then(|_| mail::env::Env::load().inspect_err(|e| eprintln!("spira-runtime: source {NAME}: mail: {e}")).ok()).map(|e| super::sources::smtp::Delivery::of(&e));
    (cfg, usable, delivery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::{Mutex, MutexGuard};

    /// RELOADS is process-global: every test that calls [`reload`] or reads [`reloads`] holds this,
    /// so no sibling's reload lands between a test's own read and its assertion (sp-anhnd6).
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test_id(t46s03p)]
    #[test]
    fn only_the_submission_source_reloads() {
        let _serial = serial();
        let before = reloads();
        assert!(reload("mail.arrived").is_err());
        assert_eq!(reloads(), before, "a refused reload asks nothing");
        assert!(reload(NAME).unwrap().contains(NAME));
        assert_eq!(reloads(), before + 1, "its own reload, and only that");
    }

    #[test_id(tw1b2hy)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_reload_ends_the_serve_and_the_next_one_reads_the_config_anew() {
        let _serial = serial();
        let reads = Cell::new(0);
        let served = RefCell::new(Vec::new());
        let closed = Cell::new(false);
        serve_reloading(
            || {
                reads.set(reads.get() + 1);
                std::future::ready(reads.get())
            },
            |cfg, mut stop| {
                served.borrow_mut().push(cfg);
                let (served, closed) = (&served, &closed);
                async move {
                    assert!(!stop.has_changed().unwrap(), "a fresh serve is not stopped");
                    if cfg == 1 {
                        reload(NAME).unwrap();
                        stop.changed().await.unwrap();
                    } else {
                        closed.set(true);
                        assert!(!served.borrow().is_empty());
                    }
                }
            },
            || closed.get(),
        )
        .await;
        assert_eq!(*served.borrow(), vec![1, 2], "the second serve has the config read after the reload");
    }

    #[test_id(tiv23by)]
    #[test]
    fn the_provision_tasks_restart_is_this_sources_reload_not_a_unit() {
        let _serial = serial();
        let before = reloads();
        let r = crate::tasks::a::submission_provision::unit_restart(&crate::Effects::default(), NAME);
        assert!(r.as_deref().is_ok_and(|m| m.contains("reloaded source submission")), "{r:?}");
        assert_eq!(reloads(), before + 1, "its own reload, and only that");
    }
}
