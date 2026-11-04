//! # mail.tidy — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/mail-tidy.md`;
//! its Acceptance section is the test file `tests/task_c_mail_tidy.rs`.
//!
//! ## Tick
//!
//! - Cadence **15 min** (unit: `OnBootSec=3min`, `OnUnitActiveSec=15min`, `AccuracySec=2min`;
//!   `Persistent=true` is inert on a monotonic timer). First tick 3 min after boot.
//! - No other wake. Mailbox is a parameter: one task instance per mailbox; production has one,
//!   `operator`.
//! - Single-flight.
//!
//! ## Observe
//!
//! `mail.tidy.observe` (input: mailbox) returns:
//!
//! ```text
//! Snapshot { store_ok: bool, open_asks: BTreeSet<String>,
//!            messages: Vec<Msg{ path, mtime, subject, bead, unread, urgent, bead_closed: Option<bool> }>,
//!            now }
//! ```
//!
//! - `open_asks`: live ask-labelled bead ids — today `bdq list <ask-live-status> --label
//!   $SPIRA_ASK_LABEL --limit 0 --brief --json` (`mail/src/bead.rs:429-446`). In-process: the
//!   `content list` verb over `LcClient` (S7a), same filter from `spira_config::nonwork`.
//! - `store_ok`: the positive control, run only when `open_asks` is empty —
//!   `bdq list --limit 1` (`bead.rs:483-490`, `tidy.rs:43-47`). In-process: `content list
//!   --limit 1`.
//! - `messages`: `<SPIRA_MAIL>/<mailbox>/{new,cur}` newest first (`tidy.rs:57-65`); bead id from
//!   `X-Spira-Bead` else the first `<prefix>-[a-z0-9]{4,}` in the body (`tidy.rs:82-86`).
//! - `bead_closed`: per message that is fresh and names a bead — `bdq show <id> --json`
//!   (`bead.rs:473-478`). In-process: `content show`. Unknown/unreadable = not closed.
//! - Refusals carried as a snapshot error, not an empty set: store not configured
//!   (`tidy.rs:38-40`), ask query failed (`bead.rs:443`), positive control failed
//!   (`tidy.rs:44-46`), mailbox missing `cur`/`new` (`tidy.rs:50-52`), ask label unresolved
//!   (`main.rs:344-346`).
//!
//! ## Decide
//!
//! `decide(&Snapshot, fresh_s) -> Vec<Archive{path}>` — the keep rule (`tidy.rs:73-93`):
//! keep iff first copy of its subject AND (bead is an open ask OR ((unread AND age <
//! fresh_s) OR (urgent AND age < 7 d)) AND bead not known closed). `URGENT_MAX_S` = 604 800,
//! fixed (`tidy.rs:19`). A snapshot error decides **nothing** (fail closed,
//! law-a-control-that-cannot-check-must-refuse). No carried state. Fixtures: the tests in
//! `tidy.rs:110-292` and `spira/test-mail-tidy.sh`, `spira/test-mail-tidy-units.sh`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `mail.tidy.archive` | host | 5 s | `<mailbox>/<maildir filename>` | source already gone (rename of a moved file is a no-op); target outside `<SPIRA_MAIL>/archive/cur` |
//!
//! One effect per message, or one batched effect carrying the list (preferred: a pass archives
//! up to the whole inbox; one journaled effect, not N). It ensures `archive/` exists first
//! (`tidy.rs:54-55`) and renames into `archive/cur` (`tidy.rs:99-103`). Today a rename failure is
//! swallowed (`let _ =`, `tidy.rs:101`); the effect reports it.
//!
//! ## Silence
//!
//! An idle tick (every message kept) emits the probe only. Today the unit prints
//! `tidy: archived 0, kept N` every 15 min (`main.rs:354`); dropped.
//!
//! ## Replaces
//!
//! - `spira-mail-tidy-prod.{service,timer}` (`TimeoutStartSec=120`, `Environment=SPIRA_DB=…`).
//!   No watchd row.
//! - Children: `timeout 5 bdq` list/show (`bead.rs:62-80`). No `spira-lc` fork in this path —
//!   the catalogue's "spira-lc" is not in the code (bdq may itself reach lc-serve; unverified).
//! - Pinned keys (`mail/src/env.rs:58-83`): `SPIRA_MAIL`, `SPIRA_MAIL_TIDY_FRESH`,
//!   `SPIRA_ID_PREFIX`, `SPIRA_ASK_LABEL`, `SPIRA_DB`; env `SPIRA_BDQ_CONN_RETRIES`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode shadows `mail.tidy.archive`, so the
//! shadowed specs are exactly the files the old `--dry-run` would have listed. An archived
//! message can be moved back by hand from `archive/cur`; nothing is deleted.
//!

use std::collections::BTreeSet;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "mail.tidy";
pub const STREAM: &str = "task.mail.tidy";
pub const TICK: EventKindTag = "mail.tidy.tick";
pub const STATE: EventKindTag = "mail.tidy.state";

pub const OBSERVE: &str = "mail.tidy.observe";
pub const ARCHIVE: &str = "mail.tidy.archive";
pub const EFFECTS: &[&str] = &[OBSERVE, ARCHIVE];

/// `URGENT_MAX_S`, fixed.
pub const URGENT_MAX_S: u64 = 604_800;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_MAIL_TIDY_SECS";
/// The mailbox this instance tidies (`operator` in production); the observe probe's input.
pub const MAILBOX_KEY: &str = "SPIRA_RUNTIME_MAIL_TIDY_MAILBOX";
pub const FRESH_KEY: &str = "SPIRA_MAIL_TIDY_FRESH";
pub const KEYS: &[&str] = &[CADENCE_KEY, MAILBOX_KEY, "SPIRA_MAIL", FRESH_KEY, "SPIRA_ID_PREFIX", "SPIRA_ASK_LABEL", "SPIRA_DB", "SPIRA_BDQ_CONN_RETRIES"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: Some(MAILBOX_KEY) };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Msg {
    pub path: String,
    pub mtime: u64,
    pub subject: String,
    pub bead: Option<String>,
    pub unread: bool,
    pub urgent: bool,
    pub bead_closed: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub store_ok: bool,
    pub open_asks: BTreeSet<String>,
    /// Newest first.
    pub messages: Vec<Msg>,
    pub now: u64,
    /// A refusal carried as an error, never an empty set: it decides nothing.
    pub error: Option<String>,
}

/// The per-pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Observe,
    Refused,
    Decided,
    Archived,
}

/// `state × event → state`. A refused snapshot cannot reach `archived`; the refusal names
/// `refused`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Observe, "observed:error", Phase::Refused), (Phase::Observe, "observed", Phase::Decided), (Phase::Decided, "archive:ok", Phase::Archived), (Phase::Refused, "tick", Phase::Observe), (Phase::Archived, "tick", Phase::Observe), (Phase::Decided, "tick", Phase::Observe)];

/// No carried state beyond the pass phase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
}

pub struct MailTidy {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// The keep rule; everything else is archived, in one batched effect.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("mail.tidy: not ported yet")
}

crate::tasks::b::hosted!(MailTidy);
