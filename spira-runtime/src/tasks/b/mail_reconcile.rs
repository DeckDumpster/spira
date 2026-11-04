//! # mail.reconcile — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/mail-reconcile.md`;
//! its Acceptance section is the test file `tests/task_b_mail_reconcile.rs`.
//!
//! ## Tick
//!
//! - Cadence **60 s** (unit: `OnBootSec=1min`, `OnUnitActiveSec=1min`, `AccuracySec=10s`,
//!   `Persistent=true` — which does nothing on a monotonic timer). First tick 60 s after boot.
//! - Also woken by `mail.arrived` (the S9 source replacing mail-deliver) on any mailbox: a reply
//!   is the only input that creates work.
//! - Single-flight: one observe in flight; a tick that lands while one is outstanding is dropped
//!   (the next tick repeats the whole pass anyway, `reconcile.rs:195`).
//!
//! ## Observe
//!
//! `mail.reconcile.observe` returns:
//!
//! ```text
//! Snapshot { replies: Vec<Reply> }
//! Reply { key, in_reply_to, actor, para, orig: Option<Orig>, done: BTreeSet<String>, fails: BTreeMap<String,u32> }
//! Orig  { bead, kind, work_bead, subject }
//! ```
//!
//! - Replies: every message in `<SPIRA_MAIL>/*/{new,cur}` younger than `WINDOW_S` = 3 days
//!   (`reconcile.rs:24,210-214`) with an `In-Reply-To` and kind ≠ `revision` (`:217`), keyed
//!   by its own Message-ID or the filename stem (`:220-221`), skipping those whose ledger says
//!   `complete` (`:226`). The original is found by `find_message_by_id` (`:121`). The maildir
//!   is mail's own store, so reading it is asking the component that decides.
//! - `done`/`fails`: the per-message ledger under `<SPIRA_RUN>/mail-reconcile/<key>`
//!   (`main.rs:320`, `reconcile.rs:41-46`). Moves to the task's carried state is **not**
//!   proposed: the ledger is what makes a store outage converge, and it must survive a
//!   restart (Probe/Timer do not, `runtime/src/event.rs:290-292`). It stays a file the observe
//!   reads and the act appends.
//! - Bead state the decision needs (`bead_status` after a failed close, `reconcile.rs:148`) is
//!   read in the act, not the observe, because it is only needed on a failure path.
//! - Store verbs: none in observe. Today the pass forks nothing to observe.
//!
//! ## Decide
//!
//! `decide(&Snapshot) -> Vec<Act>` — the branch structure of `apply` (`reconcile.rs:119-193`)
//! lifted out of IO. Per reply, by original kind:
//!
//! | Original | Acts |
//! |---|---|
//! | has bead, kind not question/decision/suit | `note` the orig bead (`:139-141`) |
//! | has bead, tracked kind, no `close` mark | `close` the tracking bead (`:142-165`); on refusal (exit 3) `note` + `inbox` once (`:149-160`) |
//! | has work bead | `note` it if no orig bead (`:169-172`); `lift` (`spira-lc reply`) (`:173-177`) |
//! | bead of kind question/decision | `answer` (`spira-lc close-ask --exit answered`) (`:180-186`) |
//! | any step at `MAX_ATTEMPTS` = 30 fails | mark abandoned; one `abandoned-inbox` row (`:25,86-89,188-191`) |
//!
//! No carried state in the scheduler (the ledger is the state). Fixtures: the eight tests at
//! `reconcile.rs:290-410` (close+lift+answer; second pass silent; duplicate Message-ID once;
//! store down then up; refused close → one note + one inbox row; routed question; notice;
//! empty body).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `mail.reconcile.note` | store | 15 s | `<msgid>/note-orig\|note-work\|refused-note` | empty bead id; writes through `bdq note` today (`bead.rs:339`) |
//! | `mail.reconcile.close` | store | 15 s | `<msgid>/close` | lifecycle refusal (exit 3) is reported, not retried (`reconcile.rs:104,149`); notes every upstream dep of a question/decision (`bead.rs:352-366`) |
//! | `mail.reconcile.lift` | store | 15 s | `<msgid>/lift` | terminal work bead (no-op, `lc.rs:46-49`); exit 1/3 quiet (`lc.rs:56`) |
//! | `mail.reconcile.answer` | store | 15 s | `<msgid>/answer` | exit 1 (legacy ask, no row) and 3 (closed) quiet (`lc.rs:65-73`) |
//! | `mail.reconcile.inbox` | host | 10 s | `<msgid>/refused-inbox\|abandoned-inbox` | writes the concierge maildir (`reconcile.rs:108-116`); becomes `inbox.append` |
//! | `mail.reconcile.mark` | host | 5 s | `<msgid>/<step>` (append-only line) | nothing; appends the ledger (`reconcile.rs:50-61`) |
//!
//! Deadline 15 s = `LC_TIMEOUT` (`spira-config/src/lc_call.rs:7`); bdq calls run under
//! `timeout 5` (`spira-config/src/bounded.rs:7`, `mail/src/bead.rs:77`). Every store act
//! marks its step on success, and `fail:<step>` on failure, so a re-offered effect is a no-op.
//!
//! ## Silence
//!
//! An idle tick (no reply in window without `complete`) emits only the observe probe. Today the
//! unit prints `reconcile: converged 0, pending 0, refused 0` every minute (`main.rs:321`) — that
//! line is dropped.
//!
//! ## Replaces
//!
//! - `spira-mail-reconcile-prod.{service,timer}` (`CPUQuota=25%`, `Nice=10`,
//!   `TimeoutStartSec=300`, `Environment=SPIRA_DB=…`). No watchd row.
//! - Children: `timeout 5 bdq` (note, show, dep list) via `BdCli` (`mail/src/bead.rs:62-80`);
//!   `timeout spira-lc close` via `spira_config::lifecycle_row::close`
//!   (`spira-config/src/lifecycle_row.rs:332-341`); `spira-lc state|reply|close-ask` via
//!   `LcCli` (`mail/src/lc.rs:23-31`).
//! - Pinned keys (`mail/src/env.rs:58-83`): `SPIRA_MAIL`, `SPIRA_DB`, `SPIRA_RUN`; unregistered
//!   env `SPIRA_BDQ_CONN_RETRIES` (default 2) joins `Settings::KEYS` (S10).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode (S4) shadows every
//! act; the ledger is untouched in Observe, so the old unit, if restored, resumes exactly where
//! the task stopped.
//!

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "mail.reconcile";
pub const STREAM: &str = "task.mail.reconcile";
pub const TICK: EventKindTag = "mail.reconcile.tick";
pub const STATE: EventKindTag = "mail.reconcile.state";

pub const OBSERVE: &str = "mail.reconcile.observe";
pub const NOTE: &str = "mail.reconcile.note";
pub const CLOSE: &str = "mail.reconcile.close";
pub const LIFT: &str = "mail.reconcile.lift";
pub const ANSWER: &str = "mail.reconcile.answer";
pub const INBOX: &str = "mail.reconcile.inbox";
pub const MARK: &str = "mail.reconcile.mark";
pub const EFFECTS: &[&str] = &[OBSERVE, NOTE, CLOSE, LIFT, ANSWER, INBOX, MARK];

/// `MAX_ATTEMPTS`: a step failing this often is abandoned.
pub const MAX_ATTEMPTS: u32 = 30;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_MAIL_RECONCILE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_MAIL", "SPIRA_DB", "SPIRA_RUN", "SPIRA_BDQ_CONN_RETRIES"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Orig {
    pub bead: String,
    pub kind: String,
    pub work_bead: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub key: String,
    pub in_reply_to: String,
    pub actor: String,
    pub para: String,
    pub orig: Option<Orig>,
    /// The ledger's done steps and failure counts for this message.
    pub done: BTreeSet<String>,
    pub fails: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub replies: Vec<Reply>,
}

/// The per-reply step machine (T3); the ledger holds it, not the scheduler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Pending,
    Noted,
    Closed,
    Lifted,
    Answered,
    Complete,
    Refused,
    Abandoned,
}

/// `state × event → state` per reply. Recording `complete` twice is refused naming `complete`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Pending, "note", Phase::Noted),
    (Phase::Pending, "close", Phase::Closed),
    (Phase::Pending, "lift", Phase::Lifted),
    (Phase::Pending, "answer", Phase::Answered),
    (Phase::Noted, "complete", Phase::Complete),
    (Phase::Closed, "complete", Phase::Complete),
    (Phase::Lifted, "complete", Phase::Complete),
    (Phase::Answered, "complete", Phase::Complete),
    (Phase::Pending, "close:refused", Phase::Refused),
    (Phase::Refused, "complete", Phase::Complete),
    (Phase::Pending, "fails:max", Phase::Abandoned),
];

/// No carried state: the per-message ledger is the state and survives a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {}

pub struct MailReconcile {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// The branch structure of `apply` lifted out of IO.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("mail.reconcile: not ported yet")
}

super::hosted!(MailReconcile);
