//! `intake.filings`: filings product hosts drop in their outbox become builder beads. Replaces
//! `spira-filings-collect-prod` (`bin/filings-collect pass --state run/filings-collect.state`).
//! Pattern P1, with a three-step act per filing. Contract:
//! `wiki/projects/spira/designs/tasks/intake-filings.md`.
//!
//! # Tick
//!
//! Every 10 min (`SPIRA_RUNTIME_INTAKE_FILINGS_SECS`), single-flight, no `world guard`. When
//! `SPIRA_FILINGS_HOSTS` names no host every tick is a no-op.
//!
//! # Observe
//!
//! `intake.filings.observe` returns a [`Snapshot`]: `outbox` (each host's `<home>/filings/outbox`
//! listed over `ssh -o BatchMode=yes -o ConnectTimeout=10`, each unapplied `*.json` parsed by
//! `parse_filing`; ssh stays a class D child) and `open` (for each carried bead, whether its
//! lifecycle row is outside LANDED, SUPERSEDED, DROPPED and DONE, an `LcClient` show).
//!
//! # Decide
//!
//! The pure core is `filings_collect::pass` with `apply`, `new_bead_body` and `append_body`, split
//! into a decision over the snapshot plus a per-filing machine `seen → applied → acked`
//! ([`TRANSITIONS`]). A filing already `applied` in the carry skips the apply and is only
//! acknowledged, so an unacknowledged filing is never applied twice. `apply` appends when the
//! carried bead for the host and class is open and files a new bead otherwise. Carried state is
//! `intake.filings.state { beads, applied }`. Fixtures port `filings-collect/src/tests.rs`
//! (5 cases) and `spira/test-filings-collect-units.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.filings.observe` (probe) | host (ssh) | 60 s per host | tick id | an unparsable `SPIRA_FILINGS_HOSTS` |
//! | `intake.filings.file` | store | 120 s | `<host>/<name>` | a filing already in `applied` |
//! | `intake.filings.append` | store | 120 s | `<host>/<name>` | the same, or a bead no longer open |
//! | `intake.filings.ack` | host (ssh) | 60 s | `<host>/<name>` | a filing not in `applied`; runs only after the Emit that records the apply |
//! | `inbox.append` (shared) | host | 10 s | `<host>/<name>/<reason>` | — |
//!
//! file: `bead.sh file "<title>" --for builder --repo <repo>`, the body plus a `## Build` section
//! when it lacks one, then `Collected from <host>: <name>`. append: `bead.sh amend <id>
//! --append-body` with `## Filing <name> from <host>`. ack: move the file from `outbox/` to
//! `acked/`. inbox: `<iso> [watch:intake.filings] filings-collect: ALARM <host>[/<name>]: <reason>`.
//! The order is file or append → Emit (`applied += name`) → ack → Emit (`applied -= name`).
//!
//! # Silence
//!
//! An idle tick (empty outboxes) emits the probe only, and no Emit when the carry is unchanged. A
//! filing that keeps failing to parse alarms once per `<host>/<name>`, not once per tick.
//!
//! # Replaces
//!
//! `spira-filings-collect-prod.service` and `.timer`; children `ssh` ×(1 + filings) per host,
//! `bead.sh file/amend`, `spira-lc show`. Pinned config: [`KEYS`]; the persona `builder` is a
//! constant.
//!
//! # Reverse
//!
//! Release rollback restores the unit; the carry renders back to the old state file with
//! `State::render`, so the old unit resumes without a double apply. Observe mode shadows `file`,
//! `append` and `ack`, leaving filings in the outbox.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use filings_collect::World;

use super::trigger_groom::walk;
use super::{commands, Cycle, Transition, INBOX};
use crate::watchers::{answered_effect, carried, iso8601};

pub const NAME: &str = "intake.filings";
pub const STREAM: &str = "task.intake.filings";
pub const TICK: EventKindTag = "intake.filings.tick";
pub const STATE: EventKindTag = "intake.filings.state";

pub const OBSERVE: &str = "intake.filings.observe";
pub const FILE: &str = "intake.filings.file";
pub const APPEND: &str = "intake.filings.append";
pub const ACK: &str = "intake.filings.ack";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE, APPEND, ACK, INBOX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_FILINGS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_FILINGS_HOSTS", "SPIRA_METRICS_ENDPOINT"];

/// The persona every filed bead is for.
pub const PERSONA: &str = "builder";

const DEADLINE_MS: u64 = 60_000;
const APPLY_DEADLINE_MS: u64 = 120_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub ssh: String,
    pub home: String,
}

/// `filings_collect::Filing`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub class: String,
    pub action: String,
    pub repo: String,
    pub title: String,
    pub body: String,
}

/// One outbox file: its name and its parse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outboxed {
    pub name: String,
    pub filing: Result<Filing, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub hosts: Vec<Host>,
    pub outbox: BTreeMap<String, Result<Vec<Outboxed>, String>>,
    pub open: BTreeMap<String, Result<bool, String>>,
}

/// `intake.filings.state`: the bead per `host/class`, the filings applied and not yet gone from
/// the outbox (`host/name`), and the alarms already raised (`host` or `host/name`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub beads: BTreeMap<String, String>,
    pub applied: BTreeSet<String>,
    pub alarmed: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilingState {
    Seen,
    Applied,
    Acked,
}

pub const TRANSITIONS: &[Transition<FilingState>] = &[
    Transition { from: FilingState::Seen, on: "file", to: FilingState::Applied, effects: &[FILE] },
    Transition { from: FilingState::Seen, on: "append", to: FilingState::Applied, effects: &[APPEND] },
    Transition { from: FilingState::Applied, on: "ack", to: FilingState::Acked, effects: &[ACK] },
];

/// The filing's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: FilingState, on: &str) -> Result<FilingState, String> {
    walk(TRANSITIONS, from, on)
}

/// What `intake.filings.file` and `intake.filings.append` are handed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Apply {
    pub host: Host,
    pub name: String,
    /// `host/class`: the carry key the filed bead is recorded under.
    pub key: String,
    pub filing: Filing,
    /// The bead an append goes to.
    pub bead: Option<String>,
    pub body: String,
}

/// What `intake.filings.file` answers, so the handler records the new bead under its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filed {
    pub key: String,
    pub id: String,
}

/// What `intake.filings.ack` is handed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub host: Host,
    pub name: String,
}

fn lib_host(h: &Host) -> filings_collect::Host {
    filings_collect::Host { name: h.name.clone(), ssh: h.ssh.clone(), home: h.home.clone() }
}

fn lib_filing(f: &Filing) -> filings_collect::Filing {
    let action = if f.action == "append" { filings_collect::Action::Append } else { filings_collect::Action::Open };
    filings_collect::Filing { class: f.class.clone(), action, repo: f.repo.clone(), title: f.title.clone(), body: f.body.clone() }
}

fn effect(name: &str, key: String, input: String, deadline_ms: u64) -> EffectSpec {
    EffectSpec::keyed(name, key, input, deadline_ms)
}

fn alarm(carry: &mut Carry, acts: &mut Vec<EffectSpec>, at: &str, reason: &str) {
    if carry.alarmed.insert(at.to_string()) {
        acts.push(effect(INBOX, format!("{at}/{reason}"), format!("[watch:{NAME}] filings-collect: ALARM {at}: {reason}"), 10_000));
    }
}

/// The pure decision over one snapshot: the next carry and the acts. Per host, in name order, as
/// `filings_collect::pass` walks it: a filing already applied is only acknowledged; any other is
/// parsed and applied, appended to the carried bead for its host and class when that bead is
/// open, filed as a new bead otherwise. A filing stays in `applied` until it leaves the outbox,
/// so it is never applied twice; the handler asks for its acknowledgement once (see
/// [`already_acked`]). A second filing of a class whose bead is being filed this tick waits for
/// the next tick, so one class never gets two beads. An unparsable filing alarms once per
/// `host/name`. An unreadable outbox is retried on the next tick and named in the tick's wide
/// event (`filings.unreadable`), not the inbox: a flapping ssh link would cost one inbox line per
/// outage, and the old per-pass alarm was the noise this task stops.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<EffectSpec>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    for h in &snap.hosts {
        let listed = match snap.outbox.get(&h.name) {
            Some(Ok(l)) => l,
            Some(Err(_)) | None => continue,
        };
        let prefix = format!("{}/", h.name);
        let present: BTreeSet<String> = listed.iter().map(|o| format!("{prefix}{}", o.name)).collect();
        next.applied.retain(|k| !k.starts_with(&prefix) || present.contains(k));
        next.alarmed.retain(|k| !k.starts_with(&prefix) || present.contains(k));
        let mut filing_now = BTreeSet::new();
        let mut sorted: Vec<&Outboxed> = listed.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for o in sorted {
            let at = format!("{prefix}{}", o.name);
            if next.applied.contains(&at) {
                acts.push(effect(ACK, at.clone(), serde_json::to_string(&Ack { host: h.clone(), name: o.name.clone() }).unwrap_or_default(), DEADLINE_MS));
                continue;
            }
            let f = match &o.filing {
                Ok(f) => f,
                Err(e) => {
                    alarm(&mut next, &mut acts, &at, &format!("cannot parse the filing: {e}"));
                    continue;
                }
            };
            let key = format!("{}/{}", h.name, f.class);
            if filing_now.contains(&key) {
                continue;
            }
            let lib = lib_filing(f);
            let bead = next.beads.get(&key).cloned();
            let (name, body, bead) = match bead.as_ref().map(|id| snap.open.get(id)) {
                Some(Some(Ok(true))) => (APPEND, filings_collect::append_body(&lib_host(h), &o.name, &lib), bead),
                Some(Some(Err(e))) => {
                    alarm(&mut next, &mut acts, &at, &format!("cannot tell whether its bead is open: {e}"));
                    continue;
                }
                _ => {
                    filing_now.insert(key.clone());
                    (FILE, filings_collect::new_bead_body(&lib_host(h), &o.name, &lib), None)
                }
            };
            next.applied.insert(at.clone());
            let input = Apply { host: h.clone(), name: o.name.clone(), key, filing: f.clone(), bead, body };
            acts.push(effect(name, at, serde_json::to_string(&input).unwrap_or_default(), APPLY_DEADLINE_MS));
        }
    }
    (next, acts)
}

/// Whether `spec` was already asked for on `stream` and has succeeded or is still in flight: an
/// acknowledgement is asked for once, and again only after it failed.
pub fn already_acked(view: &Projection, stream: &StreamKey, spec: &EffectSpec) -> bool {
    #[derive(Deserialize)]
    struct Body {
        spec: EffectSpec,
    }
    let events = view.events(stream);
    events.iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && serde_json::from_str::<Body>(&e.kind.body).is_ok_and(|b| b.spec.name == spec.name && b.spec.input == spec.input)).any(|req| !events.iter().any(|o| o.kind.tag == EFFECT_FAILED && o.cause.as_ref() == Some(&req.id)))
}

/// What the observe is handed: the pinned config, the filings it need not read (applied), and the carried beads whose openness it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveInput {
    pub config: BTreeMap<String, String>,
    pub skip: BTreeSet<String>,
    pub beads: BTreeSet<String>,
}

fn real(e: &crate::Effects) -> filings_collect::real::Real {
    filings_collect::real::Real::over(spira_config::lc_call::LcClient::socket(&e.world.lc_socket))
}

/// `intake.filings.observe`: each host's outbox over ssh, the unapplied filings parsed, and
/// whether each carried bead is still open.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("observe input: {e}"))?;
    let hosts = filings_collect::parse_hosts(input.config.get("SPIRA_FILINGS_HOSTS").map(String::as_str).unwrap_or(""))?;
    let w = real(e);
    let mut outbox = BTreeMap::new();
    for h in &hosts {
        let listed = w.list(h).map(|names| {
            names
                .into_iter()
                .map(|name| {
                    let at = format!("{}/{name}", h.name);
                    let filing = if !name.ends_with(".json") {
                        Err("not a filing name".to_string())
                    } else if input.skip.contains(&at) {
                        Err("applied; not read again".to_string())
                    } else {
                        w.read(h, &name).and_then(|t| filings_collect::parse_filing(&t)).map(|f| Filing {
                            class: f.class,
                            action: if f.action == filings_collect::Action::Append { "append" } else { "open" }.into(),
                            repo: f.repo,
                            title: f.title,
                            body: f.body,
                        })
                    };
                    Outboxed { name, filing }
                })
                .collect()
        });
        outbox.insert(h.name.clone(), listed);
    }
    let open = input.beads.iter().map(|id| (id.clone(), w.is_open(id))).collect();
    let hosts = hosts.into_iter().map(|h| Host { name: h.name, ssh: h.ssh, home: h.home }).collect();
    serde_json::to_string(&Snapshot { hosts, outbox, open }).map_err(|e| e.to_string())
}

/// `intake.filings.file`: a new builder bead; answers its id under its key.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let a: Apply = serde_json::from_str(&s.input).map_err(|e| format!("file input: {e}"))?;
    let id = real(e).file(&lib_filing(&a.filing), &a.body)?;
    serde_json::to_string(&Filed { key: a.key, id }).map_err(|e| e.to_string())
}

/// `intake.filings.append`: the filing appended to its open bead.
pub fn run_append(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let a: Apply = serde_json::from_str(&s.input).map_err(|e| format!("append input: {e}"))?;
    let id = a.bead.ok_or("an append names no bead")?;
    real(e).append(&id, &a.body, &format!("filings-collect: {} from {}", a.name, a.host.name)).map(|()| id)
}

/// `intake.filings.ack`: the filing moved from the host's outbox to its acked directory.
pub fn run_ack(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let a: Ack = serde_json::from_str(&s.input).map_err(|e| format!("ack input: {e}"))?;
    real(e).ack(&lib_host(&a.host), &a.name).map(|()| format!("{}/{}", a.host.name, a.name))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeFilings {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeFilings {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IntakeFilings { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.filings/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "beads", drop: &[] }, runtime::rows::Part::Field("applied"), runtime::rows::Part::Field("alarmed")];

impl Handler for IntakeFilings {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 == STREAM && ev.kind.tag == EFFECT_SUCCEEDED && answered_effect(view, ev).as_deref() == Some(FILE) {
            let Ok(filed) = serde_json::from_str::<Filed>(&ev.kind.body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let mut next = carry.clone();
            next.beads.insert(filed.key, filed.id);
            return commands(&carry, &next, STATE, vec![]);
        }
        let input = || {
            let c = carried::<Carry>(view, &ev.stream, STATE);
            serde_json::to_string(&ObserveInput { config: self.config.clone(), skip: c.applied.clone(), beads: c.beads.values().cloned().collect() }).unwrap_or_default()
        };
        CYCLE.handle(self.interval_ms, view, ev, input, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let unreadable: Vec<String> = snap.outbox.iter().filter_map(|(h, o)| o.as_ref().err().map(|e| format!("{h}: {e}"))).collect();
            if !unreadable.is_empty() {
                ctx.event().set("filings.unreadable", unreadable.join("; ").as_str());
            }
            let (next, mut acts) = decide(&carry, &snap);
            acts.retain(|a| a.name != ACK || !already_acked(view, &ev.stream, a));
            for a in acts.iter_mut().filter(|a| a.name == INBOX) {
                a.input = format!("{} {}", iso8601(ev.at / 1000), a.input);
            }
            commands(&carry, &next, STATE, acts)
        })
    }
}
