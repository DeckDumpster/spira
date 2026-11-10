//! # incident.probe — contract
//!
//! watchd row `alarms-probe` (`spira/watchers:196`), target `incident page --no-fallback --probe alarms-probe --interval 3600` (crate `incident`, `incident/src/main.rs:752-780`); pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: `mail` is not a child — the primary path is in-process (`MailPrimary` → `mail::cmds::send`, `incident/src/alarms_pass.rs:114-130`); the child is `bdq`, which `mail::bead::BdCli` forks for the bead store (`mail/src/bead.rs:44-77`, wired at `incident/src/main.rs:695`).
//!
//! This task is not an observer: its whole job is one write — prove the alarms component's primary delivery path by using it (law-a-fallback-has-a-forced-mode). So its "observe" is trivial and its act is the probe.
//!
//! ## Tick
//!
//! - Cadence 3600 s (`--interval 3600`, `spira/watchers:196`; loop `main.rs:769-779`). No waking events. Single-flight. Calendar alignment is not required (P1, not P4).
//!
//! ## Observe
//!
//! `incident.probe.observe` (host class) returns:
//!
//! ```text
//! Snapshot { last_probe: Option<{ at, outcome: Delivered{id} | Failed{reason} }> }
//! ```
//!
//! - From the alarm-attempt family `$SPIRA_RUN/tsd/alarm-attempt.jsonl` (`tsd/src/alarm.rs:108-112`, `tsd/src/lib.rs:23`), the record the alarms component itself writes for every delivery attempt (`alarms_pass.rs:203-233`), filtered to caller `alarms-probe` (`PROBE_CALLER`, `alarms_pass.rs:34`) — exactly what `probe_health` reads (`:702-712`).
//! - Used only for `scheduler status` and for the T3 read-back; decide does not gate the send on it (today the loop sends every interval unconditionally).
//!
//! ## Decide
//!
//! - Every tick → one `incident.probe.send`. No carried state: the attempt record is the memory, and it lives in the alarms component's own family, not in the task.
//! - On the outcome event: `effect.succeeded` → nothing more; `effect.failed` → nothing more either (today a failed probe is printed and the next hour tries again, `main.rs:773`). The alarms pass and `probe_health` own the consequence of a failed probe.
//! - Fixtures to port: `spira-lc/src/sections/tests/alarms_pass.rs:290-311` (`the_probe_proves_the_primary_path_alone_and_its_health_reads_the_latest_probe`: delivers to `alarms-probe`, health ok, stale after 2 h, failure never falls back, health reports failed).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `incident.probe.observe` | host | 5 s | (probe) | — |
//! | `incident.probe.send` | store (mail through the bead store) | 30 s | `incident_probe/<tick event id>` — a redelivery of the same tick sends no second note | the fallback: runs `deliver(…, fallback=false)` only (`alarms_pass.rs:698`, `:219-221`); a mailbox other than the configured one |
//!
//! `send` = `incident::alarms_pass::probe(ports, cfg, "alarms-probe")` (`alarms_pass.rs:689-699`): mailbox `alarms-probe`, subject `alarms probe of the primary delivery path`, kind `note`, body `## Note\nThe alarms component's hourly probe: the primary path delivered this.\n`; it appends the attempt row (`:204-209`) and returns the message id. Its config is `alarms_config(env, false)` (`main.rs:677-688`): `SPIRA_RECONCILER_DEADMAN_SECS`, `SPIRA_RECONCILER_HEARTBEAT_URL`, the causal-map floor proposal — pinned once (S10), though the probe uses only `run_dir` and `host`.
//!
//! No inbox line, today or after. The script's stdout (`main.rs:772-773`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> probe delivered as <id>
//! <YYYY-MM-DDTHH:MM:SSZ> probe FAILED: <reason>
//! ```
//!
//! lands in `run/watchd/alarms-probe.log`. Readers of that log: none by name in the release or `run/concierge-notes`; watchd's generic walkers only (`watchd notify`'s actionable filter would surface a `FAILED` line); no cursor exists. After the move these become the effect's `effect.succeeded` / `effect.failed` bodies; nothing writes the log.
//!
//! Health (4th field `incident page --probe-health --max-age 7200`, `main.rs:754-762`) keeps its meaning and moves to S13 `scheduler status` as the task's last outcome plus the same `probe_health` read, so "the latest probe failed or is over two hours old" stays red.
//!
//! ## Silence
//!
//! Not applicable in the usual sense: every tick sends by design (one journaled effect per hour, which is the budget). Between ticks nothing is emitted.
//!
//! ## Replaces
//!
//! - `spira-watch-alarms-probe-prod.service`; row `spira/watchers:193-196`.
//! - Children: `bdq` via `BdCli` (`main.rs:695`); `with_ports` also builds `spira_lc()` and a `systemctl is-enabled` closure (`main.rs:699-705`) that the probe never calls — neither is carried over.
//! - Files: `run/watchd/alarms-probe.{log,restarts}` retired; `run/tsd/alarm-attempt.jsonl` kept (shared with the alarms pass).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row. `Observe` mode shadows `send` — which means no probe is sent and `probe_health` goes red after 2 h; that is the correct signal, not a fault of the shadow.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;

pub const NAME: &str = "incident.probe";
pub const STREAM: &str = "incident.probe";
pub const TICK: EventKindTag = "incident.probe.tick";
pub const OBSERVE: &str = "incident.probe.observe";
pub const SEND: &str = "incident.probe.send";
pub const EFFECTS: &[&str] = &[OBSERVE, SEND];
/// `alarms_config(env, false)` (`main.rs:677-688`), pinned once though the probe uses only
/// `run_dir` and `host`.
pub const KEYS: &[&str] = &["SPIRA_RECONCILER_DEADMAN_SECS", "SPIRA_RECONCILER_HEARTBEAT_URL", "SPIRA_RUN"];
/// `--interval 3600`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
/// `PROBE_CALLER` (`alarms_pass.rs:34`).
pub const MAILBOX: &str = "alarms-probe";
pub const SUBJECT: &str = "alarms probe of the primary delivery path";
pub const BODY: &str = "## Note\nThe alarms component's hourly probe: the primary path delivered this.\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub deadman_secs: u64,
    pub heartbeat_url: String,
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { deadman_secs: cfg_parse("SPIRA_RECONCILER_DEADMAN_SECS")?, heartbeat_url: cfg("SPIRA_RECONCILER_HEARTBEAT_URL")?, run: cfg("SPIRA_RUN")? })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Delivered { id: String },
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub at: u64,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub last_probe: Option<Probe>,
}

const SEND_DEADLINE_MS: u64 = 30_000;

/// What `send` is asked: the note `alarms_pass::probe` sends, under the tick's key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Send {
    pub key: String,
    pub mailbox: String,
    pub subject: String,
    pub body: String,
}

/// Every tick sends once, keyed by the tick that asked: `incident_probe/<tick event id>`.
pub fn decide(_snap: &Snapshot, tick_id: &str, _cfg: &Config) -> Vec<EffectSpec> {
    let send = Send { key: format!("incident_probe/{tick_id}"), mailbox: MAILBOX.into(), subject: SUBJECT.into(), body: BODY.into() };
    vec![EffectSpec::keyed(SEND, send.key.clone(), serde_json::to_string(&send).unwrap_or_default(), SEND_DEADLINE_MS)]
}

/// The latest probe attempt in the alarm-attempt family, the record `probe_health` reads.
pub fn observe() -> Result<String, String> {
    let config = Config::pinned()?;
    let last_probe = incident::alarms_pass::last_probe(std::path::Path::new(&config.run)).and_then(|a| {
        let outcome = match a.outcome {
            tsd::alarm::Outcome::Delivered { id } => Outcome::Delivered { id },
            tsd::alarm::Outcome::Failed { reason } => Outcome::Failed { reason },
            tsd::alarm::Outcome::DeliveredFallback => return None,
        };
        Some(Probe { at: a.at, outcome })
    });
    serde_json::to_string(&Snapshot { last_probe }).map_err(|e| e.to_string())
}

/// `alarms_pass::probe` through the primary path alone, in-process: the mail environment and
/// `bdq` as `incident page` built them (`incident/src/main.rs:257-262`). Refuses any mailbox but
/// the probe's own; the outcome body is the line the loop printed.
pub fn send(input: &str) -> Result<String, String> {
    let s: Send = serde_json::from_str(input).map_err(|e| format!("{SEND}: bad input: {e}"))?;
    if s.mailbox != MAILBOX {
        return Err(format!("{SEND}: refused: mailbox {} is not {MAILBOX}", s.mailbox));
    }
    let config = Config::pinned()?;
    let env = mail::env::Env::load()?;
    let bd = mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: env.bd_conn_retries };
    let primary = incident::alarms_pass::MailPrimary { env, bd };
    let host = incident::intake::hostname();
    match incident::alarms_pass::probe_primary(&primary, &runtime::ports::WallClock, std::path::Path::new(&config.run), &host, &s.mailbox) {
        Ok(id) => Ok(format!("probe delivered as {id}")),
        Err(e) => Err(format!("probe FAILED: {e}")),
    }
}

pub struct IncidentProbe {
    pub config: Result<Config, String>,
}

impl IncidentProbe {
    pub fn new(config: Config) -> Self {
        IncidentProbe { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        IncidentProbe { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("incident.probe/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const STATE: EventKindTag = "incident.probe.state";
const ROW_PARTS: &[runtime::rows::Part] = &[];

impl Handler for IncidentProbe {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let Some(snap) = pattern::answer::<Snapshot>(view, ev, OBSERVE) else { return vec![] };
        let config = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let tick = ev.cause.as_ref().map(|c| c.0.as_str()).unwrap_or_default();
        decide(&snap, tick, config).into_iter().map(Command::Effect).collect()
    }
}
