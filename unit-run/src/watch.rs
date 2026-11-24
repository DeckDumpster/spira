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

    /// A run found already going when the listener started: its later terminal signal records it.
    pub fn adopt(&mut self, path: &str) {
        if let Some(p) = self.open.get_mut(path) {
            p.live = true;
        }
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

/// Whether the release root a process runs from is not the one `current` now
/// names. A process pinned to the `current` link follows it, and an unreadable link is no reason to stop.
pub fn superseded(started_under: Option<&str>, current: Option<&str>) -> bool {
    let base = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string();
    match (started_under.map(base), current) {
        (Some(s), Some(c)) => s != "current" && s != base(c),
        _ => false,
    }
}

/// A listener exits to be restarted when the store holds every row it has and it is on a
/// superseded release. Runs still going are adopted by the next listener's `catch_up`.
pub fn restart_now(backlog_flushed: bool, started_under: Option<&str>, current: Option<&str>) -> bool {
    backlog_flushed && superseded(started_under, current)
}

#[allow(clippy::type_complexity)]
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

/// The latest `ended_at` among stored rows, in microseconds: where a restarted listener
/// resumes from.
pub fn last_ended_us(rows: &[serde_json::Value]) -> Option<u64> {
    rows.iter()
        .filter_map(|v| tsd::parse_ts(v.get("ended_at")?.as_str()?))
        .max()
        .map(|s| s.saturating_mul(1_000_000))
}

/// Records for the runs that ended since the tracker's `since_us` while nobody was listening,
/// read from the manager's loaded units; runs still going are marked live, so their end is
/// recorded when it signals. Call after `arm`, so a run ending during the catch-up is
/// a signal rather than a gap; the caller's dedupe on invocation id absorbs any overlap.
pub fn catch_up(conn: &mut Conn, t: &mut Tracker, now_us: impl Fn() -> u64) -> Result<Vec<Record>, String> {
    let listed = conn.call(MANAGER, "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", "ListUnits", None)?;
    let Some(Val::Arr(units)) = listed.first() else { return Err("ListUnits: unexpected reply".into()) };
    let mut out = Vec::new();
    for u in units {
        let Val::Struct(f) = u else { continue };
        let (Some(name), Some(path)) = (f.first().and_then(Val::as_str), f.get(6).and_then(Val::as_str)) else { continue };
        if !is_run(name) {
            continue;
        }
        let ended = matches!(f.get(3).and_then(Val::as_str), Some("inactive" | "failed"));
        read_back(conn, t, path);
        if ended {
            out.extend(t.finish(path, now_us()));
        } else {
            t.adopt(path);
        }
    }
    Ok(out)
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

    #[test_id(t9ultmr)]
    #[test]
    fn a_unit_object_path_decodes_to_its_unit_name() {
        assert_eq!(unit_name(P).as_deref(), Some("job-x.service"));
        assert_eq!(unit_name("/org/freedesktop/systemd1/unit/bad_zz"), None);
        assert_eq!(unit_name("elsewhere"), None);
    }

    #[test_id(tysfxqj)]
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

    #[test_id(t36c4nn)]
    #[test]
    fn a_successful_run_is_one_record_with_success_and_its_duration() {
        let mut t = Tracker::new(0);
        let r = close(&mut t, "inactive", vec![("Result".into(), s("success")), ("ExecMainCode".into(), Val::I(1)), ("ExecMainStatus".into(), Val::I(0))], 9).unwrap();
        assert_eq!((r.unit.as_str(), r.result.as_str(), r.exit_code.as_deref(), r.exit_status), ("job-x.service", "success", Some("exited"), Some(0)));
        assert_eq!((r.started_us, r.ended_us, r.invocation_id.as_str()), (Some(1_000_000), 4_000_000, "07070707070707070707070707070707"));
        assert_eq!(t.open_count(), 0);
    }

    #[test_id(t0fr5ug)]
    #[test]
    fn a_nonzero_exit_and_a_timeout_carry_their_result_and_status() {
        let mut t = Tracker::new(0);
        let r = close(&mut t, "failed", vec![("Result".into(), s("exit-code")), ("ExecMainCode".into(), Val::I(1)), ("ExecMainStatus".into(), Val::I(3))], 9).unwrap();
        assert_eq!((r.result.as_str(), r.exit_status), ("exit-code", Some(3)));
        let mut t = Tracker::new(0);
        let r = close(&mut t, "failed", vec![("Result".into(), s("timeout")), ("ExecMainCode".into(), Val::I(2)), ("ExecMainStatus".into(), Val::I(15))], 9).unwrap();
        assert_eq!((r.result.as_str(), r.exit_code.as_deref(), r.exit_status), ("timeout", Some("killed"), Some(15)));
    }

    #[test_id(twev453)]
    #[test]
    fn a_run_is_recorded_once_and_history_is_not_an_event() {
        let mut t = Tracker::new(0);
        assert!(close(&mut t, "inactive", vec![], 9).is_some());
        assert!(close(&mut t, "inactive", vec![], 9).is_none());
        let mut late = Tracker::new(5_000_000);
        assert!(close(&mut late, "inactive", vec![], 9).is_none());
    }

    #[test_id(tt5t0gp)]
    #[test]
    fn non_run_units_and_idle_signals_open_nothing() {
        let mut t = Tracker::new(0);
        assert!(matches!(t.apply("/org/freedesktop/systemd1/unit/x_2etimer", UNIT_IFACE, &[("ActiveState".into(), s("inactive"))], true), Next::Nothing));
        assert!(matches!(t.apply(P, UNIT_IFACE, &[("Description".into(), s("d"))], true), Next::Nothing));
        assert!(t.finish(P, 1).is_none());
        assert!(matches!(t.apply(P, UNIT_IFACE, &[("ActiveState".into(), s("inactive"))], true), Next::Nothing), "an inactive state seen before any live one is not a run's end");
    }

    #[test_id(tdjnk4k)]
    #[test]
    fn the_resume_point_is_the_latest_recorded_end() {
        let rows = [serde_json::json!({"ended_at": "2026-10-17T10:00:05Z"}), serde_json::json!({"ended_at": "2026-10-17T10:00:01Z"}), serde_json::json!({"unit": "x"})];
        assert_eq!(last_ended_us(&rows), tsd::parse_ts("2026-10-17T10:00:05Z").map(|s| s * 1_000_000));
        assert_eq!(last_ended_us(&[]), None);
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    #[test_id(tox2jf7)]
    #[test]
    fn a_listener_is_superseded_only_when_current_names_a_different_release() {
        assert!(superseded(Some("r/spira-releases/aaa"), Some("bbb")));
        assert!(!superseded(Some("r/spira-releases/bbb/"), Some("bbb")));
        assert!(!superseded(Some("r/spira-releases/current"), Some("bbb")), "a unit pinned to the link follows it");
        assert!(!superseded(Some("r/spira-releases/aaa"), None), "an unreadable link is not a reason to stop");
        assert!(!superseded(None, Some("bbb")));
    }

    #[test_id(tlyabkq)]
    #[test]
    fn a_superseded_listener_restarts_once_its_rows_are_stored_whatever_is_running() {
        let (old, cur) = (Some("r/aaa"), Some("bbb"));
        assert!(restart_now(true, old, cur));
        assert!(!restart_now(false, old, cur), "rows the store has not taken would be lost");
        assert!(!restart_now(true, Some("r/bbb"), cur));
    }

    #[test_id(taehauf)]
    #[test]
    fn a_run_found_going_is_adopted_so_its_end_is_recorded() {
        let mut t = Tracker::new(0);
        let p = "/org/freedesktop/systemd1/unit/job_2dx_2eservice";
        t.apply(p, UNIT_IFACE, &[("Description".into(), Val::Var(Box::new(Val::S("d".into()))))], false);
        assert!(matches!(t.apply(p, UNIT_IFACE, &[("ActiveState".into(), Val::Var(Box::new(Val::S("inactive".into()))))], true), Next::Nothing), "unseen live, an end is ignored");
        t.adopt(p);
        assert!(matches!(t.apply(p, UNIT_IFACE, &[("ActiveState".into(), Val::Var(Box::new(Val::S("inactive".into()))))], true), Next::Close(_)));
    }
}

#[cfg(test)]
mod adoption_tests {
    use super::*;
    use crate::dbus::{decode, encode_signal, message_len, METHOD_RETURN};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const P: &str = "/org/freedesktop/systemd1/unit/job_2dx_2eservice";

    fn pad(b: &mut Vec<u8>, n: usize) {
        while !b.len().is_multiple_of(n) {
            b.push(0);
        }
    }
    fn u32_(b: &mut Vec<u8>, v: u32) {
        pad(b, 4);
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn str_(b: &mut Vec<u8>, s: &str) {
        u32_(b, s.len() as u32);
        b.extend_from_slice(s.as_bytes());
        b.push(0);
    }
    fn array(b: &mut Vec<u8>, elem_align: usize, fill: impl FnOnce(&mut Vec<u8>)) {
        pad(b, 4);
        let at = b.len();
        u32_(b, 0);
        pad(b, elem_align);
        let start = b.len();
        fill(b);
        let n = (b.len() - start) as u32;
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn entry(b: &mut Vec<u8>, key: &str, sig: &str, put: impl FnOnce(&mut Vec<u8>)) {
        pad(b, 8);
        str_(b, key);
        b.push(sig.len() as u8);
        b.extend_from_slice(sig.as_bytes());
        b.push(0);
        match sig {
            "t" => pad(b, 8),
            "ay" => pad(b, 1),
            _ => {}
        }
        put(b);
    }
    fn reply(serial: u32, reply_serial: u32, sig: &str, body: &[u8]) -> Vec<u8> {
        let mut f = vec![5, 1, b'u', 0];
        f.extend_from_slice(&reply_serial.to_le_bytes());
        pad(&mut f, 8);
        f.extend_from_slice(&[8, 1, b'g', 0, sig.len() as u8]);
        f.extend_from_slice(sig.as_bytes());
        f.push(0);
        let mut m = vec![b'l', METHOD_RETURN, 0, 1];
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(&serial.to_le_bytes());
        m.extend_from_slice(&(f.len() as u32).to_le_bytes());
        m.extend_from_slice(&f);
        pad(&mut m, 8);
        m.extend_from_slice(body);
        m
    }
    fn list_units() -> Vec<u8> {
        let mut b = Vec::new();
        array(&mut b, 8, |b| {
            pad(b, 8);
            for s in ["job-x.service", "d", "loaded", "active", "running", ""] {
                str_(b, s);
            }
            str_(b, P);
            u32_(b, 0);
            str_(b, "");
            str_(b, "/");
        });
        b
    }
    fn unit_props(ended: bool) -> Vec<u8> {
        let mut b = Vec::new();
        array(&mut b, 8, |b| {
            entry(b, "ActiveState", "s", |b| str_(b, if ended { "inactive" } else { "active" }));
            entry(b, "InvocationID", "ay", |b| array(b, 1, |b| b.extend_from_slice(&[7; 16])));
            entry(b, "InactiveExitTimestamp", "t", |b| b.extend_from_slice(&1_000_000u64.to_le_bytes()));
            if ended {
                entry(b, "InactiveEnterTimestamp", "t", |b| b.extend_from_slice(&4_000_000u64.to_le_bytes()));
            }
        });
        b
    }

    #[test_id(twu25qy)]
    #[test]
    fn a_unit_listed_active_is_recorded_once_when_its_inactive_signal_follows() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let mut signals = b.try_clone().unwrap();
        let ended = Arc::new(AtomicBool::new(false));
        let seen_ended = ended.clone();
        let peer = std::thread::spawn(move || loop {
            let mut head = [0u8; 16];
            if b.read_exact(&mut head).is_err() {
                return;
            }
            let total = message_len(&head).unwrap().unwrap();
            let mut rest = vec![0u8; total - 16];
            b.read_exact(&mut rest).unwrap();
            let m = decode(&[head.to_vec(), rest].concat()).unwrap();
            let r = match (m.member.as_deref(), m.body.first().and_then(Val::as_str)) {
                (Some("ListUnits"), _) => reply(1, m.serial, "a(ssssssouso)", &list_units()),
                (_, Some(UNIT_IFACE)) => reply(1, m.serial, "a{sv}", &unit_props(seen_ended.load(Ordering::SeqCst))),
                _ => reply(1, m.serial, "a{sv}", &{
                    let mut e = Vec::new();
                    array(&mut e, 8, |_| {});
                    e
                }),
            };
            b.write_all(&r).unwrap();
        });
        let mut conn = Conn::over(a);
        let mut t = Tracker::new(0);
        assert!(catch_up(&mut conn, &mut t, || 9).unwrap().is_empty(), "a run still going is not yet a record");
        ended.store(true, Ordering::SeqCst);
        let mut body = Vec::new();
        str_(&mut body, UNIT_IFACE);
        array(&mut body, 8, |b| entry(b, "ActiveState", "s", |b| str_(b, "inactive")));
        array(&mut body, 4, |_| {});
        signals.write_all(&encode_signal(5, P, PROPS_IFACE, "PropertiesChanged", "sa{sv}as", &body)).unwrap();
        let records = step(&mut conn, &mut t, || 9, Duration::from_secs(5)).unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!((records[0].invocation_id.as_str(), records[0].started_us, records[0].ended_us), ("07070707070707070707070707070707", Some(1_000_000), 4_000_000));
        drop(conn);
        peer.join().unwrap();
    }
}
