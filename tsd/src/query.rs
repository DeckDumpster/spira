// query — the observational queries over run/tsd/ families: a moved family is read from the
// metrics store (and refuses by name when it cannot be), a kept family from its file. The
// computations are pure over rows, so a test feeds the same fixture rows to either source.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::store::Reader;

/// Where a family's rows come from: the metrics store for a moved family, else its file.
pub struct Source<'a> {
    pub reader: Option<&'a Reader>,
    pub root: &'a Path,
}

pub const ERR_NO_FAMILY: i32 = 3;

pub struct Failure {
    pub code: i32,
    pub message: String,
}

fn fail(code: i32, message: String) -> Failure {
    Failure { code, message }
}

impl Source<'_> {
    /// Rows stamped at or after `since` (unix seconds). A moved family with none in the store
    /// is a refusal; a kept family whose file does not exist is exit 3, as the shell reader gave.
    pub fn rows(&self, family: &str, since: u64) -> Result<Vec<Value>, Failure> {
        if crate::store::moved(family) {
            let reader = self.reader.ok_or_else(|| fail(1, spira_otel::query::QueryError::NotConfigured.to_string()))?;
            return reader.rows(family, since).map_err(|e| fail(1, format!("tsd-read: {e}")));
        }
        let path = crate::family_path(self.root, family);
        let text = std::fs::read_to_string(&path).map_err(|_| fail(ERR_NO_FAMILY, format!("tsd-read: no rows yet for family {family:?} ({} does not exist)", path.display())))?;
        Ok(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(Value::is_object).filter(|r| ts_of(r).is_some_and(|t| t >= since)).collect())
    }
}

pub fn ts_of(row: &Value) -> Option<u64> {
    row.get("ts").and_then(Value::as_str).and_then(crate::parse_ts)
}

pub fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// A number, or a string that reads as one; anything else is absent, as a failed cast is.
pub fn num(row: &Value, key: &str) -> Option<f64> {
    match row.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn flag(row: &Value, key: &str) -> bool {
    match row.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

/// Linear-interpolated quantile, the definition `quantile_cont` uses.
pub fn quantile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let pos = (v.len() - 1) as f64 * p;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64))
}

/// A kept family whose file does not exist yet is absent, not a failure; anything else is.
fn optional(r: Result<Vec<Value>, Failure>) -> Result<Option<Vec<Value>>, Failure> {
    match r {
        Ok(rows) => Ok(Some(rows)),
        Err(f) if f.code == ERR_NO_FAMILY => Ok(None),
        Err(f) => Err(f),
    }
}

fn int(x: f64) -> i64 {
    x.round() as i64
}

fn since(now: u64, hours: u64) -> u64 {
    now.saturating_sub(hours.saturating_mul(3600))
}

const STAGE: &str = "bead-stage";
const LIVE_STATES: [&str; 6] = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

fn applied_beads(rows: &[Value]) -> impl Iterator<Item = &Value> {
    rows.iter().filter(|r| text(r, "machine") == Some("bead") && flag(r, "applied"))
}

/// WIP and dwell per bead stage, now: each bead counts once, in the state its latest applied row put it in.
pub fn wip(src: &Source, now: u64, hours: u64) -> Result<Value, Failure> {
    let rows = src.rows(STAGE, since(now, hours))?;
    let mut latest: BTreeMap<&str, (f64, &Value)> = BTreeMap::new();
    for r in applied_beads(&rows) {
        let (Some(key), Some(seq)) = (text(r, "key"), num(r, "seq")) else { continue };
        if latest.get(key).is_none_or(|(s, _)| seq > *s) {
            latest.insert(key, (seq, r));
        }
    }
    let mut by_state: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (_, r) in latest.values() {
        let (Some(state), Some(t)) = (text(r, "to_state"), ts_of(r)) else { continue };
        if LIVE_STATES.contains(&state) {
            by_state.entry(state).or_default().push(now as f64 - t as f64);
        }
    }
    Ok(Value::Array(
        LIVE_STATES
            .iter()
            .filter_map(|s| {
                let d = by_state.get(s)?;
                Some(json!({"state": s, "wip": d.len(), "dwell_p50_s": int(quantile(d, 0.5)?), "dwell_max_s": int(d.iter().cloned().fold(f64::MIN, f64::max))}))
            })
            .collect(),
    ))
}

/// Reopens (to REWORK) against beads landed, overall and by reason.
pub fn rework(src: &Source, now: u64, hours: u64) -> Result<Value, Failure> {
    let rows = src.rows(STAGE, since(now, hours))?;
    let win: Vec<&Value> = applied_beads(&rows).collect();
    let landed = win.iter().filter(|r| text(r, "to_state") == Some("LANDED")).filter_map(|r| text(r, "key")).collect::<BTreeSet<_>>().len();
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for r in win.iter().filter(|r| text(r, "to_state") == Some("REWORK")) {
        *reasons.entry(text(r, "reason").unwrap_or("unknown")).or_default() += 1;
    }
    let total: usize = reasons.values().sum();
    let mut by: Vec<(&str, usize)> = reasons.into_iter().collect();
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut out = vec![json!({"reason": "*", "reopens": total, "landed": landed, "ord": 0})];
    out.extend(by.into_iter().map(|(reason, n)| json!({"reason": reason, "reopens": n, "landed": landed, "ord": 1})));
    Ok(Value::Array(out))
}

/// Pass wall time (p50, p90) and the three phases that took longest.
pub fn sentinel(src: &Source, now: u64, hours: u64) -> Result<Value, Failure> {
    let rows = src.rows("sentinel-phase", since(now, hours))?;
    let mut per_pass: BTreeMap<&str, f64> = BTreeMap::new();
    let mut per_phase: BTreeMap<&str, f64> = BTreeMap::new();
    for r in &rows {
        let (Some(pass), Some(check)) = (text(r, "pass"), text(r, "check")) else { continue };
        let secs = num(r, "secs").unwrap_or(0.0);
        *per_pass.entry(pass).or_default() += secs;
        *per_phase.entry(check).or_default() += secs;
    }
    let totals: Vec<f64> = per_pass.values().copied().collect();
    let mut top: Vec<(&str, f64)> = per_phase.into_iter().collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal).then(a.0.cmp(b.0)));
    let top: Vec<String> = top.iter().take(3).map(|(p, s)| format!("{p}:{}", int(*s))).collect();
    Ok(json!([{"p50": quantile(&totals, 0.5), "p90": quantile(&totals, 0.9), "passes": totals.len(), "top_phases": if top.is_empty() { Value::Null } else { json!(top) }}]))
}

/// One bead's timeline, oldest first: stage moves, aeon sessions and gate runs.
pub fn bead(src: &Source, id: &str) -> Result<Value, Failure> {
    let stage = src.rows(STAGE, 0)?;
    let mut moves: Vec<&Value> = applied_beads(&stage).filter(|r| text(r, "key") == Some(id)).collect();
    moves.sort_by(|a, b| num(a, "seq").partial_cmp(&num(b, "seq")).unwrap_or(Ordering::Equal));
    let mut out: Vec<(String, &str, String, Value)> = Vec::new();
    let mut prev: Option<u64> = None;
    for r in moves {
        let t = ts_of(r);
        let what = format!("{} -> {}{}", text(r, "from_state").unwrap_or(""), text(r, "to_state").unwrap_or(""), text(r, "reason").map(|x| format!(" ({x})")).unwrap_or_default());
        let secs = match (t, prev) {
            (Some(t), Some(p)) => json!(t as i64 - p as i64),
            _ => Value::Null,
        };
        prev = t;
        out.push((text(r, "ts").unwrap_or("").to_string(), "stage", what, secs));
    }
    for r in src.rows("aeon-session", 0)?.iter().filter(|r| text(r, "bead") == Some(id)) {
        out.push((text(r, "ts").unwrap_or("").to_string(), "aeon", format!("{} {}", text(r, "rune").unwrap_or(""), text(r, "status").unwrap_or("")), num(r, "wall_s").map(|w| json!(int(w))).unwrap_or(Value::Null)));
    }
    if let Some(gates) = optional(src.rows("gate-run", 0))? {
        for r in gates.iter().filter(|r| text(r, "bead") == Some(id)) {
            out.push((text(r, "ts").unwrap_or("").to_string(), "gate", format!("{} {}", text(r, "status").unwrap_or(""), text(r, "reason").unwrap_or("")), num(r, "ran_secs").map(|w| json!(int(w))).unwrap_or(Value::Null)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    Ok(Value::Array(out.into_iter().map(|(ts, kind, what, secs)| json!({"ts": ts, "kind": kind, "what": what, "secs": secs})).collect()))
}

/// Seconds spent in aeon sessions, gate runs and batch rounds, and in empty slots.
pub fn time(src: &Source, now: u64, hours: u64) -> Result<Value, Failure> {
    let from = since(now, hours);
    let aeon = src.rows("aeon-session", from)?;
    let gate = src.rows("gate-run", from)?;
    let slots = src.rows("slots", 0)?;
    let rounds = optional(src.rows("batch-round", from))?.unwrap_or_default();
    let sum = |rows: &[Value], key: &str| rows.iter().filter_map(|r| num(r, key)).sum::<f64>();
    let mut samples: Vec<(u64, &Value)> = slots.iter().filter_map(|r| Some((ts_of(r)?, r))).collect();
    samples.sort_by_key(|s| s.0);
    let idle: f64 = samples
        .windows(2)
        .filter(|w| w[0].0 >= from)
        .map(|w| {
            let dt = (w[1].0 - w[0].0).min(300) as f64;
            let empty = match (num(w[0].1, "ceiling"), num(w[0].1, "live")) {
                (Some(c), Some(l)) => (c.round() - l.round()).max(0.0),
                _ => 0.0,
            };
            empty * dt
        })
        .sum();
    Ok(json!([{"aeon_s": int(sum(&aeon, "wall_s")), "gate_s": int(sum(&gate, "ran_secs")), "round_s": int(sum(&rounds, "duration_ms") / 1000.0), "idle_slot_s": int(idle)}]))
}

/// avg(field) over the window, and how many rows it covers.
pub fn baseline(src: &Source, family: &str, field: &str, now: u64, hours: u64) -> Result<Value, Failure> {
    let rows = src.rows(family, since(now, hours))?;
    let v: Vec<f64> = rows.iter().filter_map(|r| num(r, field)).collect();
    Ok(json!([{"baseline": (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64), "n": rows.len()}]))
}

pub fn rate(src: &Source, family: &str, now: u64, hours: u64) -> Result<Value, Failure> {
    let n = src.rows(family, since(now, hours))?.len();
    Ok(json!([{"n": n, "rate_per_hour": n as f64 / hours.max(1) as f64}]))
}

pub fn dwell(src: &Source, family: &str, field: &str, p: f64, now: u64, hours: Option<u64>) -> Result<Value, Failure> {
    let rows = src.rows(family, hours.map_or(0, |h| since(now, h)))?;
    let v: Vec<f64> = rows.iter().filter_map(|r| num(r, field)).collect();
    Ok(json!([{format!("p{p}"): quantile(&v, p), "n": rows.len()}]))
}

/// Tab-separated `group<TAB>avg(field)` lines, largest first.
pub fn by_group(src: &Source, family: &str, group: &str, field: &str, now: u64, hours: Option<u64>) -> Result<String, Failure> {
    let rows = src.rows(family, hours.map_or(0, |h| since(now, h)))?;
    let mut acc: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for r in &rows {
        let Some(g) = r.get(group).map(|g| g.as_str().map(str::to_string).unwrap_or_else(|| g.to_string())) else { continue };
        if let Some(v) = num(r, field) {
            let e = acc.entry(g).or_default();
            e.0 += v;
            e.1 += 1;
        }
    }
    let mut out: Vec<(String, f64)> = acc.into_iter().map(|(g, (s, n))| (g, s / n as f64)).collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    Ok(out.iter().map(|(g, a)| format!("{g}\t{a}\n")).collect())
}

/// Tab-separated `group<TAB>count` lines, most frequent first.
pub fn count_by(src: &Source, family: &str, group: &str, now: u64, hours: Option<u64>) -> Result<String, Failure> {
    let rows = src.rows(family, hours.map_or(0, |h| since(now, h)))?;
    let mut acc: BTreeMap<String, usize> = BTreeMap::new();
    for r in &rows {
        let Some(g) = r.get(group).map(|g| g.as_str().map(str::to_string).unwrap_or_else(|| g.to_string())) else { continue };
        *acc.entry(g).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = acc.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(out.iter().map(|(g, n)| format!("{g}\t{n}\n")).collect())
}
