//! The `Units` port (S6): start, stop, restart and read a unit, and learn when one ends.
//!
//! Three backends keep one contract: [`SystemdUnits`] over the user manager's D-Bus (Linux only),
//! [`ProcessUnits`] over a plain child process (every platform), and [`ScriptedUnits`] on the
//! virtual clock for the simulator. Nothing here needs systemd to compile or to run.
//!
//! Every unit a task starts is named by [`unit_name`], so the task that started it recognises its
//! own `unit.ran` by parsing the name back with [`parse_unit_name`].
//!
//! Past the four verbs the port reads a unit's [`UnitDetail`] (load, unit-file, active and sub
//! state, result, main pid, restarts, invocation id, start time), lists units and timers by glob,
//! starts, enables, disables and resets installed units, and runs a [`ScopeSpec`] under CPU and
//! memory limits whose output tails come back on its `unit.ran` ([`RanOutput`]). Every one of
//! them is a D-Bus method or property, never a child of the manager's command-line clients.
//! [`from_env`] picks the backend by [`UNITS_KEY`], so another crate's tests run on the
//! scripted one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitSpec {
    pub task: String,
    /// The idempotency key: the same (task, key) names the same unit.
    pub key: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// The properties a unit starts under besides its command: what `systemd-run --property=` gave
/// a dwarf call and the landing leg. A struct beside [`UnitSpec`] rather than new fields on it,
/// so every crate that builds a `UnitSpec` literal keeps compiling; [`StartSpec`] carries both as
/// the `unit.start` input, and an input with no `props` is a plain `UnitSpec`.
///
/// [`SystemdUnits`] sets each as a transient-unit property; [`ProcessUnits`] applies what a plain
/// child can honour (the two timeouts, `ExecStopPost`, the output file) and refuses the rest;
/// [`ScriptedUnits`] records them ([`ScriptedUnits::props`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitProps {
    /// `TimeoutStartSec=`: how long the start may take.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_timeout_ms: Option<u64>,
    /// `RuntimeMaxSec=`: the run is stopped once it has run this long.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_max_ms: Option<u64>,
    /// `IOSchedulingClass=`: `realtime`, `best-effort` or `idle`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_class: Option<String>,
    /// `IOWeight=`, 1 to 10000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_weight: Option<u64>,
    /// `Nice=`, -20 to 19.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nice: Option<i32>,
    /// `CPUQuota=` as a percentage of one CPU: 200 is two CPUs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_quota_pct: Option<u32>,
    /// `MemoryMax=` in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_bytes: Option<u64>,
    /// `ExecStopPost=`: each argv runs once the main process has ended, however it ended.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exec_stop_post: Vec<Vec<String>>,
    /// `StandardOutput=append:` and `StandardError=append:` this file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_append: Option<PathBuf>,
}

/// IOSchedulingClass's words and the numbers the property carries.
pub const IO_CLASSES: [(&str, i32); 3] = [("realtime", 1), ("best-effort", 2), ("idle", 3)];

impl UnitProps {
    pub fn is_empty(&self) -> bool {
        *self == UnitProps::default()
    }

    /// The properties set, by their systemd names.
    pub fn names(&self) -> Vec<&'static str> {
        [
            (self.start_timeout_ms.is_some(), "TimeoutStartSec"),
            (self.runtime_max_ms.is_some(), "RuntimeMaxSec"),
            (self.io_class.is_some(), "IOSchedulingClass"),
            (self.io_weight.is_some(), "IOWeight"),
            (self.nice.is_some(), "Nice"),
            (self.cpu_quota_pct.is_some(), "CPUQuota"),
            (self.memory_max_bytes.is_some(), "MemoryMax"),
            (!self.exec_stop_post.is_empty(), "ExecStopPost"),
            (self.output_append.is_some(), "StandardOutput"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n)
        .collect()
    }

    /// The `IOSchedulingClass` number, or a refusal naming a word systemd does not know.
    pub fn io_class_number(&self) -> Result<Option<i32>> {
        self.io_class.as_deref().map(|c| IO_CLASSES.iter().find(|(w, _)| *w == c).map(|(_, n)| *n).ok_or_else(|| RuntimeError::refused(format!("IOSchedulingClass {c:?} is not one of realtime, best-effort, idle")))).transpose()
    }
}

/// The `unit.start` input: the unit and the properties it starts under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSpec {
    #[serde(flatten)]
    pub unit: UnitSpec,
    #[serde(default, skip_serializing_if = "UnitProps::is_empty")]
    pub props: UnitProps,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitState {
    Activating,
    Active,
    Deactivating,
    Inactive,
    Failed,
    /// The backend knows no such unit.
    #[default]
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

/// Everything a backend can say about one unit: the [`UnitFacts`] summary plus the manager's own
/// words for it. `None` is "this backend cannot answer", never a guess. A struct of its own, not
/// new fields on `UnitFacts`, so every crate that builds a `UnitFacts` literal keeps compiling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitDetail {
    pub name: String,
    pub state: UnitState,
    pub exit: Option<Exit>,
    pub since_ms: Option<u64>,
    /// `LoadState`: loaded, not-found, masked, ...
    pub load_state: Option<String>,
    /// `UnitFileState` (is-enabled): enabled, disabled, static, transient, ...
    pub unit_file_state: Option<String>,
    /// `ActiveState`, verbatim.
    pub active_state: Option<String>,
    /// `SubState`: running, exited, dead, failed, ...
    pub sub_state: Option<String>,
    /// The type's `Result`: success, exit-code, signal, timeout, ...
    pub result: Option<String>,
    pub main_pid: Option<u32>,
    /// `NRestarts`: automatic restarts only, as systemd counts them.
    pub n_restarts: Option<u32>,
    /// `InvocationID` as 32 lowercase hex digits.
    pub invocation_id: Option<String>,
    /// `ExecMainStartTimestamp`, wall-clock milliseconds.
    pub exec_main_start_ms: Option<u64>,
}

impl UnitDetail {
    pub fn absent(name: &str) -> Self {
        UnitDetail { name: name.into(), ..UnitDetail::default() }
    }

    /// The summary alone; every fact the summary does not hold is unanswered.
    pub fn from_facts(name: &str, f: &UnitFacts) -> Self {
        UnitDetail { name: name.into(), state: f.state, exit: f.exit, since_ms: f.since_ms, main_pid: f.main_pid, ..UnitDetail::default() }
    }

    pub fn facts(&self) -> UnitFacts {
        UnitFacts { state: self.state, main_pid: self.main_pid, exit: self.exit, since_ms: self.since_ms }
    }
}

/// One loaded timer and when it next fires.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerFacts {
    pub name: String,
    /// The unit it activates.
    pub unit: Option<String>,
    pub active_state: Option<String>,
    /// `NextElapseUSecRealtime`, wall-clock milliseconds; `None` when no calendar elapse is due.
    pub next_elapse_ms: Option<u64>,
    /// `NextElapseUSecMonotonic`, milliseconds since boot, for `OnBootSec=`-style timers.
    pub next_elapse_monotonic_ms: Option<u64>,
    /// `LastTriggerUSec`, wall-clock milliseconds.
    pub last_trigger_ms: Option<u64>,
}

/// A unit run whose stdout and stderr come back to the caller when it ends (`--wait --pipe`),
/// under resource limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSpec {
    pub unit: UnitSpec,
    /// `CPUQuota=` as a percentage of one CPU: 200 is two CPUs.
    #[serde(default)]
    pub cpu_quota_pct: Option<u32>,
    /// `MemoryMax=` in bytes.
    #[serde(default)]
    pub memory_max_bytes: Option<u64>,
}

impl ScopeSpec {
    pub fn new(unit: UnitSpec) -> Self {
        ScopeSpec { unit, cpu_quota_pct: None, memory_max_bytes: None }
    }

    /// The limits this spec sets, by their systemd names.
    pub fn limits(&self) -> Vec<&'static str> {
        [(self.cpu_quota_pct.is_some(), "CPUQuota"), (self.memory_max_bytes.is_some(), "MemoryMax")].into_iter().filter(|(on, _)| *on).map(|(_, n)| n).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeHandle {
    pub unit: UnitHandle,
}

/// The most of a stream a `unit.ran` carries.
pub const TAIL_MAX: usize = 4096;

/// What a run wrote, its last [`TAIL_MAX`] bytes per stream; `None` for a run nobody captured.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RanOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

impl RanOutput {
    pub fn captured(stdout: &[u8], stderr: &[u8]) -> Self {
        RanOutput { stdout_tail: Some(tail(stdout)), stderr_tail: Some(tail(stderr)) }
    }

    pub fn is_empty(&self) -> bool {
        self.stdout_tail.is_none() && self.stderr_tail.is_none()
    }

    /// The tails a `unit.ran` event carries, if any.
    pub fn from_event_kind(k: &EventKind) -> Option<RanOutput> {
        (k.tag == UNIT_RAN).then(|| serde_json::from_str(&k.body).ok()).flatten()
    }
}

/// The last [`TAIL_MAX`] bytes of `b` as text: lossy, starting on a character boundary, and never
/// longer than the bound even after replacement characters.
pub fn tail(b: &[u8]) -> String {
    let mut from = b.len().saturating_sub(TAIL_MAX);
    while from < b.len() && b[from] & 0xC0 == 0x80 {
        from += 1;
    }
    let s = String::from_utf8_lossy(&b[from..]);
    let mut cut = s.len().saturating_sub(TAIL_MAX);
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    s[cut..].to_string()
}

#[derive(Serialize)]
struct RanBody<'a> {
    #[serde(flatten)]
    ran: &'a UnitRan,
    #[serde(flatten)]
    out: &'a RanOutput,
}

impl UnitRan {
    /// The `unit.ran` event with `out`'s tails beside the run; without tails it is
    /// [`UnitRan::event_kind`] exactly, and [`UnitRan::from_event_kind`] reads either.
    pub fn event_kind_with(&self, out: &RanOutput) -> EventKind {
        if out.is_empty() {
            return self.event_kind();
        }
        EventKind::new(UNIT_RAN, &serde_json::to_string(&RanBody { ran: self, out }).unwrap_or_default())
    }
}

fn unsupported(backend: &str, what: &str) -> RuntimeError {
    RuntimeError::refused(format!("{backend} cannot {what}"))
}

/// The unit manager port. The methods past `facts` have refusing defaults so a backend written
/// before them still compiles; each backend here answers what it honestly can.
pub trait Units: Send + Sync {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle>;

    /// `start` under `props`. A backend that cannot honour a property refuses the start naming
    /// it, never starts the unit without it; with no properties this is `start`.
    fn start_with(&self, spec: &UnitSpec, props: &UnitProps) -> Result<UnitHandle> {
        if props.is_empty() {
            return self.start(spec);
        }
        Err(unsupported("this backend", &format!("start task {} under {}", spec.task, props.names().join(", "))))
    }
    fn stop(&self, h: &UnitHandle) -> Result<()>;
    fn restart(&self, h: &UnitHandle) -> Result<()>;
    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts>;

    /// `facts` with the manager's own states and counters.
    fn detail(&self, h: &UnitHandle) -> Result<UnitDetail> {
        Ok(UnitDetail::from_facts(&h.name, &self.facts(h)?))
    }

    /// Every unit, loaded or only installed, whose name matches the fnmatch `glob`, by name.
    fn list(&self, glob: &str) -> Result<Vec<UnitDetail>> {
        Err(unsupported("this backend", &format!("list units matching {glob}")))
    }

    /// Every loaded timer matching `glob`, by name, with its next elapse.
    fn timers(&self, glob: &str) -> Result<Vec<TimerFacts>> {
        Err(unsupported("this backend", &format!("list timers matching {glob}")))
    }

    /// Start an installed unit by its own name (`StartUnit`).
    fn start_installed(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported("this backend", &format!("start installed unit {}", h.name)))
    }

    /// Enable an installed unit's file, then reload the manager.
    fn enable(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported("this backend", &format!("enable {}", h.name)))
    }

    /// Disable an installed unit's file, then reload the manager.
    fn disable(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported("this backend", &format!("disable {}", h.name)))
    }

    /// Forget a unit's failed state (`ResetFailedUnit`); a unit that is not failed is left as is.
    fn reset_failed(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported("this backend", &format!("reset {}", h.name)))
    }

    /// Start a unit under `spec`'s limits whose stdout and stderr come back on its `unit.ran`
    /// ([`UnitCompletions::ended`]), bounded to [`TAIL_MAX`] each.
    fn run_scope(&self, spec: &ScopeSpec) -> Result<ScopeHandle> {
        Err(unsupported("this backend", &format!("run a scope for task {}", spec.unit.task)))
    }
}

/// The completion source: every run that ended since the last call, once each.
pub trait UnitCompletions: Send + Sync {
    fn ran(&self) -> Result<Vec<UnitRan>>;

    /// `ran` with each run's captured output; a backend that captures nothing reports none.
    fn ended(&self) -> Result<Vec<(UnitRan, RanOutput)>> {
        Ok(self.ran()?.into_iter().map(|r| (r, RanOutput::default())).collect())
    }
}

/// fnmatch over a unit name: `*`, `?` and `[...]` classes (with `!` or `^` and ranges), as
/// systemd's unit patterns read them.
pub fn glob_match(pat: &str, name: &str) -> bool {
    fn m(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| m(rest, &n[i..])),
            Some((b'?', rest)) => !n.is_empty() && m(rest, &n[1..]),
            Some((b'[', rest)) => {
                let Some(close) = rest.iter().skip(1).position(|c| *c == b']').map(|i| i + 1) else {
                    return n.first() == Some(&b'[') && m(rest, &n[1..]);
                };
                let Some((&c, n_rest)) = n.split_first() else { return false };
                let (neg, set) = match rest[..close].split_first() {
                    Some((b'!' | b'^', s)) if !s.is_empty() => (true, s),
                    _ => (false, &rest[..close]),
                };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == b'-' {
                        hit |= set[i] <= c && c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == c;
                        i += 1;
                    }
                }
                hit != neg && m(&rest[close + 1..], n_rest)
            }
            Some((c, rest)) => n.first() == Some(c) && m(rest, &n[1..]),
        }
    }
    m(pat.as_bytes(), name.as_bytes())
}

pub const UNIT_START: &str = "unit.start";
pub const UNIT_STOP: &str = "unit.stop";
pub const UNIT_RESTART: &str = "unit.restart";
pub const UNIT_FACTS: &str = "unit.facts";
pub const UNIT_DETAIL: &str = "unit.detail";
pub const UNIT_LIST: &str = "unit.list";
pub const UNIT_TIMERS: &str = "unit.timers";
pub const UNIT_START_INSTALLED: &str = "unit.start_installed";
pub const UNIT_ENABLE: &str = "unit.enable";
pub const UNIT_DISABLE: &str = "unit.disable";
pub const UNIT_RESET_FAILED: &str = "unit.reset_failed";
pub const UNIT_RUN_SCOPE: &str = "unit.run_scope";
/// The effect names a `Units` backend answers, each through [`effect`], that the simulator's
/// shared doubles register.
pub const EFFECTS: [&str; 4] = [UNIT_START, UNIT_STOP, UNIT_RESTART, UNIT_FACTS];
/// The rest [`effect`] answers. Not in [`EFFECTS`] until a task needs one: the simulator's
/// registry names a double for every effect it lists.
pub const EXTENDED_EFFECTS: [&str; 8] = [UNIT_DETAIL, UNIT_LIST, UNIT_TIMERS, UNIT_START_INSTALLED, UNIT_ENABLE, UNIT_DISABLE, UNIT_RESET_FAILED, UNIT_RUN_SCOPE];

/// One unit effect on `units`: `unit.start` takes a [`UnitSpec`] and answers its [`UnitHandle`];
/// `unit.stop` and `unit.restart` take a handle and answer nothing; `unit.facts` takes a handle
/// and answers its [`UnitFacts`]. All as JSON, so a host runner and a simulator double share it.
/// Of the extended set, `unit.list` and `unit.timers` take the glob as a JSON string,
/// `unit.run_scope` a [`ScopeSpec`], and the rest a handle.
pub fn effect(units: &dyn Units, name: &str, input: &str) -> std::result::Result<String, String> {
    fn arg<T: serde::de::DeserializeOwned>(name: &str, input: &str) -> std::result::Result<T, String> {
        serde_json::from_str(input).map_err(|e| format!("{name}: bad input: {e}"))
    }
    fn out<T: Serialize>(v: &T) -> std::result::Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }
    match name {
        UNIT_START => {
            let s = arg::<StartSpec>(name, input)?;
            out(&units.start_with(&s.unit, &s.props).map_err(|e| e.0)?)
        }
        UNIT_STOP => units.stop(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_RESTART => units.restart(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_FACTS => out(&units.facts(&arg(name, input)?).map_err(|e| e.0)?),
        UNIT_DETAIL => out(&units.detail(&arg(name, input)?).map_err(|e| e.0)?),
        UNIT_LIST => out(&units.list(&arg::<String>(name, input)?).map_err(|e| e.0)?),
        UNIT_TIMERS => out(&units.timers(&arg::<String>(name, input)?).map_err(|e| e.0)?),
        UNIT_START_INSTALLED => units.start_installed(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_ENABLE => units.enable(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_DISABLE => units.disable(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_RESET_FAILED => units.reset_failed(&arg(name, input)?).map(|_| String::new()).map_err(|e| e.0),
        UNIT_RUN_SCOPE => out(&units.run_scope(&arg::<ScopeSpec>(name, input)?).map_err(|e| e.0)?),
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

/// Every unit `task` starts under the naming rule, as a [`Units::list`] glob, the task escaped
/// as [`unit_name`] escapes it: `sentinel-call` lists as `spira-run-sentinel_2dcall-*`.
pub fn task_glob(task: &str) -> String {
    format!("{PREFIX}{}-*", escape(task))
}

/// The task whose units under the naming rule are dwarves: spira-runtime's `sentinel-call`.
pub const DWARF_TASK: &str = "sentinel-call";
/// The prefix of a dwarf unit started by its own name, outside the naming rule
/// (`systemd-run --unit=spira-dwarf-<rune>-…`, `dwarf --escape`).
pub const DWARF_PREFIX: &str = "spira-dwarf-";

/// The globs that together list every dwarf unit, by either name.
pub fn dwarf_globs() -> [String; 2] {
    [format!("{DWARF_PREFIX}*{SUFFIX}"), task_glob(DWARF_TASK)]
}

/// Whether `name` is a dwarf unit: started by name under [`DWARF_PREFIX`], or by
/// [`DWARF_TASK`] under the naming rule.
pub fn is_dwarf_unit(name: &str) -> bool {
    (name.starts_with(DWARF_PREFIX) && name.ends_with(SUFFIX)) || parse_unit_name(name).is_some_and(|(t, _)| t == DWARF_TASK)
}

/// Whether a unit in `state` is a live dwarf: the one rule the fleet ceiling
/// (`SPIRA_MAX_LIVE_DWARVES`), dwarves.sh and world.sh's drain count by. A dwarf still
/// deactivating holds its slot until it is gone.
pub fn is_live_dwarf(name: &str, state: UnitState) -> bool {
    is_dwarf_unit(name) && matches!(state, UnitState::Active | UnitState::Activating | UnitState::Deactivating)
}

/// The rune a dwarf [`DWARF_TASK`] started was called for: its key is `<rune>-<at>-<n>`.
/// `None` for a `spira-dwarf-*` unit (its rune is a `RUNE_NAME` prefix, see [`is_dwarf_of`])
/// and for anything that is not a dwarf.
pub fn dwarf_rune(name: &str) -> Option<String> {
    let (task, key) = parse_unit_name(name)?;
    if task != DWARF_TASK {
        return None;
    }
    key.rsplitn(3, '-').nth(2).filter(|r| !r.is_empty()).map(str::to_string)
}

/// Whether `name` is a dwarf of `rune`: a unit [`DWARF_TASK`] started for `rune`, or a unit
/// started by name as `spira-dwarf-<rune_name>-….service` (`rune_name` is the rune's
/// `RUNE_NAME`, the name such units carry; most runes leave it as the rune id).
pub fn is_dwarf_of(name: &str, rune: &str, rune_name: &str) -> bool {
    match dwarf_rune(name) {
        Some(r) => r == rune,
        None => name.strip_prefix(DWARF_PREFIX).is_some_and(|rest| rest.ends_with(SUFFIX) && rest.starts_with(&format!("{rune_name}-"))),
    }
}

/// The live dwarf one `systemctl list-units --no-legend [--plain]` line names, if it names
/// one: the first field is the unit (a leading `●` dropped), the third its `ActiveState`, and
/// liveness is [`is_live_dwarf`].
pub fn live_dwarf_in_list_line(line: &str) -> Option<&str> {
    let mut f = line.split_whitespace().map(|t| t.trim_start_matches('●')).filter(|t| !t.is_empty());
    let unit = f.next()?;
    let active = f.nth(1)?;
    is_live_dwarf(unit, state_of_word(active)).then_some(unit)
}

/// The dwarf units a `/proc/<pid>/cgroup` text runs under, in path order.
pub fn dwarf_units_in_cgroup(cgroup: &str) -> Vec<&str> {
    cgroup.split(|c: char| c == '/' || c.is_whitespace()).filter(|seg| is_dwarf_unit(seg)).collect()
}

/// The state systemd's `ActiveState` word names, as the systemd backend reads it.
pub fn state_of_word(word: &str) -> UnitState {
    match word {
        "active" | "reloading" | "refreshing" => UnitState::Active,
        "activating" => UnitState::Activating,
        "deactivating" => UnitState::Deactivating,
        "failed" => UnitState::Failed,
        _ => UnitState::Inactive,
    }
}

pub fn description(task: &str, key: &str) -> String {
    format!("spira task {task} key {key}")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// The words systemd uses for a run's state, shared by the two backends that are not systemd.

fn result_word(exit: Option<Exit>) -> Option<String> {
    match exit {
        None | Some(Exit::Code(0)) => Some("success".into()),
        Some(Exit::Code(_)) => Some("exit-code".into()),
        Some(Exit::Signal(_)) => Some("signal".into()),
        Some(Exit::Unknown) => None,
    }
}

fn state_words(state: UnitState) -> (Option<String>, Option<String>) {
    let (a, s) = match state {
        UnitState::Active => ("active", "running"),
        UnitState::Activating => ("activating", "start"),
        UnitState::Deactivating => ("deactivating", "stop"),
        UnitState::Inactive => ("inactive", "dead"),
        UnitState::Failed => ("failed", "failed"),
        UnitState::Absent => return (None, None),
    };
    (Some(a.into()), Some(s.into()))
}

fn ended_state(exit: Exit) -> UnitState {
    if exit == Exit::Code(0) {
        UnitState::Inactive
    } else {
        UnitState::Failed
    }
}

/// A run this backend started: running since `started_ms`, or ended with `exit`.
fn run_detail(name: &str, started_ms: u64, pid: Option<u32>, ended: Option<(Exit, u64)>) -> UnitDetail {
    let state = ended.map_or(UnitState::Active, |(e, _)| ended_state(e));
    let (active_state, sub_state) = state_words(state);
    UnitDetail {
        name: name.into(),
        state,
        exit: ended.map(|(e, _)| e),
        since_ms: Some(ended.map_or(started_ms, |(_, at)| at)),
        load_state: Some("loaded".into()),
        unit_file_state: None,
        active_state,
        sub_state,
        result: result_word(ended.map(|(e, _)| e)),
        main_pid: if ended.is_none() { pid } else { None },
        n_restarts: Some(0),
        invocation_id: None,
        exec_main_start_ms: Some(started_ms),
    }
}

// ---------------------------------------------------------------------------------------------
// ProcessUnits: a child process per unit. It answers for its own children only: it has no unit
// files, no installed units, no timers and no cgroup, so it refuses what needs any of those.

const PROCESS: &str = "ProcessUnits";
/// How long a scope's output readers may outlive its process before the tails are taken:
/// a grandchild holding the pipe must not hold the run's end.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Default)]
struct Pipe {
    tail: Vec<u8>,
    closed: bool,
}

type Captured = (Arc<Mutex<Pipe>>, Arc<Mutex<Pipe>>);

fn pump(name: &str, mut r: impl std::io::Read + Send + 'static) -> Result<Arc<Mutex<Pipe>>> {
    let p = Arc::new(Mutex::new(Pipe::default()));
    let q = p.clone();
    std::thread::Builder::new()
        .name(format!("unit-out-{name}"))
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut g = lock(&q);
                g.tail.extend_from_slice(&chunk[..n]);
                let over = g.tail.len().saturating_sub(TAIL_MAX);
                g.tail.drain(..over);
            }
            lock(&q).closed = true;
        })
        .map_err(|e| RuntimeError::new(format!("output reader for {name}: {e}")))?;
    Ok(p)
}

/// The tails, once both readers have closed or the grace has passed.
fn drain(c: &Captured) -> RanOutput {
    let deadline = std::time::Instant::now() + DRAIN_GRACE;
    while !(lock(&c.0).closed && lock(&c.1).closed) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(2)); // clock-safe: waits on the two readers' close, capped at DRAIN_GRACE
    }
    RanOutput::captured(&lock(&c.0).tail, &lock(&c.1).tail)
}

struct Running {
    spec: UnitSpec,
    child: Option<Child>,
    started_ms: u64,
    exit: Option<Exit>,
    ended_ms: Option<u64>,
    pid: Option<u32>,
    /// A scope run: its output is captured, and captured again on restart.
    scope: bool,
    captured: Option<Captured>,
    /// The properties it was started under, as far as a plain child honours them.
    props: UnitProps,
}

/// What a plain child cannot honour: its scheduling and its cgroup.
fn process_refuses(p: &UnitProps) -> Vec<&'static str> {
    [(p.io_class.is_some(), "IOSchedulingClass"), (p.io_weight.is_some(), "IOWeight"), (p.nice.is_some(), "Nice"), (p.cpu_quota_pct.is_some(), "CPUQuota"), (p.memory_max_bytes.is_some(), "MemoryMax")].into_iter().filter(|(on, _)| *on).map(|(_, n)| n).collect()
}

pub struct ProcessUnits {
    clock: Arc<dyn Clock>,
    units: Mutex<BTreeMap<String, Running>>,
    ended: Mutex<VecDeque<(UnitRan, RanOutput)>>,
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

    // batch-job: a unit's process runs for its task's life; UnitProps' own timeout bounds it, not a call cap
    fn spawn(name: &str, spec: &UnitSpec, capture: bool, props: &UnitProps) -> Result<(Child, Option<Captured>)> {
        let (prog, args) = spec.argv.split_first().ok_or_else(|| RuntimeError::refused(format!("unit for task {} has an empty argv", spec.task)))?;
        let mut c = Command::new(prog);
        c.args(args).envs(spec.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null());
        if capture {
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else if let Some(path) = &props.output_append {
            let open = || std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| RuntimeError::new(format!("{name}: output {}: {e}", path.display())));
            c.stdout(open()?).stderr(open()?);
        }
        if let Some(d) = &spec.cwd {
            c.current_dir(d);
        }
        let mut child = c.spawn().map_err(|e| RuntimeError::new(format!("spawn {prog}: {e}")))?;
        if !capture {
            return Ok((child, None));
        }
        let pipes = match (child.stdout.take(), child.stderr.take()) {
            (Some(o), Some(e)) => pump(name, o).and_then(|o| Ok((o, pump(name, e)?))),
            _ => Err(RuntimeError::new(format!("{name}: no output pipes"))),
        };
        match pipes {
            Ok(p) => Ok((child, Some(p))),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    fn end(&self, name: &str, r: &mut Running, exit: Exit) {
        let now = self.clock.now_ms();
        r.child = None;
        r.exit = Some(exit);
        r.ended_ms = Some(now);
        let out = r.captured.take().map(|c| drain(&c)).unwrap_or_default();
        Self::stop_post(name, r);
        lock(&self.ended).push_back((UnitRan { handle: UnitHandle::named(name), exit, duration_ms: now.saturating_sub(r.started_ms) }, out));
    }

    /// `ExecStopPost`: each command runs once the main process has ended, with the unit's own
    /// environment, and is reaped on a thread of its own so the end is not held by it.
    // batch-job: ExecStopPost cleanup runs to its own exit on a reaper thread, never on the caller's path
    fn stop_post(name: &str, r: &Running) {
        for argv in &r.props.exec_stop_post {
            let Some((prog, args)) = argv.split_first() else { continue };
            let mut c = Command::new(prog);
            c.args(args).envs(r.spec.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            if let Some(d) = &r.spec.cwd {
                c.current_dir(d);
            }
            if let Ok(mut child) = c.spawn() {
                let _ = std::thread::Builder::new().name(format!("unit-post-{name}")).spawn(move || child.wait());
            }
        }
    }

    fn reap(&self) {
        let now = self.clock.now_ms();
        let mut units = lock(&self.units);
        for (name, r) in units.iter_mut() {
            let done = match r.child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(s))) => Some(exit_of(s)),
                Some(Err(_)) => Some(Exit::Unknown),
                _ => None,
            };
            if let Some(exit) = done {
                self.end(name, r, exit);
            } else if r.child.is_some() && r.props.runtime_max_ms.is_some_and(|max| now.saturating_sub(r.started_ms) >= max) {
                // `RuntimeMaxSec`: a run past its bound is killed, as systemd stops it.
                self.kill(name, r);
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

    fn launch(&self, spec: &UnitSpec, scope: bool, props: &UnitProps) -> Result<UnitHandle> {
        let refused = process_refuses(props);
        if !refused.is_empty() {
            return Err(unsupported(PROCESS, &format!("honour {} for task {}: a plain process has no scheduler class or cgroup of its own", refused.join(", "), spec.task)));
        }
        let h = UnitHandle::of(&spec.task, &spec.key)?;
        self.reap();
        let mut units = lock(&self.units);
        if units.get(&h.name).is_some_and(|r| r.child.is_some()) {
            return Err(RuntimeError::refused(format!("{} is already running", h.name)));
        }
        let (child, captured) = Self::spawn(&h.name, spec, scope, props)?;
        units.insert(h.name.clone(), Running { spec: spec.clone(), pid: Some(child.id()), child: Some(child), started_ms: self.clock.now_ms(), exit: None, ended_ms: None, scope, captured, props: props.clone() });
        Ok(h)
    }

    fn detail_of(name: &str, r: &Running) -> UnitDetail {
        let ended = if r.child.is_some() { None } else { r.exit.map(|e| (e, r.ended_ms.unwrap_or(r.started_ms))) };
        run_detail(name, r.started_ms, r.pid, ended)
    }
}

impl Units for ProcessUnits {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
        self.launch(spec, false, &UnitProps::default())
    }

    /// The two timeouts, `ExecStopPost` and the output file are honoured (a start of a child is
    /// immediate, so `TimeoutStartSec` always holds); scheduling and cgroup properties refuse.
    fn start_with(&self, spec: &UnitSpec, props: &UnitProps) -> Result<UnitHandle> {
        self.launch(spec, false, props)
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
        let (child, captured) = Self::spawn(&h.name, &r.spec, r.scope, &r.props)?;
        r.pid = Some(child.id());
        r.child = Some(child);
        r.captured = captured;
        r.started_ms = self.clock.now_ms();
        r.ended_ms = None;
        Ok(())
    }

    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
        self.detail(h).map(|d| d.facts())
    }

    /// No unit file and no invocation id: those two are always `None`. `n_restarts` is 0, since
    /// a plain process is never restarted automatically.
    fn detail(&self, h: &UnitHandle) -> Result<UnitDetail> {
        self.reap();
        Ok(lock(&self.units).get(&h.name).map_or_else(|| UnitDetail::absent(&h.name), |r| Self::detail_of(&h.name, r)))
    }

    /// Its own children only: a plain process sees no other unit.
    fn list(&self, glob: &str) -> Result<Vec<UnitDetail>> {
        self.reap();
        Ok(lock(&self.units).iter().filter(|(n, _)| glob_match(glob, n)).map(|(n, r)| Self::detail_of(n, r)).collect())
    }

    fn timers(&self, glob: &str) -> Result<Vec<TimerFacts>> {
        Err(unsupported(PROCESS, &format!("list timers matching {glob}: a plain process has no timers")))
    }

    fn start_installed(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported(PROCESS, &format!("start {}: a plain process has no installed units", h.name)))
    }

    fn enable(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported(PROCESS, &format!("enable {}: a plain process has no unit files", h.name)))
    }

    fn disable(&self, h: &UnitHandle) -> Result<()> {
        Err(unsupported(PROCESS, &format!("disable {}: a plain process has no unit files", h.name)))
    }

    /// A failed run of its own is forgotten, as systemd collects a reset transient unit.
    fn reset_failed(&self, h: &UnitHandle) -> Result<()> {
        self.reap();
        let mut units = lock(&self.units);
        let r = units.get(&h.name).ok_or_else(|| RuntimeError::refused(format!("{} was not started by this backend", h.name)))?;
        if r.child.is_none() && r.exit.is_some_and(|e| ended_state(e) == UnitState::Failed) {
            units.remove(&h.name);
        }
        Ok(())
    }

    /// Captures stdout and stderr through pipes; refuses `CPUQuota` and `MemoryMax`, which a plain
    /// process cannot enforce.
    fn run_scope(&self, spec: &ScopeSpec) -> Result<ScopeHandle> {
        let limits = spec.limits();
        if !limits.is_empty() {
            return Err(unsupported(PROCESS, &format!("enforce {} for task {}: a plain process has no cgroup", limits.join(", "), spec.unit.task)));
        }
        Ok(ScopeHandle { unit: self.launch(&spec.unit, true, &UnitProps::default())? })
    }
}

impl UnitCompletions for ProcessUnits {
    fn ran(&self) -> Result<Vec<UnitRan>> {
        Ok(self.ended()?.into_iter().map(|(r, _)| r).collect())
    }

    fn ended(&self) -> Result<Vec<(UnitRan, RanOutput)>> {
        self.reap();
        Ok(lock(&self.ended).drain(..).collect())
    }
}

// ---------------------------------------------------------------------------------------------
// ScriptedUnits: the simulator's backend. A task's unit runs for its scripted duration on the
// virtual clock and ends with its scripted exit.

struct Live {
    start: u64,
    duration: u64,
    exit: Exit,
    out: RanOutput,
}

#[derive(Default)]
struct Script {
    runs: BTreeMap<String, (u64, Exit)>,
    outputs: BTreeMap<String, RanOutput>,
    live: BTreeMap<String, Live>,
    installed: BTreeMap<String, UnitDetail>,
    timers: BTreeMap<String, TimerFacts>,
    scopes: Vec<ScopeSpec>,
    /// The properties each unit was last started under, by unit name.
    props: BTreeMap<String, UnitProps>,
    /// A run's exit, its start and its end.
    last: BTreeMap<String, (Exit, u64, u64)>,
    calls: Vec<String>,
    ended: VecDeque<(UnitRan, RanOutput)>,
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
        let mut d = UnitDetail::from_facts(name, &facts);
        d.load_state = Some(if facts.state == UnitState::Absent { "not-found" } else { "loaded" }.into());
        (d.active_state, d.sub_state) = state_words(facts.state);
        self.install_detail(d);
    }

    /// An installed unit with every fact the manager would state.
    pub fn install_detail(&self, d: UnitDetail) {
        lock(&self.s).installed.insert(d.name.clone(), d);
    }

    /// A loaded timer.
    pub fn install_timer(&self, t: TimerFacts) {
        lock(&self.s).timers.insert(t.name.clone(), t);
    }

    /// What every scope `task` runs writes, delivered with its `unit.ran`.
    pub fn script_output(&self, task: &str, stdout: &[u8], stderr: &[u8]) {
        lock(&self.s).outputs.insert(task.into(), RanOutput::captured(stdout, stderr));
    }

    /// Every scope started, with the limits it asked for.
    pub fn scopes(&self) -> Vec<ScopeSpec> {
        lock(&self.s).scopes.clone()
    }

    /// The properties unit `name` was last started under; empty for one started without any.
    pub fn props(&self, name: &str) -> UnitProps {
        lock(&self.s).props.get(name).cloned().unwrap_or_default()
    }

    /// Every call that changes a unit, in order, as `<verb> <unit>`.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.s).calls.clone()
    }

    /// When the earliest running unit ends on the virtual clock: the time a completion
    /// source next has something to deliver.
    pub fn next_end_at(&self) -> Option<u64> {
        lock(&self.s).live.values().map(|l| l.start + l.duration).min()
    }

    fn settle(&self, s: &mut Script) {
        let now = self.clock.now_ms();
        let done: Vec<String> = s.live.iter().filter(|(_, l)| l.start + l.duration <= now).map(|(n, _)| n.clone()).collect();
        for n in done {
            if let Some(l) = s.live.remove(&n) {
                s.last.insert(n.clone(), (l.exit, l.start, l.start + l.duration));
                s.ended.push_back((UnitRan { handle: UnitHandle::named(&n), exit: l.exit, duration_ms: l.duration }, l.out));
            }
        }
    }

    fn launch(&self, spec: &UnitSpec, verb: &str, out: impl FnOnce(&Script) -> RanOutput) -> Result<UnitHandle> {
        let h = UnitHandle::of(&spec.task, &spec.key)?;
        let mut s = lock(&self.s);
        self.settle(&mut s);
        let (duration, exit) = *s.runs.get(&spec.task).ok_or_else(|| RuntimeError::refused(format!("no script for task {}", spec.task)))?;
        if s.live.contains_key(&h.name) {
            return Err(RuntimeError::refused(format!("{} is already running", h.name)));
        }
        let out = out(&s);
        s.calls.push(format!("{verb} {}", h.name));
        s.live.insert(h.name.clone(), Live { start: self.clock.now_ms(), duration, exit, out });
        Ok(h)
    }

    fn detail_in(s: &Script, name: &str) -> UnitDetail {
        if let Some(d) = s.installed.get(name) {
            return d.clone();
        }
        if let Some(l) = s.live.get(name) {
            return UnitDetail { unit_file_state: Some("transient".into()), ..run_detail(name, l.start, None, None) };
        }
        match s.last.get(name) {
            Some((exit, start, end)) => UnitDetail { unit_file_state: Some("transient".into()), ..run_detail(name, *start, None, Some((*exit, *end))) },
            None => UnitDetail::absent(name),
        }
    }

    /// An installed unit that is there (not scripted as absent), for `verb`.
    fn installed_mut<'a>(s: &'a mut Script, h: &UnitHandle, verb: &str) -> Result<&'a mut UnitDetail> {
        s.installed.get_mut(&h.name).filter(|d| d.state != UnitState::Absent).ok_or_else(|| RuntimeError::refused(format!("cannot {verb} {}: it is not installed in this script", h.name)))
    }
}

fn set_state(d: &mut UnitDetail, state: UnitState, now: u64) {
    d.state = state;
    d.since_ms = Some(now);
    (d.active_state, d.sub_state) = state_words(state);
}

impl Units for ScriptedUnits {
    fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
        self.start_with(spec, &UnitProps::default())
    }

    /// Every property is accepted and recorded ([`ScriptedUnits::props`]); a bad
    /// `IOSchedulingClass` word refuses, as the manager would.
    fn start_with(&self, spec: &UnitSpec, props: &UnitProps) -> Result<UnitHandle> {
        props.io_class_number()?;
        let h = self.launch(spec, "start", |_| RanOutput::default())?;
        lock(&self.s).props.insert(h.name.clone(), props.clone());
        Ok(h)
    }

    fn stop(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        self.settle(&mut s);
        s.calls.push(format!("stop {}", h.name));
        if let Some(d) = s.installed.get_mut(&h.name) {
            set_state(d, UnitState::Inactive, now);
            d.main_pid = None;
            d.exit = Some(Exit::Signal(15));
        } else if let Some(l) = s.live.remove(&h.name) {
            s.last.insert(h.name.clone(), (Exit::Signal(15), l.start, now));
            s.ended.push_back((UnitRan { handle: h.clone(), exit: Exit::Signal(15), duration_ms: now.saturating_sub(l.start) }, l.out));
        }
        Ok(())
    }

    fn restart(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        s.calls.push(format!("restart {}", h.name));
        match s.installed.get_mut(&h.name) {
            Some(d) => {
                set_state(d, UnitState::Active, now);
                Ok(())
            }
            None => Err(RuntimeError::refused(format!("{} is not installed in this script", h.name))),
        }
    }

    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
        self.detail(h).map(|d| d.facts())
    }

    fn detail(&self, h: &UnitHandle) -> Result<UnitDetail> {
        let mut s = lock(&self.s);
        self.settle(&mut s);
        Ok(Self::detail_in(&s, &h.name))
    }

    fn list(&self, glob: &str) -> Result<Vec<UnitDetail>> {
        let mut s = lock(&self.s);
        self.settle(&mut s);
        let names: BTreeSet<&String> = s.installed.keys().chain(s.live.keys()).chain(s.last.keys()).filter(|n| glob_match(glob, n)).collect();
        Ok(names.into_iter().map(|n| Self::detail_in(&s, n)).filter(|d| d.state != UnitState::Absent).collect())
    }

    fn timers(&self, glob: &str) -> Result<Vec<TimerFacts>> {
        Ok(lock(&self.s).timers.values().filter(|t| glob_match(glob, &t.name)).cloned().collect())
    }

    fn start_installed(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        let d = Self::installed_mut(&mut s, h, "start")?;
        if d.state != UnitState::Active {
            set_state(d, UnitState::Active, now);
        }
        s.calls.push(format!("start {}", h.name));
        Ok(())
    }

    fn enable(&self, h: &UnitHandle) -> Result<()> {
        let mut s = lock(&self.s);
        Self::installed_mut(&mut s, h, "enable")?.unit_file_state = Some("enabled".into());
        s.calls.push(format!("enable {}", h.name));
        Ok(())
    }

    fn disable(&self, h: &UnitHandle) -> Result<()> {
        let mut s = lock(&self.s);
        Self::installed_mut(&mut s, h, "disable")?.unit_file_state = Some("disabled".into());
        s.calls.push(format!("disable {}", h.name));
        Ok(())
    }

    /// An installed failed unit goes inactive with its result and restart count reset; a
    /// failed transient run is collected.
    fn reset_failed(&self, h: &UnitHandle) -> Result<()> {
        let now = self.clock.now_ms();
        let mut s = lock(&self.s);
        self.settle(&mut s);
        if let Some(d) = s.installed.get_mut(&h.name).filter(|d| d.state != UnitState::Absent) {
            if d.state == UnitState::Failed {
                set_state(d, UnitState::Inactive, now);
                d.result = Some("success".into());
                d.n_restarts = Some(0);
            }
        } else if let Some((exit, _, _)) = s.last.get(&h.name) {
            if ended_state(*exit) == UnitState::Failed {
                s.last.remove(&h.name);
            }
        } else if !s.live.contains_key(&h.name) {
            return Err(RuntimeError::refused(format!("cannot reset {}: this script knows no such unit", h.name)));
        }
        s.calls.push(format!("reset-failed {}", h.name));
        Ok(())
    }

    /// Any limits are accepted and recorded ([`ScriptedUnits::scopes`]); the run ends with the
    /// output [`ScriptedUnits::script_output`] gave its task.
    fn run_scope(&self, spec: &ScopeSpec) -> Result<ScopeHandle> {
        let unit = self.launch(&spec.unit, "run-scope", |s| s.outputs.get(&spec.unit.task).cloned().unwrap_or_else(|| RanOutput::captured(b"", b"")))?;
        lock(&self.s).scopes.push(spec.clone());
        Ok(ScopeHandle { unit })
    }
}

impl UnitCompletions for ScriptedUnits {
    fn ran(&self) -> Result<Vec<UnitRan>> {
        Ok(self.ended()?.into_iter().map(|(r, _)| r).collect())
    }

    fn ended(&self) -> Result<Vec<(UnitRan, RanOutput)>> {
        let mut s = lock(&self.s);
        self.settle(&mut s);
        Ok(s.ended.drain(..).collect())
    }
}

// ---------------------------------------------------------------------------------------------
// SystemdUnits: the user manager over D-Bus, with unit-run's connection and signal tracker.

#[cfg(target_os = "linux")]
pub use systemd::{encode_args, encode_call_body, scope_args, start_transient_args, start_transient_args_with, Arg, Bus, SystemdUnits};

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
    const TIMER_IFACE: &str = "org.freedesktop.systemd1.Timer";
    /// The manager's job ends, which the user manager sends once subscribed.
    pub const JOB_REMOVED_RULE: &str = "type='signal',sender='org.freedesktop.systemd1',interface='org.freedesktop.systemd1.Manager',member='JobRemoved',path='/org/freedesktop/systemd1'";

    /// A D-Bus argument, enough of the type system for StartTransientUnit.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Arg {
        S(String),
        B(bool),
        U32(u32),
        I32(i32),
        T(u64),
        Arr(&'static str, Vec<Arg>),
        Struct(Vec<Arg>),
        Var(Box<Arg>),
    }

    impl Arg {
        fn sig(&self) -> String {
            match self {
                Arg::S(_) => "s".into(),
                Arg::B(_) => "b".into(),
                Arg::U32(_) => "u".into(),
                Arg::I32(_) => "i".into(),
                Arg::T(_) => "t".into(),
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
            Arg::U32(v) => put_u32(buf, *v),
            Arg::I32(v) => put_u32(buf, *v as u32),
            Arg::T(v) => {
                align(buf, 8);
                buf.extend_from_slice(&v.to_le_bytes());
            }
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

    /// `args` marshalled: their signature and their body bytes.
    pub fn encode_args(args: &[Arg]) -> (String, Vec<u8>) {
        let mut body = Vec::new();
        for a in args {
            put(&mut body, a);
        }
        (args.iter().map(Arg::sig).collect(), body)
    }

    /// A METHOD_CALL carrying `args`: the general form of unit-run's one-string `encode_call`.
    pub fn encode_call_body(serial: u32, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> Vec<u8> {
        let (sig, body) = encode_args(args);
        unit_run::dbus::encode_call_with(serial, dest, path, iface, member, &sig, &body)
    }

    fn prop(name: &str, v: Arg) -> Arg {
        Arg::Struct(vec![Arg::S(name.into()), Arg::Var(Box::new(v))])
    }

    fn strings(v: &[String]) -> Arg {
        Arg::Arr("s", v.iter().cloned().map(Arg::S).collect())
    }

    /// `StartTransientUnit(name, "fail", properties, [])` for `spec` under `props`: each
    /// [`UnitProps`] field as the transient-unit property `systemd-run --property=` set.
    pub fn start_transient_args_with(spec: &UnitSpec, props: &UnitProps) -> Result<Vec<Arg>> {
        let mut args = start_transient_args(spec)?;
        let io_class = props.io_class_number()?;
        if let Some(Arg::Arr(_, p)) = args.get_mut(2) {
            let usec = |ms: u64| Arg::T(ms.saturating_mul(1000));
            if let Some(ms) = props.start_timeout_ms {
                p.push(prop("TimeoutStartUSec", usec(ms)));
            }
            if let Some(ms) = props.runtime_max_ms {
                p.push(prop("RuntimeMaxUSec", usec(ms)));
            }
            if let Some(c) = io_class {
                p.push(prop("IOSchedulingClass", Arg::I32(c)));
            }
            if let Some(w) = props.io_weight {
                p.push(prop("IOWeight", Arg::T(w)));
            }
            if let Some(n) = props.nice {
                p.push(prop("Nice", Arg::I32(n)));
            }
            if let Some(pct) = props.cpu_quota_pct {
                p.push(prop("CPUQuotaPerSecUSec", Arg::T(u64::from(pct) * 10_000)));
            }
            if let Some(max) = props.memory_max_bytes {
                p.push(prop("MemoryMax", Arg::T(max)));
            }
            let post: Vec<Arg> = props.exec_stop_post.iter().filter_map(|argv| argv.first().map(|prog| Arg::Struct(vec![Arg::S(prog.clone()), strings(argv), Arg::B(false)]))).collect();
            if !post.is_empty() {
                p.push(prop("ExecStopPost", Arg::Arr("(sasb)", post)));
            }
            if let Some(path) = &props.output_append {
                p.push(prop("StandardOutputFileToAppend", Arg::S(path.display().to_string())));
                p.push(prop("StandardErrorFileToAppend", Arg::S(path.display().to_string())));
            }
        }
        Ok(args)
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

    /// `StartTransientUnit` for a scope run: [`start_transient_args`] plus its limits and the two
    /// files its stdout and stderr are written to, read back when it ends.
    pub fn scope_args(spec: &ScopeSpec, stdout: &std::path::Path, stderr: &std::path::Path) -> Result<Vec<Arg>> {
        let mut args = start_transient_args(&spec.unit)?;
        if let Some(Arg::Arr(_, props)) = args.get_mut(2) {
            props.push(prop("StandardOutputFile", Arg::S(stdout.display().to_string())));
            props.push(prop("StandardErrorFile", Arg::S(stderr.display().to_string())));
            if let Some(pct) = spec.cpu_quota_pct {
                props.push(prop("CPUQuotaPerSecUSec", Arg::T(u64::from(pct) * 10_000)));
            }
            if let Some(max) = spec.memory_max_bytes {
                props.push(prop("MemoryMax", Arg::T(max)));
            }
        }
        Ok(args)
    }

    pub trait Bus: Send {
        fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String>;
        fn next_signal(&mut self, timeout: Duration) -> std::result::Result<Option<Msg>, String>;
    }

    impl Bus for Conn {
        fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String> {
            let (sig, body) = encode_args(args);
            Conn::call_body(self, dest, path, iface, member, &sig, &body)
        }

        fn next_signal(&mut self, timeout: Duration) -> std::result::Result<Option<Msg>, String> {
            Conn::next_signal(self, timeout)
        }
    }

    /// Units whose start job ends without a run: the job's end is the only end they will have.
    const NEVER_RAN: [&str; 4] = ["dependency", "assert", "unsupported", "invalid"];
    /// Job results that may or may not follow a run; a unit seen live or just closed has its end
    /// from its properties already.
    const MAYBE_RAN: [&str; 2] = ["failed", "timeout"];
    /// How many just-closed units are remembered against a late `JobRemoved`.
    const CLOSED_CAP: usize = 256;

    pub struct SystemdUnits<B: Bus> {
        bus: Mutex<B>,
        clock: Arc<dyn Clock>,
        tracker: Mutex<Tracker>,
        capture_dir: Option<PathBuf>,
        /// Unit object paths seen live and not yet closed, and those closed most recently.
        seen: Mutex<(BTreeSet<String>, VecDeque<String>)>,
        /// A scope run's capture files, by unit name.
        scopes: Mutex<BTreeMap<String, (PathBuf, PathBuf)>>,
    }

    fn fault(e: String) -> RuntimeError {
        RuntimeError::new(format!("systemd: {e}"))
    }

    fn s(v: &str) -> Arg {
        Arg::S(v.into())
    }

    fn names(v: &[&str]) -> Arg {
        Arg::Arr("s", v.iter().map(|x| s(x)).collect())
    }

    /// The interface a unit's type-specific properties live on: `x.service` -> `...Service`.
    fn type_iface(name: &str) -> String {
        let ty = name.rsplit_once('.').map_or("", |(_, t)| t);
        let mut c = ty.chars();
        let cap: String = c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default();
        format!("org.freedesktop.systemd1.{cap}")
    }

    /// The last [`TAIL_MAX`] bytes of a file as a tail; empty for a file that is not there.
    fn read_tail(p: &std::path::Path) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(p) else { return String::new() };
        let len = f.metadata().map_or(0, |m| m.len());
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL_MAX as u64)));
        let mut b = Vec::new();
        let _ = f.take(TAIL_MAX as u64).read_to_end(&mut b);
        tail(&b)
    }

    impl SystemdUnits<Conn> {
        /// The user manager on the session bus.
        pub fn connect(clock: Arc<dyn Clock>) -> Result<Self> {
            let path = unit_run::dbus::bus_path().map_err(fault)?;
            SystemdUnits::new(Conn::connect(&path).map_err(fault)?, clock)
        }
    }

    impl<B: Bus> SystemdUnits<B> {
        /// Subscribes to unit signals and job ends, so a run ending after this is a `unit.ran`.
        pub fn new(mut bus: B, clock: Arc<dyn Clock>) -> Result<Self> {
            for rule in MATCH_RULES.iter().chain([JOB_REMOVED_RULE].iter()) {
                bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", &[s(rule)]).map_err(fault)?;
            }
            bus.call(MANAGER, ROOT, MANAGER_IFACE, "Subscribe", &[]).map_err(fault)?;
            let since_us = clock.now_ms().saturating_mul(1000);
            let capture_dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("spira-units"));
            Ok(SystemdUnits { bus: Mutex::new(bus), clock, tracker: Mutex::new(Tracker::new(since_us)), capture_dir, seen: Mutex::default(), scopes: Mutex::default() })
        }

        /// Where a scope run's stdout and stderr files go; `$XDG_RUNTIME_DIR/spira-units` unless
        /// set here. With none, `run_scope` is refused.
        pub fn with_capture_dir(mut self, dir: Option<PathBuf>) -> Self {
            self.capture_dir = dir;
            self
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

        fn manager(&self, member: &str, args: &[Arg]) -> Result<Vec<Val>> {
            lock(&self.bus).call(MANAGER, ROOT, MANAGER_IFACE, member, args).map_err(fault)
        }

        /// `ListUnitsByPatterns` rows matching `glob`: (name, active state, object path).
        fn loaded(bus: &mut B, glob: &str) -> Result<Vec<(String, String, String)>> {
            let r = bus.call(MANAGER, ROOT, MANAGER_IFACE, "ListUnitsByPatterns", &[names(&[]), names(&[glob])]).map_err(fault)?;
            let Some(Val::Arr(rows)) = r.first() else { return Err(fault("ListUnitsByPatterns: unexpected reply".into())) };
            Ok(rows
                .iter()
                .filter_map(|row| {
                    let f = row.fields()?;
                    Some((f.first()?.as_str()?.to_string(), f.get(3)?.as_str()?.to_string(), f.get(6)?.as_str()?.to_string()))
                })
                .collect())
        }

        /// The tails of a scope run's capture files, which go once read.
        fn output_of(&self, name: &str) -> RanOutput {
            let Some((o, e)) = lock(&self.scopes).remove(name) else { return RanOutput::default() };
            let out = RanOutput { stdout_tail: Some(read_tail(&o)), stderr_tail: Some(read_tail(&e)) };
            let _ = std::fs::remove_file(&o);
            let _ = std::fs::remove_file(&e);
            out
        }

        /// A `JobRemoved` that is a run's only end: a start that never ran, or a failure of a unit
        /// not seen live and not just closed.
        fn job_end(&self, m: &Msg) -> Option<UnitRan> {
            let (unit, result) = (m.body.get(2)?.as_str()?, m.body.get(3)?.as_str()?);
            if !NEVER_RAN.contains(&result) {
                if !MAYBE_RAN.contains(&result) {
                    return None;
                }
                let path = unit_run::dbus::unit_object_path(unit);
                let mut seen = lock(&self.seen);
                if seen.0.contains(&path) {
                    return None;
                }
                if let Some(i) = seen.1.iter().position(|p| *p == path) {
                    seen.1.remove(i);
                    return None;
                }
            }
            Some(UnitRan { handle: UnitHandle::named(unit), exit: Exit::Unknown, duration_ms: 0 })
        }

        fn mark(&self, path: &str, live: bool) {
            let mut seen = lock(&self.seen);
            if live {
                seen.0.insert(path.into());
                seen.1.retain(|p| p != path);
            } else {
                seen.0.remove(path);
                seen.1.push_back(path.into());
                while seen.1.len() > CLOSED_CAP {
                    seen.1.pop_front();
                }
            }
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
        let state = find(unit, "ActiveState").and_then(Val::as_str).map_or(UnitState::Inactive, super::state_of_word);
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

    fn usec_ms(v: Option<&Val>) -> Option<u64> {
        v.and_then(Val::as_u64).filter(|u| *u != 0 && *u != u64::MAX).map(|u| u / 1000)
    }

    /// The detail a unit's `Unit` and type-specific property sets state.
    pub(super) fn detail_from(name: &str, unit: &[(String, Val)], ty: &[(String, Val)]) -> UnitDetail {
        let f = facts_from(unit, ty);
        let text = |d: &[(String, Val)], k: &str| find(d, k).and_then(Val::as_str).filter(|s| !s.is_empty()).map(String::from);
        UnitDetail {
            name: name.into(),
            state: f.state,
            exit: f.exit,
            since_ms: f.since_ms,
            load_state: text(unit, "LoadState"),
            unit_file_state: text(unit, "UnitFileState"),
            active_state: text(unit, "ActiveState"),
            sub_state: text(unit, "SubState"),
            result: text(ty, "Result"),
            main_pid: f.main_pid,
            n_restarts: find(ty, "NRestarts").and_then(Val::as_u64).and_then(|n| u32::try_from(n).ok()),
            invocation_id: find(unit, "InvocationID").and_then(Val::as_bytes).filter(|b| b.iter().any(|x| *x != 0)).map(|b| b.iter().map(|x| format!("{x:02x}")).collect()),
            exec_main_start_ms: usec_ms(find(ty, "ExecMainStartTimestamp")),
        }
    }

    fn live_invocation(d: &[(String, Val)]) -> bool {
        find(d, "InvocationID").and_then(Val::as_bytes).is_some_and(|b| b.iter().any(|x| *x != 0))
    }

    impl<B: Bus> Units for SystemdUnits<B> {
        fn start(&self, spec: &UnitSpec) -> Result<UnitHandle> {
            self.start_with(spec, &UnitProps::default())
        }

        fn start_with(&self, spec: &UnitSpec, props: &UnitProps) -> Result<UnitHandle> {
            let args = start_transient_args_with(spec, props)?;
            self.manager("StartTransientUnit", &args)?;
            UnitHandle::of(&spec.task, &spec.key)
        }

        fn stop(&self, h: &UnitHandle) -> Result<()> {
            self.unit_call(h, "Stop")
        }

        fn restart(&self, h: &UnitHandle) -> Result<()> {
            self.unit_call(h, "Restart")
        }

        fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
            self.detail(h).map(|d| d.facts())
        }

        fn detail(&self, h: &UnitHandle) -> Result<UnitDetail> {
            let mut bus = lock(&self.bus);
            let path = Self::unit_path(&mut bus, &h.name)?;
            let unit = get_all(&mut *bus, &path, UNIT_IFACE);
            let ty = get_all(&mut *bus, &path, &type_iface(&h.name));
            Ok(detail_from(&h.name, &unit, &ty))
        }

        /// Loaded units read back one by one, then unit files the manager has not loaded, which
        /// are inactive by definition.
        fn list(&self, glob: &str) -> Result<Vec<UnitDetail>> {
            let mut bus = lock(&self.bus);
            let mut out = BTreeMap::new();
            for (name, _, path) in Self::loaded(&mut bus, glob)? {
                let unit = get_all(&mut *bus, &path, UNIT_IFACE);
                let ty = get_all(&mut *bus, &path, &type_iface(&name));
                out.insert(name.clone(), detail_from(&name, &unit, &ty));
            }
            let files = bus.call(MANAGER, ROOT, MANAGER_IFACE, "ListUnitFilesByPatterns", &[names(&[]), names(&[glob])]).map_err(fault)?;
            let Some(Val::Arr(rows)) = files.first() else { return Err(fault("ListUnitFilesByPatterns: unexpected reply".into())) };
            for row in rows {
                let Some([path, state]) = row.fields() else { continue };
                let (Some(path), Some(state)) = (path.as_str(), state.as_str()) else { continue };
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                out.entry(name.clone()).or_insert_with(|| UnitDetail {
                    name,
                    state: UnitState::Inactive,
                    unit_file_state: Some(state.into()),
                    active_state: Some("inactive".into()),
                    sub_state: Some("dead".into()),
                    ..UnitDetail::default()
                });
            }
            Ok(out.into_values().collect())
        }

        /// Loaded timers only, as `list-timers --all` shows them.
        fn timers(&self, glob: &str) -> Result<Vec<TimerFacts>> {
            let mut bus = lock(&self.bus);
            let mut out = Vec::new();
            for (name, active, path) in Self::loaded(&mut bus, glob)? {
                if !name.ends_with(".timer") {
                    continue;
                }
                let t = get_all(&mut *bus, &path, TIMER_IFACE);
                out.push(TimerFacts {
                    unit: find(&t, "Unit").and_then(Val::as_str).filter(|u| !u.is_empty()).map(String::from),
                    active_state: Some(active),
                    next_elapse_ms: usec_ms(find(&t, "NextElapseUSecRealtime")),
                    next_elapse_monotonic_ms: usec_ms(find(&t, "NextElapseUSecMonotonic")),
                    last_trigger_ms: usec_ms(find(&t, "LastTriggerUSec")),
                    name,
                });
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        }

        fn start_installed(&self, h: &UnitHandle) -> Result<()> {
            self.manager("StartUnit", &[s(&h.name), s("replace")]).map(|_| ())
        }

        fn enable(&self, h: &UnitHandle) -> Result<()> {
            self.manager("EnableUnitFiles", &[names(&[&h.name]), Arg::B(false), Arg::B(false)])?;
            self.manager("Reload", &[]).map(|_| ())
        }

        fn disable(&self, h: &UnitHandle) -> Result<()> {
            self.manager("DisableUnitFiles", &[names(&[&h.name]), Arg::B(false)])?;
            self.manager("Reload", &[]).map(|_| ())
        }

        fn reset_failed(&self, h: &UnitHandle) -> Result<()> {
            self.manager("ResetFailedUnit", &[s(&h.name)]).map(|_| ())
        }

        /// A transient service whose stdout and stderr go to two fresh files in the capture
        /// directory; its `unit.ran` carries their tails and removes them.
        fn run_scope(&self, spec: &ScopeSpec) -> Result<ScopeHandle> {
            let h = UnitHandle::of(&spec.unit.task, &spec.unit.key)?;
            let dir = self.capture_dir.as_ref().ok_or_else(|| RuntimeError::refused(format!("cannot capture {}: neither XDG_RUNTIME_DIR nor a capture directory is set", h.name)))?;
            std::fs::create_dir_all(dir).map_err(|e| RuntimeError::new(format!("{}: {e}", dir.display())))?;
            let (o, e) = (dir.join(format!("{}.stdout", h.name)), dir.join(format!("{}.stderr", h.name)));
            for p in [&o, &e] {
                std::fs::File::create(p).map_err(|err| RuntimeError::new(format!("{}: {err}", p.display())))?;
            }
            // Registered before the start, so an end read on another thread finds its files.
            lock(&self.scopes).insert(h.name.clone(), (o.clone(), e.clone()));
            let started = scope_args(spec, &o, &e).and_then(|args| self.manager("StartTransientUnit", &args));
            if let Err(err) = started {
                lock(&self.scopes).remove(&h.name);
                let _ = std::fs::remove_file(&o);
                let _ = std::fs::remove_file(&e);
                return Err(err);
            }
            Ok(ScopeHandle { unit: h })
        }
    }

    /// A PropertiesChanged signal's object path, interface and changed properties.
    type Changed = (String, String, Vec<(String, Val)>);

    fn changed(m: &Msg) -> Option<Changed> {
        if m.kind != SIGNAL || m.member.as_deref() != Some("PropertiesChanged") {
            return None;
        }
        Some((m.path.clone()?, m.body.first()?.as_str()?.to_string(), m.body.get(1)?.dict()))
    }

    impl<B: Bus> UnitCompletions for SystemdUnits<B> {
        fn ran(&self) -> Result<Vec<UnitRan>> {
            Ok(self.ended()?.into_iter().map(|(r, _)| r).collect())
        }

        /// Every unit's end, a task's or an installed one's: a run closed by its properties, or a
        /// start job that ended without one.
        fn ended(&self) -> Result<Vec<(UnitRan, RanOutput)>> {
            let mut bus = lock(&self.bus);
            let mut t = lock(&self.tracker);
            let mut out = Vec::new();
            while let Some(m) = bus.next_signal(Duration::ZERO).map_err(fault)? {
                match m.member.as_deref() {
                    Some("NameOwnerChanged") => {
                        bus.call(MANAGER, ROOT, MANAGER_IFACE, "Subscribe", &[]).map_err(fault)?;
                        continue;
                    }
                    Some("JobRemoved") => {
                        if let Some(r) = self.job_end(&m) {
                            let o = self.output_of(&r.handle.name);
                            out.push((r, o));
                        }
                        continue;
                    }
                    _ => {}
                }
                let Some((path, iface, props)) = changed(&m) else { continue };
                let close = match t.apply(&path, &iface, &props, true) {
                    Next::Nothing => continue,
                    Next::Snapshot(_) => false,
                    Next::Close(_) => true,
                };
                self.mark(&path, !close);
                let unit = get_all(&mut *bus, &path, UNIT_IFACE);
                if live_invocation(&unit) {
                    let ty = unit_run::watch::unit_name(&path).map_or_else(|| SERVICE_IFACE.to_string(), |n| type_iface(&n));
                    t.apply(&path, UNIT_IFACE, &unit, false);
                    t.apply(&path, &ty, &get_all(&mut *bus, &path, &ty), false);
                }
                if !close {
                    continue;
                }
                let Some(r) = t.finish(&path, self.clock.now_ms().saturating_mul(1000)) else { continue };
                let exit = match (r.exit_code.as_deref(), r.exit_status.and_then(|s| i32::try_from(s).ok())) {
                    (Some("exited"), Some(st)) => Exit::Code(st),
                    (Some("killed" | "dumped"), Some(st)) => Exit::Signal(st),
                    _ if r.result == "success" => Exit::Code(0),
                    _ => Exit::Unknown,
                };
                let duration_ms = r.started_us.map_or(0, |s| r.ended_us.saturating_sub(s) / 1000);
                let o = self.output_of(&r.unit);
                out.push((UnitRan { handle: UnitHandle::named(&r.unit), exit, duration_ms }, o));
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Selection by config: the double other crates' tests run against.

/// The config key that picks the backend, registered in `spira/conf.d/SPIRA_UNITS` as required
/// (the shipped default is `systemd`). A binary wired to [`from_env`] resolves it through the
/// config door; a caller holding the value hands it to [`from_value`]. Values: `scripted`,
/// `process`, `systemd`; any other is refused. A test that hands [`from_value`] no value gets the
/// platform's own (systemd on Linux, child processes elsewhere).
pub const UNITS_KEY: &str = "SPIRA_UNITS";

/// The backend a process runs against, with both faces of it.
pub enum Backend {
    Scripted(Arc<ScriptedUnits>),
    Process(Arc<ProcessUnits>),
    #[cfg(target_os = "linux")]
    Systemd(Arc<SystemdUnits<unit_run::dbus::Conn>>),
}

impl Backend {
    pub fn units(&self) -> Arc<dyn Units> {
        match self {
            Backend::Scripted(u) => u.clone(),
            Backend::Process(u) => u.clone(),
            #[cfg(target_os = "linux")]
            Backend::Systemd(u) => u.clone(),
        }
    }

    pub fn completions(&self) -> Arc<dyn UnitCompletions> {
        match self {
            Backend::Scripted(u) => u.clone(),
            Backend::Process(u) => u.clone(),
            #[cfg(target_os = "linux")]
            Backend::Systemd(u) => u.clone(),
        }
    }

    /// The scripted backend, for a test to script.
    pub fn scripted(&self) -> Option<Arc<ScriptedUnits>> {
        match self {
            Backend::Scripted(u) => Some(u.clone()),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Backend::Scripted(_) => "scripted",
            Backend::Process(_) => "process",
            #[cfg(target_os = "linux")]
            Backend::Systemd(_) => "systemd",
        }
    }
}

/// The backend [`UNITS_KEY`] names in this process's environment.
pub fn from_env(clock: Arc<dyn Clock>) -> Result<Backend> {
    from_value(clock, std::env::var(UNITS_KEY).ok().as_deref())
}

/// The backend `value` names; see [`UNITS_KEY`]. An unknown value is refused, naming it.
pub fn from_value(clock: Arc<dyn Clock>, value: Option<&str>) -> Result<Backend> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some("scripted") => Ok(Backend::Scripted(Arc::new(ScriptedUnits::new(clock)))),
        Some("process") => Ok(Backend::Process(Arc::new(ProcessUnits::new(clock)))),
        #[cfg(target_os = "linux")]
        Some("systemd") | None => Ok(Backend::Systemd(Arc::new(SystemdUnits::connect(clock)?))),
        #[cfg(not(target_os = "linux"))]
        Some("systemd") => Err(RuntimeError::refused(format!("{UNITS_KEY}=systemd: systemd is Linux only"))),
        #[cfg(not(target_os = "linux"))]
        None => Ok(Backend::Process(Arc::new(ProcessUnits::new(clock)))),
        Some(other) => Err(RuntimeError::refused(format!("{UNITS_KEY}={other}: not one of scripted, process, systemd"))),
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
    fn a_task_glob_lists_exactly_the_units_the_task_starts() {
        let g = task_glob("sentinel-call");
        assert_eq!(g, "spira-run-sentinel_2dcall-*");
        assert!(glob_match(&g, &unit_name("sentinel-call", "builder-1-0").unwrap()));
        assert!(!glob_match(&g, &unit_name("sentinel-pass", "k").unwrap()));
    }

    #[test]
    fn sp_li4zys_a_dwarf_names_its_rune_its_line_and_its_cgroup() {
        let own = "spira-run-sentinel_2dcall-czar_2descape_2d1794009721297_2d1.service";
        assert_eq!(dwarf_rune(own).as_deref(), Some("czar-escape"));
        assert_eq!(dwarf_rune("spira-dwarf-builder-17.service"), None);
        assert_eq!(dwarf_rune("spira-run-sentinel_2dpass-builder_2d1_2d2.service"), None, "not sentinel-call's");
        assert!(is_dwarf_of(own, "czar-escape", "czar-escape") && !is_dwarf_of(own, "czar", "czar"));
        assert!(is_dwarf_of("spira-dwarf-brokk-17.service", "builder", "brokk") && !is_dwarf_of("spira-dwarf-brokk-17.service", "builder", "builder"));
        assert_eq!(live_dwarf_in_list_line(&format!("● {own} loaded active running dwarf")), Some(own));
        assert_eq!(live_dwarf_in_list_line(&format!("{own} loaded failed failed dwarf")), None);
        assert_eq!(live_dwarf_in_list_line("spira-run-sentinel_2dpass-landing.service loaded active running x"), None);
        let cg = format!("0::/user.slice/user-1000.slice/user@1000.service/app.slice/{own}\n");
        assert_eq!(dwarf_units_in_cgroup(&cg), vec![own]);
        assert!(dwarf_units_in_cgroup("0::/app.slice/spira-run-sentinel_2dpass-landing.service\n").is_empty());
    }

    #[test]
    fn a_dwarf_is_a_unit_of_either_name_and_live_while_it_holds_a_slot() {
        let own = unit_name(DWARF_TASK, "builder-1-0").unwrap();
        for n in [own.as_str(), "spira-dwarf-builder-17.service", "spira-dwarf-czar-escape-1-prod.service"] {
            assert!(is_dwarf_unit(n) && dwarf_globs().iter().any(|g| glob_match(g, n)), "{n}");
        }
        for n in ["spira-dwarf-x.scope", "spira-run-czar_2dpass-r7.service", "spira-runtime.service"] {
            assert!(!is_dwarf_unit(n), "{n}");
        }
        let live: Vec<_> = ["active", "reloading", "activating", "deactivating", "failed", "inactive"].iter().map(|w| is_live_dwarf(&own, state_of_word(w))).collect();
        assert_eq!(live, [true, true, true, true, false, false]);
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
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: polls a real child's exit, capped at 2.5 s
        }
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].handle.clone(), got[0].exit), (h.clone(), Exit::Code(3)));
        assert!(u.ran().unwrap().is_empty());
        let f = u.facts(&h).unwrap();
        assert_eq!((f.state, f.exit, f.main_pid), (UnitState::Failed, Some(Exit::Code(3)), None));
        assert_eq!(u.facts(&UnitHandle::named("other.service")).unwrap().state, UnitState::Absent);
    }

    /// A plain child honours `RuntimeMaxSec` (killed once past it on the backend's clock),
    /// `ExecStopPost` (run after the end) and the output file; scheduling and cgroup
    /// properties refuse the start rather than run without them.
    #[test]
    fn a_process_unit_applies_what_a_child_can_and_refuses_the_rest() {
        let dir = testkit::TempDir::new("units-props");
        let (log, post) = (dir.path().join("out.log"), dir.path().join("post"));
        let clock = VirtualClock::new(0);
        let u = ProcessUnits::new(Arc::new(clock.clone()));
        let props = UnitProps { runtime_max_ms: Some(1_000), start_timeout_ms: Some(5_000), exec_stop_post: vec![vec!["touch".into(), post.display().to_string()]], output_append: Some(log.clone()), ..UnitProps::default() };
        let h = u.start_with(&spec("t", "k", &["sh", "-c", "echo hello; sleep 30"]), &props).unwrap();
        for _ in 0..500 {
            if std::fs::read_to_string(&log).unwrap_or_default().contains("hello") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: waits for the real child's first line, capped at 2.5 s
        }
        assert!(u.ran().unwrap().is_empty(), "within RuntimeMaxSec the run goes on");
        clock.advance(1_000);
        let mut got = vec![];
        for _ in 0..500 {
            got.extend(u.ran().unwrap());
            if !got.is_empty() && post.exists() && std::fs::read_to_string(&log).unwrap_or_default().contains("hello") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: polls a real child's end and its stop-post, capped at 2.5 s
        }
        assert_eq!(got.iter().map(|r| (r.handle.clone(), r.exit)).collect::<Vec<_>>(), [(h, Exit::Signal(9))], "past RuntimeMaxSec the run is killed");
        assert!(post.exists(), "ExecStopPost ran after the end");
        assert!(std::fs::read_to_string(&log).unwrap().contains("hello"), "stdout appended to the output file");
        for refused in [UnitProps { nice: Some(5), ..UnitProps::default() }, UnitProps { io_class: Some("idle".into()), ..UnitProps::default() }, UnitProps { cpu_quota_pct: Some(100), ..UnitProps::default() }, UnitProps { memory_max_bytes: Some(1), ..UnitProps::default() }] {
            let names = refused.names();
            let err = u.start_with(&spec("t", "r", &["true"]), &refused).unwrap_err();
            assert!(err.is_refused() && err.0.contains(names[0]), "{err:?}");
        }
        assert_eq!(u.facts(&UnitHandle::of("t", "r").unwrap()).unwrap().state, UnitState::Absent, "a refused start starts nothing");
    }

    /// `unit.start` reads a plain `UnitSpec` or one with `props`; the scripted backend records
    /// what it was started under.
    #[test]
    fn the_start_effect_carries_props_and_a_bare_spec_still_starts() {
        let u = ScriptedUnits::new(Arc::new(VirtualClock::new(0)));
        u.script("t", 10, Exit::Code(0));
        let bare = serde_json::to_string(&spec("t", "a", &["x"])).unwrap();
        effect(&u, UNIT_START, &bare).unwrap();
        assert!(u.props(&unit_name("t", "a").unwrap()).is_empty());
        let props = UnitProps { io_class: Some("idle".into()), io_weight: Some(10), ..UnitProps::default() };
        let with = serde_json::to_string(&StartSpec { unit: spec("t", "b", &["x"]), props: props.clone() }).unwrap();
        effect(&u, UNIT_START, &with).unwrap();
        assert_eq!(u.props(&unit_name("t", "b").unwrap()), props);
        assert_eq!(serde_json::to_string(&StartSpec { unit: spec("t", "a", &["x"]), props: UnitProps::default() }).unwrap(), bare, "a start with no props is the bare spec");
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

    #[test]
    fn a_glob_matches_as_fnmatch_does() {
        for (p, n) in [("spira-*", "spira-runtime.service"), ("*.timer", "a.timer"), ("x?.service", "x1.service"), ("[ab]*", "b.service"), ("[!a]*", "b.service"), ("[a-c]x", "bx"), ("*", ""), ("a[", "a[")] {
            assert!(glob_match(p, n), "{p} ~ {n}");
        }
        for (p, n) in [("spira-*", "other.service"), ("*.timer", "a.service"), ("x?.service", "x.service"), ("[!a]*", "a.service"), ("[a-c]x", "dx"), ("a", "ab")] {
            assert!(!glob_match(p, n), "{p} !~ {n}");
        }
    }

    #[test]
    fn a_tail_is_the_last_four_kib_on_a_character_boundary() {
        assert_eq!(tail(b"short"), "short");
        let long: Vec<u8> = (0..10_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let t = tail(&long);
        assert_eq!(t.len(), TAIL_MAX);
        assert!(long.ends_with(t.as_bytes()));
        let mut multi = "é".repeat(3000).into_bytes();
        multi.insert(0, b'x');
        let t = tail(&multi);
        assert!(t.len() <= TAIL_MAX && t.chars().all(|c| c == 'é'), "cut mid-character: {}", t.len());
        let t = tail(&[0xff; 5000]);
        assert!(t.len() <= TAIL_MAX, "replacement characters stay inside the bound: {}", t.len());
    }

    #[test]
    fn a_unit_ran_with_tails_reads_back_both_ways_and_without_is_unchanged() {
        let ran = UnitRan { handle: UnitHandle::of("t", "k").unwrap(), exit: Exit::Code(4), duration_ms: 9 };
        assert_eq!(ran.event_kind_with(&RanOutput::default()), ran.event_kind());
        let out = RanOutput::captured(b"out", b"err");
        let k = ran.event_kind_with(&out);
        assert_eq!(UnitRan::from_event_kind(&k), Some(ran.clone()));
        assert_eq!(RanOutput::from_event_kind(&k), Some(out));
        assert_eq!(RanOutput::from_event_kind(&ran.event_kind()), Some(RanOutput::default()));
    }

    #[test]
    fn a_scripted_installed_unit_answers_every_verb_and_its_detail() {
        let clock = VirtualClock::new(100);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        let h = UnitHandle::named("spira-loom.service");
        u.install_detail(UnitDetail { name: h.name.clone(), state: UnitState::Failed, exit: Some(Exit::Code(1)), load_state: Some("loaded".into()), unit_file_state: Some("disabled".into()), active_state: Some("failed".into()), sub_state: Some("failed".into()), result: Some("exit-code".into()), n_restarts: Some(3), invocation_id: Some("ab".repeat(16)), ..UnitDetail::default() });
        let d = u.detail(&h).unwrap();
        assert_eq!((d.load_state.as_deref(), d.unit_file_state.as_deref(), d.result.as_deref(), d.n_restarts), (Some("loaded"), Some("disabled"), Some("exit-code"), Some(3)));
        assert_eq!(u.facts(&h).unwrap().state, UnitState::Failed, "facts is the detail's summary");
        u.reset_failed(&h).unwrap();
        let d = u.detail(&h).unwrap();
        assert_eq!((d.state, d.active_state.as_deref(), d.result.as_deref(), d.n_restarts), (UnitState::Inactive, Some("inactive"), Some("success"), Some(0)));
        u.enable(&h).unwrap();
        assert_eq!(u.detail(&h).unwrap().unit_file_state.as_deref(), Some("enabled"));
        clock.advance(5);
        u.start_installed(&h).unwrap();
        let d = u.detail(&h).unwrap();
        assert_eq!((d.state, d.active_state.as_deref(), d.sub_state.as_deref(), d.since_ms), (UnitState::Active, Some("active"), Some("running"), Some(105)));
        u.disable(&h).unwrap();
        assert_eq!(u.detail(&h).unwrap().unit_file_state.as_deref(), Some("disabled"));
        let other = UnitHandle::named("nope.service");
        for r in [u.start_installed(&other), u.enable(&other), u.disable(&other), u.reset_failed(&other)] {
            assert!(r.unwrap_err().is_refused());
        }
        assert_eq!(u.calls(), ["reset-failed spira-loom.service", "enable spira-loom.service", "start spira-loom.service", "disable spira-loom.service"]);
    }

    #[test]
    fn a_scripted_list_and_timers_match_their_glob() {
        let clock = VirtualClock::new(0);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        u.install("spira-a.service", UnitFacts { state: UnitState::Active, main_pid: Some(7), exit: None, since_ms: Some(1) });
        u.install_detail(UnitDetail { name: "spira-b.timer".into(), state: UnitState::Active, ..UnitDetail::default() });
        u.install("other.service", UnitFacts::absent());
        u.script("t", 10, Exit::Code(0));
        u.start(&spec("t", "k", &["x"])).unwrap();
        u.install_timer(TimerFacts { name: "spira-b.timer".into(), unit: Some("spira-b.service".into()), next_elapse_ms: Some(60_000), ..TimerFacts::default() });
        u.install_timer(TimerFacts { name: "other.timer".into(), ..TimerFacts::default() });
        let names: Vec<String> = u.list("spira-*").unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["spira-a.service", "spira-b.timer", "spira-run-t-k.service"]);
        assert_eq!(u.list("spira-a*").unwrap()[0].main_pid, Some(7));
        assert!(u.list("other*").unwrap().is_empty(), "an absent unit is not listed");
        let t = u.timers("spira-*").unwrap();
        assert_eq!((t.len(), t[0].unit.as_deref(), t[0].next_elapse_ms), (1, Some("spira-b.service"), Some(60_000)));
    }

    #[test]
    fn a_scripted_scope_ends_with_its_tails_and_records_its_limits() {
        let clock = VirtualClock::new(0);
        let u = ScriptedUnits::new(Arc::new(clock.clone()));
        u.script("build", 50, Exit::Code(4));
        u.script_output("build", b"compiled", b"warning: x");
        let mut sp = ScopeSpec::new(spec("build", "k", &["cargo"]));
        sp.cpu_quota_pct = Some(400);
        sp.memory_max_bytes = Some(1 << 30);
        let s = u.run_scope(&sp).unwrap();
        assert_eq!(s.unit, UnitHandle::of("build", "k").unwrap());
        assert_eq!(u.scopes(), [sp]);
        assert_eq!(u.detail(&s.unit).unwrap().exec_main_start_ms, Some(0));
        clock.advance(50);
        let ended = u.ended().unwrap();
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].0.exit, ended[0].1.stdout_tail.as_deref(), ended[0].1.stderr_tail.as_deref()), (Exit::Code(4), Some("compiled"), Some("warning: x")));
        assert!(u.ended().unwrap().is_empty(), "once each");
        let d = u.detail(&s.unit).unwrap();
        assert_eq!((d.state, d.result.as_deref(), d.sub_state.as_deref()), (UnitState::Failed, Some("exit-code"), Some("failed")));
        u.reset_failed(&s.unit).unwrap();
        assert_eq!(u.detail(&s.unit).unwrap().state, UnitState::Absent, "a reset transient unit is collected");
        assert_eq!(u.calls(), ["run-scope spira-run-build-k.service", "reset-failed spira-run-build-k.service"]);
    }

    #[test]
    fn extended_effects_round_trip_as_json() {
        let u = ScriptedUnits::new(Arc::new(VirtualClock::new(0)));
        u.install("spira-a.service", UnitFacts { state: UnitState::Failed, main_pid: None, exit: None, since_ms: None });
        let hj = serde_json::to_string(&UnitHandle::named("spira-a.service")).unwrap();
        let d: UnitDetail = serde_json::from_str(&effect(&u, UNIT_DETAIL, &hj).unwrap()).unwrap();
        assert_eq!(d.state, UnitState::Failed);
        let l: Vec<UnitDetail> = serde_json::from_str(&effect(&u, UNIT_LIST, "\"spira-*\"").unwrap()).unwrap();
        assert_eq!(l.len(), 1);
        let t: Vec<TimerFacts> = serde_json::from_str(&effect(&u, UNIT_TIMERS, "\"*\"").unwrap()).unwrap();
        assert!(t.is_empty());
        for e in [UNIT_RESET_FAILED, UNIT_ENABLE, UNIT_DISABLE, UNIT_START_INSTALLED] {
            assert_eq!(effect(&u, e, &hj), Ok(String::new()), "{e}");
        }
        u.script("t", 1, Exit::Code(0));
        let sj = serde_json::to_string(&ScopeSpec::new(spec("t", "k", &["x"]))).unwrap();
        let s: ScopeHandle = serde_json::from_str(&effect(&u, UNIT_RUN_SCOPE, &sj).unwrap()).unwrap();
        assert_eq!(s.unit.name, "spira-run-t-k.service");
        assert!(EXTENDED_EFFECTS.iter().all(|e| !EFFECTS.contains(e)));
    }

    #[test]
    fn a_process_unit_answers_the_honest_subset_and_refuses_the_rest() {
        let u = ProcessUnits::new(Arc::new(VirtualClock::new(7)));
        let h = u.start(&spec("t", "k", &["sleep", "30"])).unwrap();
        let d = u.detail(&h).unwrap();
        assert_eq!((d.state, d.load_state.as_deref(), d.active_state.as_deref(), d.sub_state.as_deref(), d.exec_main_start_ms, d.n_restarts), (UnitState::Active, Some("loaded"), Some("active"), Some("running"), Some(7), Some(0)));
        assert!(d.main_pid.is_some());
        assert_eq!((d.unit_file_state, d.invocation_id), (None, None), "a plain process has no unit file and no invocation id");
        assert_eq!(u.list("spira-run-t-*").unwrap().len(), 1);
        assert!(u.list("other*").unwrap().is_empty());
        let installed = UnitHandle::named("spira-loom.service");
        for r in [u.start_installed(&installed), u.enable(&installed), u.disable(&installed)] {
            assert!(r.unwrap_err().is_refused());
        }
        assert!(u.timers("*").unwrap_err().is_refused());
        let mut limited = ScopeSpec::new(spec("t", "k2", &["true"]));
        limited.cpu_quota_pct = Some(100);
        let e = u.run_scope(&limited).unwrap_err();
        assert!(e.is_refused() && e.0.contains("CPUQuota"), "{e}");
        limited.cpu_quota_pct = None;
        limited.memory_max_bytes = Some(1);
        assert!(u.run_scope(&limited).unwrap_err().0.contains("MemoryMax"));
        assert!(u.reset_failed(&installed).unwrap_err().is_refused());
        u.stop(&h).unwrap();
        assert_eq!(u.detail(&h).unwrap().result.as_deref(), Some("signal"));
        u.reset_failed(&h).unwrap();
        assert_eq!(u.detail(&h).unwrap().state, UnitState::Absent);
    }

    #[cfg(unix)]
    fn ended_within(u: &ProcessUnits) -> Vec<(UnitRan, RanOutput)> {
        for _ in 0..1000 {
            let got = u.ended().unwrap();
            if !got.is_empty() {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: polls a real child's exit, capped at 5 s
        }
        vec![]
    }

    #[cfg(unix)]
    #[test]
    fn a_process_scope_returns_its_output_tails_when_it_ends() {
        let u = ProcessUnits::new(Arc::new(VirtualClock::new(0)));
        let mut sp = spec("t", "k", &["sh", "-c", "printf out; printf err >&2; printf %s \"$V\"; exit 4"]);
        sp.env = vec![("V".into(), "-v".into())];
        let s = u.run_scope(&ScopeSpec::new(sp)).unwrap();
        let got = ended_within(&u);
        assert_eq!(got.len(), 1);
        assert_eq!((&got[0].0.handle, got[0].0.exit), (&s.unit, Exit::Code(4)));
        assert_eq!((got[0].1.stdout_tail.as_deref(), got[0].1.stderr_tail.as_deref()), (Some("out-v"), Some("err")));
        let big = u.run_scope(&ScopeSpec::new(spec("t", "big", &["seq", "1", "3000"]))).unwrap();
        let got = ended_within(&u);
        assert_eq!(got[0].0.handle, big.unit);
        let out = got[0].1.stdout_tail.clone().unwrap();
        assert_eq!(out.len(), TAIL_MAX);
        assert!(out.ends_with("2999\n3000\n"));
        let plain = u.start(&spec("t", "plain", &["true"])).unwrap();
        let got = ended_within(&u);
        assert_eq!((&got[0].0.handle, got[0].1.is_empty()), (&plain, true), "a plain start captures nothing");
    }

    #[test]
    fn the_backend_is_chosen_by_its_config_value() {
        let c: Arc<dyn Clock> = Arc::new(VirtualClock::new(0));
        let b = from_value(c.clone(), Some("scripted")).unwrap();
        assert_eq!(b.kind(), "scripted");
        let s = b.scripted().expect("a scripted backend hands itself to the test");
        s.install("x.service", UnitFacts { state: UnitState::Active, main_pid: None, exit: None, since_ms: None });
        assert_eq!(b.units().facts(&UnitHandle::named("x.service")).unwrap().state, UnitState::Active, "the units face is the scripted one");
        assert!(b.completions().ran().unwrap().is_empty());
        assert_eq!(from_value(c.clone(), Some("process")).unwrap().kind(), "process");
        let e = from_value(c.clone(), Some("bogus")).err().expect("refused");
        assert!(e.is_refused() && e.0.contains("bogus") && e.0.contains(UNITS_KEY), "{e}");
        #[cfg(not(target_os = "linux"))]
        {
            assert!(from_value(c.clone(), Some("systemd")).err().expect("refused").is_refused());
            assert_eq!(from_value(c, None).unwrap().kind(), "process");
        }
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

        /// The properties a dwarf call and the landing leg carried under `systemd-run`, as the
        /// transient unit's own: each set when given, and none when not.
        #[test]
        fn start_props_encode_as_transient_unit_properties() {
            let sp = spec("sentinel-call", "k", &["/bin/dwarf"]);
            let props = UnitProps {
                start_timeout_ms: Some(3_600_000),
                runtime_max_ms: Some(900_000),
                io_class: Some("idle".into()),
                io_weight: Some(10),
                nice: Some(5),
                cpu_quota_pct: Some(200),
                memory_max_bytes: Some(1 << 30),
                exec_stop_post: vec![vec!["/bin/true".into(), "x".into()]],
                output_append: Some("/run/landing.log".into()),
            };
            let raw = encode_call_body(3, "d", "/p", "i", "StartTransientUnit", &start_transient_args_with(&sp, &props).unwrap());
            let m = decode(&raw).unwrap();
            let got = m.body[2].dict();
            let get = |k: &str| got.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("{k} not set"));
            let var = |v: Val| Val::Var(Box::new(v));
            assert_eq!(get("TimeoutStartUSec"), var(Val::U(3_600_000_000)));
            assert_eq!(get("RuntimeMaxUSec"), var(Val::U(900_000_000)));
            assert_eq!(get("IOSchedulingClass"), var(Val::I(3)));
            assert_eq!(get("IOWeight"), var(Val::U(10)));
            assert_eq!(get("Nice"), var(Val::I(5)));
            assert_eq!(get("CPUQuotaPerSecUSec"), var(Val::U(2_000_000)));
            assert_eq!(get("MemoryMax"), var(Val::U(1 << 30)));
            assert_eq!(get("StandardOutputFileToAppend"), var(Val::S("/run/landing.log".into())));
            assert_eq!(get("StandardErrorFileToAppend"), var(Val::S("/run/landing.log".into())));
            let Val::Var(post) = get("ExecStopPost") else { panic!("ExecStopPost not a variant") };
            let Val::Arr(cmds) = *post else { panic!("ExecStopPost not an array") };
            assert_eq!(cmds.len(), 1);
            let bare = start_transient_args_with(&sp, &UnitProps::default()).unwrap();
            assert_eq!(bare, start_transient_args(&sp).unwrap(), "no properties is the plain start");
            assert!(start_transient_args_with(&sp, &UnitProps { io_class: Some("lazy".into()), ..UnitProps::default() }).is_err(), "an unknown IO class refuses");
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

        /// A call's member and its arguments.
        type Sent = (String, Vec<Arg>);

        /// A bus that records calls, answers from a table keyed `<member> <path> <first arg>`,
        /// and hands out the signals pushed into it, oldest first.
        #[derive(Clone, Default)]
        struct Fake {
            calls: Arc<Mutex<Vec<String>>>,
            sent: Arc<Mutex<Vec<Sent>>>,
            answers: Arc<Mutex<BTreeMap<String, Vec<Val>>>>,
            signals: Arc<Mutex<VecDeque<Msg>>>,
        }

        impl Fake {
            fn answer(&self, key: &str, v: Vec<Val>) {
                self.answers.lock().unwrap().insert(key.into(), v);
            }
            fn signal(&self, m: Msg) {
                self.signals.lock().unwrap().push_back(m);
            }
            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
            }
            fn units(&self) -> SystemdUnits<FakeBus> {
                SystemdUnits::new(FakeBus(self.clone()), Arc::new(crate::ports::VirtualClock::new(0))).unwrap()
            }
        }

        struct FakeBus(Fake);

        impl Bus for FakeBus {
            fn call(&mut self, _dest: &str, path: &str, _iface: &str, member: &str, args: &[Arg]) -> std::result::Result<Vec<Val>, String> {
                let first = match args.first() {
                    Some(Arg::S(s)) => s.clone(),
                    Some(Arg::Arr(_, a)) => a.iter().filter_map(|x| if let Arg::S(s) = x { Some(s.clone()) } else { None }).collect::<Vec<_>>().join(","),
                    _ => String::new(),
                };
                let key = format!("{member} {path} {first}");
                self.0.calls.lock().unwrap().push(key.clone());
                self.0.sent.lock().unwrap().push((member.into(), args.to_vec()));
                if let Some(v) = self.0.answers.lock().unwrap().get(&key) {
                    return Ok(v.clone());
                }
                Ok(match member {
                    "LoadUnit" => vec![Val::S("/org/freedesktop/systemd1/unit/x".into())],
                    _ => vec![],
                })
            }
            fn next_signal(&mut self, _: Duration) -> std::result::Result<Option<Msg>, String> {
                Ok(self.0.signals.lock().unwrap().pop_front())
            }
        }

        #[test]
        fn restart_and_stop_go_to_the_unit_object_and_start_to_the_manager() {
            let f = Fake::default();
            let u = f.units();
            let h = UnitHandle::named("spira-runtime.service");
            u.restart(&h).unwrap();
            u.stop(&h).unwrap();
            u.start(&spec("t", "k", &["/bin/true"])).unwrap();
            let c = f.calls();
            assert_eq!(c.iter().filter(|l| l.starts_with("AddMatch")).count(), 3, "unit properties, the manager's owner, and JobRemoved");
            assert!(c.iter().any(|l| l.starts_with("AddMatch") && l.contains("member='JobRemoved'")));
            assert!(c.contains(&"Subscribe /org/freedesktop/systemd1 ".to_string()));
            let tail: Vec<&str> = c.iter().skip(4).map(String::as_str).collect();
            assert_eq!(tail, vec![
                "LoadUnit /org/freedesktop/systemd1 spira-runtime.service",
                "Restart /org/freedesktop/systemd1/unit/x replace",
                "LoadUnit /org/freedesktop/systemd1 spira-runtime.service",
                "Stop /org/freedesktop/systemd1/unit/x replace",
                "StartTransientUnit /org/freedesktop/systemd1 spira-run-t-k.service",
            ]);
        }

        #[test]
        fn installed_unit_verbs_are_manager_methods_and_file_changes_reload() {
            let f = Fake::default();
            let u = f.units();
            let h = UnitHandle::named("spira-loom.service");
            u.start_installed(&h).unwrap();
            u.enable(&h).unwrap();
            u.disable(&h).unwrap();
            u.reset_failed(&h).unwrap();
            let c = f.calls();
            let tail: Vec<&str> = c.iter().skip(4).map(String::as_str).collect();
            assert_eq!(tail, vec![
                "StartUnit /org/freedesktop/systemd1 spira-loom.service",
                "EnableUnitFiles /org/freedesktop/systemd1 spira-loom.service",
                "Reload /org/freedesktop/systemd1 ",
                "DisableUnitFiles /org/freedesktop/systemd1 spira-loom.service",
                "Reload /org/freedesktop/systemd1 ",
                "ResetFailedUnit /org/freedesktop/systemd1 spira-loom.service",
            ]);
            let sent = f.sent.lock().unwrap().clone();
            let sig = |m: &str| sent.iter().find(|(n, _)| n == m).map(|(_, a)| encode_args(a).0).unwrap();
            assert_eq!((sig("StartUnit"), sig("EnableUnitFiles"), sig("DisableUnitFiles"), sig("ResetFailedUnit")), ("ss".into(), "asbb".into(), "asb".into(), "s".into()));
        }

        fn changed(path: &str, props: Vec<(String, Val)>) -> Msg {
            let body = vec![Val::S("org.freedesktop.systemd1.Unit".into()), Val::Arr(props.into_iter().map(|(k, v)| Val::Struct(vec![Val::S(k), v])).collect()), Val::Arr(vec![])];
            Msg { kind: unit_run::dbus::SIGNAL, serial: 1, reply_serial: None, path: Some(path.into()), interface: Some("org.freedesktop.DBus.Properties".into()), member: Some("PropertiesChanged".into()), error: None, body }
        }

        fn dict(pairs: &[(&str, Val)]) -> Val {
            Val::Arr(d(pairs).into_iter().map(|(k, v)| Val::Struct(vec![Val::S(k), v])).collect())
        }

        const UNIT: &str = "org.freedesktop.systemd1.Unit";

        #[test]
        fn detail_reads_the_unit_and_its_type_interface() {
            let f = Fake::default();
            let p = "/org/freedesktop/systemd1/unit/x";
            f.answer(&format!("GetAll {p} {UNIT}"), vec![dict(&[("LoadState", Val::S("loaded".into())), ("UnitFileState", Val::S("enabled".into())), ("ActiveState", Val::S("active".into())), ("SubState", Val::S("running".into())), ("InvocationID", Val::Arr(vec![Val::U(0xab); 16]))])]);
            f.answer(&format!("GetAll {p} org.freedesktop.systemd1.Service"), vec![dict(&[("Result", Val::S("success".into())), ("MainPID", Val::U(42)), ("NRestarts", Val::U(2)), ("ExecMainStartTimestamp", Val::U(9_000_000))])]);
            let d = f.units().detail(&UnitHandle::named("spira-loom.service")).unwrap();
            assert_eq!(d, UnitDetail {
                name: "spira-loom.service".into(),
                state: UnitState::Active,
                exit: None,
                since_ms: None,
                load_state: Some("loaded".into()),
                unit_file_state: Some("enabled".into()),
                active_state: Some("active".into()),
                sub_state: Some("running".into()),
                result: Some("success".into()),
                main_pid: Some(42),
                n_restarts: Some(2),
                invocation_id: Some("ab".repeat(16)),
                exec_main_start_ms: Some(9_000),
            });
            assert_eq!(f.units().facts(&UnitHandle::named("spira-loom.service")).unwrap(), d.facts());
        }

        fn listed(name: &str, active: &str, path: &str) -> Val {
            let s = |v: &str| Val::S(v.into());
            Val::Struct(vec![s(name), s("d"), s("loaded"), s(active), s("running"), s(""), s(path), Val::U(0), s(""), s("/")])
        }

        #[test]
        fn list_joins_loaded_units_with_unit_files_and_timers_carry_their_next_elapse() {
            let f = Fake::default();
            let (a, t) = ("/org/freedesktop/systemd1/unit/a", "/org/freedesktop/systemd1/unit/t");
            f.answer("ListUnitsByPatterns /org/freedesktop/systemd1 ", vec![Val::Arr(vec![listed("spira-a.service", "active", a), listed("spira-t.timer", "active", t)])]);
            f.answer("ListUnitFilesByPatterns /org/freedesktop/systemd1 ", vec![Val::Arr(vec![
                Val::Struct(vec![Val::S("/etc/systemd/user/spira-a.service".into()), Val::S("enabled".into())]),
                Val::Struct(vec![Val::S("/etc/systemd/user/spira-off.service".into()), Val::S("disabled".into())]),
            ])]);
            f.answer(&format!("GetAll {a} {UNIT}"), vec![dict(&[("LoadState", Val::S("loaded".into())), ("ActiveState", Val::S("active".into())), ("UnitFileState", Val::S("enabled".into()))])]);
            f.answer(&format!("GetAll {t} {UNIT}"), vec![dict(&[("LoadState", Val::S("loaded".into())), ("ActiveState", Val::S("active".into()))])]);
            f.answer(&format!("GetAll {t} org.freedesktop.systemd1.Timer"), vec![dict(&[("Unit", Val::S("spira-t.service".into())), ("NextElapseUSecRealtime", Val::U(60_000_000)), ("NextElapseUSecMonotonic", Val::U(0)), ("LastTriggerUSec", Val::U(1_000_000))])]);
            let u = f.units();
            let l = u.list("spira-*").unwrap();
            let rows: Vec<(&str, UnitState, Option<&str>)> = l.iter().map(|d| (d.name.as_str(), d.state, d.unit_file_state.as_deref())).collect();
            assert_eq!(rows, [("spira-a.service", UnitState::Active, Some("enabled")), ("spira-off.service", UnitState::Inactive, Some("disabled")), ("spira-t.timer", UnitState::Active, None)]);
            let sent = f.sent.lock().unwrap().clone();
            let (_, args) = sent.iter().find(|(m, _)| m == "ListUnitsByPatterns").unwrap();
            assert_eq!(args[1], Arg::Arr("s", vec![Arg::S("spira-*".into())]), "the glob goes to the manager as its pattern");
            let timers = u.timers("spira-*").unwrap();
            assert_eq!(timers, [TimerFacts { name: "spira-t.timer".into(), unit: Some("spira-t.service".into()), active_state: Some("active".into()), next_elapse_ms: Some(60_000), next_elapse_monotonic_ms: None, last_trigger_ms: Some(1_000) }]);
        }

        #[test]
        fn scope_args_carry_limits_and_the_capture_files() {
            let mut sp = ScopeSpec::new(spec("build", "k", &["/bin/cargo", "build"]));
            sp.unit.cwd = Some("/src".into());
            sp.unit.env = vec![("A".into(), "1".into())];
            sp.cpu_quota_pct = Some(250);
            sp.memory_max_bytes = Some(2 << 30);
            let raw = encode_call_body(1, "d", "/p", "i", "StartTransientUnit", &scope_args(&sp, std::path::Path::new("/run/o"), std::path::Path::new("/run/e")).unwrap());
            let m = decode(&raw).unwrap();
            let props = m.body[2].dict();
            let get = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("no {k}"));
            assert_eq!(get("CPUQuotaPerSecUSec").as_u64(), Some(2_500_000));
            assert_eq!(get("MemoryMax").as_u64(), Some(2 << 30));
            assert_eq!(get("StandardOutputFile").as_str(), Some("/run/o"));
            assert_eq!(get("StandardErrorFile").as_str(), Some("/run/e"));
            assert_eq!(get("WorkingDirectory").as_str(), Some("/src"));
            assert_eq!(get("Environment"), Val::Var(Box::new(Val::Arr(vec![Val::S("A=1".into())]))));
        }

        fn run(f: &Fake, path: &str, inv: u8, start_us: u64, end_us: u64, end: &str) {
            f.signal(changed(path, d(&[("ActiveState", Val::S("active".into())), ("InvocationID", Val::Arr(vec![Val::U(inv as u64); 16])), ("InactiveExitTimestamp", Val::U(start_us))])));
            f.signal(changed(path, d(&[("ActiveState", Val::S(end.into())), ("InactiveEnterTimestamp", Val::U(end_us))])));
        }

        #[test]
        fn a_scope_run_ends_with_the_tails_of_its_capture_files() {
            let dir = testkit::TempDir::new("units-scope");
            let f = Fake::default();
            let u = f.units().with_capture_dir(Some(dir.path().to_path_buf()));
            let s = u.run_scope(&ScopeSpec::new(spec("build", "k", &["/bin/cargo"]))).unwrap();
            let sent = f.sent.lock().unwrap().clone();
            let (_, args) = sent.iter().find(|(m, _)| m == "StartTransientUnit").unwrap();
            let Arg::Arr(_, props) = &args[2] else { panic!("no properties") };
            let file = |k: &str| props.iter().find_map(|p| match p { Arg::Struct(kv) if kv[0] == Arg::S(k.into()) => match &kv[1] { Arg::Var(v) => match &**v { Arg::S(s) => Some(std::path::PathBuf::from(s)), _ => None }, _ => None }, _ => None }).unwrap();
            let (out, err) = (file("StandardOutputFile"), file("StandardErrorFile"));
            assert!(out.starts_with(dir.path()) && err.starts_with(dir.path()));
            assert_eq!(std::fs::read(&out).unwrap(), b"", "created empty, so a stale file is never read back");
            std::fs::write(&out, "x".repeat(10_000) + "end").unwrap();
            std::fs::write(&err, "oops").unwrap();
            run(&f, &unit_run::dbus::unit_object_path(&s.unit.name), 3, 1_000_000, 2_000_000, "failed");
            let ended = u.ended().unwrap();
            assert_eq!(ended.len(), 1);
            assert_eq!(ended[0].0.handle, s.unit);
            let o = ended[0].1.stdout_tail.clone().unwrap();
            assert_eq!((o.len(), o.ends_with("end")), (TAIL_MAX, true));
            assert_eq!(ended[0].1.stderr_tail.as_deref(), Some("oops"));
            assert!(!out.exists() && !err.exists(), "the capture files go once read");
            let no_dir = Fake::default().units().with_capture_dir(None);
            assert!(no_dir.run_scope(&ScopeSpec::new(spec("b", "k", &["/bin/x"]))).unwrap_err().is_refused());
        }

        #[test]
        fn installed_and_task_units_both_end_as_unit_ran() {
            let f = Fake::default();
            let ours = "/org/freedesktop/systemd1/unit/spira_2drun_2dt_2dk_2eservice";
            let installed = "/org/freedesktop/systemd1/unit/spira_2dloom_2eservice";
            run(&f, ours, 7, 1_000_000, 4_500_000, "inactive");
            run(&f, installed, 8, 2_000_000, 3_000_000, "inactive");
            let u = f.units();
            let ran = u.ran().unwrap();
            assert_eq!(ran, vec![
                UnitRan { handle: UnitHandle::of("t", "k").unwrap(), exit: Exit::Code(0), duration_ms: 3_500 },
                UnitRan { handle: UnitHandle::named("spira-loom.service"), exit: Exit::Code(0), duration_ms: 1_000 },
            ]);
            assert_eq!(ran[0].key_for("t").as_deref(), Some("k"));
            assert_eq!(ran[1].handle.owner(), None, "an installed unit belongs to no task");
            assert!(u.ran().unwrap().is_empty());
        }

        fn job_removed(unit: &str, result: &str) -> Msg {
            let body = vec![Val::U(5), Val::S("/org/freedesktop/systemd1/job/5".into()), Val::S(unit.into()), Val::S(result.into())];
            Msg { kind: unit_run::dbus::SIGNAL, serial: 2, reply_serial: None, path: Some("/org/freedesktop/systemd1".into()), interface: Some("org.freedesktop.systemd1.Manager".into()), member: Some("JobRemoved".into()), error: None, body }
        }

        #[test]
        fn a_start_job_that_never_ran_ends_once_and_a_run_that_did_is_not_doubled() {
            let f = Fake::default();
            let u = f.units();
            f.signal(job_removed("spira-dep.service", "dependency"));
            f.signal(job_removed("spira-ok.service", "done"));
            f.signal(job_removed("spira-limit.service", "failed"));
            let path = unit_run::dbus::unit_object_path("spira-ran.service");
            run(&f, &path, 9, 1_000_000, 2_000_000, "failed");
            f.signal(job_removed("spira-ran.service", "failed"));
            let ran = u.ran().unwrap();
            let names: Vec<(&str, Exit, u64)> = ran.iter().map(|r| (r.handle.name.as_str(), r.exit, r.duration_ms)).collect();
            assert_eq!(names, [("spira-dep.service", Exit::Unknown, 0), ("spira-limit.service", Exit::Unknown, 0), ("spira-ran.service", Exit::Unknown, 1_000)]);
            f.signal(job_removed("spira-pending.service", "timeout"));
            let path = unit_run::dbus::unit_object_path("spira-pending.service");
            f.signal(changed(&path, d(&[("ActiveState", Val::S("activating".into())), ("InvocationID", Val::Arr(vec![Val::U(4); 16])), ("InactiveExitTimestamp", Val::U(5_000_000))])));
            f.signal(job_removed("spira-pending.service", "timeout"));
            let ran = u.ran().unwrap();
            assert_eq!(ran.len(), 1, "a job ending while its unit is live is the run's own end, not a second one: {ran:?}");
        }

        /// A bus on a socket: answers EXTERNAL auth, then each of `n` calls with an empty
        /// METHOD_RETURN, and hands back every call it decoded.
        fn fake_bus(n: usize) -> (testkit::TempDir, String, std::thread::JoinHandle<Vec<Msg>>) {
            use std::io::{Read, Write};
            let dir = testkit::TempDir::new("units-bus");
            let path = dir.join("bus");
            let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let peer = std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut line = vec![];
                let mut b = [0u8; 1];
                while !line.ends_with(b"\r\n") {
                    s.read_exact(&mut b).unwrap();
                    line.push(b[0]);
                }
                s.write_all(b"OK 0123456789abcdef\r\n").unwrap();
                let mut begin = [0u8; 7];
                s.read_exact(&mut begin).unwrap();
                assert_eq!(&begin, b"BEGIN\r\n");
                let mut seen = vec![];
                for _ in 0..n {
                    let mut head = [0u8; 16];
                    s.read_exact(&mut head).unwrap();
                    let body = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
                    let fields = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
                    let mut rest = vec![0u8; (16 + fields).div_ceil(8) * 8 + body - 16];
                    s.read_exact(&mut rest).unwrap();
                    let m = decode(&[head.to_vec(), rest].concat()).unwrap();
                    let mut reply = vec![b'l', unit_run::dbus::METHOD_RETURN, 0, 1];
                    reply.extend_from_slice(&0u32.to_le_bytes());
                    reply.extend_from_slice(&1u32.to_le_bytes());
                    reply.extend_from_slice(&8u32.to_le_bytes());
                    reply.extend_from_slice(&[5, 1, b'u', 0]);
                    reply.extend_from_slice(&m.serial.to_le_bytes());
                    s.write_all(&reply).unwrap();
                    seen.push(m);
                }
                seen
            });
            let path = path.to_string_lossy().into_owned();
            (dir, path, peer)
        }

        #[test]
        fn the_connection_carries_start_transient_units_four_arguments_to_the_bus() {
            let (_dir, path, peer) = fake_bus(2);
            let mut c = unit_run::dbus::Conn::connect(&path).unwrap();
            let args = start_transient_args(&spec("czar-pass", "r7", &["/bin/czar-pass", "--pass"])).unwrap();
            Bus::call(&mut c, "org.freedesktop.systemd1", "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", "StartTransientUnit", &args).unwrap();
            let seen = peer.join().unwrap();
            assert_eq!(seen[0].member.as_deref(), Some("Hello"));
            let m = &seen[1];
            assert_eq!((m.kind, m.member.as_deref(), m.path.as_deref()), (METHOD_CALL, Some("StartTransientUnit"), Some("/org/freedesktop/systemd1")));
            assert_eq!(m.body.len(), 4, "name, mode, properties, aux");
            assert_eq!(m.body[0].as_str(), Some("spira-run-czar_2dpass-r7.service"));
            assert_eq!(m.body[1].as_str(), Some("fail"));
            assert!(m.body[2].dict().iter().any(|(k, _)| k == "ExecStart"));
        }
    }
}
