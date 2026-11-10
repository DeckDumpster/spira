//! # incident.settle — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/incident-settle.md`;
//! its Acceptance section is the test file `tests/it/task_b_incident_settle.rs`.
//!
//! **Pattern (corrections from the contracts).** A P2 machine, not a P1/P4 tick: the state
//! table is `TRANSITIONS`, and a long leg run as a transient unit ends on its `unit.ran`,
//! recognised by `UnitRan::key_for(NAME)`.
//!
//! ## Tick
//!
//! - Cadence 5 min (`OnUnitActiveSec=5min`, `OnBootSec=3min`, `AccuracySec=30s`). Boot tick at +180 s.
//! - Also woken by a lifecycle transition to `LANDED`/`DONE` of a bead that `blocks` an open
//!   incident, and by `release.activated` (unverified: neither event is delivered to handlers today;
//!   without them, cadence alone suffices — the fix waits for the active release anyway).
//! - Single-flight. No `world guard` on the unit; settle runs while halted. Proposed unchanged.
//! - Refuses to arm without a content database: today `SPIRA_DB` unset exits early (`main.rs:872`).
//!
//! ## Observe
//!
//! Probe `incident.settle.observe` returns one row per open incident:
//!
//! ```text
//! Snapshot { incidents: Vec<Inc> }
//! Inc { id, external_ref, labels, fixes: Vec<Fix>, detector: Quiet | Firing(String) | NoProbe | Err(String) }
//! Fix { id, state: String /* lifecycle state */, landing: InForce(sha) | NotInRelease | Err(String) }
//! ```
//!
//! - Candidates: content `list --all --json` by label `SPIRA_INCIDENT_LABEL` (default `incident`),
//!   open only (`settle_real.rs:57-62`), joined to the lifecycle list (`real.rs:136`) → S7 `content`
//!   verb + lifecycle projection.
//! - Fixes: the incident's `blocks` edges (`settle_real.rs:27`, read via `bdq list --id`, `:19`) →
//!   `content` verb.
//! - Fix state: `lc_state::row(fix)` (`settle_real.rs:70`; `spira-config/src/lc_state.rs:241`) →
//!   `LcClient` show. `LANDED`/`DONE` is landed, `DROPPED` is non-blocking (`settle.rs:8,81`).
//! - Landing in force: `git log <base> --grep <id>` then `merge-base --is-ancestor <sha> <release>`
//!   (`settle_real.rs:87,95`); repo from the fix's `repo:` label and the registry. Git stays a child
//!   (class B, allowed). Release sha from `$SPIRA_RELEASE` (`settle_real.rs:135`) → pinned at boot.
//! - Detector: `systemctl --user is-failed <unit>` for `incident:<unit>` refs (`settle_real.rs:102-112`)
//!   → `unit.facts` (S6) read in-process. Non-unit refs are `NoProbe`.
//!
//! The probe stops reading an incident at its first unlanded fix, as today (`settle.rs:85`), so the
//! detector is never run for one.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)`: `settle_one` with the port's read methods answered
//! from `Inc`. Outcomes `Closed | Persists | Waiting | Skipped` (`settle.rs:52`).
//!
//! - Close when every fix is landed-in-force or dropped and the detector is `Quiet`; evidence names
//!   each fix, its sha, and "detector quiet at <now>" (`settle.rs:98-102`).
//! - Persist (note + label `settle-fired:<hash of sorted fixes>`, `settle.rs:59,112`) once per fix set;
//!   an incident already carrying the label waits (`settle.rs:109`).
//! - Carry: the `Waiting` set as `id\twhy` lines (`settle.rs:124`), today the file
//!   `<incident.log>.settle-waiting` (`main.rs:626-631`). It now lives in the stream's carried state;
//!   a changed line is logged, an unchanged one is not.
//! - Fixtures: the eight tests in `incident/src/settle.rs:209-286` become snapshot fixtures.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `incident.settle.close` (`spira_config::lifecycle_row::close(id, evidence, "incident")`, `settle_real.rs:116`) | store | 5 s | incident id | row already terminal (store answers 3); unreachable store |
//! | `incident.settle.note` (`bdq note <id> --stdin`, `settle_real.rs:120`) | store | 10 s | incident id + fired label | missing bead |
//! | `incident.settle.label` (`bdq label add <id> settle-fired:<h>`, `settle_real.rs:123`) | store | 10 s | incident id + label (set-add) | missing bead |
//! | `incident.settle.log` (append `settle: <outcome>` to `incident.log`, `main.rs:629`) | host | 2 s | event id | none |
//!
//! `note` then `label` are ordered: label only after note succeeds, as today (`settle_real.rs:120-125`).
//! A failed note leaves no label, so the next tick retries.
//!
//! ## Silence
//!
//! Idle: no incident changes outcome. Nothing is emitted beyond the probe; carried state is emitted
//! only when the waiting set changes; `incident.settle.log` fires only for a changed waiting line or a
//! non-waiting outcome. Today `Skipped` outcomes (e.g. "no fix dependency") are logged every pass
//! (`settle.rs:137`, `_ => fresh.push(o)`); proposed: carry them like `Waiting`, so a standing skip is
//! silent.
//!
//! ## Replaces
//!
//! - `spira-incident-settle-prod.service` + `.timer`; `incident settle` stays a CLI for hand use.
//! - File `run/incident.settle-waiting` (carried state instead).
//! - Children per tick: `bdq` 1 + 2·incidents (`real.rs:33`), `spira-lc` list + 1 per fix
//!   (`real.rs:136`, `lc_state.rs:241`), `systemctl` 1 per unit-referenced incident
//!   (`settle_real.rs:107`), `git` 1–2 per landed fix (stays).
//! - Keys pinned: `SPIRA_DB`, `SPIRA_INCIDENT_LABEL`, `SPIRA_HOME_REPO` (default `spira`,
//!   `main.rs:167`), `SPIRA_INCIDENT_LOG`, `SPIRA_RELEASE`, the repo registry (`main.rs:621`),
//!   `BD_TIMEOUT`, `SPIRA_BDQ_CONN_RETRIES` (`real.rs:23-24`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode journals `close`/`note`/
//! `label` as `effect.shadowed`; nothing closes. Running the old unit alongside is safe: close is
//! terminal-refusing and the label is set-add.
//!

use std::collections::BTreeMap;

use runtime::event::{fnv1a, EventKindTag};
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "incident.settle";
pub const STREAM: &str = "task.incident.settle";
pub const TICK: EventKindTag = "incident.settle.tick";
pub const STATE: EventKindTag = "incident.settle.state";

pub const OBSERVE: &str = "incident.settle.observe";
pub const CLOSE: &str = "incident.settle.close";
pub const NOTE: &str = "incident.settle.note";
pub const LABEL: &str = "incident.settle.label";
pub const LOG: &str = "incident.settle.log";
pub const EFFECTS: &[&str] = &[OBSERVE, CLOSE, NOTE, LABEL, LOG];

/// The label a persisting incident carries per fix set.
pub const FIRED_PREFIX: &str = "settle-fired:";

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INCIDENT_SETTLE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_INCIDENT_LABEL", "SPIRA_HOME_REPO", "SPIRA_INCIDENT_LOG", "SPIRA_RELEASE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Landing {
    InForce(String),
    #[default]
    NotInRelease,
    Err(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fix {
    pub id: String,
    /// The lifecycle state: `LANDED`/`DONE` is landed, `DROPPED` does not block.
    pub state: String,
    pub landing: Landing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    Quiet,
    Firing(String),
    #[default]
    NoProbe,
    Err(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inc {
    pub id: String,
    pub external_ref: String,
    pub labels: Vec<String>,
    pub fixes: Vec<Fix>,
    pub detector: Detector,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub incidents: Vec<Inc>,
}

/// The P2 machine, per incident (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Open,
    Waiting,
    Persists,
    Skipped,
    Closing,
    Closed,
}

/// `state × event → state` per incident, as data. A second `persists` for a fix set already
/// labelled is refused naming the `settle-fired:` label. The long leg, when the check runs as a
/// transient unit, ends on its `unit.ran` (`UnitRan::key_for(NAME)` names the incident).
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "fix:unlanded", Phase::Waiting),
    (Phase::Open, "no-fix", Phase::Skipped),
    (Phase::Open, "landed:quiet", Phase::Closing),
    (Phase::Open, "landed:firing", Phase::Persists),
    (Phase::Waiting, "landed:quiet", Phase::Closing),
    (Phase::Waiting, "landed:firing", Phase::Persists),
    (Phase::Persists, "labelled", Phase::Waiting),
    (Phase::Closing, "close:ok", Phase::Closed),
    (Phase::Closing, "unit.ran", Phase::Closed),
    (Phase::Closing, "close:failed", Phase::Open),
    (Phase::Skipped, "fix:unlanded", Phase::Waiting),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub incidents: BTreeMap<String, Phase>,
    /// `id\twhy` per waiting or skipped incident, so a standing line is silent.
    pub waiting: BTreeMap<String, String>,
}

pub struct IncidentSettle {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `incident::settle::Settler` answered from one observed [`Inc`]: its reads are the
/// snapshot's, and its two writes record what `settle_one` decided instead of doing it.
struct Observed<'a> {
    inc: &'a Inc,
    now_iso: String,
    closed: std::cell::RefCell<Option<String>>,
    persisted: std::cell::RefCell<Option<(String, String)>>,
}

impl incident::settle::Settler for Observed<'_> {
    fn candidates(&self) -> Result<Vec<incident::settle::Candidate>, String> {
        Ok(vec![candidate(self.inc)])
    }
    fn fixes(&self, _: &str) -> Result<Vec<String>, String> {
        Ok(self.inc.fixes.iter().map(|f| f.id.clone()).collect())
    }
    fn fix_state(&self, fix: &str) -> Result<incident::settle::FixState, String> {
        let f = self.inc.fixes.iter().find(|f| f.id == fix).ok_or_else(|| format!("{fix} was not observed"))?;
        Ok(if matches!(f.state.as_str(), "LANDED" | "DONE") { incident::settle::FixState::Landed } else { incident::settle::FixState::Unlanded(f.state.clone()) })
    }
    fn landing_in_force(&self, fix: &str) -> Result<Option<incident::settle::Landing>, String> {
        match self.inc.fixes.iter().find(|f| f.id == fix).map(|f| &f.landing) {
            Some(Landing::InForce(sha)) => Ok(Some(incident::settle::Landing { sha: sha.clone() })),
            Some(Landing::NotInRelease) => Ok(None),
            Some(Landing::Err(e)) => Err(e.clone()),
            None => Err(format!("{fix} was not observed")),
        }
    }
    fn detect(&self, _: &incident::settle::Candidate) -> Result<incident::settle::Detector, String> {
        match &self.inc.detector {
            Detector::Quiet => Ok(incident::settle::Detector::Quiet),
            Detector::Firing(d) => Ok(incident::settle::Detector::Firing(d.clone())),
            Detector::NoProbe => Ok(incident::settle::Detector::NoProbe),
            Detector::Err(e) => Err(e.clone()),
        }
    }
    fn close(&self, _: &str, evidence: &str) -> Result<(), String> {
        *self.closed.borrow_mut() = Some(evidence.into());
        Ok(())
    }
    fn persist(&self, _: &str, label: &str, note: &str) -> Result<(), String> {
        *self.persisted.borrow_mut() = Some((label.into(), note.into()));
        Ok(())
    }
    fn now_iso(&self) -> String {
        self.now_iso.clone()
    }
}

fn candidate(inc: &Inc) -> incident::settle::Candidate {
    incident::settle::Candidate { id: inc.id.clone(), external_ref: Some(inc.external_ref.clone()).filter(|r| !r.is_empty()), labels: inc.labels.clone() }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Open => "open",
        Phase::Waiting => "waiting",
        Phase::Persists => "persists",
        Phase::Skipped => "skipped",
        Phase::Closing => "closing",
        Phase::Closed => "closed",
    }
}

/// Every act's input: the incident, and what the act writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    /// The close's evidence, the note's text, or the log line.
    pub text: String,
    pub label: String,
    /// `SPIRA_INCIDENT_LOG`, for `log`.
    pub log: String,
}

fn act(name: &str, key: String, j: Job, deadline_ms: u64) -> Act {
    Act { name: name.into(), key, input: serde_json::to_string(&j).unwrap_or_default(), deadline_ms, probe: false }
}

/// `settle_one` with the port's reads answered from `Inc`: a close asks `close`, a persisting
/// alarm asks `note` (then `label` on its success), and a wait or skip whose line changed asks
/// one `log`. A second persist while one is in flight is refused naming the fired label.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let log = cfg.get("SPIRA_INCIDENT_LOG").cloned().unwrap_or_default();
    let line = |text: String| act(LOG, format!("{NAME}:log:{now_ms}:{:016x}", fnv1a(&text)), Job { text, log: log.clone(), ..Job::default() }, 2_000);
    let mut next = carry.clone();
    let mut acts = vec![];
    match input {
        Input::Fire => {}
        Input::Observed(s) => {
            if cfg.get("SPIRA_DB").is_some_and(|d| d.trim().is_empty()) {
                return Ok((next, acts));
            }
            next.incidents.retain(|id, _| s.incidents.iter().any(|i| &i.id == id));
            next.waiting.retain(|id, _| s.incidents.iter().any(|i| &i.id == id));
            for inc in &s.incidents {
                let phase = carry.incidents.get(&inc.id).copied().unwrap_or_default();
                if matches!(phase, Phase::Closing | Phase::Closed) {
                    continue;
                }
                let port = Observed { inc, now_iso: incident::run::iso_now_public((now_ms / 1000) as i64), closed: Default::default(), persisted: Default::default() };
                match incident::settle::settle_one(&port, &candidate(inc)) {
                    incident::settle::Outcome::Closed { evidence, .. } => {
                        next.incidents.insert(inc.id.clone(), Phase::Closing);
                        next.waiting.remove(&inc.id);
                        acts.push(act(CLOSE, format!("{}:close", inc.id), Job { id: inc.id.clone(), text: evidence, ..Job::default() }, 5_000));
                    }
                    incident::settle::Outcome::Persists { .. } => {
                        let (label, note) = port.persisted.borrow().clone().unwrap_or_default();
                        if phase == Phase::Persists {
                            return Err(format!("{NAME}: {} is in state persists; a second persist for its fix set ({label}) is refused while the first is recorded", inc.id));
                        }
                        next.incidents.insert(inc.id.clone(), Phase::Persists);
                        next.waiting.remove(&inc.id);
                        acts.push(act(NOTE, format!("{}:note:{label}", inc.id), Job { id: inc.id.clone(), text: note, label, ..Job::default() }, 10_000));
                    }
                    o @ (incident::settle::Outcome::Waiting { .. } | incident::settle::Outcome::Skipped { .. }) => {
                        let (skipped, why) = match o {
                            incident::settle::Outcome::Skipped { why, .. } => (true, why),
                            incident::settle::Outcome::Waiting { why, .. } => (false, why),
                            other => (true, format!("{other:?}")),
                        };
                        next.incidents.insert(inc.id.clone(), if skipped { Phase::Skipped } else { Phase::Waiting });
                        let l = format!("{}\t{}", inc.id, why.replace('\n', " "));
                        if carry.waiting.get(&inc.id) != Some(&l) {
                            acts.push(line(format!("settle: {} {}: {why}", if skipped { "skipped" } else { "waiting" }, inc.id)));
                        }
                        next.waiting.insert(inc.id.clone(), l);
                    }
                }
            }
        }
        Input::Outcome { effect, ok, body } if [CLOSE, NOTE, LABEL].contains(&effect.as_str()) => {
            let j: Job = serde_json::from_str(body).unwrap_or_else(|_| Job { id: body.split(": ").next().unwrap_or_default().into(), ..Job::default() });
            let phase = carry.incidents.get(&j.id).copied().unwrap_or_default();
            let want = if effect == CLOSE { Phase::Closing } else { Phase::Persists };
            if phase != want {
                return Err(format!("{NAME}: {} is in state {}; an outcome of {effect} is refused", j.id, phase_name(phase)));
            }
            match (effect.as_str(), ok) {
                (CLOSE, true) => {
                    next.incidents.insert(j.id.clone(), Phase::Closed);
                    acts.push(line(format!("settle: closed {}: {}", j.id, j.text)));
                }
                (NOTE, true) => acts.push(act(LABEL, format!("{}:label:{}", j.id, j.label), Job { id: j.id, label: j.label, ..Job::default() }, 10_000)),
                (LABEL, true) => {
                    next.incidents.insert(j.id.clone(), Phase::Waiting);
                    next.waiting.insert(j.id.clone(), format!("{}\tfix landed but the alarm persists; already with Ops", j.id));
                    acts.push(line(format!("settle: persists {} ({})", j.id, j.label)));
                }
                _ => {
                    next.incidents.insert(j.id.clone(), Phase::Open);
                }
            }
        }
        Input::Outcome { effect, .. } if effect == LOG => {}
        Input::Outcome { effect, .. } => return Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Ran(r) => {
            let id = r.key_for(NAME).unwrap_or_default();
            let phase = carry.incidents.get(&id).copied().unwrap_or_default();
            if phase != Phase::Closing {
                return Err(format!("{NAME}: {id} is in state {}; its unit's end is refused", phase_name(phase)));
            }
            next.incidents.insert(id, Phase::Closed);
        }
    }
    Ok((next, acts))
}

/// `Arc<dyn Units>` as the boxed port `RealSettler` reads its detector through.
struct Shared(std::sync::Arc<dyn runtime::units::Units>);

impl runtime::units::Units for Shared {
    fn start(&self, spec: &runtime::units::UnitSpec) -> runtime::Result<runtime::units::UnitHandle> {
        self.0.start(spec)
    }
    fn stop(&self, h: &runtime::units::UnitHandle) -> runtime::Result<()> {
        self.0.stop(h)
    }
    fn restart(&self, h: &runtime::units::UnitHandle) -> runtime::Result<()> {
        self.0.restart(h)
    }
    fn facts(&self, h: &runtime::units::UnitHandle) -> runtime::Result<runtime::units::UnitFacts> {
        self.0.facts(h)
    }
}

fn bd(e: &crate::Effects) -> Result<incident::real::RealBd, String> {
    Ok(incident::real::RealBd { bdq: spira_config::release_env::tool("bdq")?, timeout_secs: 10, retries: 2, lc: Some(super::lc(e)) })
}

/// The observe: `RealSettler`'s reads (the content list, the blocks edges, each fix's state
/// and landing, the detector through the unit port), stopping at an incident's first
/// unlanded fix as `settle_one` does. The label, home repo and release are the row's pinned keys.
pub fn observe(e: &crate::Effects) -> Result<String, String> {
    use incident::settle::Settler;
    let cfg = spira_config::process::cfg;
    let (label, home_repo, release) = (cfg("SPIRA_INCIDENT_LABEL")?, cfg("SPIRA_HOME_REPO")?, cfg("SPIRA_RELEASE")?);
    let bd = bd(e)?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let settler = incident::settle_real::RealSettler {
        bd: &bd,
        db: String::new(),
        incident_label: label,
        home_repo,
        registry: spira_config::repos::Registry::from_env(BTreeMap::new(), &home),
        release_sha: incident::settle_real::release_sha_of(&release),
        units: e.units.clone().map(|u| Box::new(Shared(u)) as Box<dyn runtime::units::Units>).ok_or_else(|| "no unit manager configured".to_string()),
    };
    let mut incidents = vec![];
    for c in settler.candidates()? {
        let mut inc = Inc { id: c.id.clone(), external_ref: c.external_ref.clone().unwrap_or_default(), labels: c.labels.clone(), fixes: vec![], detector: Detector::NoProbe };
        let mut all_landed = true;
        for fix in settler.fixes(&c.id).unwrap_or_default() {
            let state = match settler.fix_state(&fix) {
                Ok(incident::settle::FixState::Landed) => "LANDED".to_string(),
                Ok(incident::settle::FixState::Unlanded(s)) => s,
                Err(e) => format!("unknown: {e}"),
            };
            let landed = state == "LANDED";
            let landing = if !landed {
                Landing::NotInRelease
            } else {
                match settler.landing_in_force(&fix) {
                    Ok(Some(l)) => Landing::InForce(l.sha),
                    Ok(None) => Landing::NotInRelease,
                    Err(e) => Landing::Err(e),
                }
            };
            let stop = state != "DROPPED" && !matches!(landing, Landing::InForce(_));
            inc.fixes.push(Fix { id: fix, state, landing });
            if stop {
                all_landed = false;
                break;
            }
        }
        if all_landed && !inc.fixes.is_empty() {
            inc.detector = match settler.detect(&c) {
                Ok(incident::settle::Detector::Quiet) => Detector::Quiet,
                Ok(incident::settle::Detector::Firing(d)) => Detector::Firing(d),
                Ok(incident::settle::Detector::NoProbe) => Detector::NoProbe,
                Err(e) => Detector::Err(e),
            };
        }
        incidents.push(inc);
    }
    serde_json::to_string(&Snapshot { incidents }).map_err(|e| e.to_string())
}

/// Every act's runner but the observe, by `spec.name`. Success answers the job back.
pub fn run(e: &crate::Effects, spec: &runtime::EffectSpec) -> Result<String, String> {
    use incident::ports::Bd;
    let j: Job = serde_json::from_str(&spec.input).map_err(|e| e.to_string())?;
    let done = || serde_json::to_string(&j).map_err(|e| e.to_string());
    match spec.name.as_str() {
        CLOSE => {
            let r = super::lc(e).call_stdin(&["close", j.id.as_str(), "--reason-file", "-", "--actor", "incident"], &j.text).map_err(|why| format!("{}: {why}", j.id))?;
            if r.code != 0 {
                return Err(format!("{}: close exited {}: {}", j.id, r.code, r.why()));
            }
            done()
        }
        NOTE => {
            if !bd(e)?.note("", &j.id, &j.text) {
                return Err(format!("{}: bd note failed", j.id));
            }
            done()
        }
        LABEL => {
            if !bd(e)?.label_add("", &j.id, &j.label) {
                return Err(format!("{}: bd label add failed", j.id));
            }
            done()
        }
        LOG => {
            use std::io::Write;
            let path = if j.log.is_empty() { e.world.run_dir.join("incident.log") } else { std::path::PathBuf::from(&j.log) };
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            writeln!(f, "{}", j.text).map_err(|err| err.to_string())?;
            Ok("logged".into())
        }
        other => Err(format!("{NAME}: no runner for {other}")),
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[];

super::hosted!(IncidentSettle);
