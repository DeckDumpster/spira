//! The `DwarfFleet` section: what the call pass publishes about the dwarves, and the applier
//! that steps `max_live_dwarves` for the reconciler's intents. The status core is pure; the
//! store, the config and the step log are ports.

use std::collections::BTreeMap;

use reconciler_engine::goodput_pass::Lc;
use reconciler_engine::select::knob_key;
use serde_json::{json, Value};
use spira_store::lc_state::past_builder;
use spira_desired_state::intent::{parse_intent, parse_key, Intent};
use spira_desired_state::section::{parse_rfc3339, parse_section, ConditionStatus, Section};
use spira_desired_state::sections::dwarf_fleet as fleet;
use spira_desired_state::store::format_rfc3339;

use crate::model::LcRow;

const HOUR: u64 = 3_600;
const DAY: u64 = 24 * HOUR;
/// A section unchanged is still published this often, inside its 15 m `max_age`.
pub const KEEPALIVE_SECS: u64 = 300;

use spira_desired_state::sections::host;

/// The kind and condition of the `host` section this one mirrors. Absent until it is armed.
pub const HOST_KIND: &str = host::NAME;
pub const HOST_SHEDDING_CONDITION: &str = host::HOST_SHEDDING;

/// One dwarf run: a `born` line matched with the `done` line it ended on, or still alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub rune: String,
    pub start: u64,
    pub end: u64,
    pub bead: String,
    /// Still alive: it has no bead and no end, so only the hour's busy time counts it.
    pub open: bool,
}

impl Run {
    fn secs_within(&self, from: u64, to: u64) -> u64 {
        self.end.min(to).saturating_sub(self.start.max(from))
    }
}

/// The runs the ledger records. A `done` carries its own wall time, which gives its start; one
/// without it starts where the `born` it closes did. That `born` is the oldest whose process is
/// dead, since the dwarf that wrote the `done` has exited. A `born` never closed is a run to
/// `now` only while its process lives.
pub fn runs(ledger: &str, now: u64, alive: &dyn Fn(u32) -> bool) -> Vec<Run> {
    let mut open: BTreeMap<&str, Vec<(u64, u32)>> = BTreeMap::new();
    let mut out = Vec::new();
    for line in ledger.lines() {
        let mut w = line.split_whitespace();
        let (Some(ts), Some(what), Some(rune)) = (w.next().and_then(parse_rfc3339), w.next(), w.next()) else { continue };
        match what {
            "born" => {
                let pid = w.next().and_then(|p| p.parse().ok()).unwrap_or(0);
                open.entry(rune).or_default().push((ts, pid));
            }
            "done" => {
                let Some(bead) = w.next() else { continue };
                let wall = w.find_map(|f| f.strip_prefix("wall_s=")?.parse::<u64>().ok());
                let q = open.entry(rune).or_default();
                let closed = q.iter().position(|(_, pid)| *pid == 0 || !alive(*pid)).or(if q.is_empty() { None } else { Some(0) });
                let born = closed.map(|i| q.remove(i).0);
                if let Some(start) = wall.map(|s| ts.saturating_sub(s)).or(born) {
                    out.push(Run { rune: rune.into(), start, end: ts, bead: bead.into(), open: false });
                }
            }
            _ => {}
        }
    }
    for (rune, q) in open {
        for (start, pid) in q {
            if pid != 0 && alive(pid) {
                out.push(Run { rune: rune.into(), start, end: now, bead: String::new(), open: true });
            }
        }
    }
    out
}

/// What the call pass saw this pass.
pub struct Reading<'a> {
    pub now: u64,
    pub runs: &'a [Run],
    pub rows: &'a [LcRow],
    pub live: u64,
    pub ceiling: u64,
    pub claimable_ready: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fleet {
    pub utilisation_1h: f64,
    pub minutes_per_landed_24h: Option<f64>,
    pub never_submitted_share_24h: Option<f64>,
    pub live: u64,
    pub ceiling: u64,
    pub claimable_ready: u64,
}

fn landed_in(rows: &[LcRow], now: u64, secs: u64) -> usize {
    let from = now.saturating_sub(secs) as i64;
    rows.iter().filter(|r| r.state == "LANDED" && r.updated_at.is_some_and(|t| t > from && t <= now as i64)).count()
}

pub fn measure(r: &Reading) -> Fleet {
    let hour_from = r.now.saturating_sub(HOUR);
    let busy: u64 = r.runs.iter().map(|run| run.secs_within(hour_from, r.now)).sum();
    let capacity = r.ceiling * HOUR;
    let utilisation = if r.claimable_ready == 0 || capacity == 0 { 0.0 } else { (busy as f64 / capacity as f64).min(1.0) };

    let day_from = r.now.saturating_sub(DAY);
    let ended: Vec<&Run> = r.runs.iter().filter(|run| !run.open && run.end > day_from && run.end <= r.now).collect();
    let landed = landed_in(r.rows, r.now, DAY);
    let state_of: BTreeMap<&str, &str> = r.rows.iter().map(|row| (row.bead_id.as_str(), row.state.as_str())).collect();
    let (mut work, mut lost) = (0u64, 0u64);
    for run in &ended {
        // A run on a bead with no lifecycle row is not builder work on a bead (an ops sweep).
        let Some(state) = state_of.get(run.bead.as_str()) else { continue };
        let secs = run.end - run.start;
        work += secs;
        if !past_builder(state) {
            lost += secs;
        }
    }
    Fleet {
        utilisation_1h: utilisation,
        minutes_per_landed_24h: (landed > 0).then(|| work as f64 / 60.0 / landed as f64),
        never_submitted_share_24h: (work > 0).then(|| lost as f64 / work as f64),
        live: r.live,
        ceiling: r.ceiling,
        claimable_ready: r.claimable_ready,
    }
}

fn term(v: Option<f64>, window: &str, why: &str) -> Value {
    match v {
        Some(v) => json!({ "value": v, "window": window }),
        None => json!({ "unobservable": why, "window": window }),
    }
}

fn level(v: u64) -> Value {
    json!({ "value": v as f64, "window": "level" })
}

fn condition(prev: Option<&Section>, type_: &str, on: bool, reason: &str, now: u64) -> Value {
    let want = if on { ConditionStatus::True } else { ConditionStatus::False };
    let since = prev
        .and_then(|s| s.status.conditions.iter().find(|c| c.type_ == type_ && c.status == want))
        .map_or_else(|| format_rfc3339(now), |c| c.last_transition_time.clone());
    json!({ "type": type_, "status": if on { "True" } else { "False" }, "reason": reason, "lastTransitionTime": since })
}

/// The section's spec target for utilisation, as the store holds it.
pub fn target_of(prev: Option<&Section>) -> Option<f64> {
    prev?.spec.get(fleet::UTILISATION_SPEC)?.value.as_f64()
}

/// The status for this reading, and whether `prev` already says the same inside the keepalive.
pub fn status_of(prev: Option<&Section>, f: &Fleet, host_shedding: bool, now: u64) -> (Value, bool) {
    let over = target_of(prev).is_some_and(|t| f.claimable_ready > 0 && f.utilisation_1h > t);
    let status = json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        "conditions": [
            condition(prev, fleet::SATURATED, over, &format!("utilisation {:.2} with {} claimable", f.utilisation_1h, f.claimable_ready), now),
            condition(prev, fleet::HOST_SHEDDING, host_shedding, "the host section is shedding", now),
        ],
        fleet::UTILISATION: json!({ "value": f.utilisation_1h, "window": "1h" }),
        fleet::MINUTES_PER_LANDED: term(f.minutes_per_landed_24h, "24h", "no bead landed in the window"),
        fleet::NEVER_SUBMITTED: term(f.never_submitted_share_24h, "24h", "no run on a bead ended in the window"),
        "live": level(f.live),
        "ceiling": level(f.ceiling),
        "claimable_ready": level(f.claimable_ready),
    });
    let unchanged = prev.is_some_and(|s| {
        let kept = s.metadata.observed_at.as_deref().and_then(parse_rfc3339).is_some_and(|t| now.saturating_sub(t) < KEEPALIVE_SECS);
        kept && serde_json::to_value(&s.status).ok().as_ref() == Some(&status)
    });
    (status, unchanged)
}

pub fn section_body(status: &Value, now: u64) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": fleet::NAME,
        "metadata": { "name": fleet::SECTION, "owner": fleet::OWNER, "observedAt": format_rfc3339(now), "target_latency": "5m", "max_age": "15m" },
        "status": status,
        "flow": { "consumes": ["supply/landable", "host/scratch"], "produces": ["lifecycle/submitted"] },
        "distance": { "fn": fleet::DISTANCE, "open": 0.05, "close": 0.0 },
        "actions": [
            { "name": fleet::RAISE, "class": "capacity-raise", "step": "+1", "moves": fleet::UTILISATION,
              "addresses": [fleet::SATURATED], "expected_effect": -0.1, "horizon": "65m" },
            { "name": fleet::LOWER, "class": "capacity-lower", "step": "-1", "moves": fleet::UTILISATION,
              "addresses": [fleet::HOST_SHEDDING], "expected_effect": 0.1, "horizon": "65m" },
        ],
    })
}

/// What an intent comes to against the ceiling the config holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Set(u64),
    AlreadyApplied,
    Refuse(String),
}

/// The compare-and-swap: the ceiling at `to` is a step already taken; at `from` it is taken
/// now, if `to` stays in 1..=`max`; anywhere else the intent was written against a ceiling
/// that has since moved.
pub fn decide(i: &Intent, ceiling: u64, max: u64) -> Step {
    let (Some(from), Some(to)) = (i.from.as_u64(), i.to.as_u64()) else {
        return Step::Refuse(format!("{} -> {} is not one whole ceiling to another", i.from, i.to));
    };
    let direction_ok = match i.action.as_str() {
        fleet::RAISE => to == from + 1,
        fleet::LOWER => to + 1 == from,
        other => return Step::Refuse(format!("{other:?} is not an action {} declares", fleet::NAME)),
    };
    if !direction_ok {
        return Step::Refuse(format!("{} is one step, not {from} -> {to}", i.action));
    }
    if ceiling == to {
        return Step::AlreadyApplied;
    }
    if ceiling != from {
        return Step::Refuse(format!("stale: the ceiling is {ceiling}, not {from}"));
    }
    if to < 1 || to > max {
        return Step::Refuse(format!("{to} is outside 1..={max} (SPIRA_MAX_DWARVES)"));
    }
    Step::Set(to)
}

/// `max_live_dwarves` and `max_dwarves`, read and written through `spira-config`.
pub trait Config {
    fn get(&self, key: &str) -> Result<u64, String>;
    fn set(&self, key: &str, value: u64) -> Result<(), String>;
}

pub struct Ports<'a> {
    pub lc: &'a dyn Lc,
    pub config: &'a dyn Config,
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

/// Every unacknowledged DwarfFleet step in the snapshot: its key and intent.
pub fn open_intents(snap: &Value) -> Vec<(String, Intent)> {
    let items = snap.get("intents").and_then(Value::as_array).cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            let key = item.get("key")?.as_str()?;
            let (kind, _, _) = parse_key(key)?;
            let i = parse_intent(item.get("intent")?).ok()?;
            (kind == fleet::NAME && i.ack.is_none()).then(|| (key.to_string(), i))
        })
        .collect()
}

fn apply_one(p: &Ports, key: &str, i: &Intent, f: &Fleet) -> Result<String, String> {
    let ceiling = p.config.get(fleet::CEILING_KEY)?;
    let max = p.config.get(fleet::CEILING_MAX_KEY)?;
    let ack = |result: &str, reason: Option<&str>, measurement: Option<&Value>| {
        let mut a = argv(&["intent", "ack", key, "--actor", fleet::OWNER, "--result", result]);
        if let Some(r) = reason {
            a.extend(argv(&["--reason", r]));
        }
        if let Some(m) = measurement {
            a.extend(argv(&["--measurement", &m.to_string()]));
        }
        a
    };
    let (args, word) = match decide(i, ceiling, max) {
        Step::AlreadyApplied => (ack("already-applied", None, None), "already-applied"),
        Step::Refuse(why) => (ack("refused", Some(&why), None), "refused"),
        Step::Set(to) => {
            p.config.set(fleet::CEILING_KEY, to)?;
            let measured = json!({
                fleet::UTILISATION: f.utilisation_1h,
                fleet::MINUTES_PER_LANDED: f.minutes_per_landed_24h,
                fleet::NEVER_SUBMITTED: f.never_submitted_share_24h,
                "claimable_ready": f.claimable_ready,
            });
            (p.record)(&format!("{key} {} {} -> {to}: {measured}", i.action, i.from));
            (ack("applied", None, Some(&measured)), "applied")
        }
    };
    lc_ok(p, &format!("ack {key}"), &args)?;
    Ok(word.to_string())
}

fn sync_knobs(p: &Ports, ceiling: u64) -> Result<(), String> {
    for action in [fleet::RAISE, fleet::LOWER] {
        let knob = knob_key(&format!("{}/{}", fleet::NAME, fleet::SECTION), action);
        lc_ok(p, &knob, &argv(&["intent", "knob-init", &knob, "--actor", fleet::OWNER, "--overwrite", "--json", &ceiling.to_string()]))?;
    }
    Ok(())
}

fn previous(snap: &Value) -> Option<Section> {
    snap.get("sections")?
        .as_array()?
        .iter()
        .filter_map(|i| parse_section(i.get("section")?).ok())
        .find(|s| s.kind == fleet::NAME && s.metadata.name == fleet::SECTION)
}

pub(crate) fn host_shedding(snap: &Value) -> bool {
    let items = snap.get("sections").and_then(Value::as_array).cloned().unwrap_or_default();
    items.iter().filter_map(|i| parse_section(i.get("section")?).ok()).any(|s| {
        s.kind == HOST_KIND && s.status.conditions.iter().any(|c| c.type_ == HOST_SHEDDING_CONDITION && c.status == ConditionStatus::True)
    })
}

/// One call pass's turn on the section: apply each open step, then publish what the fleet looks
/// like now. `Err` when the store cannot be read or written, so the pass reports it.
pub fn run(p: &Ports, now: u64, runs: &[Run], rows: &[LcRow], live: u64, claimable_ready: u64) -> Result<Report, String> {
    let snap = lc_ok(p, "section snapshot", &argv(&["section", "snapshot"]))?;
    let reading = |ceiling| Reading { now, runs, rows, live, ceiling, claimable_ready };
    let mut report = Report::default();
    let before = measure(&reading(p.config.get(fleet::CEILING_KEY)?));
    for (key, i) in open_intents(&snap) {
        let word = apply_one(p, &key, &i, &before)?;
        report.acked.push((key, word));
    }
    let ceiling = p.config.get(fleet::CEILING_KEY)?;
    sync_knobs(p, ceiling)?;
    let prev = previous(&snap);
    let (status, unchanged) = status_of(prev.as_ref(), &measure(&reading(ceiling)), host_shedding(&snap), now);
    if unchanged {
        return Ok(report);
    }
    let rev = prev.and_then(|s| s.metadata.resource_version).unwrap_or_else(|| "0".into());
    let body = section_body(&status, now).to_string();
    lc_ok(p, "publish DwarfFleet/fleet", &argv(&["section", "publish-status", "--actor", fleet::OWNER, "--expect-rev", &rev, "--json", &body]))?;
    report.published = true;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_475_600;

    fn at(ago: u64) -> String {
        format_rfc3339(NOW - ago)
    }

    #[test]
    fn a_done_without_wall_time_starts_where_the_dead_born_it_closes_did() {
        let text = format!(
            "{} born builder 7\n{} born builder 8\n{} done builder sp-a rc=0 status=open wall_s=?\ngarbage\n{} awake builder sp-b\n{} done\n",
            at(900),
            at(800),
            at(100),
            at(50),
            at(40)
        );
        let runs = runs(&text, NOW, &|pid| pid == 7);
        assert_eq!(
            runs,
            [
                Run { rune: "builder".into(), start: NOW - 800, end: NOW - 100, bead: "sp-a".into(), open: false },
                Run { rune: "builder".into(), start: NOW - 900, end: NOW, bead: String::new(), open: true },
            ],
            "the done closes the born whose process is dead; the live one stays a run to now"
        );
    }

    #[test]
    fn a_done_with_no_born_and_no_wall_time_is_no_run_and_with_wall_time_is() {
        let text = format!("{} done builder sp-a rc=0 status=open wall_s=?\n{} done builder sp-b rc=0 status=open wall_s=60\n", at(100), at(50));
        let runs = runs(&text, NOW, &|_| false);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].bead.as_str(), runs[0].start), ("sp-b", NOW - 110));
    }

    #[test]
    fn the_ceiling_the_config_holds_decides_set_already_applied_or_stale() {
        let i = parse_intent(&json!({
            "id": "1", "action": fleet::LOWER, "target": "knobs/DwarfFleet/fleet/max-live-dwarves-lower", "from": 3, "to": 2,
            "moves": "utilisation_1h", "baseline": 0.1, "expected_effect": 0.1, "horizon": "65m",
            "addresses": ["HostShedding"], "taken_at": "2026-10-31T19:40:00Z", "by": { "pass": "p", "lock_token": "1.0" },
        }))
        .unwrap();
        assert_eq!(decide(&i, 3, 4), Step::Set(2));
        assert_eq!(decide(&i, 2, 4), Step::AlreadyApplied);
        assert!(matches!(decide(&i, 4, 4), Step::Refuse(m) if m.starts_with("stale")));
        assert!(matches!(decide(&i, 9, 4), Step::Refuse(m) if m.starts_with("stale")), "a moved ceiling is stale before it is out of bounds");
    }

    fn host_section(shedding: bool) -> Value {
        json!({ "section": {
            "apiVersion": "spira/v1", "kind": HOST_KIND,
            "metadata": { "name": "box", "owner": "host", "observedAt": "2026-10-31T19:40:00Z", "target_latency": "5m", "max_age": "15m" },
            "status": { "conditions": [{ "type": HOST_SHEDDING_CONDITION, "status": if shedding { "True" } else { "False" }, "reason": "t", "lastTransitionTime": "2026-10-31T19:40:00Z" }] },
        }})
    }

    #[test]
    fn a_shedding_host_section_raises_host_shedding_and_a_calm_or_absent_one_does_not() {
        assert!(host_shedding(&json!({ "sections": [host_section(true)] })));
        assert!(!host_shedding(&json!({ "sections": [host_section(false)] })));
        assert!(!host_shedding(&json!({ "sections": [] })));
        let f = Fleet { utilisation_1h: 0.1, minutes_per_landed_24h: None, never_submitted_share_24h: None, live: 2, ceiling: 4, claimable_ready: 3 };
        let (status, _) = status_of(None, &f, true, NOW);
        let shedding = status["conditions"].as_array().unwrap().iter().find(|c| c["type"] == fleet::HOST_SHEDDING).unwrap();
        assert_eq!(shedding["status"], "True");
    }

    #[test]
    fn the_mirror_reads_the_condition_the_watchtower_publishes() {
        use watchtower::host_section::{status_of, Host, Sample};
        let h = |shed_from| Host {
            sample: Sample { scratch: Some(0.9), worktrees: Some(0.9), root: Some(0.9), io: Some(1.0) },
            certify_par: 1,
            shed_from,
            stale_scratch: 0,
            stale_targets: 0,
        };
        let published = |shed_from| {
            let (status, _) = status_of(None, &h(shed_from), NOW);
            json!({ "section": {
                "apiVersion": "spira/v1", "kind": HOST_KIND,
                "metadata": { "name": "host", "owner": "watchtower", "observedAt": "2026-10-31T19:40:00Z", "target_latency": "5m", "max_age": "15m" },
                "status": status,
            }})
        };
        assert!(host_shedding(&json!({ "sections": [published(Some(2))] })), "a shed in force raises what the fleet mirrors");
        assert!(!host_shedding(&json!({ "sections": [published(None)] })));
    }

    #[test]
    fn only_unacknowledged_dwarf_fleet_intents_are_open() {
        let intent = |action: &str, ack: Value| {
            let mut v = json!({
                "id": "1", "action": action, "target": "knobs/x/y/z", "from": 3, "to": 4, "moves": "utilisation_1h", "baseline": 0.9,
                "expected_effect": -0.1, "horizon": "65m", "addresses": [], "taken_at": "2026-10-31T19:40:00Z",
                "by": { "pass": "p", "lock_token": "1.0" },
            });
            if !ack.is_null() {
                v["ack"] = ack;
            }
            v
        };
        let acked = json!({ "at": "2026-10-31T19:41:00Z", "result": "applied", "observed_rev": 3 });
        let snap = json!({ "intents": [
            { "key": "intents/DwarfFleet/fleet/1", "intent": intent(fleet::RAISE, Value::Null) },
            { "key": "intents/DwarfFleet/fleet/2", "intent": intent(fleet::RAISE, acked) },
            { "key": "intents/Alarms/all/3", "intent": intent("retry-delivery", Value::Null) },
            { "key": "intents/DwarfFleet/fleet/4", "intent": { "id": "4" } },
            { "key": "not-an-intent-key", "intent": intent(fleet::RAISE, Value::Null) },
        ]});
        assert_eq!(open_intents(&snap).iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["intents/DwarfFleet/fleet/1"]);
    }

    #[test]
    fn the_dwarf_ledgers_own_writer_is_what_the_reader_reads() {
        let dir = testkit::TempDir::new("fleet-ledger");
        let l = dwarf::ledger::Ledger { path: dir.join("dwarf-ledger.log"), dry: false };
        l.born(100, "builder", 4242);
        l.done(700, "builder", "sp-a", 0, "open", &dwarf::ledger::SessionFields { wall_s: Some(600), ..Default::default() });
        l.born(800, "builder", 4343);
        let text = std::fs::read_to_string(dir.join("dwarf-ledger.log")).unwrap();
        let runs = runs(&text, 1000, &|pid| pid == 4343);
        let spans: Vec<_> = runs.iter().map(|r| (r.bead.as_str(), r.start, r.end, r.open)).collect();
        assert_eq!(spans, [("sp-a", 100, 700, false), ("", 800, 1000, true)]);
    }
}
