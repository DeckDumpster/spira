//! # incident.alarms — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/incident-alarms.md`;
//! its Acceptance section is the test file `tests/it/task_b_incident_alarms.rs`.
//!
//! **Contract amended (2026-11-06):** the stub's Snapshot and Carry were simplified shapes
//! (a string `no_lever`, a timer-derived `goodput_armed`, page rows without the ask id) that
//! could not carry what the crate's alarms pass reads, so the task could not reuse it. The
//! snapshot now carries the store's whole `section snapshot` (parsed by
//! `incident::alarms_pass::seen` and the full `reconciler_engine` PassRecord inside it), the
//! goodput task's armed state and last fired time from `scheduler status`, and the attempt
//! record's reading (`AttemptReading`). The carry holds the page rows as `page::Row` with their
//! ask ids, the floor's binding and ask, the retries in flight, and the last status asked for.
//! The dead-man reads the goodput task's last fired time from `scheduler status` (`deadman_of`),
//! not the pass record's age behind a timer unit: an idle goodput task no longer writes a record
//! every minute. Code wins where code and contract disagree (sp-ipwlic).
//!
//! ## Tick
//!
//! - Cadence 60 s (`OnUnitActiveSec=60s`, `OnBootSec=90s`, `AccuracySec=10s`). Boot tick at +90 s.
//! - Also woken by `reconcile.goodput`'s record outcome (in-process, same journal), so a page follows
//!   the pass that caused it without waiting a minute.
//! - Single-flight. No `world guard`: observability plane. Its own halt is
//!   `run/world.halted.observability` (`alarms_pass.rs:41,79`), which only sets the dead-man to Waiting.
//!
//! ## Observe
//!
//! Probe `incident.alarms.observe` returns:
//!
//! ```text
//! Snapshot { section_snapshot: Value, pass: Option<PassRecord>, observability_halted: bool,
//!            goodput_armed: bool, attempts: Vec<Attempt>, floor_proposal: Option<String> }
//! ```
//!
//! - `section snapshot` — store verb (`alarms_pass.rs:643`): sections with freshness, the latest
//!   pass record (`:649`), intents (`:506`). Raised conditions are read from sections, never
//!   recomputed (`:350-370`).
//! - `goodput_armed` — today `systemctl is-enabled` on the goodput **timer** (`main.rs:700-703`).
//!   After the move there is no timer: it becomes "task `reconcile.goodput` registered and in Act
//!   mode" from `scheduler status` (S13). This is a semantic change, not a port.
//! - `attempts` — `run/tsd/alarm-attempt.jsonl` (`tsd/src/alarm.rs:108-118`); read for retry and
//!   for the Undelivered condition (`attempts::undelivered`, `alarms_pass.rs:517,565`).
//! - `floor_proposal` — `[floor] proposal` of `reconciler/causal-map.toml` (`main.rs:679,686`),
//!   pinned at boot or read per tick (file is Concierge-edited; per tick, unverified that it matters).
//!
//! ## Decide
//!
//! `decide(carry, snap, cfg) -> (Carry, Vec<Act>)`, composed of pieces that are already pure:
//!
//! - `deadman` (`alarms_pass.rs:372`): Waiting | Fresh | Stale over `SPIRA_RECONCILER_DEADMAN_SECS`
//!   (default 180, `main.rs:682`).
//! - candidates `from_pass` (no-lever, exhausted; `:318`), `raised` (page-on-raise; `:350`),
//!   `deadman_candidate` (`:385`); each through `page::decide` (`incident/src/page.rs:174`) → Send |
//!   Note | Quiet against the page rows.
//! - floor: one standing policy ask while Lifecycle `BelowFloor` is open, per binding condition
//!   (`alarms_pass.rs:454-492`); rows cleared when the condition clears (`:459-462`).
//! - retry-delivery intents: un-acked `Alarms` intents with action `RetryDelivery` (`:505-539`).
//! - the Alarms section status, and whether it is unchanged within 300 s (`:606-619`).
//! - Carry: the page rows (today `run/alarms-pages.tsv`, `:35,423`) and floor rows
//!   (`run/alarms-floor.tsv`, `:36,455`) move into carried state. Only `incident` reads them
//!   (grep of the release).
//! - Fixtures: `spira-lc/src/sections/tests/alarms_pass.rs` (rig with fake Primary/Http), `page.rs` tests.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `incident.alarms.page` (primary `mail send`, then maildir fallback; `alarms_pass.rs:203-233`) | store (primary), host (fallback) | 15 s | `page:<section>/<condition>:<identity>` | invalid mailbox (`:96`); forced mode skips fallback (`:219`) |
//! | `incident.alarms.note` (`mail::bead::note` on the open ask, `:139-144,439`) | store | 10 s | ask id + evidence hash | ask id empty or not a bead id (`:140`) |
//! | `incident.alarms.floor_ask` (policy ask, MOOT-WHEN `incident floor-cleared`, `:487-488`) | store | 15 s | `floor:<binding>` | binding already asked (`:466`) |
//! | `incident.alarms.retry` (re-deliver, then `intent apply` or `intent ack --result`, `:527-533`) | store | 15 s | intent key | intent already acked (`:513`); no recorded message → ack `refused` (`:525`) |
//! | `incident.alarms.attempt` (append to `tsd/alarm-attempt.jsonl`, `alarm.rs:109`) | host | 2 s | (at, caller, kind, reference) | none; failure is logged (`:206`) |
//! | `incident.alarms.knob_init` (`intent knob-init`, `:560`) | store | 5 s | knob key | already initialised (store no-op) |
//! | `incident.alarms.publish` (`section publish-status --expect-rev`, `:632`) | store | 5 s | section resource version | stale rev; unchanged within 300 s is not emitted (`:617`) |
//! | `incident.alarms.heartbeat` (HTTP GET of `SPIRA_RECONCILER_HEARTBEAT_URL`, `:494-503`) | host | 5 s (`:43`) | tick id | URL empty → Unconfigured, no effect; only when dead-man Fresh (`:663`) |
//!
//! `attempt` is recorded by the `page`/`retry`/`floor_ask` runners themselves, so a page and its
//! attempt row never diverge.
//!
//! ## Silence
//!
//! Idle: no candidate Sends or Notes, no retry intent, no floor change, section unchanged and
//! observed < 300 s ago. Then only the probe. Two standing exceptions today: `knob_init` runs every
//! pass (`:560`) — move to boot; and the heartbeat GET runs every Fresh minute — that is its job
//! (off-box dead-man), so it is an effect by design, not journaled as an event beyond the
//! section's `heartbeat_last` field. `publish` fires at least every 300 s to keep the section fresh
//! (`target_latency 5m`, `:623`); that is the floor of the journal budget.
//!
//! **Overlap.** `spira-runtime` already pings `SPIRA_RUNTIME_HEARTBEAT_URL` from its heartbeat
//! handler (`spira-runtime/src/lib.rs:203-204`, `settings.rs:265`). Once both live in one process,
//! two off-box heartbeats measure the same liveness; keep both until Ryan decides (they may point
//! at different monitors; unverified).
//!
//! ## Replaces
//!
//! - `spira-alarms-prod.service` + `.timer`. `incident page --probe` (watchd `alarms-probe`) and
//!   `incident floor-cleared` remain CLI/other tasks (`incident.probe`).
//! - Files `run/alarms-pages.tsv`, `run/alarms-floor.tsv` (carried state). The attempt log stays.
//! - Children per tick: `spira-lc` ≥ 3 (`snapshot`, `knob-init`, `publish-status`) via `bounded`
//!   (`main.rs:648`); `systemctl` 2–3 (`main.rs:701-702`); `bdq` per page (`mail/src/bead.rs:77`).
//! - Keys pinned: `SPIRA_RECONCILER_DEADMAN_SECS`, `SPIRA_RECONCILER_HEARTBEAT_URL`, mail `Env`
//!   (`mail::env::Env::load`, `main.rs:694`), `SPIRA_RUN`, hostname.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode shadows pages, asks, acks and publishes —
//! **but then nobody is paged**, so Observe on this task must be paired with the old unit running.
//! Running both in Act double-pages only inside the page window, since page rows are not shared
//! (today's file vs carried state): avoid; reverse by rollback, not overlap.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "incident.alarms";
pub const STREAM: &str = "task.incident.alarms";
pub const TICK: EventKindTag = "incident.alarms.tick";
pub const STATE: EventKindTag = "incident.alarms.state";

pub const OBSERVE: &str = "incident.alarms.observe";
pub const PAGE: &str = "incident.alarms.page";
pub const NOTE: &str = "incident.alarms.note";
pub const FLOOR_ASK: &str = "incident.alarms.floor_ask";
pub const RETRY: &str = "incident.alarms.retry";
pub const ATTEMPT: &str = "incident.alarms.attempt";
pub const KNOB_INIT: &str = "incident.alarms.knob_init";
pub const PUBLISH: &str = "incident.alarms.publish";
pub const HEARTBEAT: &str = "incident.alarms.heartbeat";
pub const EFFECTS: &[&str] = &[OBSERVE, PAGE, NOTE, FLOOR_ASK, RETRY, ATTEMPT, KNOB_INIT, PUBLISH, HEARTBEAT];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INCIDENT_ALARMS_SECS";
pub const DEADMAN_KEY: &str = "SPIRA_RECONCILER_DEADMAN_SECS";
pub const HEARTBEAT_URL_KEY: &str = "SPIRA_RECONCILER_HEARTBEAT_URL";
/// The scheduler's control socket, where the observe reads the goodput task.
pub const CONTROL_SOCKET_KEY: &str = "SPIRA_RUNTIME_SOCKET";
pub const KEYS: &[&str] = &[CADENCE_KEY, DEADMAN_KEY, HEARTBEAT_URL_KEY, "SPIRA_RUN", "SPIRA_MAIL", CONTROL_SOCKET_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: Some(CONTROL_SOCKET_KEY) };

/// The goodput task as `scheduler status` (S13) reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoodputStatus {
    /// Hosted and acting: `false` is the switch turned off, so the dead-man waits.
    pub armed: bool,
    pub last_fired_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// The store's `section snapshot`: sections with freshness, intents, and `pass`, the latest
    /// goodput pass record in full.
    pub section_snapshot: serde_json::Value,
    pub observability_halted: bool,
    pub goodput: GoodputStatus,
    /// The attempt record's reading at observe time.
    pub attempts: incident::alarms_pass::AttemptReading,
    pub floor_proposal: Option<String>,
}

/// The dead-man, the page rows and the floor rows each run a machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deadman {
    #[default]
    Waiting,
    Fresh,
    Stale,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Absent,
    Sent,
    Noted,
    Quiet,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Floor {
    #[default]
    None,
    /// The ask is in flight.
    Asking,
    Asked,
    Cleared,
}

pub const DEADMAN: &[(Deadman, &str, Deadman)] = &[(Deadman::Waiting, "pass:fresh", Deadman::Fresh), (Deadman::Fresh, "pass:stale", Deadman::Stale), (Deadman::Stale, "pass:fresh", Deadman::Fresh), (Deadman::Fresh, "halted", Deadman::Waiting), (Deadman::Stale, "halted", Deadman::Waiting)];
pub const PAGES: &[(Page, &str, Page)] = &[(Page::Absent, "send", Page::Sent), (Page::Sent, "note", Page::Noted), (Page::Noted, "quiet", Page::Quiet), (Page::Quiet, "window:expired", Page::Sent)];
/// A second floor ask for an asked binding is refused, naming `asked`.
pub const FLOORS: &[(Floor, &str, Floor)] = &[(Floor::None, "below_floor", Floor::Asking), (Floor::Asking, "asked", Floor::Asked), (Floor::Asked, "cleared", Floor::Cleared), (Floor::Cleared, "below_floor", Floor::Asking)];

/// One page row, `incident::page::Row` carried: what was paged, under which identity and
/// evidence, as which ask, and when.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageRow {
    pub section: String,
    pub condition: String,
    pub identity: String,
    pub evidence: String,
    pub ask: String,
    pub sent: i64,
}

impl PageRow {
    fn of(r: &incident::page::Row) -> PageRow {
        PageRow { section: r.section.clone(), condition: r.condition.clone(), identity: r.identity.clone(), evidence: r.evidence.clone(), ask: r.ask.clone(), sent: r.sent }
    }

    fn row(&self) -> incident::page::Row {
        incident::page::Row { section: self.section.clone(), condition: self.condition.clone(), identity: self.identity.clone(), evidence: self.evidence.clone(), ask: self.ask.clone(), sent: self.sent }
    }
}

/// The page rows (once `run/alarms-pages.tsv`) and the floor's binding (once
/// `run/alarms-floor.tsv`), carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub deadman: Deadman,
    pub pages: Vec<PageRow>,
    /// The floor's standing ask, by binding condition.
    pub floors: BTreeMap<String, Floor>,
    /// The ask each binding was filed as.
    pub floor_asks: BTreeMap<String, String>,
    /// Retry-delivery intents asked for and still listed.
    pub retries: std::collections::BTreeSet<String>,
    /// The retry knob was initialised (once, at the task's first pass).
    pub knob_inited: bool,
    pub published_at: Option<u64>,
    /// The last Alarms status asked to publish, without its clock fields.
    pub status: String,
}

pub struct IncidentAlarms {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// What a page or floor ask sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Send {
    /// The attempt reference: `page:<section>:<conditions>` or `floor:<binding>`.
    pub reference: String,
    pub kind: String,
    pub message: tsd::alarm::Message,
    /// The page's rows, `(section, condition)`, which take the ask id it is delivered as.
    pub rows: Vec<(String, String)>,
    pub binding: Option<String>,
}

/// What a page's outcome says: the rows it fills and the ask it was delivered as.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub rows: Vec<(String, String)>,
    pub ask: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub ask: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retry {
    pub key: String,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Publish {
    pub body: serde_json::Value,
    pub rev: String,
}

pub const STORE_DEADLINE_MS: u64 = 15_000;
pub const SHORT_DEADLINE_MS: u64 = 5_000;

fn act(name: &str, key: String, input: &impl Serialize, deadline_ms: u64, probe: bool) -> Result<Act, String> {
    Ok(Act { name: name.into(), key, input: serde_json::to_string(input).map_err(|e| e.to_string())?, deadline_ms, probe })
}

/// The status without its clock fields (`pass_age_secs`, each condition's transition time),
/// so one standing status is asked for once.
fn status_key(status: &serde_json::Value) -> String {
    let mut s = status.clone();
    if let Some(o) = s.as_object_mut() {
        o.remove("pass_age_secs");
    }
    if let Some(cs) = s.get_mut("conditions").and_then(serde_json::Value::as_array_mut) {
        for c in cs.iter_mut().filter_map(serde_json::Value::as_object_mut) {
            c.remove("lastTransitionTime");
        }
    }
    s.to_string()
}

fn floor_name(f: Floor) -> &'static str {
    match f {
        Floor::None => "none",
        Floor::Asking => "asking",
        Floor::Asked => "asked",
        Floor::Cleared => "cleared",
    }
}

/// `deadman_of` over the goodput task's `scheduler status`; the candidates (`from_pass`,
/// `raised`, `deadman_candidate`) through `page::decide` against the carried rows; the floor's
/// one standing ask; the retry-delivery intents; the knob, once; the heartbeat while Fresh; and
/// the Alarms section, asked once per status and refreshed as a memory-only probe while it stands.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    use incident::alarms_pass as ap;
    let snap = match input {
        Input::Fire => return Ok((carry.clone(), vec![])),
        Input::Observed(s) => s,
        Input::Outcome { effect, ok, body } => return outcome(carry, effect, *ok, body),
        Input::Ran(r) => return Err(format!("{NAME}: no unit is expected, got {r:?}")),
    };
    let deadman_secs = cfg.get(DEADMAN_KEY).and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v > 0).ok_or_else(|| format!("{DEADMAN_KEY} must be a positive integer"))?;
    let heartbeat_url = cfg.get(HEARTBEAT_URL_KEY).map(|u| u.trim().to_string()).unwrap_or_default();
    let now = now_ms / 1000;
    let seen = ap::seen(&snap.section_snapshot);
    let pass_raw = snap.section_snapshot.get("pass").cloned().unwrap_or(serde_json::Value::Null);
    let pass: Option<reconciler_engine::pass_record::PassRecord> = match &pass_raw {
        serde_json::Value::Null => None,
        v => Some(serde_json::from_value(v.clone()).map_err(|e| format!("{NAME}: the pass record does not decode: {e}"))?),
    };
    let mut next = carry.clone();
    let mut acts = vec![];
    if !carry.knob_inited {
        next.knob_inited = true;
        acts.push(act(KNOB_INIT, format!("{NAME}:knob-init"), &serde_json::json!({}), SHORT_DEADLINE_MS, false)?);
    }
    let intents = ap::retry_intents(&snap.section_snapshot);
    next.retries = intents.iter().map(|(k, _)| k.clone()).collect();
    for (key, target) in intents.into_iter().filter(|(k, _)| !carry.retries.contains(k)) {
        acts.push(act(RETRY, format!("{NAME}:retry:{key}"), &Retry { key, target }, STORE_DEADLINE_MS, false)?);
    }
    let dm = ap::deadman_of(snap.observability_halted, snap.goodput.armed, snap.goodput.last_fired_ms.map(|ms| ms / 1000), now, deadman_secs);
    next.deadman = match dm {
        ap::DeadMan::Waiting(_) => Deadman::Waiting,
        ap::DeadMan::Fresh { .. } => Deadman::Fresh,
        ap::DeadMan::Stale { .. } => Deadman::Stale,
    };
    let mut cands = pass.as_ref().map(|r| ap::from_pass(r, &seen, now)).unwrap_or_default();
    cands.extend(ap::raised(&seen, now));
    if let ap::DeadMan::Stale { age_secs } = dm {
        cands.push(ap::deadman_candidate(pass.as_ref(), age_secs, deadman_secs));
    }
    let mut rows: Vec<incident::page::Row> = carry.pages.iter().map(PageRow::row).collect();
    let now_i = now as i64;
    for c in &cands {
        match incident::page::decide(&rows, c, now_i) {
            incident::page::Verdict::Quiet => {}
            incident::page::Verdict::Send => {
                rows = incident::page::sent(&rows, c, "", now_i);
                let mine = rows.iter().filter(|r| r.section == c.section && r.sent == now_i && r.ask.is_empty()).map(|r| (r.section.clone(), r.condition.clone())).collect();
                acts.push(act(PAGE, format!("{NAME}:page:{}", c.reference()), &Send { reference: c.reference(), kind: ap::PAGE.into(), message: ap::message_of(c), rows: mine, binding: None }, STORE_DEADLINE_MS, false)?);
            }
            incident::page::Verdict::Note { ask } => {
                rows = incident::page::noted(&rows, c, now_i);
                if !ask.is_empty() {
                    let text = format!("Changed inside the page window, not re-sent:\n{}", c.body());
                    acts.push(act(NOTE, format!("{NAME}:note:{ask}:{:016x}", runtime::event::fnv1a(&text)), &Note { ask, text }, 10_000, false)?);
                }
            }
        }
    }
    next.pages = rows.iter().map(PageRow::of).collect();
    match ap::floor_ask(&seen, pass.as_ref(), &pass_raw, snap.floor_proposal.as_deref()) {
        None => {
            next.floors.clear();
            next.floor_asks.clear();
        }
        Some((binding, _)) if carry.floors.contains_key(&binding) => {}
        Some((binding, message)) => {
            next.floors = BTreeMap::from([(binding.clone(), Floor::Asking)]);
            next.floor_asks.clear();
            acts.push(act(FLOOR_ASK, format!("{NAME}:floor:{binding}"), &Send { reference: format!("floor:{binding}"), kind: ap::FLOOR.into(), message, rows: vec![], binding: Some(binding) }, STORE_DEADLINE_MS, false)?);
        }
    }
    if matches!(dm, ap::DeadMan::Fresh { .. }) && !heartbeat_url.is_empty() {
        acts.push(Act { name: HEARTBEAT.into(), key: format!("{NAME}:heartbeat:{now_ms}"), input: heartbeat_url, deadline_ms: SHORT_DEADLINE_MS, probe: true });
    }
    let prev = ap::previous(&seen);
    let (status, unchanged) = ap::status_of(prev, pass.as_ref(), &dm, &snap.attempts, None, now);
    if !unchanged {
        let key = status_key(&status);
        let rev = prev.and_then(|s| s.metadata.resource_version.clone()).unwrap_or_else(|| "0".into());
        let publish = Publish { body: ap::section_body(&status, now), rev };
        if key != carry.status {
            next.status = key;
            next.published_at = Some(now_ms);
            acts.push(act(PUBLISH, format!("{NAME}:publish:{}", publish.rev), &publish, SHORT_DEADLINE_MS, false)?);
        } else {
            acts.push(act(PUBLISH, format!("{NAME}:publish:{}", publish.rev), &publish, SHORT_DEADLINE_MS, true)?);
        }
    }
    Ok((next, acts))
}

fn outcome(carry: &Carry, effect: &str, ok: bool, body: &str) -> Decision<Carry> {
    match effect {
        PAGE if ok => {
            let sent: Sent = serde_json::from_str(body).map_err(|e| format!("{NAME}: a page's outcome does not decode: {e}"))?;
            let mut next = carry.clone();
            for r in next.pages.iter_mut().filter(|r| r.ask.is_empty() && sent.rows.iter().any(|(s, c)| *s == r.section && *c == r.condition)) {
                r.ask = sent.ask.clone();
            }
            Ok((next, vec![]))
        }
        FLOOR_ASK => {
            let (binding, ask) = body.split_once('\t').unwrap_or((body, ""));
            let binding = binding.trim();
            match carry.floors.get(binding).copied() {
                Some(Floor::Asking) => {
                    let mut next = carry.clone();
                    next.floors.insert(binding.to_string(), Floor::Asked);
                    next.floor_asks.insert(binding.to_string(), if ok { ask.to_string() } else { String::new() });
                    Ok((next, vec![]))
                }
                Some(f) if ok => Err(format!("{NAME}: the floor for {binding} is {}; it is asked once", floor_name(f))),
                _ if !ok => {
                    // A failed ask names no binding: the one in flight is asked, unanswered, as the
                    // pass's own row was written with an empty ask.
                    let mut next = carry.clone();
                    for f in next.floors.values_mut().filter(|f| **f == Floor::Asking) {
                        *f = Floor::Asked;
                    }
                    Ok((next, vec![]))
                }
                _ => Err(format!("{NAME}: no floor ask for {binding} is in flight")),
            }
        }
        PAGE | NOTE | RETRY | KNOB_INIT | PUBLISH | HEARTBEAT | ATTEMPT => Ok((carry.clone(), vec![])),
        other => Err(format!("{NAME}: no outcome of {other} is expected")),
    }
}

/// The observe probe's inputs, read here and nowhere else.
pub struct Observed<'a> {
    pub lc: &'a spira_config::lc_call::LcClient,
    pub run_dir: &'a std::path::Path,
    /// The scheduler's control socket, for `scheduler status`.
    pub control: &'a std::path::Path,
    /// `reconciler/causal-map.toml`, for the floor proposal.
    pub causal_map: Option<std::path::PathBuf>,
    pub now_ms: u64,
}

/// The goodput task's name in `scheduler status`.
pub const GOODPUT_TASK: &str = "reconcile.goodput";

/// The runner of [`OBSERVE`]: the store's `section snapshot`, the observability halt stamp, the
/// goodput task from `scheduler status`, the attempt record's reading and the floor proposal.
pub fn observe(o: &Observed) -> Result<String, String> {
    let section_snapshot: serde_json::Value = serde_json::from_str(o.lc.answer(&["section", "snapshot"]).map_err(String::from)?.trim()).map_err(|e| format!("section snapshot answered non-JSON: {e}"))?;
    let status: Option<serde_json::Value> = runtime::socket::lc_call(o.control, &["scheduler", "status"]).ok().and_then(|t| serde_json::from_str(&t).ok());
    let task = status.as_ref().and_then(|s| s["tasks"].as_array()?.iter().find(|t| t["name"] == GOODPUT_TASK).cloned());
    let goodput = match task {
        Some(t) => GoodputStatus { armed: t["mode"] == "act" && !t["next_due_at"].is_null(), last_fired_ms: t["last_fired_at"].as_u64() },
        None => GoodputStatus::default(),
    };
    let attempts = incident::alarms_pass::AttemptReading::of(&tsd::alarm::read(o.run_dir), o.now_ms / 1000);
    let floor_proposal = o.causal_map.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| incident::alarms_pass::floor_proposal(&t));
    let snap = Snapshot { section_snapshot, observability_halted: o.run_dir.join(incident::alarms_pass::OBSERVABILITY_HALT).exists(), goodput, attempts, floor_proposal };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The ports a page, note or retry delivers through: the store, mail in-process with its
/// mailbox fallback, the wall clock. No HTTP and no switch are read by them.
pub struct Delivery {
    pub lc: reconciler_engine::goodput_pass::Socket,
    pub primary: incident::alarms_pass::MailPrimary,
    pub fallback: incident::alarms_pass::MaildirFallback,
    pub cfg: incident::alarms_pass::Config,
}

struct NoHttp;
impl incident::alarms_pass::Http for NoHttp {
    fn get(&self, url: &str, _: std::time::Duration) -> Result<String, String> {
        Err(format!("a delivery makes no HTTP call ({url})"))
    }
}

struct NoSwitch;
impl incident::alarms_pass::Switch for NoSwitch {
    fn observability_halted(&self) -> bool {
        false
    }
    fn goodput_timer_on(&self) -> bool {
        true
    }
}

impl Delivery {
    /// The configured delivery: the process's mail environment, as `incident alarms-pass` built it.
    pub fn configured(lc: &spira_config::lc_call::LcClient, run_dir: &std::path::Path) -> Result<Delivery, String> {
        let env = mail::env::Env::load()?;
        let bd = mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: env.bd_conn_retries };
        let fallback = incident::alarms_pass::MaildirFallback { mail_root: env.mail_root.clone(), mute: env.mute };
        let cfg = incident::alarms_pass::Config { run_dir: run_dir.into(), host: incident::intake::hostname(), deadman_secs: 0, heartbeat_url: String::new(), fallback: true, floor_proposal: None };
        Ok(Delivery { lc: reconciler_engine::goodput_pass::Socket(lc.clone()), primary: incident::alarms_pass::MailPrimary { env, bd }, fallback, cfg })
    }

    fn with<R>(&self, f: impl FnOnce(&incident::alarms_pass::Ports) -> R) -> R {
        let ports = incident::alarms_pass::Ports { lc: &self.lc, primary: &self.primary, fallback: &self.fallback, http: &NoHttp, switch: &NoSwitch, clock: &runtime::ports::WallClock };
        f(&ports)
    }
}

/// The observe runner over the process's run dir, socket and spira home.
pub fn observe_with(lc: &spira_config::lc_call::LcClient, run_dir: &std::path::Path, control: &str, now_ms: u64) -> Result<String, String> {
    let causal_map = incident::intake::spira_home_dir().ok().and_then(|h| h.parent().map(|r| r.join("reconciler/causal-map.toml")));
    observe(&Observed { lc, run_dir, control: std::path::Path::new(control), causal_map, now_ms })
}

/// The runner of [`PAGE`] and [`FLOOR_ASK`]: `deliver` (primary, then the fallback), which
/// records each attempt itself. A page answers the rows and the ask; a floor ask answers
/// `<binding>\t<ask>`.
pub fn send(d: &Delivery, input: &str) -> Result<String, String> {
    let s: Send = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let ask = d.with(|p| incident::alarms_pass::deliver(p, &d.cfg, incident::alarms_pass::CALLER, &s.kind, &s.reference, &s.message, d.cfg.fallback))?;
    match s.binding {
        Some(b) => Ok(format!("{b}\t{ask}")),
        None => serde_json::to_string(&Sent { rows: s.rows, ask }).map_err(|e| e.to_string()),
    }
}

/// The runner of [`NOTE`]: a note on the ask an earlier page opened.
pub fn note(d: &Delivery, input: &str) -> Result<String, String> {
    use incident::alarms_pass::Primary;
    let n: Note = serde_json::from_str(input).map_err(|e| e.to_string())?;
    d.primary.note(&n.ask, &n.text).map(|()| n.ask)
}

/// The runner of [`RETRY`]: one retry-delivery intent, re-sent and acknowledged.
pub fn retry(d: &Delivery, input: &str) -> Result<String, String> {
    let r: Retry = serde_json::from_str(input).map_err(|e| e.to_string())?;
    Ok(d.with(|p| incident::alarms_pass::retry_one(p, &d.cfg, &r.key, &r.target)))
}

/// The runner of [`KNOB_INIT`].
pub fn knob_init(lc: &spira_config::lc_call::LcClient) -> Result<String, String> {
    incident::alarms_pass::knob_init(&reconciler_engine::goodput_pass::Socket(lc.clone())).map(|()| "initialised".into())
}

/// The runner of [`PUBLISH`]: `section publish-status --expect-rev`.
pub fn publish(lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    let p: Publish = serde_json::from_str(input).map_err(|e| e.to_string())?;
    incident::alarms_pass::publish_section(&reconciler_engine::goodput_pass::Socket(lc.clone()), &p.body, &p.rev).map(|()| "published".into())
}

/// The runner of [`HEARTBEAT`]: one GET of the off-box heartbeat URL.
pub fn heartbeat(url: &str) -> Result<String, String> {
    let r = ureq::AgentBuilder::new().timeout(incident::alarms_pass::HEARTBEAT_DEADLINE).build().get(url).call().map_err(|e| e.to_string())?;
    Ok(format!("status {}", r.status()))
}

/// The runner of [`ATTEMPT`]: one attempt row, as `incident alarm` writes it. The page and
/// retry runners record their own; this is for an attempt made outside them.
pub fn attempt(run_dir: &std::path::Path, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let outcome = if v["delivered"].as_bool() == Some(true) { tsd::alarm::Outcome::Delivered { id: s("id") } } else { tsd::alarm::Outcome::Failed { reason: s("reason") } };
    let a = tsd::alarm::Attempt { at: v["at"].as_u64().unwrap_or(0), caller: s("caller"), kind: s("kind"), reference: s("reference"), outcome, message: None };
    a.write(run_dir, &incident::intake::hostname()).map(|_| "recorded".into()).map_err(|e| e.to_string())
}

const ROW_PARTS: &[runtime::rows::Part] = &[];

super::hosted!(IncidentAlarms);
