//! unit-run — fold the user journal's unit lifecycle messages into one record per execution.
//!
//! An execution is one systemd invocation id. The journal's own message ids mark its edges:
//! `Starting` opens it, the exit message names how the main process ended, `Deactivated
//! successfully` / `Failed with result` close it (a oneshot that succeeds logs only `Finished`), and the `Consumed` resources line follows
//! with the accounting. The fold is pure; the clock, the journal and `systemctl` arrive as
//! the traits below so a test drives all three.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const FAMILY: &str = "unit-run";

pub const ID_STARTING: &str = "7d4958e842da4a758f6c1cdc7b36dcc5";
pub const ID_STARTED: &str = "39f53479d3a045ac8e11786248231fbf";
pub const ID_EXIT: &str = "98e322203f7a4ed290d09fe03c09fe15";
pub const ID_SUCCESS: &str = "7ad2d189f7e94e70a38c781354912448";
pub const ID_FAILURE: &str = "d9b373ed55a64feb8242e02dbe79a49c";
pub const ID_RESOURCES: &str = "ae8f7b866b0347b9af31fe1c80b127c0";

/// The `MESSAGE_ID=` matches handed to journalctl: the only entries the fold reads.
pub const MESSAGE_IDS: [&str; 6] = [ID_STARTING, ID_STARTED, ID_EXIT, ID_SUCCESS, ID_FAILURE, ID_RESOURCES];

/// How long past its close an execution waits for its `Consumed` line before it is written
/// without accounting.
pub const ACCOUNTING_GRACE_US: u64 = 2_000_000;

pub trait Clock {
    fn now_epoch(&self) -> u64;
}

pub trait Proc {
    fn run(&self, argv: &[&str]) -> Result<String, String>;
}

pub enum Item {
    Line(String),
    Idle,
}

pub trait Source {
    fn open(&self, after_cursor: Option<&str>, follow: bool) -> Result<Box<dyn Iterator<Item = Item>>, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub cursor: String,
    pub at_us: u64,
    pub message_id: String,
    pub unit: String,
    pub invocation: String,
    pub exit_code: Option<String>,
    pub exit_status: Option<i64>,
    pub command: Option<String>,
    pub message: Option<String>,
    pub job_result: Option<String>,
    pub unit_result: Option<String>,
    pub cpu_nsec: Option<u64>,
    pub memory_peak: Option<u64>,
    pub io_read: Option<u64>,
    pub io_written: Option<u64>,
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn number<T: std::str::FromStr>(v: &Value, key: &str) -> Option<T> {
    field(v, key)?.trim().parse().ok()
}

/// One `journalctl -o json` line. `None` for anything that is not a user-manager lifecycle
/// entry carrying a unit and an invocation id — those cannot be keyed to an execution.
pub fn parse_entry(line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line).ok()?;
    let message_id = field(&v, "MESSAGE_ID")?.to_string();
    if !MESSAGE_IDS.contains(&message_id.as_str()) {
        return None;
    }
    Some(Entry {
        cursor: field(&v, "__CURSOR")?.to_string(),
        at_us: number(&v, "__REALTIME_TIMESTAMP")?,
        message_id,
        unit: field(&v, "USER_UNIT")?.to_string(),
        invocation: field(&v, "USER_INVOCATION_ID")?.to_string(),
        exit_code: field(&v, "EXIT_CODE").map(String::from),
        exit_status: number(&v, "EXIT_STATUS"),
        command: field(&v, "COMMAND").map(String::from),
        message: field(&v, "MESSAGE").map(String::from),
        job_result: field(&v, "JOB_RESULT").map(String::from),
        unit_result: field(&v, "UNIT_RESULT").map(String::from),
        cpu_nsec: number(&v, "CPU_USAGE_NSEC"),
        memory_peak: number(&v, "MEMORY_PEAK"),
        io_read: number(&v, "IO_METRIC_READ_BYTES"),
        io_written: number(&v, "IO_METRIC_WRITE_BYTES"),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Acct {
    cpu_usec: Option<u64>,
    memory_peak: Option<u64>,
    io_read_bytes: Option<u64>,
    io_write_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct End {
    at_us: u64,
    result: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Open {
    unit: String,
    started_us: Option<u64>,
    exit_code: Option<String>,
    exit_status: Option<i64>,
    main_exit: bool,
    end: Option<End>,
    acct: Option<Acct>,
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

/// Where a reader of the journal stands: the cursor of the last entry folded and every
/// execution seen but not yet written. Resuming from it loses and repeats nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub cursor: Option<String>,
    latest_us: u64,
    open: BTreeMap<String, Open>,
}

impl Folder {
    pub fn push(&mut self, e: &Entry) -> Vec<Record> {
        self.cursor = Some(e.cursor.clone());
        self.latest_us = self.latest_us.max(e.at_us);
        let o = self.open.entry(e.invocation.clone()).or_default();
        o.unit = e.unit.clone();
        match e.message_id.as_str() {
            ID_STARTING => o.started_us = Some(o.started_us.map_or(e.at_us, |s| s.min(e.at_us))),
            ID_STARTED => {
                let msg = e.message.as_deref().unwrap_or("");
                if msg.starts_with("Finished ") && e.job_result.as_deref() == Some("done") {
                    o.end.get_or_insert(End { at_us: e.at_us, result: "success".into() });
                } else if msg.starts_with("Started ") {
                    o.started_us.get_or_insert(e.at_us);
                }
            }
            ID_EXIT => {
                let main = matches!(e.command.as_deref(), None | Some("ExecStart"));
                if main || !o.main_exit {
                    o.exit_code = e.exit_code.clone();
                    o.exit_status = e.exit_status;
                    o.main_exit = o.main_exit || main;
                }
            }
            ID_SUCCESS => o.end = Some(End { at_us: e.at_us, result: "success".into() }),
            ID_FAILURE => o.end = Some(End { at_us: e.at_us, result: e.unit_result.clone().unwrap_or_else(|| "failed".into()) }),
            ID_RESOURCES => {
                o.acct = Some(Acct {
                    cpu_usec: e.cpu_nsec.map(|n| n / 1000),
                    memory_peak: e.memory_peak,
                    io_read_bytes: e.io_read,
                    io_write_bytes: e.io_written,
                })
            }
            _ => {}
        }
        self.drain(self.latest_us, false)
    }

    /// The clock moved with no journal entry: executions past their accounting grace are due.
    pub fn tick(&mut self, now_us: u64) -> Vec<Record> {
        self.drain(now_us, false)
    }

    /// End of input that will not resume (backfill): every closed execution is written now.
    pub fn finish(&mut self) -> Vec<Record> {
        self.drain(u64::MAX, true)
    }

    fn drain(&mut self, now_us: u64, all: bool) -> Vec<Record> {
        let due: Vec<String> = self
            .open
            .iter()
            .filter(|(_, o)| match &o.end {
                Some(end) => all || o.acct.is_some() || now_us >= end.at_us.saturating_add(ACCOUNTING_GRACE_US),
                None => false,
            })
            .map(|(id, _)| id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| {
                let o = self.open.remove(&id)?;
                let end = o.end?;
                let a = o.acct.unwrap_or_default();
                Some(Record {
                    invocation_id: id,
                    unit: o.unit,
                    started_us: o.started_us,
                    ended_us: end.at_us,
                    result: end.result,
                    exit_code: o.exit_code,
                    exit_status: o.exit_status,
                    cpu_usec: a.cpu_usec,
                    memory_peak: a.memory_peak,
                    io_read_bytes: a.io_read_bytes,
                    io_write_bytes: a.io_write_bytes,
                })
            })
            .collect()
    }
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

/// Where a pass writes and what it must not write again.
pub struct Sink<'a> {
    pub path: std::path::PathBuf,
    pub host: String,
    pub instance: String,
    pub clock: &'a dyn Clock,
    pub triggers: Triggers<'a>,
    pub seen: BTreeSet<String>,
    pub scanned: u64,
}

impl Sink<'_> {
    /// Adopt the ids another writer appended since the last look, so two passes over one file
    /// (the follower and a backfill) never write the same execution twice.
    fn catch_up(&mut self) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else { return };
        if f.seek(SeekFrom::Start(self.scanned)).is_err() {
            return;
        }
        let mut tail = String::new();
        if f.read_to_string(&mut tail).is_err() {
            return;
        }
        let whole = tail.rfind('\n').map_or(0, |i| i + 1);
        self.seen.extend(written_ids(&tail[..whole]));
        self.scanned += whole as u64;
    }

    pub fn write(&mut self, records: &[Record]) -> Result<usize, String> {
        let mut n = 0;
        for r in records {
            self.catch_up();
            if !self.seen.insert(r.invocation_id.clone()) {
                continue;
            }
            let trig = self.triggers.of(&r.unit);
            let fields = record_fields(r, trig.as_deref(), &self.instance);
            let row = tsd::build_row(&tsd::iso_utc(self.clock.now_epoch()), &self.host, FAMILY, &fields)?;
            tsd::append_line(&self.path, &row)?;
            n += 1;
        }
        Ok(n)
    }
}

const SAVE_EVERY: usize = 500;

/// Read `source` to its end, writing each closed execution once. `save` receives the folder
/// after every write, on idle, and every few hundred lines — always after the records it
/// accounts for are on disk, so a crash between the two repeats a record the sink drops.
pub fn pass(
    source: &dyn Source,
    start: Folder,
    follow: bool,
    sink: &mut Sink,
    save: &mut dyn FnMut(&Folder) -> Result<(), String>,
) -> Result<(Folder, usize), String> {
    let mut folder = start;
    let mut written = 0;
    let mut since_save = 0;
    for item in source.open(folder.cursor.as_deref(), follow)? {
        let records = match item {
            Item::Idle => folder.tick(sink.clock.now_epoch().saturating_mul(1_000_000)),
            Item::Line(l) => match parse_entry(&l) {
                Some(e) => folder.push(&e),
                None => continue,
            },
        };
        let wrote = sink.write(&records)?;
        written += wrote;
        since_save += 1;
        if wrote > 0 || since_save >= SAVE_EVERY {
            save(&folder)?;
            since_save = 0;
        }
    }
    if !follow {
        written += sink.write(&folder.finish())?;
    }
    save(&folder)?;
    Ok((folder, written))
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
