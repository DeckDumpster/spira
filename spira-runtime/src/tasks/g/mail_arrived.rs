//! Source `mail.arrived`: a registered mailbox received mail. Replaces the watch half of
//! `spira/spira-mail-deliver.sh` (`spira-mail-deliver-prod.service`, `Type=simple`). Pattern P3.
//! Contract: `wiki/projects/spira/designs/tasks/source-mail-arrived.md`. Not ported yet: the real
//! source's [`Source::run`] is `unimplemented!`; under virtual time a `ScriptedSource` stands in.
//!
//! # Tick
//!
//! No timer. One file watch per registered mailbox on `<mailbox>/new` (close-write and moved-to),
//! for every `SPIRA_MAIL_READERS` line `<mailbox>=<wake cmd>`, through a portable watch crate
//! (inotify on Linux, kqueue elsewhere). Unregistered mailboxes are never watched. One catch-up
//! submit per mailbox at source start, because mail already unread gets no watch event.
//!
//! # Observe
//!
//! Off the wire: the watch event and the `X-Spira-Kind` header of each file in `new/`. Submits
//! [`ARRIVED`] with body [`Arrived`] `{mailbox, kind_event}` on stream `mail/<mailbox>`
//! ([`stream`]). The consumer is the `mail.wake` handler (settle, then wake with backoff), which
//! is a task of its own and not this module.
//!
//! # Decide
//!
//! The source decides nothing beyond which mailbox and whether the newest file is an event-kind
//! mail; settle and backoff belong to `mail.wake`.
//!
//! # Act
//!
//! The submit, `Submit.id` = `mail.arrived/<mailbox>/<seq>`. No effect.
//!
//! # Silence
//!
//! No watch event: nothing.
//!
//! # Replaces
//!
//! The unit and its watchd `extern` row and lease files (liveness becomes the source's
//! registration in `scheduler status`, S13); children `inotifywait` per mailbox, `flock`, `awk`,
//! `ls`, `sleep`, `date`. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit. A source has no Observe mode.

use std::path::PathBuf;

use runtime::event::{EventKind, EventKindTag};
use runtime::host::Submit;
use runtime::sources::Source;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// The source's registered name.
pub const NAME: &str = "mail.arrived";
/// The event it submits.
pub const ARRIVED: EventKindTag = "mail.arrived";
/// What the real source reads.
pub const BACKEND: &str = "inotify";

pub const KEYS: &[&str] = &["SPIRA_MAIL_READERS", "SPIRA_MAIL"];

/// The body of [`ARRIVED`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrived {
    pub mailbox: String,
    pub kind_event: bool,
}

/// The stream a mailbox's arrivals are submitted on.
pub fn stream(mailbox: &str) -> String {
    format!("mail/{mailbox}")
}

impl Arrived {
    pub fn event_kind(&self) -> EventKind {
        EventKind::new(ARRIVED, &serde_json::to_string(self).unwrap_or_default())
    }

    pub fn from_event_kind(k: &EventKind) -> Option<Arrived> {
        (k.tag == ARRIVED).then(|| serde_json::from_str(&k.body).ok()).flatten()
    }

    /// The submission of this arrival, the `seq`th on its mailbox.
    pub fn submit(&self, seq: u64) -> Submit {
        Submit::new(&format!("{ARRIVED}/{}/{seq}", self.mailbox), &stream(&self.mailbox), self.event_kind())
    }
}

/// The real source: one watch per registered mailbox directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailArrived {
    pub mailboxes: Vec<(String, PathBuf)>,
}

impl Source for MailArrived {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, _tx: mpsc::Sender<Submit>) {
        unimplemented!("mail.arrived: not ported yet")
    }
}
