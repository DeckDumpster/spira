//! The D-Bus listener: systemd's own PropertiesChanged signals on unit objects, folded into the
//! same `Record` the journal fold produces. A unit reaching `inactive`/`failed` is read back with
//! GetAll, so the record carries the manager's final Result, exit status and timestamps rather than
//! whatever the signal happened to include.

use crate::dbus::{Conn, Msg, Val, SIGNAL};
use crate::Record;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

pub const MANAGER: &str = "org.freedesktop.systemd1";
const UNIT_PATH: &str = "/org/freedesktop/systemd1/unit/";
const UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";
const SERVICE_IFACE: &str = "org.freedesktop.systemd1.Service";
const SCOPE_IFACE: &str = "org.freedesktop.systemd1.Scope";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

pub const MATCH_RULES: [&str; 2] = [
    "type='signal',sender='org.freedesktop.systemd1',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='/org/freedesktop/systemd1/unit'",
    "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.systemd1'",
];

/// Executions remembered as already recorded, so a daemon-reload re-announcing an idle unit
/// cannot record it again.
const DONE_CAP: usize = 8192;

/// `x_2eservice` -> `x.service`: the bus object-path escaping of a unit name.
pub fn unit_name(path: &str) -> Option<String> {
    let esc = path.strip_prefix(UNIT_PATH)?;
    let b = esc.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'_' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The unit name a metric label carries: transient `run-r<hex>` names collapse to one series.
pub fn metric_unit(unit: &str) -> String {
    let id = unit.strip_prefix("run-").and_then(|r| r.split('.').next()).unwrap_or("");
    let hex = id.strip_prefix(['r', 'u', 'p']).unwrap_or(id);
    if hex.len() >= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return format!("run-*.{}", unit.rsplit('.').next().unwrap_or("service"));
    }
    unit.to_string()
}

#[derive(Debug, Clone, Default)]
struct Props {
    live: bool,
    active_state: Option<String>,
    invocation: Option<String>,
    started_us: Option<u64>,
    ended_us: Option<u64>,
    result: Option<String>,
    exit_code: Option<i64>,
    exit_status: Option<i64>,
    cpu_usec: Option<u64>,
    memory_peak: Option<u64>,
    io_read: Option<u64>,
    io_write: Option<u64>,
}

fn nonzero(v: &Val) -> Option<u64> {
    v.as_u64().filter(|u| *u != 0 && *u != u64::MAX)
}

impl Props {
    fn merge(&mut self, iface: &str, changed: &[(String, Val)]) {
        for (k, v) in changed {
            match (iface, k.as_str()) {
                (UNIT_IFACE, "ActiveState") => self.active_state = v.as_str().map(String::from),
                (UNIT_IFACE, "InvocationID") => {
                    if let Some(b) = v.as_bytes().filter(|b| b.len() == 16 && b.iter().any(|x| *x != 0)) {
                        self.invocation = Some(b.iter().map(|x| format!("{x:02x}")).collect());
                    }
                }
                (UNIT_IFACE, "InactiveExitTimestamp") => self.started_us = nonzero(v).or(self.started_us),
                (UNIT_IFACE, "InactiveEnterTimestamp") => self.ended_us = nonzero(v).or(self.ended_us),
                (SERVICE_IFACE | SCOPE_IFACE, "Result") => self.result = v.as_str().map(String::from),
                (SERVICE_IFACE, "ExecMainCode") => self.exit_code = v.as_i64(),
                (SERVICE_IFACE, "ExecMainStatus") => self.exit_status = v.as_i64(),
                (SERVICE_IFACE, "CPUUsageNSec") => self.cpu_usec = nonzero(v).map(|n| n / 1000),
                (SERVICE_IFACE, "MemoryPeak") => self.memory_peak = nonzero(v),
                (SERVICE_IFACE, "IOReadBytes") => self.io_read = nonzero(v),
                (SERVICE_IFACE, "IOWriteBytes") => self.io_write = nonzero(v),
                _ => {}
            }
        }
    }
}

fn exit_code_name(code: i64) -> Option<&'static str> {
    match code {
        1 => Some("exited"),
        2 => Some("killed"),
        3 => Some("dumped"),
        _ => None,
    }
}

pub enum Next {
    Nothing,
    /// A live state: read the unit's properties now, in case it is gone by the time it ends.
    Snapshot(String),
    /// A terminal state: read the final properties, then call `finish`.
    Close(String),
}

pub struct Tracker {
    since_us: u64,
    open: BTreeMap<String, Props>,
    done: BTreeSet<String>,
    done_order: VecDeque<String>,
}

fn is_run(unit: &str) -> bool {
    unit.ends_with(".service") || unit.ends_with(".scope")
}

impl Tracker {
    /// `since_us`: executions that ended before this are history, not events.
    pub fn new(since_us: u64) -> Tracker {
        Tracker { since_us, open: BTreeMap::new(), done: BTreeSet::new(), done_order: VecDeque::new() }
    }

    fn invocation(&self, path: &str) -> Option<String> {
        self.open.get(path)?.invocation.clone()
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Fold one interface's properties for the unit at `path`. `signal` marks a
    /// PropertiesChanged (which may start or end a run) as against a GetAll read-back.
    pub fn apply(&mut self, path: &str, iface: &str, changed: &[(String, Val)], signal: bool) -> Next {
        if !unit_name(path).is_some_and(|u| is_run(&u)) {
            return Next::Nothing;
        }
        let p = self.open.entry(path.to_string()).or_default();
        p.merge(iface, changed);
        if !signal {
            return Next::Nothing;
        }
        match changed.iter().find(|(k, _)| k == "ActiveState").and_then(|(_, v)| v.as_str()) {
            Some("inactive" | "failed") if p.live => Next::Close(path.to_string()),
            Some("inactive" | "failed") | None => Next::Nothing,
            Some(_) => {
                p.live = true;
                Next::Snapshot(path.to_string())
            }
        }
    }

    /// The record for the run that just ended at `path`, if it is one not already recorded.
    pub fn finish(&mut self, path: &str, now_us: u64) -> Option<Record> {
        let unit = unit_name(path)?;
        let p = self.open.remove(path)?;
        let invocation = p.invocation?;
        if self.done.contains(&invocation) {
            return None;
        }
        let ended_us = p.ended_us.unwrap_or(now_us);
        if ended_us < self.since_us || p.started_us.is_none() {
            return None;
        }
        self.done.insert(invocation.clone());
        self.done_order.push_back(invocation.clone());
        while self.done_order.len() > DONE_CAP {
            if let Some(old) = self.done_order.pop_front() {
                self.done.remove(&old);
            }
        }
        let failed = p.active_state.as_deref() == Some("failed");
        Some(Record {
            invocation_id: invocation,
            unit,
            started_us: p.started_us,
            ended_us,
            result: p.result.unwrap_or_else(|| if failed { "failed".into() } else { "success".into() }),
            exit_code: p.exit_code.and_then(exit_code_name).map(String::from),
            exit_status: p.exit_status.filter(|_| p.exit_code.is_some_and(|c| c > 0)),
            cpu_usec: p.cpu_usec,
            memory_peak: p.memory_peak,
            io_read_bytes: p.io_read,
            io_write_bytes: p.io_write,
        })
    }
}

fn properties_changed(m: &Msg) -> Option<(&str, String, Vec<(String, Val)>)> {
    if m.kind != SIGNAL || m.member.as_deref() != Some("PropertiesChanged") {
        return None;
    }
    let path = m.path.as_deref().filter(|p| p.starts_with(UNIT_PATH))?;
    Some((path, m.body.first()?.as_str()?.to_string(), m.body.get(1)?.dict()))
}

/// Read the unit's properties back. A unit already collected answers GetAll with a blank stub
/// (no invocation id), which would overwrite what the signals carried, so a read whose invocation
/// is blank or not the tracked one is dropped.
fn read_back(conn: &mut Conn, t: &mut Tracker, path: &str) {
    let get = |conn: &mut Conn, iface: &str| conn.call(MANAGER, path, PROPS_IFACE, "GetAll", Some(iface)).ok().and_then(|b| b.first().map(Val::dict));
    let Some(unit) = get(conn, UNIT_IFACE) else { return };
    let mut probe = Props::default();
    probe.merge(UNIT_IFACE, &unit);
    if probe.invocation.is_none() || t.invocation(path).is_some_and(|known| Some(&known) != probe.invocation.as_ref()) {
        return;
    }
    t.apply(path, UNIT_IFACE, &unit, false);
    for iface in [SERVICE_IFACE, SCOPE_IFACE] {
        if let Some(d) = get(conn, iface) {
            t.apply(path, iface, &d, false);
        }
    }
}

pub fn subscribe(conn: &mut Conn) -> Result<(), String> {
    conn.call(MANAGER, "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", "Subscribe", None)?;
    Ok(())
}

pub fn arm(conn: &mut Conn) -> Result<(), String> {
    for rule in MATCH_RULES {
        conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", Some(rule))?;
    }
    subscribe(conn)
}

/// Wait up to `timeout` for the next signal and fold it; the records it completes are returned.
pub fn step(conn: &mut Conn, t: &mut Tracker, now_us: impl Fn() -> u64, timeout: Duration) -> Result<Vec<Record>, String> {
    let Some(m) = conn.next_signal(timeout)? else { return Ok(Vec::new()) };
    if m.member.as_deref() == Some("NameOwnerChanged") {
        if m.body.get(2).and_then(Val::as_str).is_some_and(|new| !new.is_empty()) {
            subscribe(conn)?;
        }
        return Ok(Vec::new());
    }
    let Some((path, iface, changed)) = properties_changed(&m) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    match t.apply(path, &iface, &changed, true) {
        Next::Nothing => {}
        Next::Snapshot(p) => read_back(conn, t, &p),
        Next::Close(p) => {
            read_back(conn, t, &p);
            out.extend(t.finish(&p, now_us()));
        }
    }
    Ok(out)
}

/// How two family files agree about the runs they share.
#[derive(Debug, Default, PartialEq)]
pub struct Comparison {
    pub agree: usize,
    pub disagree: Vec<String>,
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
}

fn rows(text: &str) -> BTreeMap<String, serde_json::Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| Some((v.get("invocation_id")?.as_str()?.to_string(), v)))
        .collect()
}

/// Join two family files on invocation id over the `.service` runs that ended in `[from, to]`
/// (ISO-8601 stamps compare as text). Agreement is the same unit, result and exit status.
pub fn compare(a: &str, b: &str, from: &str, to: &str) -> Comparison {
    let in_window = |v: &serde_json::Value| {
        let end = v.get("ended_at").and_then(|e| e.as_str()).unwrap_or("");
        v.get("unit").and_then(|u| u.as_str()).is_some_and(|u| u.ends_with(".service")) && end >= from && end <= to
    };
    let (ra, rb) = (rows(a), rows(b));
    let mut c = Comparison::default();
    for (id, va) in ra.iter().filter(|(_, v)| in_window(v)) {
        match rb.get(id) {
            None => c.only_a.push(format!("{id} {}", va["unit"].as_str().unwrap_or(""))),
            Some(vb) => {
                let key = |v: &serde_json::Value| (v["unit"].clone(), v["result"].clone(), v["exit_status"].clone());
                if key(va) == key(vb) {
                    c.agree += 1;
                } else {
                    c.disagree.push(format!("{id} {} journal={:?} dbus={:?}", va["unit"].as_str().unwrap_or(""), key(va), key(vb)));
                }
            }
        }
    }
    for (id, vb) in rb.iter().filter(|(_, v)| in_window(v)) {
        if !ra.contains_key(id) {
            c.only_b.push(format!("{id} {}", vb["unit"].as_str().unwrap_or("")));
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Val {
        Val::Var(Box::new(Val::S(v.into())))
    }
    fn u(v: u64) -> Val {
        Val::Var(Box::new(Val::U(v)))
    }
    fn inv(b: u8) -> Val {
        Val::Var(Box::new(Val::Arr(vec![Val::U(b as u64); 16])))
    }
    const P: &str = "/org/freedesktop/systemd1/unit/job_2dx_2eservice";

    #[test]
    fn a_unit_object_path_decodes_to_its_unit_name() {
        assert_eq!(unit_name(P).as_deref(), Some("job-x.service"));
        assert_eq!(unit_name("/org/freedesktop/systemd1/unit/bad_zz"), None);
        assert_eq!(unit_name("/elsewhere"), None);
    }

    #[test]
    fn transient_run_units_share_one_metric_label() {
        assert_eq!(metric_unit("run-r1a2b3c4d5e6f7081.service"), "run-*.service");
        assert_eq!(metric_unit("spira-loom.service"), "spira-loom.service");
        assert_eq!(metric_unit("run-of-the-mill.service"), "run-of-the-mill.service");
    }

    fn close(t: &mut Tracker, state: &str, extra: Vec<(String, Val)>, now: u64) -> Option<Record> {
        t.apply(P, UNIT_IFACE, &[("ActiveState".into(), s("activating"))], true);
        t.apply(P, UNIT_IFACE, &[("InvocationID".into(), inv(7)), ("InactiveExitTimestamp".into(), u(1_000_000))], false);
        let next = t.apply(P, UNIT_IFACE, &[("ActiveState".into(), s(state))], true);
        assert!(matches!(next, Next::Close(_)));
        t.apply(P, UNIT_IFACE, &[("InactiveEnterTimestamp".into(), u(4_000_000))], false);
        t.apply(P, SERVICE_IFACE, &extra, false);
        t.finish(P, now)
    }

    #[test]
    fn a_successful_run_is_one_record_with_success_and_its_duration() {
        let mut t = Tracker::new(0);
        let r = close(&mut t, "inactive", vec![("Result".into(), s("success")), ("ExecMainCode".into(), Val::I(1)), ("ExecMainStatus".into(), Val::I(0))], 9).unwrap();
        assert_eq!((r.unit.as_str(), r.result.as_str(), r.exit_code.as_deref(), r.exit_status), ("job-x.service", "success", Some("exited"), Some(0)));
        assert_eq!((r.started_us, r.ended_us, r.invocation_id.as_str()), (Some(1_000_000), 4_000_000, "07070707070707070707070707070707"));
        assert_eq!(t.open_count(), 0);
    }

    #[test]
    fn a_nonzero_exit_and_a_timeout_carry_their_result_and_status() {
        let mut t = Tracker::new(0);
        let r = close(&mut t, "failed", vec![("Result".into(), s("exit-code")), ("ExecMainCode".into(), Val::I(1)), ("ExecMainStatus".into(), Val::I(3))], 9).unwrap();
        assert_eq!((r.result.as_str(), r.exit_status), ("exit-code", Some(3)));
        let mut t = Tracker::new(0);
        let r = close(&mut t, "failed", vec![("Result".into(), s("timeout")), ("ExecMainCode".into(), Val::I(2)), ("ExecMainStatus".into(), Val::I(15))], 9).unwrap();
        assert_eq!((r.result.as_str(), r.exit_code.as_deref(), r.exit_status), ("timeout", Some("killed"), Some(15)));
    }

    #[test]
    fn a_run_is_recorded_once_and_history_is_not_an_event() {
        let mut t = Tracker::new(0);
        assert!(close(&mut t, "inactive", vec![], 9).is_some());
        assert!(close(&mut t, "inactive", vec![], 9).is_none());
        let mut late = Tracker::new(5_000_000);
        assert!(close(&mut late, "inactive", vec![], 9).is_none());
    }

    #[test]
    fn non_run_units_and_idle_signals_open_nothing() {
        let mut t = Tracker::new(0);
        assert!(matches!(t.apply("/org/freedesktop/systemd1/unit/x_2etimer", UNIT_IFACE, &[("ActiveState".into(), s("inactive"))], true), Next::Nothing));
        assert!(matches!(t.apply(P, UNIT_IFACE, &[("Description".into(), s("d"))], true), Next::Nothing));
        assert!(t.finish(P, 1).is_none());
        assert!(matches!(t.apply(P, UNIT_IFACE, &[("ActiveState".into(), s("inactive"))], true), Next::Nothing), "an inactive state seen before any live one is not a run's end");
    }

    #[test]
    fn comparison_names_every_disagreement_and_every_unmatched_run() {
        let row = |id: &str, res: &str| format!("{{\"invocation_id\":\"{id}\",\"unit\":\"a.service\",\"result\":\"{res}\",\"ended_at\":\"2026-10-17T10:00:00Z\"}}\n");
        let a = format!("{}{}{}", row("1", "success"), row("2", "success"), row("3", "success"));
        let b = format!("{}{}{}", row("1", "success"), row("2", "exit-code"), row("4", "success"));
        let c = compare(&a, &b, "2026-10-17T00:00:00Z", "2026-10-18T00:00:00Z");
        assert_eq!((c.agree, c.disagree.len(), c.only_a.len(), c.only_b.len()), (1, 1, 1, 1));
        let out = compare(&a, &b, "2026-10-18T00:00:00Z", "2026-10-19T00:00:00Z");
        assert_eq!(out, Comparison::default());
    }
}
