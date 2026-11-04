//! The `Units` port (S6): start, stop, restart and read a unit, and learn when one ends.
//!
//! Three backends keep one contract: [`SystemdUnits`] over the user manager's D-Bus (Linux only),
//! [`ProcessUnits`] over a plain child process (every platform), and [`ScriptedUnits`] on the
//! virtual clock for the simulator. Nothing here needs systemd to compile or to run.
//!
//! Every unit a task starts is named by [`unit_name`], so the task that started it recognises its
//! own `unit.ran` by parsing the name back with [`parse_unit_name`].

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::event::EventKind;
use crate::ports::Clock;
use crate::{Result, RuntimeError};

pub const UNIT_RAN: &str = "unit.ran";

/// Reserved for units a task starts; installed units never carry it, so a parse of one is `None`.
pub const PREFIX: &str = "spira-run-";
const SUFFIX: &str = ".service";
/// systemd refuses a longer unit name.
const NAME_MAX: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitSpec {
    pub task: String,
    /// The idempotency key: the same (task, key) names the same unit.
    pub key: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnitHandle {
    pub name: String,
}

impl UnitHandle {
    /// An installed unit, addressed by its own name.
    pub fn named(name: &str) -> Self {
        UnitHandle { name: name.into() }
    }

    pub fn of(task: &str, key: &str) -> Result<Self> {
        Ok(UnitHandle { name: unit_name(task, key)? })
    }

    /// `(task, key)` when the unit was started under the naming rule.
    pub fn owner(&self) -> Option<(String, String)> {
        parse_unit_name(&self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitState {
    Activating,
    Active,
    Deactivating,
    Inactive,
    Failed,
    /// The backend knows no such unit.
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFacts {
    pub state: UnitState,
    pub main_pid: Option<u32>,
    /// The last run's exit, once one has ended.
    pub exit: Option<Exit>,
    /// When the unit entered `state`, on the backend's clock.
    pub since_ms: Option<u64>,
}

impl UnitFacts {
    pub fn absent() -> Self {
        UnitFacts { state: UnitState::Absent, main_pid: None, exit: None, since_ms: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRan {
    pub handle: UnitHandle,
    pub exit: Exit,
    pub duration_ms: u64,
}

impl UnitRan {
    pub fn event_kind(&self) -> EventKind {
        EventKind::new(UNIT_RAN, &serde_json::to_string(self).unwrap_or_default())
    }

    pub fn from_event_kind(k: &EventKind) -> Option<UnitRan> {
        (k.tag == UNIT_RAN).then(|| serde_json::from_str(&k.body).ok()).flatten()
    }

    /// The key of this run when `task` started it.
    pub fn key_for(&self, task: &str) -> Option<String> {
        self.handle.owner().filter(|(t, _)| t == task).map(|(_, k)| k)
    }
}

pub trait Units: Send + Sync {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle>;
    fn stop(&self, h: &UnitHandle) -> Result<()>;
    fn restart(&self, h: &UnitHandle) -> Result<()>;
    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts>;
}

/// The completion source: every run that ended since the last call, once each.
pub trait UnitCompletions: Send + Sync {
    fn ran(&self) -> Result<Vec<UnitRan>>;
}

pub const UNIT_START: &str = "unit.start";
pub const UNIT_STOP: &str = "unit.stop";
pub const UNIT_RESTART: &str = "unit.restart";
pub const UNIT_FACTS: &str = "unit.facts";
/// The effect names a `Units` backend answers, each through [`effect`].
pub const EFFECTS: [&str; 4] = [UNIT_START, UNIT_STOP, UNIT_RESTART, UNIT_FACTS];

/// One unit effect on `units`: `unit.start` takes a [`UnitSpec`] and answers its [`UnitHandle`];
/// `unit.stop` and `unit.restart` take a handle and answer nothing; `unit.facts` takes a handle
/// and answers its [`UnitFacts`]. All as JSON, so a host runner and a simulator double share it.
pub fn effect(units: &dyn Units, name: &str, input: &str) -> std::result::Result<String, String> {
    fn arg<T: serde::de::DeserializeOwned>(name: &str, input: &str) -> std::result::Result<T, String> {
        serde_json::from_str(input).map_err(|e| format!("{name}: bad input: {e}"))
    }
    fn out<T: Serialize>(v: &T) -> std::result::Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }
    match name {
        UNIT_START => out(&units.start(&arg::<UnitSpec>(name, input)?).map_err(|e| e.0)?),
        UNIT_STOP => units.stop(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_RESTART => units.restart(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_FACTS => out(&units.facts(&arg(name, input)?).map_err(|e| e.0)?),
        _ => Err(format!("{name} is not a unit effect")),
    }
}

fn keep(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b':'
}

fn escape(s: &str) -> String {
    s.bytes().map(|b| if keep(b) { (b as char).to_string() } else { format!("_{b:02x}") }).collect()
}

fn unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'_' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                if keep(byte) || hex.bytes().any(|h| h.is_ascii_uppercase()) {
                    return None;
                }
                out.push(byte);
                i += 3;
            }
            c if keep(c) => {
                out.push(c);
                i += 1;
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
}

/// `spira-run-<task>-<key>.service`, each part escaped so `-` appears only as the separator.
pub fn unit_name(task: &str, key: &str) -> Result<String> {
    if task.is_empty() || key.is_empty() {
        return Err(RuntimeError::refused(format!("a unit needs a task and a key (task {task:?}, key {key:?})")));
    }
    let name = format!("{PREFIX}{}-{}{SUFFIX}", escape(task), escape(key));
    if name.len() > NAME_MAX {
        return Err(RuntimeError::refused(format!("unit name for task {task:?} is {} bytes, over systemd's {NAME_MAX}", name.len())));
    }
    Ok(name)
}

/// The inverse of [`unit_name`]; `None` for any name it could not have produced.
pub fn parse_unit_name(name: &str) -> Option<(String, String)> {
    let body = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (task, key) = body.split_once('-')?;
    let (task, key) = (unescape(task)?, unescape(key)?);
    (!task.is_empty() && !key.is_empty()).then_some((task, key))
}

pub fn description(task: &str, key: &str) -> String {
    format!("spira task {task} key {key}")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// ProcessUnits: a child process per unit.

struct Running {
    spec: UnitSpec,
    child: Option<Child>,
    started_ms: u64,
    exit: Option<Exit>,
    ended_ms: Option<u64>,
    pid: Option<u32>,
}

pub struct ProcessUnits {
    clock: Arc<dyn Clock>,
    units: Mutex<BTreeMap<String, Running>>,
    ended: Mutex<VecDeque<UnitRan>>,
}

#[cfg(unix)]
fn exit_of(s: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt;
    match (s.code(), s.signal()) {
        (Some(c), _) => Exit::Code(c),
        (None, Some(sig)) => Exit::Signal(sig),
        _ => Exit::Unknown,
    }
}

#[cfg(not(unix))]
fn exit_of(s: std::process::ExitStatus) -> Exit {
    s.code().map_or(Exit::Unknown, Exit::Code)
}

impl ProcessUnits {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        ProcessUnits { clock, units: Mutex::new(BTreeMap::new()), ended: Mutex::new(VecDeque::new()) }
    }

    fn spawn(spec: &UnitSpec) -> Result<Child> {
        let (prog, args) = spec.argv.split_first().ok_or_else(|| RuntimeError::refused(format!("unit for task {} has an empty argv", spec.task)))?;
        let mut c = Command::new(prog);
        c.args(args).envs(spec.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null());
        if let Some(d) = &spec.cwd {
            c.current_dir(d);
        }
        c.spawn().map_err(|e| RuntimeError::new(format!("spawn {prog}: {e}")))
    }

    fn end(&self, name: &str, r: &mut Running, exit: Exit) {
        let now = self.clock.now_ms();
        r.child = None;
        r.exit = Some(exit);
        r.ended_ms = Some(now);
        lock(&self.ended).push_back(UnitRan { handle: UnitHandle::named(name), exit, duration_ms: now.saturating_sub(r.started_ms) });
    }

    fn reap(&self) {
        let mut units = lock(&self.units);
        for (name, r) in units.iter_mut() {
            let done = match r.child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(s))) => Some(exit_of(s)),
                Some(Err(_)) => Some(Exit::Unknown),
                _ => None,
            };
            if let Some(exit) = done {
                self.end(name, r, exit);
            }
        }
    }

    fn kill(&self, name: &str, r: &mut Running) {
        if let Some(mut c) = r.child.take() {
            let _ = c.kill();
            let exit = c.wait().map_or(Exit::Unknown, exit_of);
            self.end(name, r, exit);
        }
    }
}

impl Units for ProcessUnits {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
        let h = UnitHandle::of(&spec.task, &spec.key)?;
        self.reap();
        let mut units = lock(&self.units);
        if units.get(&h.name).is_some_and(|r| r.child.is_some()) {
            return Err(RuntimeError::refused(format!("{} is already running", h.name)));
        }
        let child = Self::spawn(spec)?;
        units.insert(h.name.clone(), Running { spec: spec.clone(), pid: Some(child.id()), child: Some(child), started_ms: self.clock.now_ms(), exit: None, ended_ms: None });
        Ok(h)
    }

    fn stop(&self, h: &UnitHandle) -> Result<()> {
        let mut units = lock(&self.units);
        let r = units.get_mut(&h.name).ok_or_else(|| RuntimeError::refused(format!("{} was not started by this backend", h.name)))?;
        self.kill(&h.name, r);
        Ok(())
    }

    fn restart(&self, h: &UnitHandle) -> Result<()> {
        let mut units = lock(&self.units);
        let r = units.get_mut(&h.name).ok_or_else(|| RuntimeError::refused(format!("{} was not started by this backend", h.name)))?;
        self.kill(&h.name, r);
        let child = Self::spawn(&r.spec)?;
        r.pid = Some(child.id());
        r.child = Some(child);
        r.started_ms = self.clock.now_ms();
        r.ended_ms = None;
        Ok(())
    }

    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
        self.reap();
        let units = lock(&self.units);
        let Some(r) = units.get(&h.name) else { return Ok(UnitFacts::absent()) };
        let state = match (&r.child, r.exit) {
            (Some(_), _) => UnitState::Active,
            (None, Some(Exit::Code(0))) => UnitState::Inactive,
            (None, _) => UnitState::Failed,
        };
        Ok(UnitFacts { state, main_pid: r.child.as_ref().and(r.pid), exit: r.exit, since_ms: Some(r.ended_ms.unwrap_or(r.started_ms)) })
    }
}

impl UnitCompletions for ProcessUnits {
    fn ran(&self) -> Result<Vec<UnitRan>> {
        self.reap();
        Ok(lock(&self.ended).drain(..).collect())
    }
}

// ---------------------------------------------------------------------------------------------
// ScriptedUnits: the simulator's backend. A task's unit runs for its scripted duration on the
// virtual clock and ends with its scripted exit.

#[derive(Default)]
struct Script {
    runs: BTreeMap<String, (u64, Exit)>,
    live: BTreeMap<String, (u64, u64, Exit)>,
    installed: BTreeMap<String, UnitFacts>,
    last: BTreeMap<String, (Exit, u64)>,
    calls: Vec<String>,
    ended: VecDeque<UnitRan>,
}

pub struct ScriptedUnits {
    clock: Arc<dyn Clock>,
    s: Mutex<Script>,
}

impl ScriptedUnits {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        ScriptedUnits { clock, s: Mutex::new(Script::default()) }
    }

    /// How every unit `task` starts behaves: it runs `duration_ms`, then exits with `exit`.
    pub fn script(&self, task: &str, duration_ms: u64, exit: Exit) {
        lock(&self.s).runs.insert(task.into(), (duration_ms, exit));
    }

    /// An installed unit (the scheduler's own, say) and the facts it reports until changed.
    pub fn install(&self, name: &str, facts: UnitFacts) {
        lock(&self.s).installed.insert(name.into(), facts);
    }

    /// Every start, stop and restart, in order, as `<verb> <unit>`.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.s).calls.clone()
    }

    fn settle(&self, s: &mut Script) {
        let now = self.clock.now_ms();
        let done: Vec<String> = s.live.iter().filter(|(_, (start, d, _))| start + d <= now).map(|(n, _)| n.clone()).collect();
        for n in done {
            if let Some((start, d, exit)) = s.live.remove(&n) {
                s.last.insert(n.clone(), (exit, start + d));
                s.ended.push_back(UnitRan { handle: UnitHandle::named(&n), exit, duration_ms: d });
            }
        }
    }
}

impl Units for ScriptedUnits {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
        let h = UnitHandle::of(&spec.task, &spec.key)?;
        let mut s = lock(&self.s);
        self.settle(&mut s);
        let (d, exit) = *s.runs.get(&spec.task).ok_or_else(|| RuntimeError::refused(format!("no script for task {}", spec.task)))?;
        if s.live.contains_key(&h.name) {
            return Err(RuntimeError::refused(format!("{} is already running", h.name)));
        }
        s.calls.push(format!("start {}", h.name));
        s.live.insert(h.name.clone(), (self.clock.now_ms(), d, exit));
        Ok(h)
    }

    fn stop(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        self.settle(&mut s);
        s.calls.push(format!("stop {}", h.name));
        if let Some(f) = s.installed.get_mut(&h.name) {
            *f = UnitFacts { state: UnitState::Inactive, main_pid: None, exit: Some(Exit::Signal(15)), since_ms: Some(now) };
        } else if let Some((start, _, _)) = s.live.remove(&h.name) {
            s.last.insert(h.name.clone(), (Exit::Signal(15), now));
            s.ended.push_back(UnitRan { handle: h.clone(), exit: Exit::Signal(15), duration_ms: now.saturating_sub(start) });
        }
        Ok(())
    }

    fn restart(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        s.calls.push(format!("restart {}", h.name));
        match s.installed.get_mut(&h.name) {
            Some(f) => {
                *f = UnitFacts { state: UnitState::Active, main_pid: f.main_pid, exit: f.exit, since_ms: Some(now) };
                Ok(())
            }
            None => Err(RuntimeError::refused(format!("{} is not installed in this script", h.name))),
        }
    }

    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
        let mut s = lock(&self.s);
        self.settle(&mut s);
        if let Some(f) = s.installed.get(&h.name) {
            return Ok(f.clone());
        }
        if let Some((start, _, _)) = s.live.get(&h.name) {
            return Ok(UnitFacts { state: UnitState::Active, main_pid: None, exit: None, since_ms: Some(*start) });
        }
        Ok(match s.last.get(&h.name) {
            Some((exit, at)) => UnitFacts { state: if *exit == Exit::Code(0) { UnitState::Inactive } else { UnitState::Failed }, main_pid: None, exit: Some(*exit), since_ms: Some(*at) },
            None => UnitFacts::absent(),
        })
    }
}

impl UnitCompletions for ScriptedUnits {
    fn ran(&self) -> Result<Vec<UnitRan>> {
        let mut s = lock(&self.s);
        self.settle(&mut s);
        Ok(s.ended.drain(..).collect())
    }
}

// ---------------------------------------------------------------------------------------------
// SystemdUnits: the user manager over D-Bus, with unit-run's connection and signal tracker.

#[cfg(target_os = "linux")]
pub use systemd::{encode_call_body, start_transient_args, Arg, Bus, SystemdUnits};

#[cfg(target_os = "linux")]
mod systemd {
    use super::*;
    use std::time::Duration;
    use unit_run::dbus::{Conn, Msg, Val, SIGNAL};
    use unit_run::watch::{Next, Tracker, MANAGER, MATCH_RULES};

    const ROOT: &str = "/org/freedesktop/systemd1";
    const MANAGER_IFACE: &str = "org.freedesktop.systemd1.Manager";
    const UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";
    const SERVICE_IFACE: &str = "org.freedesktop.systemd1.Service";
    const PROPS: &str = "org.freedesktop.DBus.Properties";

    /// A D-Bus argument, enough of the type system for StartTransientUnit.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Arg {
        S(String),
        B(bool),
        Arr(&'static str, Vec<Arg>),
        Struct(Vec<Arg>),
        Var(Box<Arg>),
    }

    impl Arg {
        fn sig(&self) -> String {
            match self {
                Arg::S(_) => "s".into(),
                Arg::B(_) => "b".into(),
                Arg::Arr(elem, _) => format!("a{elem}"),
                Arg::Struct(f) => format!("({})", f.iter().map(Arg::sig).collect::<String>()),
                Arg::Var(_) => "v".into(),
            }
        }
    }

    fn align(buf: &mut Vec<u8>, n: usize) {
        while !buf.len().is_multiple_of(n) {
            buf.push(0);
        }
    }

    fn put_u32(buf: &mut Vec<u8>, v: u32) {
        align(buf, 4);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_sig(buf: &mut Vec<u8>, s: &str) {
        buf.push(s.len() as u8);
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }

    fn elem_align(sig: &str) -> usize {
        match sig.as_bytes().first() {
            Some(b'(' | b'{' | b'x' | b't' | b'd') => 8,
            Some(b'y' | b'g' | b'v') => 1,
            Some(b'n' | b'q') => 2,
            _ => 4,
        }
    }

    fn put(buf: &mut Vec<u8>, a: &Arg) {
        match a {
            Arg::S(s) => {
                put_u32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
                buf.push(0);
            }
            Arg::B(b) => put_u32(buf, u32::from(*b)),
            Arg::Arr(elem, items) => {
                put_u32(buf, 0);
                let at = buf.len() - 4;
                align(buf, elem_align(elem));
                let start = buf.len();
                for i in items {
                    put(buf, i);
                }
                let n = (buf.len() - start) as u32;
                buf[at..at + 4].copy_from_slice(&n.to_le_bytes());
            }
            Arg::Struct(f) => {
                align(buf, 8);
                for x in f {
                    put(buf, x);
                }
            }
            Arg::Var(v) => {
                put_sig(buf, &v.sig());
                put(buf, v);
            }
        }
    }

    fn put_field(buf: &mut Vec<u8>, code: u8, sig: &str, s: &str) {
        align(buf, 8);
        buf.push(code);
        put_sig(buf, sig);
        put(buf, &Arg::S(s.into()));
    }

    /// A METHOD_CALL carrying `args`: the general form of unit-run's one-string `encode_call`.
    pub fn encode_call_body(serial: u32, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> Vec<u8> {
        let mut body = Vec::new();
        for a in args {
            put(&mut body, a);
        }
        let sig: String = args.iter().map(Arg::sig).collect();
        let mut fields = Vec::new();
        put_field(&mut fields, 1, "o", path);
        put_field(&mut fields, 2, "s", iface);
        put_field(&mut fields, 3, "s", member);
        put_field(&mut fields, 6, "s", dest);
        if !sig.is_empty() {
            align(&mut fields, 8);
            fields.push(8);
            put_sig(&mut fields, "g");
            put_sig(&mut fields, &sig);
        }
        let mut m = vec![b'l', unit_run::dbus::METHOD_CALL, 0, 1];
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(&serial.to_le_bytes());
        m.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        m.extend_from_slice(&fields);
        align(&mut m, 8);
        m.extend_from_slice(&body);
        m
    }

    fn prop(name: &str, v: Arg) -> Arg {
        Arg::Struct(vec![Arg::S(name.into()), Arg::Var(Box::new(v))])
    }

    fn strings(v: &[String]) -> Arg {
        Arg::Arr("s", v.iter().cloned().map(Arg::S).collect())
    }

    /// `StartTransientUnit(name, "fail", properties, [])` for `spec`.
    pub fn start_transient_args(spec: &UnitSpec) -> Result<Vec<Arg>> {
        let name = unit_name(&spec.task, &spec.key)?;
        let prog = spec.argv.first().ok_or_else(|| RuntimeError::refused(format!("unit for task {} has an empty argv", spec.task)))?;
        let mut props = vec![
            prop("Description", Arg::S(description(&spec.task, &spec.key))),
            prop("Type", Arg::S("exec".into())),
            prop("CollectMode", Arg::S("inactive-or-failed".into())),
            prop("ExecStart", Arg::Arr("(sasb)", vec![Arg::Struct(vec![Arg::S(prog.clone()), strings(&spec.argv), Arg::B(false)])])),
        ];
        if !spec.env.is_empty() {
            props.push(prop("Environment", strings(&spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>())));
        }
        if let Some(d) = &spec.cwd {
            props.push(prop("WorkingDirectory", Arg::S(d.display().to_string())));
        }
        Ok(vec![Arg::S(name), Arg::S("fail".into()), Arg::Arr("(sv)", props), Arg::Arr("(sa(sv))", vec![])])
    }

    pub trait Bus: Send {
        fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String>;
        fn next_signal(&mut self, timeout: Duration) -> std::result::Result<Option<Msg>, String>;
    }

    impl Bus for Conn {
        fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String> {
            match args {
                [] => Conn::call(self, dest, path, iface, member, None),
                [Arg::S(s)] => Conn::call(self, dest, path, iface, member, Some(s)),
                _ => Err(format!("{member}: unit-run's Conn sends at most one string argument; a {} body needs Conn::call_body", args.iter().map(Arg::sig).collect::<String>())),
            }
        }

        fn next_signal(&mut self, timeout: Duration) -> std::result::Result<Option<Msg>, String> {
            Conn::next_signal(self, timeout)
        }
    }

    pub struct SystemdUnits<B: Bus> {
        bus: Mutex<B>,
        clock: Arc<dyn Clock>,
        tracker: Mutex<Tracker>,
    }

    fn fault(e: String) -> RuntimeError {
        RuntimeError::new(format!("systemd: {e}"))
    }

    fn s(v: &str) -> Arg {
        Arg::S(v.into())
    }

    impl SystemdUnits<Conn> {
        /// The user manager on the session bus.
        pub fn connect(clock: Arc<dyn Clock>) -> Result<Self> {
            let path = unit_run::dbus::bus_path().map_err(fault)?;
            SystemdUnits::new(Conn::connect(&path).map_err(fault)?, clock)
        }
    }

    impl<B: Bus> SystemdUnits<B> {
        /// Subscribes to unit signals, so a run ending after this is a `unit.ran`.
        pub fn new(mut bus: B, clock: Arc<dyn Clock>) -> Result<Self> {
            for rule in MATCH_RULES {
                bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", &[s(rule)]).map_err(fault)?;
            }
            bus.call(MANAGER, ROOT, MANAGER_IFACE, "Subscribe", &[]).map_err(fault)?;
            let since_us = clock.now_ms().saturating_mul(1000);
            Ok(SystemdUnits { bus: Mutex::new(bus), clock, tracker: Mutex::new(Tracker::new(since_us)) })
        }

        fn unit_path(bus: &mut B, name: &str) -> Result<String> {
            let r = bus.call(MANAGER, ROOT, MANAGER_IFACE, "LoadUnit", &[s(name)]).map_err(fault)?;
            r.first().and_then(Val::as_str).map(String::from).ok_or_else(|| fault(format!("LoadUnit {name}: no object path")))
        }

        fn unit_call(&self, h: &UnitHandle, member: &str) -> Result<()> {
            let mut bus = lock(&self.bus);
            let path = Self::unit_path(&mut bus, &h.name)?;
            bus.call(MANAGER, &path, UNIT_IFACE, member, &[s("replace")]).map_err(fault).map(|_| ())
        }
    }

    fn get_all(bus: &mut impl Bus, path: &str, iface: &str) -> Vec<(String, Val)> {
        bus.call(MANAGER, path, PROPS, "GetAll", &[Arg::S(iface.into())]).ok().and_then(|b| b.first().map(Val::dict)).unwrap_or_default()
    }

    fn find<'a>(d: &'a [(String, Val)], k: &str) -> Option<&'a Val> {
        d.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    /// The facts a unit's `Unit` and `Service` property sets state.
    pub(super) fn facts_from(unit: &[(String, Val)], service: &[(String, Val)]) -> UnitFacts {
        if find(unit, "LoadState").and_then(Val::as_str).is_none_or(|l| l == "not-found") {
            return UnitFacts::absent();
        }
        let state = match find(unit, "ActiveState").and_then(Val::as_str) {
            Some("active" | "reloading" | "refreshing") => UnitState::Active,
            Some("activating") => UnitState::Activating,
            Some("deactivating") => UnitState::Deactivating,
            Some("failed") => UnitState::Failed,
            _ => UnitState::Inactive,
        };
        let code = find(service, "ExecMainCode").and_then(Val::as_i64);
        let status = find(service, "ExecMainStatus").and_then(Val::as_i64).and_then(|v| i32::try_from(v).ok());
        let exit = match (code, status) {
            (Some(1), Some(st)) => Some(Exit::Code(st)),
            (Some(2 | 3), Some(st)) => Some(Exit::Signal(st)),
            (Some(c), _) if c > 0 => Some(Exit::Unknown),
            _ => None,
        };
        let main_pid = find(service, "MainPID").and_then(Val::as_u64).and_then(|p| u32::try_from(p).ok()).filter(|p| *p != 0);
        let since_ms = find(unit, "StateChangeTimestamp").and_then(Val::as_u64).filter(|u| *u != 0).map(|u| u / 1000);
        UnitFacts { state, main_pid, exit, since_ms }
    }

    fn live_invocation(d: &[(String, Val)]) -> bool {
        find(d, "InvocationID").and_then(Val::as_bytes).is_some_and(|b| b.iter().any(|x| *x != 0))
    }

    impl<B: Bus> Units for SystemdUnits<B> {
        fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
            let args = start_transient_args(spec)?;
            lock(&self.bus).call(MANAGER, ROOT, MANAGER_IFACE, "StartTransientUnit", &args).map_err(fault)?;
            UnitHandle::of(&spec.task, &spec.key)
        }

        fn stop(&self, h: &UnitHandle) -> Result<()> {
            self.unit_call(h, "Stop")
        }

        fn restart(&self, h: &UnitHandle) -> Result<()> {
            self.unit_call(h, "Restart")
        }

        fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
            let mut bus = lock(&self.bus);
            let path = Self::unit_path(&mut bus, &h.name)?;
            let unit = get_all(&mut *bus, &path, UNIT_IFACE);
            let service = get_all(&mut *bus, &path, SERVICE_IFACE);
            Ok(facts_from(&unit, &service))
        }
    }

    fn changed(m: &Msg) -> Option<(String, String, Vec<(String, Val)>)> {
        if m.kind != SIGNAL || m.member.as_deref() != Some("PropertiesChanged") {
            return None;
        }
        Some((m.path.clone()?, m.body.first()?.as_str()?.to_string(), m.body.get(1)?.dict()))
    }

    impl<B: Bus> UnitCompletions for SystemdUnits<B> {
        fn ran(&self) -> Result<Vec<UnitRan>> {
            let mut bus = lock(&self.bus);
            let mut t = lock(&self.tracker);
            let mut out = Vec::new();
            while let Some(m) = bus.next_signal(Duration::ZERO).map_err(fault)? {
                if m.member.as_deref() == Some("NameOwnerChanged") {
                    bus.call(MANAGER, ROOT, MANAGER_IFACE, "Subscribe", &[]).map_err(fault)?;
                    continue;
                }
                let Some((path, iface, props)) = changed(&m) else { continue };
                let close = match t.apply(&path, &iface, &props, true) {
                    Next::Nothing => continue,
                    Next::Snapshot(_) => false,
                    Next::Close(_) => true,
                };
                let unit = get_all(&mut *bus, &path, UNIT_IFACE);
                if live_invocation(&unit) {
                    t.apply(&path, UNIT_IFACE, &unit, false);
                    t.apply(&path, SERVICE_IFACE, &get_all(&mut *bus, &path, SERVICE_IFACE), false);
                }
                if !close {
                    continue;
                }
                let Some(r) = t.finish(&path, self.clock.now_ms().saturating_mul(1000)) else { continue };
                if parse_unit_name(&r.unit).is_none() {
                    continue;
                }
                let exit = match (r.exit_code.as_deref(), r.exit_status.and_then(|s| i32::try_from(s).ok())) {
                    (Some("exited"), Some(st)) => Exit::Code(st),
                    (Some("killed" | "dumped"), Some(st)) => Exit::Signal(st),
                    _ if r.result == "success" => Exit::Code(0),
                    _ => Exit::Unknown,
                };
                let duration_ms = r.started_us.map_or(0, |s| r.ended_us.saturating_sub(s) / 1000);
                out.push(UnitRan { handle: UnitHandle::named(&r.unit), exit, duration_ms });
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::VirtualClock;

    fn spec(task: &str, key: &str, argv: &[&str]) -> UnitSpec {
        UnitSpec { task: task.into(), key: key.into(), argv: argv.iter().map(|s| s.to_string()).collect(), env: vec![], cwd: None }
    }

    #[test]
    fn a_unit_name_carries_its_task_and_key_and_parses_back() {
        let cases = [("czar-pass", "round/7"), ("a_b", "k"), ("x", "ünï code"), ("land.round", "2026-11-03T04:20:00Z")];
        for (task, key) in cases {
            let name = unit_name(task, key).unwrap();
            assert!(name.starts_with(PREFIX) && name.ends_with(".service"), "{name}");
            assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)), "{name} holds a byte systemd refuses");
            assert_eq!(parse_unit_name(&name), Some((task.to_string(), key.to_string())), "{name}");
        }
        assert_eq!(unit_name("czar-pass", "r7").unwrap(), "spira-run-czar_2dpass-r7.service");
    }

    #[test]
    fn distinct_task_and_key_pairs_never_share_a_name() {
        assert_ne!(unit_name("a-b", "c").unwrap(), unit_name("a", "b-c").unwrap());
        assert_ne!(unit_name("a_2d", "c").unwrap(), unit_name("a-", "c").unwrap());
    }

    #[test]
    fn installed_and_malformed_names_belong_to_no_task() {
        for n in ["spira-runtime.service", "spira-runtime-watch.service", "spira-run-x.service", "spira-run-a-b-c.service", "spira-run-a_zz-b.service", "spira-run-a_2Db-c.service", "spira-run-a_61-b.service", "spira-run-a-b.timer", "run-r1234.service"] {
            assert_eq!(parse_unit_name(n), None, "{n}");
        }
    }

    #[test]
    fn an_empty_or_overlong_name_is_refused() {
        assert!(unit_name("", "k").unwrap_err().is_refused());
        assert!(unit_name("t", "").unwrap_err().is_refused());
        assert!(unit_name("t", &"/".repeat(100)).unwrap_err().is_refused());
    }

    #[test]
    fn a_task_recognises_its_own_unit_ran_and_no_other() {
        let ran = UnitRan { handle: UnitHandle::of("czar-pass", "r7").unwrap(), exit: Exit::Code(0), duration_ms: 5 };
        let back = UnitRan::from_event_kind(&ran.event_kind()).unwrap();
        assert_eq!(back, ran);
        assert_eq!(back.key_for("czar-pass").as_deref(), Some("r7"));
        assert_eq!(back.key_for("czar"), None);
        assert_eq!(UnitRan::from_event_kind(&EventKind::new("unit.other", &ran.event_kind().body)), None);
    }

    #[test]
    fn a_scripted_unit_ends_after_its_duration_on_the_virtual_clock() {
        let clock = VirtualClock::new(1_000);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        u.script("sweep", 30_000, Exit::Code(2));
        let h = u.start(&spec("sweep", "k1", &["x"])).unwrap();
        assert!(u.start(&spec("sweep", "k1", &["x"])).unwrap_err().is_refused(), "the same key twice while live");
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Active);
        clock.advance(29_999);
        assert!(u.ran().unwrap().is_empty());
        clock.advance(1);
        assert_eq!(u.ran().unwrap(), vec![UnitRan { handle: h.clone(), exit: Exit::Code(2), duration_ms: 30_000 }]);
        assert!(u.ran().unwrap().is_empty(), "once each");
        assert_eq!(u.facts(&h).unwrap(), UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Code(2)), since_ms: Some(31_000) });
        assert!(u.start(&spec("unscripted", "k", &["x"])).unwrap_err().is_refused());
    }

    #[test]
    fn unit_effects_round_trip_as_json() {
        let clock = VirtualClock::new(0);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        u.script("sweep", 10, Exit::Code(0));
        let h: UnitHandle = serde_json::from_str(&effect(&u, UNIT_START, &serde_json::to_string(&spec("sweep", "k", &["x"])).unwrap()).unwrap()).unwrap();
        assert_eq!(h, UnitHandle::of("sweep", "k").unwrap());
        let hj = serde_json::to_string(&h).unwrap();
        let f: UnitFacts = serde_json::from_str(&effect(&u, UNIT_FACTS, &hj).unwrap()).unwrap();
        assert_eq!(f.state, UnitState::Active);
        assert_eq!(effect(&u, UNIT_STOP, &hj), Ok(String::new()));
        assert!(effect(&u, UNIT_RESTART, &hj).unwrap_err().contains("not installed"));
        assert!(effect(&u, UNIT_FACTS, "nope").unwrap_err().contains("bad input"));
        assert!(effect(&u, "unit.other", &hj).is_err());
        assert_eq!(u.calls(), vec!["start spira-run-sweep-k.service", "stop spira-run-sweep-k.service", "restart spira-run-sweep-k.service"]);
    }

    #[test]
    fn a_scripted_installed_unit_restarts_into_active() {
        let clock = VirtualClock::new(5);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        let h = UnitHandle::named("spira-runtime.service");
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Absent);
        u.install(&h.name, UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Code(1)), since_ms: Some(1) });
        clock.advance(10);
        u.restart(&h).unwrap();
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Active);
        assert_eq!(u.facts(&h).unwrap().since_ms, Some(15));
        assert_eq!(u.calls(), vec!["restart spira-runtime.service"]);
    }

    #[test]
    fn a_process_unit_reports_its_exit_once_and_its_facts() {
        let clock = VirtualClock::new(0);
        let u = ProcessUnits::new(Arc::new(clock.clone()));
        let h = u.start(&spec("t", "k", &["sh", "-c", "exit 3"])).unwrap();
        let mut got = vec![];
        for _ in 0..500 {
            got = u.ran().unwrap();
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].handle.clone(), got[0].exit), (h.clone(), Exit::Code(3)));
        assert!(u.ran().unwrap().is_empty());
        let f = u.facts(&h).unwrap();
        assert_eq!((f.state, f.exit, f.main_pid), (UnitState::Failed, Some(Exit::Code(3)), None));
        assert_eq!(u.facts(&UnitHandle::named("other.service")).unwrap().state, UnitState::Absent);
    }

    #[test]
    fn a_stopped_process_unit_ends_on_a_signal() {
        let u = ProcessUnits::new(Arc::new(VirtualClock::new(0)));
        let h = u.start(&spec("t", "k", &["sleep", "30"])).unwrap();
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Active);
        assert!(u.start(&spec("t", "k", &["sleep", "30"])).unwrap_err().is_refused());
        u.stop(&h).unwrap();
        let ran = u.ran().unwrap();
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0].exit, Exit::Signal(9));
        u.restart(&h).unwrap();
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Active);
        u.stop(&h).unwrap();
        assert!(u.stop(&UnitHandle::named("never.service")).unwrap_err().is_refused());
    }

    #[cfg(target_os = "linux")]
    mod systemd {
        use super::super::systemd::facts_from;
        use super::super::*;
        use super::spec;
        use std::time::Duration;
        use unit_run::dbus::{decode, Msg, Val, METHOD_CALL};

        #[test]
        fn start_transient_unit_encodes_name_mode_and_exec_start() {
            let mut sp = spec("czar-pass", "r7", &["/bin/czar-pass", "--pass"]);
            sp.env = vec![("A".into(), "1".into())];
            let raw = encode_call_body(9, "org.freedesktop.systemd1", "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", "StartTransientUnit", &start_transient_args(&sp).unwrap());
            let m = decode(&raw).unwrap();
            assert_eq!((m.kind, m.serial, m.member.as_deref()), (METHOD_CALL, 9, Some("StartTransientUnit")));
            assert_eq!(m.body.len(), 4);
            assert_eq!(m.body[0].as_str(), Some("spira-run-czar_2dpass-r7.service"));
            assert_eq!(m.body[1].as_str(), Some("fail"));
            let props = m.body[2].dict();
            let get = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
            assert_eq!(get("Description").as_str(), Some("spira task czar-pass key r7"));
            assert_eq!(get("Type").as_str(), Some("exec"));
            let Val::Var(exec) = get("ExecStart") else { panic!("ExecStart not a variant") };
            let Val::Arr(cmds) = *exec else { panic!("ExecStart not an array") };
            let Val::Struct(f) = &cmds[0] else { panic!("ExecStart entry not a struct") };
            assert_eq!(f[0].as_str(), Some("/bin/czar-pass"));
            assert_eq!(f[1], Val::Arr(vec![Val::S("/bin/czar-pass".into()), Val::S("--pass".into())]));
            assert_eq!(f[2], Val::B(false));
            assert_eq!(get("Environment"), Val::Var(Box::new(Val::Arr(vec![Val::S("A=1".into())]))));
            assert_eq!(m.body[3], Val::Arr(vec![]));
        }

        fn d(pairs: &[(&str, Val)]) -> Vec<(String, Val)> {
            pairs.iter().map(|(k, v)| (k.to_string(), Val::Var(Box::new(v.clone())))).collect()
        }

        #[test]
        fn unit_and_service_properties_fold_into_facts() {
            let unit = d(&[("LoadState", Val::S("loaded".into())), ("ActiveState", Val::S("failed".into())), ("StateChangeTimestamp", Val::U(7_000_000))]);
            let svc = d(&[("ExecMainCode", Val::I(2)), ("ExecMainStatus", Val::I(9)), ("MainPID", Val::U(0))]);
            assert_eq!(facts_from(&unit, &svc), UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Signal(9)), since_ms: Some(7_000) });
            let unit = d(&[("LoadState", Val::S("loaded".into())), ("ActiveState", Val::S("active".into()))]);
            let svc = d(&[("ExecMainCode", Val::I(0)), ("MainPID", Val::U(42))]);
            assert_eq!(facts_from(&unit, &svc), UnitFacts { state: UnitState::Active, main_pid: Some(42), exit: None, since_ms: None });
            assert_eq!(facts_from(&d(&[("LoadState", Val::S("not-found".into()))]), &[]).state, UnitState::Absent);
        }

        /// A bus that records calls and answers from a table.
        struct FakeBus {
            calls: Arc<Mutex<Vec<String>>>,
            signals: Vec<Msg>,
        }

        impl Bus for FakeBus {
            fn call(&mut self, _dest: &str, path: &str, _iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String> {
                let first = match args.first() {
                    Some(Arg::S(s)) => s.clone(),
                    _ => String::new(),
                };
                self.calls.lock().unwrap().push(format!("{member} {path} {first}"));
                Ok(match member {
                    "LoadUnit" => vec![Val::S("/org/freedesktop/systemd1/unit/x".into())],
                    _ => vec![],
                })
            }
            fn next_signal(&mut self, _: Duration) -> std::result::Result<Option<Msg>, String> {
                Ok(self.signals.pop())
            }
        }

        #[test]
        fn restart_and_stop_go_to_the_unit_object_and_start_to_the_manager() {
            let calls = Arc::new(Mutex::new(vec![]));
            let u = SystemdUnits::new(FakeBus { calls: calls.clone(), signals: vec![] }, Arc::new(crate::ports::VirtualClock::new(0))).unwrap();
            let h = UnitHandle::named("spira-runtime.service");
            u.restart(&h).unwrap();
            u.stop(&h).unwrap();
            u.start(&spec("t", "k", &["/bin/true"])).unwrap();
            let c = calls.lock().unwrap().clone();
            assert_eq!(c.iter().filter(|l| l.starts_with("AddMatch")).count(), 2);
            assert!(c.contains(&"Subscribe /org/freedesktop/systemd1 ".to_string()));
            let tail: Vec<&str> = c.iter().skip(3).map(String::as_str).collect();
            assert_eq!(tail, vec![
                "LoadUnit /org/freedesktop/systemd1 spira-runtime.service",
                "Restart /org/freedesktop/systemd1/unit/x replace",
                "LoadUnit /org/freedesktop/systemd1 spira-runtime.service",
                "Stop /org/freedesktop/systemd1/unit/x replace",
                "StartTransientUnit /org/freedesktop/systemd1 spira-run-t-k.service",
            ]);
        }

        fn changed(path: &str, props: Vec<(String, Val)>) -> Msg {
            let body = vec![Val::S("org.freedesktop.systemd1.Unit".into()), Val::Arr(props.into_iter().map(|(k, v)| Val::Struct(vec![Val::S(k), v])).collect()), Val::Arr(vec![])];
            Msg { kind: unit_run::dbus::SIGNAL, serial: 1, reply_serial: None, path: Some(path.into()), interface: Some("org.freedesktop.DBus.Properties".into()), member: Some("PropertiesChanged".into()), error: None, body }
        }

        #[test]
        fn a_task_units_end_is_one_unit_ran_and_a_foreign_units_is_none() {
            let ours = "/org/freedesktop/systemd1/unit/spira_2drun_2dt_2dk_2eservice";
            let foreign = "/org/freedesktop/systemd1/unit/other_2eservice";
            let mut signals = vec![];
            for p in [ours, foreign] {
                signals.push(changed(p, d(&[("ActiveState", Val::S("active".into())), ("InvocationID", Val::Arr(vec![Val::U(7); 16])), ("InactiveExitTimestamp", Val::U(1_000_000))])));
                signals.push(changed(p, d(&[("ActiveState", Val::S("inactive".into())), ("InactiveEnterTimestamp", Val::U(4_500_000))])));
            }
            signals.reverse();
            let u = SystemdUnits::new(FakeBus { calls: Arc::new(Mutex::new(vec![])), signals }, Arc::new(crate::ports::VirtualClock::new(0))).unwrap();
            assert_eq!(u.ran().unwrap(), vec![UnitRan { handle: UnitHandle::of("t", "k").unwrap(), exit: Exit::Code(0), duration_ms: 3_500 }]);
            assert!(u.ran().unwrap().is_empty());
        }
    }
}
