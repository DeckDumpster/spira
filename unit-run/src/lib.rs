//! unit-run — one record per systemd user-unit execution, learned from the user manager's D-Bus
//! signals. Nothing here reads the journal. The clock and `systemctl` arrive as the traits below
//! so a test drives both.

pub mod dbus;
pub mod watch;

use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

pub const FAMILY: &str = "unit-run-dbus";

pub trait Clock {
    fn now_epoch(&self) -> u64;
}

pub trait Proc {
    fn run(&self, argv: &[&str]) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub invocation_id: String,
    pub unit: String,
    pub started_us: Option<u64>,
    pub ended_us: u64,
    pub result: String,
    pub exit_code: Option<String>,
    pub exit_status: Option<i64>,
    pub cpu_usec: Option<u64>,
    pub memory_peak: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
}

pub fn kind(unit: &str, triggered_by: Option<&str>) -> &'static str {
    if triggered_by.is_some() {
        "timer-triggered"
    } else if unit.ends_with(".service") {
        "service"
    } else {
        "other"
    }
}

/// The timer (or other trigger) behind a unit, asked of `systemctl` once per unit name.
pub struct Triggers<'a> {
    proc: &'a dyn Proc,
    cache: HashMap<String, Option<String>>,
}

impl<'a> Triggers<'a> {
    pub fn new(proc: &'a dyn Proc) -> Self {
        Triggers { proc, cache: HashMap::new() }
    }

    pub fn of(&mut self, unit: &str) -> Option<String> {
        if let Some(t) = self.cache.get(unit) {
            return t.clone();
        }
        let t = self
            .proc
            .run(&["systemctl", "--user", "show", "-p", "TriggeredBy", "--value", unit])
            .ok()
            .and_then(|out| out.split_whitespace().next().map(String::from));
        self.cache.insert(unit.to_string(), t.clone());
        t
    }
}

fn stamp(us: u64) -> String {
    tsd::iso_utc(us / 1_000_000)
}

/// The record's fields for `tsd::build_row`; absent facts are left out, not nulled.
pub fn record_fields(r: &Record, triggered_by: Option<&str>, instance: &str) -> Vec<(String, Value)> {
    let mut f: Vec<(String, Value)> = vec![
        ("unit".into(), r.unit.clone().into()),
        ("kind".into(), kind(&r.unit, triggered_by).into()),
        ("invocation_id".into(), r.invocation_id.clone().into()),
    ];
    if let Some(s) = r.started_us {
        f.push(("started_at".into(), stamp(s).into()));
    }
    f.push(("ended_at".into(), stamp(r.ended_us).into()));
    if let Some(s) = r.started_us {
        f.push(("duration_ms".into(), (r.ended_us.saturating_sub(s) / 1000).into()));
    }
    f.push(("result".into(), r.result.clone().into()));
    if let Some(c) = &r.exit_code {
        f.push(("exit_code".into(), c.clone().into()));
    }
    if let Some(s) = r.exit_status {
        f.push(("exit_status".into(), s.into()));
    }
    for (k, v) in [
        ("cpu_usec", r.cpu_usec),
        ("memory_peak", r.memory_peak),
        ("io_read_bytes", r.io_read_bytes),
        ("io_write_bytes", r.io_write_bytes),
    ] {
        if let Some(v) = v {
            f.push((k.into(), v.into()));
        }
    }
    if let Some(t) = triggered_by {
        f.push(("triggered_by".into(), t.into()));
    }
    f.push(("instance".into(), instance.into()));
    f
}

/// The invocation ids already in a family file's text: the dedupe key of every writer.
pub fn written_ids(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("invocation_id")?.as_str().map(String::from))
        .collect()
}

pub const ACCOUNTING_DROPIN: &str = "[Manager]\nDefaultIOAccounting=yes\n";

pub fn accounting_dropin_path(config_home: &std::path::Path) -> std::path::PathBuf {
    config_home.join("systemd/user.conf.d/spira-io-accounting.conf")
}

/// Turn IO accounting on for every user unit: true when the drop-in had to be written, which
/// is also when the manager must re-exec to apply it.
pub fn io_controller_available(controllers_file: &std::path::Path) -> bool {
    std::fs::read_to_string(controllers_file).is_ok_and(|t| t.split_whitespace().any(|c| c == "io"))
}

pub fn own_controllers_file() -> Option<std::path::PathBuf> {
    let t = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = t.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(std::path::Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/')).join("cgroup.controllers"))
}

pub fn ensure_io_accounting(config_home: &std::path::Path) -> Result<bool, String> {
    let path = accounting_dropin_path(config_home);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(ACCOUNTING_DROPIN) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, ACCOUNTING_DROPIN).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}
