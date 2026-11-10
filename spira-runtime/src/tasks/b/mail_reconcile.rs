//! # mail.reconcile — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/mail-reconcile.md`;
//! its Acceptance section is the test file `tests/it/task_b_mail_reconcile.rs`.
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
//! | `mail.reconcile.close` | store | 15 s | `<msgid>/close` | a typed `LcError::Refused` answers `<msgid>/close:refused` and is reported, not retried; notes every upstream dep of a question/decision (`bead.rs:352-366`) |
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
use spira_config::lc_call::LcError;
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

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

/// One store or host act for one reply's step: the effect input of every act but the observe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub key: String,
    /// The ledger line the act marks on success (`note-orig`, `close`, …), or for `mark` the line.
    pub step: String,
    pub bead: String,
    pub kind: String,
    pub work_bead: String,
    pub actor: String,
    pub para: String,
    pub subject: String,
    /// A note's text, or an inbox row's body.
    pub text: String,
    pub conn_retries: u32,
}

fn job(r: &Reply, step: &str, text: String, retries: u32) -> Job {
    let o = r.orig.clone().unwrap_or_default();
    Job { key: r.key.clone(), step: step.into(), bead: o.bead, kind: o.kind, work_bead: o.work_bead, actor: r.actor.clone(), para: r.para.clone(), subject: o.subject, text, conn_retries: retries }
}

fn act(name: &str, j: &Job, deadline_ms: u64) -> Act {
    let key = if name == MARK { format!("{}/mark/{}", j.key, j.step) } else { format!("{}/{}", j.key, j.step) };
    Act { name: name.into(), key, input: serde_json::to_string(j).unwrap_or_default(), deadline_ms, probe: false }
}

const STORE_MS: u64 = 15_000;
const INBOX_MS: u64 = 10_000;
const MARK_MS: u64 = 5_000;

/// The acts one reply still needs, by the branch structure of `mail::reconcile::apply`: each
/// step the ledger has not recorded, an abandoned step said once, nothing once all are done.
pub fn plan(r: &Reply, retries: u32) -> Vec<Act> {
    let Some(o) = &r.orig else { return vec![] };
    let mut acts = vec![];
    let mut abandoned: Vec<&str> = vec![];
    let mut want = |name: &'static str, step: &'static str, text: String, deadline: u64, acts: &mut Vec<Act>| {
        if r.done.contains(step) {
            return;
        }
        if r.fails.get(step).copied().unwrap_or(0) >= MAX_ATTEMPTS {
            abandoned.push(step);
            return;
        }
        acts.push(act(name, &job(r, step, text, retries), deadline));
    };
    let tracked = matches!(o.kind.as_str(), "question" | "decision" | "suit");
    let (actor, subject, para) = (&r.actor, &o.subject, &r.para);
    if !o.bead.is_empty() && !tracked {
        want(NOTE, "note-orig", format!("Reply from {actor} to \"{subject}\": {para}"), STORE_MS, &mut acts);
    } else if !o.bead.is_empty() && !r.done.contains("close") {
        if r.done.contains("close:refused") {
            if r.done.contains("refused-note") && r.done.contains("refused-inbox") {
                acts.push(act(MARK, &job(r, "close", String::new(), retries), MARK_MS));
            }
            let bead = &o.bead;
            want(NOTE, "refused-note", format!("Answer from {actor} to \"{subject}\" (not applied as a close: the lifecycle refused it): {para}"), STORE_MS, &mut acts);
            let mut inbox = job(r, "refused-inbox", format!("{actor} replied to \"{subject}\" (thread bead {bead}); the lifecycle refused the close.\nThe answer is noted on {bead}:\n{para}"), retries);
            inbox.subject = format!("Mail reply to {bead} refused as a close");
            if !r.done.contains("refused-inbox") {
                acts.push(act(INBOX, &inbox, INBOX_MS));
            }
        } else {
            want(CLOSE, "close", String::new(), STORE_MS, &mut acts);
        }
    }
    if !o.work_bead.is_empty() {
        if o.bead.is_empty() {
            want(NOTE, "note-work", format!("Answer from {actor} to \"{subject}\": {para}"), STORE_MS, &mut acts);
        }
        want(LIFT, "lift", String::new(), STORE_MS, &mut acts);
    }
    if !o.bead.is_empty() && matches!(o.kind.as_str(), "question" | "decision") {
        want(ANSWER, "answer", String::new(), STORE_MS, &mut acts);
    }
    if !abandoned.is_empty() && !r.done.contains("abandoned-inbox") {
        let mut j = job(r, "abandoned-inbox", format!("Reply {} to \"{subject}\": steps [{}] failed {MAX_ATTEMPTS} times and were abandoned; apply by hand.", r.key, abandoned.join(", ")), retries);
        j.subject = "Mail reply reconciliation gave up".into();
        acts.push(act(INBOX, &j, INBOX_MS));
    }
    acts
}

/// `key/step` from an act's answer: its success body, or the head of its failure.
fn key_step(body: &str) -> Option<(String, String, String)> {
    let (head, why) = body.split_once(": ").unwrap_or((body, ""));
    let (key, step) = head.rsplit_once('/')?;
    Some((key.to_string(), step.to_string(), why.to_string()))
}

/// The branch structure of `apply` lifted out of IO: an observed reply asks for the steps its
/// ledger lacks; a step's outcome is marked in the ledger (`<step>`, `fail:<step>`, or
/// `close:refused` for a lifecycle refusal). A reply already `complete` is refused.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    let retries = cfg.get("SPIRA_BDQ_CONN_RETRIES").and_then(|v| v.trim().parse().ok()).unwrap_or(1);
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(s) => {
            if cfg.get("SPIRA_DB").is_some_and(|d| d.is_empty()) {
                return Ok((carry.clone(), vec![]));
            }
            if let Some(r) = s.replies.iter().find(|r| r.done.contains("complete")) {
                return Err(format!("{NAME}: reply {} is already in state complete; recording it again is refused", r.key));
            }
            Ok((carry.clone(), s.replies.iter().flat_map(|r| plan(r, retries)).collect()))
        }
        Input::Outcome { effect, .. } if effect == MARK => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, ok, body } if EFFECTS.contains(&effect.as_str()) => {
            let Some((key, step, _)) = key_step(body) else { return Ok((carry.clone(), vec![])) };
            let line = if *ok {
                step
            } else {
                format!("fail:{step}")
            };
            Ok((carry.clone(), vec![act(MARK, &Job { key, step: line, conn_retries: retries, ..Job::default() }, MARK_MS)]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state pending no outcome of {effect} is expected")),
        Input::Ran(r) => Err(format!("{NAME}: in state pending no unit is expected, got {r:?}")),
    }
}

/// The ledger directory under the run dir (`main.rs`'s `<SPIRA_RUN>/mail-reconcile`).
fn ledger(e: &crate::Effects) -> std::path::PathBuf {
    e.world.run_dir.join("mail-reconcile")
}

/// The observe: the maildir and the ledger, read in-process.
pub fn observe(e: &crate::Effects) -> Result<String, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    serde_json::to_string(&serde_json::json!({ "replies": mail::reconcile::pending(&e.world.mail, &ledger(e), now) })).map_err(|e| e.to_string())
}

/// Every act's runner, by `spec.name`: the mail library's own steps. Success answers
/// `key/step`; a failure is `key/step: why`, so the outcome marks the ledger.
pub fn run(e: &crate::Effects, spec: &runtime::EffectSpec) -> Result<String, String> {
    if spec.name == OBSERVE {
        return observe(e);
    }
    let j: Job = serde_json::from_str(&spec.input).map_err(|e| e.to_string())?;
    let tag = format!("{}/{}", j.key, j.step);
    let bd = || -> Result<mail::bead::BdCli, String> { Ok(mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: j.conn_retries.max(1) }) };
    let client = super::lc(e);
    let lc = mail::lc::Routed { client: Ok(&client), unserved: &mail::lc::LcProgram };
    let mut refused = false;
    let out = match spec.name.as_str() {
        MARK => {
            mail::reconcile::mark(&ledger(e), &j.key, &j.step);
            Ok(())
        }
        NOTE => bd().and_then(|bd| mail::bead::note(&bd, if j.step == "note-work" { &j.work_bead } else { &j.bead }, &j.text)),
        CLOSE => bd().and_then(|bd| match mail::bead::sendmail_close_bead(&bd, true, &j.bead, &j.kind, &j.para) {
            Ok(_) => Ok(()),
            Err(_) if mail::bead::bead_status(&bd, true, &j.bead).as_deref() == Some("closed") => Ok(()),
            Err(LcError::Refused(_)) => {
                refused = true;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }),
        LIFT => mail::lc::lift_ask(&lc, &j.work_bead, mail::lc::Lift::Reply { message_id: &j.key }, &j.actor).map_or(Ok(()), Err),
        ANSWER => mail::lc::answer_ask(&lc, &j.bead, &j.para, &j.actor, "mail", &j.key).map_or(Ok(()), Err),
        INBOX => mail::reconcile::inbox_row(&e.world.mail, &j.subject, &j.text),
        other => Err(format!("{NAME}: no runner for {other}")),
    };
    out.map(|_| if refused { format!("{}/close:refused", j.key) } else { tag.clone() }).map_err(|why| format!("{tag}: {why}"))
}

// No carry: the ledger on each reply is the state, so no row is kept (`it_keeps_no_carry_row…`).
const ROW_PARTS: &[runtime::rows::Part] = &[];

super::hosted!(MailReconcile);
