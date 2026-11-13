//! The `Host` section: what the watchtower publishes about the box's room and IO, and the
//! appliers that shed or restore a gate slot and reap a tenth of what is stale for the
//! reconciler's intents. The status core is pure; the store, the config, the reaps and the
//! step log are ports.

use reconciler_engine::goodput_pass::Lc;
use reconciler_engine::select::knob_key;
use serde_json::{json, Value};
use spira_desired_state::intent::{parse_intent, parse_key, Intent};
use spira_desired_state::section::{parse_rfc3339, parse_section, ConditionStatus, Section};
use spira_desired_state::sections::host;
use spira_desired_state::store::format_rfc3339;

/// A section unchanged is still published this often, inside its 15 m `max_age`.
pub const KEEPALIVE_SECS: u64 = 300;
/// The share of the stale set one reap takes, by count.
pub const REAP_FRACTION: f64 = 0.1;

/// The paths each role is measured at, from configuration.
#[derive(Clone, Debug)]
pub struct Cfg {
    pub scratch: String,
    pub worktrees: String,
    pub root: String,
    pub psi_dir: String,
}

/// What the box looked like this pass: each free share and the io `full avg300`, `None` where
/// it could not be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    pub scratch: Option<f64>,
    pub worktrees: Option<f64>,
    pub root: Option<f64>,
    pub io: Option<f64>,
}

pub fn free_share(avail_mib: i64, size_mib: i64) -> Option<f64> {
    (size_mib > 0).then(|| (avail_mib.max(0) as f64 / size_mib as f64).min(1.0))
}

/// The box as `usage` and `read` (a file's text) show it, at the roles' configured paths.
pub fn sample(cfg: &Cfg, usage: &dyn Fn(&str) -> Option<crate::fsstat::Usage>, read: &dyn Fn(&str) -> Option<String>) -> Sample {
    let share = |path: &str| usage(path).and_then(|u| free_share(u.avail_mib, u.size_mib));
    Sample {
        scratch: share(&cfg.scratch),
        worktrees: share(&cfg.worktrees),
        root: share(&cfg.root),
        io: read(&format!("{}/io", cfg.psi_dir)).and_then(|t| crate::probes::psi_full_avg300(&t)),
    }
}

/// The spec's three targets as the store holds them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Targets {
    pub scratch: f64,
    pub worktrees: f64,
    pub io: f64,
}

pub fn targets_of(prev: Option<&Section>) -> Option<Targets> {
    let spec = &prev?.spec;
    let of = |k: &str| spec.get(k)?.value.as_f64();
    Some(Targets { scratch: of(host::SCRATCH_SPEC)?, worktrees: of(host::WORKTREES_SPEC)?, io: of(host::IO_SPEC)? })
}

/// The section's whole state for one pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Host {
    pub sample: Sample,
    pub certify_par: u64,
    /// `certify_par` before the first shed of the one in force.
    pub shed_from: Option<u64>,
    pub stale_scratch: u64,
    pub stale_targets: u64,
}

/// The sum of each term's excess over its spec, normalised by the spec; `None` while a term
/// cannot be read, since a distance over a guess is no distance.
pub fn distance(s: &Sample, t: &Targets) -> Option<f64> {
    let short = |have: f64, want: f64| (want - have).max(0.0) / want;
    Some(short(s.scratch?, t.scratch) + short(s.worktrees?, t.worktrees) + (s.io? - t.io).max(0.0) / t.io)
}

fn level(v: f64) -> Value {
    json!({ "value": v, "window": "level" })
}

fn condition(prev: Option<&Section>, type_: &str, on: bool, reason: &str, now: u64) -> Value {
    let want = if on { ConditionStatus::True } else { ConditionStatus::False };
    let since = prev
        .and_then(|s| s.status.conditions.iter().find(|c| c.type_ == type_ && c.status == want))
        .map_or_else(|| format_rfc3339(now), |c| c.last_transition_time.clone());
    json!({ "type": type_, "status": if on { "True" } else { "False" }, "reason": reason, "lastTransitionTime": since })
}

/// The status for this pass, and whether `prev` already says the same inside the keepalive.
pub fn status_of(prev: Option<&Section>, h: &Host, now: u64) -> (Value, bool) {
    let t = targets_of(prev);
    let s = &h.sample;
    let below = |have: Option<f64>, want: Option<f64>| matches!((have, want), (Some(a), Some(b)) if a < b);
    let scratch_short = below(s.scratch, t.map(|t| t.scratch));
    let disk_short = below(s.worktrees, t.map(|t| t.worktrees));
    let io_pressure = matches!((s.io, t), (Some(io), Some(t)) if io > t.io);
    let shedding = h.shed_from.is_some_and(|from| h.certify_par < from);
    let shed = shedding && t.and_then(|t| distance(s, &t)).is_some_and(|d| d < host::CLOSE);
    let mut status = json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        "conditions": [
            condition(prev, host::SCRATCH_SHORT, scratch_short, "scratch is under its free share", now),
            condition(prev, host::DISK_SHORT, disk_short, "the worktree disk is under its free share", now),
            condition(prev, host::IO_PRESSURE, io_pressure, "io full avg300 is over its spec", now),
            condition(prev, host::HOST_SHEDDING, shedding, "a gate slot is shed", now),
            condition(prev, host::SHED, shed, "a shed is in force and the host is inside its spec", now),
        ],
        host::CERTIFY_PAR: level(h.certify_par as f64),
        host::STALE_SCRATCH: level(h.stale_scratch as f64),
        host::STALE_TARGETS: level(h.stale_targets as f64),
    });
    let fields = status.as_object_mut().expect("a status is an object");
    for (name, v) in [(host::SCRATCH, s.scratch), (host::WORKTREES, s.worktrees), (host::ROOT, s.root)] {
        if let Some(v) = v {
            fields.insert(name.into(), level(v));
        }
    }
    if let Some(io) = s.io {
        fields.insert(host::IO.into(), json!({ "value": io, "window": "5m" }));
    }
    if let Some(from) = h.shed_from {
        fields.insert(host::SHED_FROM.into(), level(from as f64));
    }
    let unchanged = prev.is_some_and(|p| {
        let kept = p.metadata.observed_at.as_deref().and_then(parse_rfc3339).is_some_and(|t| now.saturating_sub(t) < KEEPALIVE_SECS);
        kept && serde_json::to_value(&p.status).ok().as_ref() == Some(&status)
    });
    (status, unchanged)
}

pub fn section_body(status: &Value, now: u64) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": host::NAME,
        "metadata": { "name": host::SECTION, "owner": host::OWNER, "observedAt": format_rfc3339(now), "target_latency": "5m", "max_age": "15m" },
        "status": status,
        "flow": { "consumes": [], "produces": ["host/scratch", "host/io"] },
        "distance": { "fn": host::DISTANCE, "open": host::OPEN, "close": host::CLOSE },
        "actions": host::actions(),
    })
}

/// What a gate-slot intent comes to against the `certify_par` the config holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Set(u64),
    AlreadyApplied,
    Refuse(String),
}

/// The compare-and-swap: `certify_par` at `to` is a step already taken; at `from` it is taken
/// now; anywhere else the intent was written against a value that has since moved. A shed
/// leaves at least one slot, and a restore never goes above where the shed began.
pub fn decide(i: &Intent, par: u64, shed_from: Option<u64>) -> Step {
    let (Some(from), Some(to)) = (i.from.as_u64(), i.to.as_u64()) else {
        return Step::Refuse(format!("{} -> {} is not one whole slot count to another", i.from, i.to));
    };
    let direction_ok = match i.action.as_str() {
        host::SHED_SLOT => to + 1 == from,
        host::RESTORE_SLOT => to == from + 1,
        other => return Step::Refuse(format!("{other:?} is not a gate-slot action {} declares", host::NAME)),
    };
    if !direction_ok {
        return Step::Refuse(format!("{} is one step, not {from} -> {to}", i.action));
    }
    if par == to {
        return Step::AlreadyApplied;
    }
    if par != from {
        return Step::Refuse(format!("stale: certify_par is {par}, not {from}"));
    }
    if to < 1 {
        return Step::Refuse("a shed leaves at least one gate slot".into());
    }
    if i.action == host::RESTORE_SLOT && !shed_from.is_some_and(|limit| to <= limit) {
        return Step::Refuse(match shed_from {
            Some(limit) => format!("{to} is above {limit}, where the shed began"),
            None => "no shed is in force, so there is nothing to restore".into(),
        });
    }
    Step::Set(to)
}

/// What a reap intent comes to against the stale count now: the knob it was written from
/// mirrors that count, so a count already at or under `to` is a reap already done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReapStep {
    Reap,
    AlreadyApplied,
    Refuse(String),
}

pub fn decide_reap(i: &Intent, stale_now: u64) -> ReapStep {
    let (Some(from), Some(to)) = (i.from.as_u64(), i.to.as_u64()) else {
        return ReapStep::Refuse(format!("{} -> {} is not one whole stale count to another", i.from, i.to));
    };
    if to >= from {
        return ReapStep::Refuse(format!("{} only reaps: {from} -> {to} does not shrink the stale set", i.action));
    }
    if stale_now <= to {
        ReapStep::AlreadyApplied
    } else {
        ReapStep::Reap
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    Scratch,
    Targets,
}

/// `spira-config` for `certify_par`.
pub trait Config {
    fn get(&self, key: &str) -> Result<u64, String>;
    fn set(&self, key: &str, value: u64) -> Result<(), String>;
}

/// The two reaps over what is stale.
pub trait Reaper {
    fn stale(&self, what: Stale) -> Result<u64, String>;
    /// Removes the oldest `fraction` of the stale set by count; returns how many went.
    fn reap(&self, what: Stale, fraction: f64) -> Result<u64, String>;
}

pub struct Ports<'a> {
    pub lc: &'a dyn Lc,
    pub config: &'a dyn Config,
    pub reaper: &'a dyn Reaper,
    /// One line per step taken, with the measurement that justified it.
    pub record: &'a dyn Fn(&str),
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Each intent key and the word it was acked with.
    pub acked: Vec<(String, String)>,
    pub published: bool,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn lc_ok(p: &Ports, what: &str, args: &[String]) -> Result<Value, String> {
    match p.lc.call(args) {
        (0, v) => Ok(v),
        (code, v) => Err(format!("{what}: exit {code}: {v}")),
    }
}

/// Every unacknowledged Host step in the snapshot: its key and intent.
pub fn open_intents(snap: &Value) -> Vec<(String, Intent)> {
    let items = snap.get("intents").and_then(Value::as_array).cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            let key = item.get("key")?.as_str()?;
            let (kind, _, _) = parse_key(key)?;
            let i = parse_intent(item.get("intent")?).ok()?;
            (kind == host::NAME && i.ack.is_none()).then(|| (key.to_string(), i))
        })
        .collect()
}

fn previous(snap: &Value) -> Option<Section> {
    snap.get("sections")?
        .as_array()?
        .iter()
        .filter_map(|i| parse_section(i.get("section")?).ok())
        .find(|s| s.kind == host::NAME && s.metadata.name == host::SECTION)
}

fn shed_from_of(prev: Option<&Section>) -> Option<u64> {
    let v = prev?.status.fields.get(host::SHED_FROM)?.get("value")?.as_f64()?;
    Some(v as u64)
}

fn ack_args(key: &str, result: &str, reason: Option<&str>, measurement: Option<&Value>) -> Vec<String> {
    let mut a = argv(&["intent", "ack", key, "--actor", host::OWNER, "--result", result]);
    if let Some(r) = reason {
        a.extend(argv(&["--reason", r]));
    }
    if let Some(m) = measurement {
        a.extend(argv(&["--measurement", &m.to_string()]));
    }
    a
}

fn reap_of(action: &str) -> Option<Stale> {
    match action {
        host::REAP_SCRATCH => Some(Stale::Scratch),
        host::REAP_TARGETS => Some(Stale::Targets),
        _ => None,
    }
}

/// Applies one intent; `shed_from` is the shed in force, set on the first shed.
fn apply_one(p: &Ports, key: &str, i: &Intent, shed_from: &mut Option<u64>, sample: &Sample) -> Result<String, String> {
    let measured = json!({ host::SCRATCH: sample.scratch, host::WORKTREES: sample.worktrees, host::IO: sample.io });
    let (args, word) = if let Some(what) = reap_of(&i.action) {
        match decide_reap(i, p.reaper.stale(what)?) {
            ReapStep::AlreadyApplied => (ack_args(key, "already-applied", None, None), "already-applied"),
            ReapStep::Refuse(why) => (ack_args(key, "refused", Some(&why), None), "refused"),
            ReapStep::Reap => {
                let removed = p.reaper.reap(what, REAP_FRACTION)?;
                let m = json!({ "removed": removed, "of": i.from, "measured": measured });
                (p.record)(&format!("{key} {} {} -> {}: {m}", i.action, i.from, i.to));
                (ack_args(key, "applied", None, Some(&m)), "applied")
            }
        }
    } else {
        let par = p.config.get(host::CERTIFY_PAR_KEY)?;
        match decide(i, par, *shed_from) {
            Step::AlreadyApplied => (ack_args(key, "already-applied", None, None), "already-applied"),
            Step::Refuse(why) => (ack_args(key, "refused", Some(&why), None), "refused"),
            Step::Set(to) => {
                if i.action == host::SHED_SLOT && shed_from.is_none() {
                    *shed_from = Some(par);
                }
                p.config.set(host::CERTIFY_PAR_KEY, to)?;
                (p.record)(&format!("{key} {} {} -> {to}: {measured}", i.action, i.from));
                (ack_args(key, "applied", None, Some(&measured)), "applied")
            }
        }
    };
    lc_ok(p, &format!("ack {key}"), &args)?;
    Ok(word.to_string())
}

/// The knobs the selector steps mirror what the owner holds, so each intent has a `from`.
fn sync_knobs(p: &Ports, par: u64, stale_scratch: u64, stale_targets: u64) -> Result<(), String> {
    let section = format!("{}/{}", host::NAME, host::SECTION);
    for (action, value) in [
        (host::SHED_SLOT, par),
        (host::RESTORE_SLOT, par),
        (host::REAP_SCRATCH, stale_scratch),
        (host::REAP_TARGETS, stale_targets),
    ] {
        let knob = knob_key(&section, action);
        lc_ok(p, &knob, &argv(&["intent", "knob-init", &knob, "--actor", host::OWNER, "--overwrite", "--json", &value.to_string()]))?;
    }
    Ok(())
}

/// One watchtower pass's turn on the section: apply each open step, then publish what the host
/// looks like now. `Err` when the store, the config or a reap cannot answer, so the pass
/// reports it.
pub fn run(p: &Ports, now: u64, sample: &Sample) -> Result<Report, String> {
    let snap = lc_ok(p, "section snapshot", &argv(&["section", "snapshot"]))?;
    let prev = previous(&snap);
    let mut shed_from = shed_from_of(prev.as_ref());
    let mut report = Report::default();
    for (key, i) in open_intents(&snap) {
        let word = apply_one(p, &key, &i, &mut shed_from, sample)?;
        report.acked.push((key, word));
    }
    let par = p.config.get(host::CERTIFY_PAR_KEY)?;
    if shed_from.is_some_and(|from| par >= from) {
        shed_from = None;
    }
    let (stale_scratch, stale_targets) = (p.reaper.stale(Stale::Scratch)?, p.reaper.stale(Stale::Targets)?);
    sync_knobs(p, par, stale_scratch, stale_targets)?;
    let h = Host { sample: sample.clone(), certify_par: par, shed_from, stale_scratch, stale_targets };
    let (status, unchanged) = status_of(prev.as_ref(), &h, now);
    if unchanged {
        return Ok(report);
    }
    let rev = prev.and_then(|s| s.metadata.resource_version).unwrap_or_else(|| "0".into());
    let body = section_body(&status, now).to_string();
    lc_ok(p, "publish Host/host", &argv(&["section", "publish-status", "--actor", host::OWNER, "--expect-rev", &rev, "--json", &body]))?;
    report.published = true;
    Ok(report)
}
