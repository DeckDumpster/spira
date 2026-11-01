//! The assembler: one `spira-lc section snapshot` in, the current and desired documents out,
//! both at that snapshot's `list_rev`, with every intent and the previous pass record read at
//! the same revision, and every knob an intent may move with them. Nothing is carried from an earlier pass but what the store holds, so a
//! fresh process assembles what an uninterrupted one would. A stale section stays in both
//! documents with its reason (law-a-control-that-cannot-check-must-refuse).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spira_desired_state::intent::{parse_intent, parse_key, Intent};
use spira_desired_state::section::{
    caught_up, freshness, parse_duration, parse_rfc3339, parse_section, section_key, Action, Distance, Flow,
    Freshness, Section, SectionMeta, Status, Target,
};

use crate::pass_record::{PassRecord, PublishLatency, SectionAge};

/// What `spira-lc section snapshot` answers: sections, intents and the pass record from one
/// list at `list_rev`.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub list_rev: u64,
    pub sections: Vec<Section>,
    pub intents: Vec<ListedIntent>,
    pub pass: Option<PassRecord>,
    /// Every key under `knobs/` and its value: a step's `from` is read here, at `list_rev`.
    pub knobs: BTreeMap<String, Value>,
    /// Keys listed at `list_rev` that do not decode: kept, so a bad record is seen, not lost.
    pub undecodable: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListedIntent {
    pub key: String,
    pub mod_rev: u64,
    pub intent: Intent,
}

pub trait SnapshotSource {
    fn snapshot(&self) -> Result<Snapshot, String>;
}

fn listed<'a>(v: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    v.get(field).and_then(Value::as_array).ok_or_else(|| format!("the snapshot lists no {field}"))
}

impl Snapshot {
    /// The verb's JSON answer. Every field is required, so a short answer is refused rather than
    /// read as an empty world or as no pass record, which would renumber the passes from 1 and
    /// forget which gaps were open. A record that does not decode is kept by key.
    pub fn parse(v: &Value) -> Result<Snapshot, String> {
        let list_rev = v.get("list_rev").and_then(Value::as_u64).ok_or("the snapshot names no list_rev")?;
        let mut undecodable = Vec::new();
        for k in listed(v, "undecodable")? {
            undecodable.push(k.as_str().ok_or("an undecodable entry is not a key")?.to_string());
        }
        let mut sections = Vec::new();
        for (i, entry) in listed(v, "sections")?.iter().enumerate() {
            let raw = entry.get("section").unwrap_or(&Value::Null);
            match parse_section(raw) {
                Ok(s) => sections.push(s),
                Err(_) => undecodable.push(match (raw.get("kind").and_then(Value::as_str), raw.pointer("/metadata/name").and_then(Value::as_str)) {
                    (Some(kind), Some(name)) => section_key(kind, name),
                    _ => format!("sections[{i}]"),
                }),
            }
        }
        let mut intents = Vec::new();
        for entry in listed(v, "intents")? {
            let key = entry.get("key").and_then(Value::as_str).ok_or("a listed intent names no key")?;
            let mod_rev = entry.get("mod_rev").and_then(Value::as_u64).ok_or("a listed intent names no mod_rev")?;
            match parse_intent(entry.get("intent").unwrap_or(&Value::Null)) {
                Ok(intent) => intents.push(ListedIntent { key: key.to_string(), mod_rev, intent }),
                Err(_) => undecodable.push(key.to_string()),
            }
        }
        let knobs = v
            .get("knobs")
            .and_then(Value::as_object)
            .ok_or("the snapshot lists no knobs")?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let pass = match v.get("pass").ok_or("the snapshot names no pass record, not even null")? {
            Value::Null => None,
            p => Some(serde_json::from_value(p.clone()).map_err(|e| format!("the pass record does not decode: {e}"))?),
        };
        Ok(Snapshot { list_rev, sections, intents, pass, knobs, undecodable })
    }
}

/// [`SnapshotSource`] over the `spira-lc section snapshot` verb.
pub struct Verb {
    /// The program and any leading arguments; `section snapshot` is appended.
    pub argv: Vec<String>,
}

impl Verb {
    pub fn new(program: impl Into<String>) -> Verb {
        Verb { argv: vec![program.into()] }
    }
}

impl SnapshotSource for Verb {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let (program, lead) = self.argv.split_first().ok_or("no program to ask for the snapshot")?;
        let out = spira_config::bounded::bounded(program)
            .args(lead)
            .args(["section", "snapshot"])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        answer(out.status.code(), &String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr))
    }
}

fn answer(code: Option<i32>, stdout: &str, stderr: &str) -> Result<Snapshot, String> {
    if code != Some(0) {
        return Err(format!("section snapshot exited {code:?}: {} {}", stdout.trim(), stderr.trim()));
    }
    let v: Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("section snapshot answered non-JSON: {e}"))?;
    Snapshot::parse(&v)
}

/// One document: every listed section at one revision, keyed `<Kind>/<name>`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Document<T> {
    pub list_rev: u64,
    pub sections: BTreeMap<String, T>,
}

/// A condition the assembler adds from what it can see, beside the owner's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum Derived {
    /// `n` intents at this section sit unacknowledged past their horizon: the owner's own gap,
    /// never a failed step.
    IntentsUnacknowledged { n: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Current {
    pub freshness: Freshness,
    pub metadata: SectionMeta,
    pub status: Status,
    pub derived: Vec<Derived>,
    pub flow: Flow,
    pub distance: Option<Distance>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Desired {
    pub freshness: Freshness,
    pub metadata: SectionMeta,
    pub spec: BTreeMap<String, Target>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Assembled {
    /// This pass's number: one past the previous record's, or 1 with none.
    pub n: u64,
    pub list_rev: u64,
    pub current: Document<Current>,
    pub desired: Document<Desired>,
    pub intents: Vec<ListedIntent>,
    pub prev: Option<PassRecord>,
    pub knobs: BTreeMap<String, Value>,
    pub ages: BTreeMap<String, SectionAge>,
    pub undecodable: Vec<String>,
    /// Unacknowledged intents past their horizon with no section in the snapshot to mark,
    /// keyed by the section their key names, or by the key itself when it names none.
    pub unattributed: BTreeMap<String, usize>,
}

pub fn section_ref(s: &Section) -> String {
    format!("{}/{}", s.kind, s.metadata.name)
}

pub fn assemble(snap: &Snapshot, now_ms: u64) -> Assembled {
    let mut unacked = unacknowledged(&snap.intents, now_ms / 1000);
    let mut current = BTreeMap::new();
    let mut desired = BTreeMap::new();
    let mut ages = BTreeMap::new();
    for s in &snap.sections {
        let r = section_ref(s);
        let fresh = freshness(s, now_ms);
        let derived = match unacked.remove(&r) {
            Some(n) => vec![Derived::IntentsUnacknowledged { n }],
            None => Vec::new(),
        };
        ages.insert(r.clone(), age(s, &fresh, now_ms / 1000));
        current.insert(
            r.clone(),
            Current {
                freshness: fresh.clone(),
                metadata: s.metadata.clone(),
                status: s.status.clone(),
                derived,
                flow: s.flow.clone(),
                distance: s.distance.clone(),
                actions: s.actions.clone(),
            },
        );
        desired.insert(r, Desired { freshness: fresh, metadata: s.metadata.clone(), spec: s.spec.clone() });
    }
    Assembled {
        n: PassRecord::next_n(snap.pass.as_ref()),
        list_rev: snap.list_rev,
        current: Document { list_rev: snap.list_rev, sections: current },
        desired: Document { list_rev: snap.list_rev, sections: desired },
        intents: snap.intents.clone(),
        prev: snap.pass.clone(),
        knobs: snap.knobs.clone(),
        ages,
        undecodable: snap.undecodable.clone(),
        unattributed: unacked,
    }
}

/// Per section, the intents with no ack whose `taken_at + horizon` is past. One whose times do
/// not read cannot be shown inside its horizon, so it counts.
fn unacknowledged(intents: &[ListedIntent], now: u64) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for li in intents.iter().filter(|li| li.intent.ack.is_none()) {
        let target = parse_key(&li.key).map_or_else(|| li.key.clone(), |(kind, name, _)| format!("{kind}/{name}"));
        let due = parse_rfc3339(&li.intent.taken_at)
            .zip(parse_duration(&li.intent.horizon))
            .is_none_or(|(taken, horizon)| taken + horizon < now);
        if due {
            *out.entry(target).or_insert(0) += 1;
        }
    }
    out
}

fn age(s: &Section, fresh: &Freshness, now: u64) -> SectionAge {
    let age_secs = s.metadata.observed_at.as_deref().and_then(parse_rfc3339).map(|at| now.saturating_sub(at));
    let target = s.metadata.target_latency.as_deref().and_then(parse_duration);
    let publish_latency = match (age_secs, target) {
        (None, _) => PublishLatency::Unmeasured,
        (Some(_), None) => PublishLatency::Undeclared,
        (Some(a), Some(t)) if a <= t => PublishLatency::Within { target_secs: t },
        (Some(a), Some(t)) => PublishLatency::Late { target_secs: t, over_secs: a - t },
    };
    SectionAge { age_secs, freshness: fresh.clone(), caught_up: caught_up(s), publish_latency }
}

#[cfg(test)]
mod tests;
