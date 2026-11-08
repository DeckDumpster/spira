//! spira-watchdog (S15): one pass per invocation, run by the platform's timer. It asks the
//! scheduler for its last turn (`scheduler status` on the control socket) and the service
//! manager for the scheduler unit's facts (the `Units` port); when the scheduler is absent or its
//! last turn is older than the bound, it opens an incident. While the incident is open every
//! pass whose backoff is due starts the unit again (`reset-failed` then `start` for a unit that
//! is not running, so systemd's start limit never ends the retries; `restart` for a wedged one),
//! pages the Concierge inbox once when the starts have not brought it back, with the unit's own
//! last log lines, and reports the scheduler down rather than success.

pub mod render;

use std::path::{Path, PathBuf};

use runtime::ports::Clock;
use runtime::units::{UnitFacts, UnitHandle, UnitState, Units};
use serde::{Deserialize, Serialize};

pub const KEY_BOUND: &str = "SPIRA_WATCHDOG_BOUND_SECS";
pub const KEY_SOCKET: &str = "SPIRA_RUNTIME_SOCKET";
pub const KEY_UNIT: &str = "SPIRA_WATCHDOG_UNIT";
pub const KEY_RUN: &str = "SPIRA_RUN";
/// The file the scheduler unit appends its stdout and stderr to; its tail is a page's evidence.
pub const KEY_UNIT_LOG: &str = "SPIRA_WATCHDOG_UNIT_LOG";
pub const KEYS: [&str; 5] = [KEY_BOUND, KEY_SOCKET, KEY_UNIT, KEY_RUN, KEY_UNIT_LOG];
/// The seconds between passes: read by the installer that renders the timer, not by a pass.
pub const KEY_EVERY: &str = "SPIRA_WATCHDOG_EVERY_SECS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub bound_ms: u64,
    pub socket: PathBuf,
    pub unit: String,
    /// The scheduler's in-flight marker, read when the socket does not answer.
    pub marker: PathBuf,
    /// The open incident, so one incident pages once across passes.
    pub incident: PathBuf,
    /// The scheduler unit's own log.
    pub unit_log: PathBuf,
}

impl Config {
    /// Every key is required; a missing or empty one refuses, named.
    pub fn from_lookup(get: impl Fn(&str) -> Result<String, String>) -> Result<Config, String> {
        let req = |k: &str| get(k).and_then(|v| if v.trim().is_empty() { Err(format!("{k} is empty — refusing")) } else { Ok(v) });
        let bound: u64 = req(KEY_BOUND)?.trim().parse().map_err(|_| format!("{KEY_BOUND} is not a whole number of seconds — refusing"))?;
        if bound == 0 {
            return Err(format!("{KEY_BOUND} is 0 — every scheduler would be wedged; refusing"));
        }
        let run = PathBuf::from(req(KEY_RUN)?);
        Ok(Config {
            bound_ms: bound * 1000,
            socket: PathBuf::from(req(KEY_SOCKET)?),
            unit: req(KEY_UNIT)?,
            marker: run.join("runtime-inflight.json"),
            incident: run.join("watchdog-incident.json"),
            unit_log: PathBuf::from(req(KEY_UNIT_LOG)?),
        })
    }
}

/// What the scheduler says of its own last turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    Turn { at_ms: u64 },
    /// It answers but has not finished a turn since it started.
    NoTurnYet,
    /// No answer on the socket.
    Unreachable(String),
}

pub trait StatusSource {
    fn read(&self) -> Reading;
}

/// `scheduler status` on the runtime's control socket.
pub struct SocketStatus(pub PathBuf);

/// The `at` of a `scheduler status` document.
pub fn turn_of(doc: &str) -> Result<u64, String> {
    let v: serde_json::Value = serde_json::from_str(doc).map_err(|e| format!("scheduler status is not JSON: {e}"))?;
    v.get("at").and_then(serde_json::Value::as_u64).ok_or_else(|| "scheduler status carries no `at`".to_string())
}

impl StatusSource for SocketStatus {
    fn read(&self) -> Reading {
        match runtime::socket::lc_call(&self.0, &["scheduler", "status"]) {
            Ok(doc) => match turn_of(&doc) {
                Ok(at_ms) => Reading::Turn { at_ms },
                Err(e) => Reading::Unreachable(e),
            },
            Err(e) if e.0.contains("no turn has run yet") => Reading::NoTurnYet,
            Err(e) => Reading::Unreachable(e.0),
        }
    }
}

/// When the handler call now in flight began, if one is.
pub trait MarkerSource {
    fn in_flight_since(&self) -> Option<u64>;
}

pub struct FileMarkerAge(pub PathBuf);

impl MarkerSource for FileMarkerAge {
    fn in_flight_since(&self) -> Option<u64> {
        let t = std::fs::metadata(&self.0).ok()?.modified().ok()?;
        t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
    }
}

pub trait Pager {
    fn page(&self, subject: &str, body: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Healthy,
    /// The socket did not answer and nothing else shows a wedge; no action.
    Unknown(String),
    Absent(String),
    Wedged(String),
}

fn secs(ms: u64) -> u64 {
    ms / 1000
}

pub fn decide(now_ms: u64, bound_ms: u64, unit: &str, facts: &UnitFacts, reading: &Reading, marker: Option<u64>) -> Verdict {
    let stale = |t: u64| now_ms.saturating_sub(t) > bound_ms;
    let age = |t: u64| secs(now_ms.saturating_sub(t));
    let bound = secs(bound_ms);
    match facts.state {
        UnitState::Absent | UnitState::Inactive | UnitState::Failed => Verdict::Absent(format!("{unit} is {:?}", facts.state).to_lowercase()),
        UnitState::Activating | UnitState::Deactivating => match facts.since_ms {
            Some(t) if stale(t) => Verdict::Wedged(format!("{unit} has been {:?} for {}s, bound {bound}s", facts.state, age(t)).to_lowercase()),
            _ => Verdict::Healthy,
        },
        UnitState::Active => match reading {
            Reading::Turn { at_ms } if stale(*at_ms) => Verdict::Wedged(format!("last turn {}s ago, bound {bound}s", age(*at_ms))),
            Reading::Turn { .. } => Verdict::Healthy,
            Reading::NoTurnYet => match facts.since_ms {
                Some(t) if stale(t) => Verdict::Wedged(format!("no turn in the {}s since {unit} started, bound {bound}s", age(t))),
                _ => Verdict::Healthy,
            },
            Reading::Unreachable(why) => match marker {
                Some(t) if stale(t) => Verdict::Wedged(format!("control socket silent ({why}); a handler call has been in flight {}s, bound {bound}s", age(t))),
                _ => Verdict::Unknown(why.clone()),
            },
        },
    }
}

/// The first retry waits this long after the first start; each retry doubles it.
pub const BACKOFF_FIRST_MS: u64 = 60_000;
/// No retry waits longer than this.
pub const BACKOFF_CAP_MS: u64 = 900_000;
/// A timer that fires a little early still counts as due.
const BACKOFF_SLACK_MS: u64 = 5_000;
/// The incident pages once this many starts have been tried and the scheduler is still down.
pub const PAGE_AFTER_STARTS: u32 = 2;
/// The page carries this many of the unit log's last lines.
pub const LOG_TAIL_LINES: usize = 8;
const LOG_LINE_MAX: usize = 300;

/// How long after the `n`th start (1-based) the next one is due.
pub fn backoff_ms(n: u32) -> u64 {
    BACKOFF_FIRST_MS.saturating_mul(1u64 << n.saturating_sub(1).min(20)).min(BACKOFF_CAP_MS)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    pub since_ms: u64,
    pub reason: String,
    /// Starts (or restarts) tried in this incident.
    #[serde(default)]
    pub starts: u32,
    #[serde(default)]
    pub last_start_ms: Option<u64>,
    /// What the last start call itself refused with, if it did.
    #[serde(default)]
    pub last_error: Option<String>,
    pub paged: bool,
    /// Why the last page attempt failed, until one succeeds.
    #[serde(default)]
    pub page_error: Option<String>,
}

fn load(p: &Path) -> Result<Option<Incident>, String> {
    match std::fs::read(p) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn save(p: &Path, i: &Incident) -> Result<(), String> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(i).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
}

fn clear(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
        _ => Ok(()),
    }
}

/// The last `n` non-empty lines of `path`, each cut to a readable length; reads at most the
/// file's last 64 KiB, so a large log costs one bounded read.
pub fn log_tail(path: &Path, n: usize) -> Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len();
    let from = len.saturating_sub(64 * 1024);
    f.seek(SeekFrom::Start(from)).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().skip(usize::from(from > 0)).filter(|l| !l.trim().is_empty()).collect();
    lines = lines.split_off(lines.len().saturating_sub(n));
    Ok(lines.into_iter().map(|l| if l.chars().count() > LOG_LINE_MAX { format!("{}…", l.chars().take(LOG_LINE_MAX).collect::<String>()) } else { l.to_string() }).collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Healthy,
    Unknown(String),
    /// The scheduler is down or wedged and an incident is open: what this pass did about it.
    Down {
        since_ms: u64,
        reason: String,
        /// Starts tried in the incident so far, this pass's included.
        starts: u32,
        /// This pass's start, if one was due: `Ok` when the manager accepted it.
        started: Option<Result<(), String>>,
        /// This pass's page, if one was due.
        page: Option<Result<(), String>>,
        paged: bool,
    },
    Recovered { since_ms: u64 },
}

impl Outcome {
    /// The scheduler is not running as it should: the pass must not report success.
    pub fn scheduler_down(&self) -> bool {
        matches!(self, Outcome::Down { .. })
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let res = |r: &Result<(), String>| r.as_ref().map_or_else(|e| format!("failed ({e})"), |_| "ok".into());
        match self {
            Outcome::Healthy => write!(f, "healthy"),
            Outcome::Unknown(why) => write!(f, "cannot tell: {why}"),
            Outcome::Down { since_ms, reason, starts, started, page, paged } => {
                write!(f, "incident open since {since_ms}: {reason}; starts {starts}")?;
                if let Some(r) = started {
                    write!(f, "; reset-failed+start this pass {}", res(r))?;
                }
                if let Some(r) = page {
                    write!(f, "; page {}", res(r))?;
                }
                write!(f, "; paged {paged}")
            }
            Outcome::Recovered { since_ms } => write!(f, "recovered from the incident opened at {since_ms}"),
        }
    }
}

pub struct Deps<'a> {
    pub clock: &'a dyn Clock,
    pub units: &'a dyn Units,
    pub status: &'a dyn StatusSource,
    pub marker: &'a dyn MarkerSource,
    pub pager: &'a dyn Pager,
}

/// Bring the unit up: a unit that is not running has its failed state (and with it systemd's
/// start-limit count) forgotten, then is started; a running one that is wedged is restarted.
fn kick(units: &dyn Units, h: &UnitHandle, state: UnitState) -> Result<(), String> {
    match state {
        UnitState::Absent | UnitState::Inactive | UnitState::Failed => {
            let reset = units.reset_failed(h).map_err(|e| format!("reset-failed: {}", e.0));
            units.start_installed(h).map_err(|e| format!("start: {}", e.0)).and(reset)
        }
        UnitState::Activating | UnitState::Deactivating | UnitState::Active => units.restart(h).map_err(|e| format!("restart: {}", e.0)),
    }
}

/// One pass. `Err` is a pass that could not look (the unit's facts unreadable, the incident file
/// unwritable); it changes nothing.
///
/// While the scheduler is down, every pass whose backoff is due resets and starts it again, so
/// systemd's start limit never ends the retries; once [`PAGE_AFTER_STARTS`] starts have not
/// brought it back, the incident pages once with the unit's own last log lines. A page that
/// fails is retried on the next pass and never counted as paged.
pub fn pass(cfg: &Config, d: &Deps) -> Result<Outcome, String> {
    let now = d.clock.now_ms();
    let h = UnitHandle::named(&cfg.unit);
    let detail = d.units.detail(&h).map_err(|e| format!("{}: facts: {e}", cfg.unit))?;
    let facts = detail.facts();
    let open = load(&cfg.incident)?;
    let verdict = decide(now, cfg.bound_ms, &cfg.unit, &facts, &d.status.read(), d.marker.in_flight_since());
    let reason = match verdict {
        Verdict::Healthy => {
            return Ok(match open {
                Some(i) => {
                    clear(&cfg.incident)?;
                    Outcome::Recovered { since_ms: i.since_ms }
                }
                None => Outcome::Healthy,
            });
        }
        Verdict::Unknown(why) => return Ok(Outcome::Unknown(why)),
        Verdict::Absent(r) | Verdict::Wedged(r) => r,
    };
    let mut i = open.unwrap_or(Incident { since_ms: now, reason: reason.clone(), starts: 0, last_start_ms: None, last_error: None, paged: false, page_error: None });
    let page = (!i.paged && i.starts >= PAGE_AFTER_STARTS).then(|| {
        let r = d.pager.page(&subject(cfg), &body(cfg, &reason, &i, &detail));
        i.paged = r.is_ok();
        i.page_error = r.as_ref().err().cloned();
        r
    });
    let due = match i.last_start_ms {
        None => true,
        Some(t) => now.saturating_sub(t) + BACKOFF_SLACK_MS >= backoff_ms(i.starts),
    };
    let started = due.then(|| {
        let r = kick(d.units, &h, facts.state);
        i.starts += 1;
        i.last_start_ms = Some(now);
        i.last_error = r.as_ref().err().cloned();
        r
    });
    save(&cfg.incident, &i)?;
    Ok(Outcome::Down { since_ms: i.since_ms, reason, starts: i.starts, started, page, paged: i.paged })
}

fn subject(cfg: &Config) -> String {
    format!("spira-watchdog: scheduler {} is DOWN", cfg.unit)
}

/// One line: the inbox is line-oriented. The evidence is the unit's own words — its state and
/// result as the service manager reports them, then the last lines it wrote to its log.
fn body(cfg: &Config, reason: &str, i: &Incident, d: &runtime::units::UnitDetail) -> String {
    let word = |o: &Option<String>| o.clone().unwrap_or_else(|| "?".into());
    let mins = (i.last_start_ms.unwrap_or(i.since_ms).saturating_sub(i.since_ms)) / 60_000;
    let mut s = format!(
        "{reason}; {} reset-failed+start attempts over {mins} min have not brought it back; unit active={} sub={} result={} n_restarts={}",
        i.starts,
        word(&d.active_state),
        word(&d.sub_state),
        word(&d.result),
        d.n_restarts.map_or_else(|| "?".into(), |n| n.to_string()),
    );
    if let Some(e) = &i.last_error {
        s.push_str(&format!("; last start refused: {e}"));
    }
    match log_tail(&cfg.unit_log, LOG_TAIL_LINES) {
        Ok(lines) if lines.is_empty() => s.push_str(&format!("; {} is empty", cfg.unit_log.display())),
        Ok(lines) => s.push_str(&format!("; last lines of {}: {}", cfg.unit_log.display(), lines.join(" ⏎ "))),
        Err(e) => s.push_str(&format!("; unit log unreadable: {e}")),
    }
    s.push_str(&format!("; the watchdog keeps retrying with backoff (cap {}s); this incident pages once and the next healthy pass closes it", BACKOFF_CAP_MS / 1000));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::units::{Exit, ScriptedUnits};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Arc;

    const UNIT: &str = "spira-runtime.service";

    #[test]
    fn every_key_the_watchdog_and_its_timer_read_is_registered_in_conf_d() {
        let conf_d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
        let missing = |keys: &[&str]| keys.iter().filter(|k| !conf_d.join(k).is_file()).map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(missing(&["SPIRA_WATCHDOG_PLANTED_UNREGISTERED"]).len(), 1, "positive control: an unregistered key is reported");
        let all: Vec<&str> = KEYS.iter().copied().chain([KEY_EVERY]).collect();
        assert!(missing(&all).is_empty(), "keys the watchdog reads but spira/conf.d does not register: {:?}", missing(&all));
    }

    struct Fixed(RefCell<Reading>);
    impl StatusSource for Fixed {
        fn read(&self) -> Reading {
            self.0.borrow().clone()
        }
    }
    struct Marker(Option<u64>);
    impl MarkerSource for Marker {
        fn in_flight_since(&self) -> Option<u64> {
            self.0
        }
    }
    #[derive(Default)]
    struct Pages(RefCell<Vec<String>>, RefCell<bool>);
    impl Pager for Pages {
        fn page(&self, subject: &str, _body: &str) -> Result<(), String> {
            if *self.1.borrow() {
                return Err("mail down".into());
            }
            self.0.borrow_mut().push(subject.to_string());
            Ok(())
        }
    }

    fn cfg(dir: &Path) -> Config {
        let m: HashMap<&str, String> = [(KEY_BOUND, "90".to_string()), (KEY_SOCKET, "/nonexistent/control.sock".into()), (KEY_UNIT, UNIT.into()), (KEY_RUN, dir.display().to_string()), (KEY_UNIT_LOG, dir.join("spira-runtime.log").display().to_string())].into();
        Config::from_lookup(|k| m.get(k).cloned().ok_or_else(|| format!("{k} unset"))).unwrap()
    }

    fn active(since: u64) -> UnitFacts {
        UnitFacts { state: UnitState::Active, main_pid: Some(7), exit: None, since_ms: Some(since) }
    }

    struct World {
        clock: VirtualClock,
        units: ScriptedUnits,
        status: Fixed,
        marker: Marker,
        pages: Pages,
    }

    impl World {
        fn new(now: u64, facts: UnitFacts, reading: Reading) -> World {
            let clock = VirtualClock::new(now);
            let units = ScriptedUnits::new(Arc::new(clock.clone()));
            units.install(UNIT, facts);
            World { clock, units, status: Fixed(RefCell::new(reading)), marker: Marker(None), pages: Pages::default() }
        }
        fn pass(&self, c: &Config) -> Outcome {
            pass(c, &Deps { clock: &self.clock, units: &self.units, status: &self.status, marker: &self.marker, pager: &self.pages }).unwrap()
        }
    }

    #[test]
    fn every_key_is_required_and_the_bound_must_be_positive() {
        let full: HashMap<&str, &str> = [(KEY_BOUND, "90"), (KEY_SOCKET, "/s"), (KEY_UNIT, "u"), (KEY_RUN, "/r"), (KEY_UNIT_LOG, "/r/u.log")].into();
        for missing in KEYS {
            let err = Config::from_lookup(|k| if k == missing { Err(format!("{k} unset")) } else { Ok(full[k].to_string()) }).unwrap_err();
            assert!(err.contains(missing), "{err}");
            let err = Config::from_lookup(|k| Ok(if k == missing { String::new() } else { full[k].to_string() })).unwrap_err();
            assert!(err.contains(missing), "{err}");
        }
        for bad in ["0", "ninety", "-1"] {
            assert!(Config::from_lookup(|k| Ok(if k == KEY_BOUND { bad.to_string() } else { full[k].to_string() })).is_err(), "{bad}");
        }
        let c = Config::from_lookup(|k| Ok(full[k].to_string())).unwrap();
        assert_eq!((c.bound_ms, c.marker.clone()), (90_000, PathBuf::from("/r/runtime-inflight.json")));
    }

    fn started(o: &Outcome) -> Option<Result<(), String>> {
        match o {
            Outcome::Down { started, .. } => started.clone(),
            _ => None,
        }
    }

    #[test]
    fn a_wedged_scheduler_is_restarted_with_backoff_and_paged_once_per_incident() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(1_000_000, active(0), Reading::Turn { at_ms: 1_000_000 - 91_000 });
        let o = w.pass(&c);
        assert!(o.scheduler_down() && started(&o) == Some(Ok(())), "{o}");
        assert_eq!(w.units.calls(), vec![format!("restart {UNIT}")]);
        assert!(w.pages.0.borrow().is_empty(), "the first restart may yet bring it back: no page");
        for _ in 0..4 {
            w.clock.advance(60_000);
            assert!(w.pass(&c).scheduler_down());
        }
        assert_eq!(w.units.calls().len(), 3, "passes at 0..240 s restart at 0, 60 and 180 s, not every pass");
        assert_eq!(w.pages.0.borrow().len(), 1, "one page in the incident");
        *w.status.0.borrow_mut() = Reading::Turn { at_ms: w.clock.now_ms() };
        assert_eq!(w.pass(&c), Outcome::Recovered { since_ms: 1_000_000 });
        assert!(!w.pass(&c).scheduler_down());
    }

    #[test]
    fn a_healthy_scheduler_is_left_alone() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(1_000_000, active(0), Reading::Turn { at_ms: 1_000_000 - 90_000 });
        assert_eq!(w.pass(&c), Outcome::Healthy);
        assert!(w.units.calls().is_empty());
        assert!(w.pages.0.borrow().is_empty());
        assert!(!c.incident.exists());
    }

    #[test]
    fn a_healthy_pass_closes_the_incident_and_the_next_one_starts_fresh() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(500_000, UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Code(1)), since_ms: Some(1) }, Reading::Unreachable("refused".into()));
        assert_eq!(started(&w.pass(&c)), Some(Ok(())));
        assert_eq!(w.units.calls(), vec![format!("reset-failed {UNIT}"), format!("start {UNIT}")]);
        *w.status.0.borrow_mut() = Reading::Turn { at_ms: 500_000 };
        assert_eq!(w.pass(&c), Outcome::Recovered { since_ms: 500_000 });
        w.clock.advance(200_000);
        let o = w.pass(&c);
        assert!(matches!(&o, Outcome::Down { since_ms: 700_000, starts: 1, .. }), "{o}");
        assert_eq!(w.units.calls().last(), Some(&format!("restart {UNIT}")));
    }

    #[test]
    fn a_failed_page_is_retried_and_never_counted_as_paged() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(500_000, UnitFacts::absent(), Reading::Unreachable("refused".into()));
        *w.pages.1.borrow_mut() = true;
        let mut outs = Vec::new();
        for _ in 0..3 {
            outs.push(w.pass(&c));
            w.clock.advance(60_000);
        }
        assert!(outs.iter().all(Outcome::scheduler_down), "{outs:?}");
        assert!(matches!(started(&outs[0]), Some(Err(_))), "an uninstalled unit cannot be started: {}", outs[0]);
        assert!(matches!(&outs[2], Outcome::Down { page: Some(Err(_)), paged: false, .. }), "{}", outs[2]);
        *w.pages.1.borrow_mut() = false;
        assert!(matches!(w.pass(&c), Outcome::Down { page: Some(Ok(())), paged: true, .. }));
        w.clock.advance(60_000);
        assert!(matches!(w.pass(&c), Outcome::Down { page: None, paged: true, .. }));
        assert_eq!(w.pages.0.borrow().len(), 1);
    }

    #[test]
    fn the_backoff_doubles_to_its_cap_and_the_log_tail_is_bounded() {
        assert_eq!((1..=7).map(backoff_ms).collect::<Vec<_>>(), vec![60_000, 120_000, 240_000, 480_000, 900_000, 900_000, 900_000]);
        assert_eq!(backoff_ms(u32::MAX), BACKOFF_CAP_MS);
        let tmp = testkit::TempDir::new("watchdog");
        let p = tmp.path().join("u.log");
        assert!(log_tail(&p, 3).is_err());
        let long = "x".repeat(LOG_LINE_MAX + 50);
        std::fs::write(&p, format!("{}a\n\nb\n{long}\nc\n", "filler line\n".repeat(10_000))).unwrap();
        let t = log_tail(&p, 3).unwrap();
        assert_eq!((t[0].as_str(), t[2].as_str()), ("b", "c"));
        assert_eq!(t[1].chars().count(), LOG_LINE_MAX + 1);
    }

    #[test]
    fn the_verdict_reads_the_turn_the_start_and_the_marker() {
        let b = 90_000;
        let now = 1_000_000;
        assert_eq!(decide(now, b, UNIT, &active(0), &Reading::NoTurnYet, None), Verdict::Wedged(format!("no turn in the 1000s since {UNIT} started, bound 90s")));
        assert_eq!(decide(now, b, UNIT, &active(now - 10_000), &Reading::NoTurnYet, None), Verdict::Healthy);
        assert_eq!(decide(now, b, UNIT, &active(0), &Reading::Unreachable("x".into()), None), Verdict::Unknown("x".into()));
        assert_eq!(decide(now, b, UNIT, &active(0), &Reading::Unreachable("x".into()), Some(now - 30_000)), Verdict::Unknown("x".into()));
        assert!(matches!(decide(now, b, UNIT, &active(0), &Reading::Unreachable("x".into()), Some(now - 91_000)), Verdict::Wedged(_)));
        let starting = UnitFacts { state: UnitState::Activating, main_pid: None, exit: None, since_ms: Some(now - 91_000) };
        assert!(matches!(decide(now, b, UNIT, &starting, &Reading::NoTurnYet, None), Verdict::Wedged(_)));
        assert_eq!(decide(now, b, UNIT, &UnitFacts::absent(), &Reading::Turn { at_ms: now }, None), Verdict::Absent(format!("{UNIT} is absent")));
    }

    /// The service manager as it behaved on 2026-11-07: a unit that hit its start limit stays
    /// failed through any start or restart (the job is accepted, then refused) until
    /// `reset-failed`; with `crashes` set, every start that does get through fails again.
    struct StartLimited {
        inner: ScriptedUnits,
        clock: VirtualClock,
        limited: std::sync::atomic::AtomicBool,
        crashes: bool,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl StartLimited {
        fn new(clock: &VirtualClock, crashes: bool) -> StartLimited {
            let inner = ScriptedUnits::new(Arc::new(clock.clone()));
            let s = StartLimited { inner, clock: clock.clone(), limited: true.into(), crashes, calls: Default::default() };
            s.fail();
            s
        }
        fn fail(&self) {
            let d = runtime::units::UnitDetail {
                result: Some("start-limit-hit".into()),
                n_restarts: Some(6),
                ..runtime::units::UnitDetail::from_facts(UNIT, &UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Code(1)), since_ms: Some(self.clock.now_ms()) })
            };
            self.inner.install_detail(runtime::units::UnitDetail { active_state: Some("failed".into()), sub_state: Some("failed".into()), load_state: Some("loaded".into()), ..d });
            self.limited.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn go(&self, verb: &str, h: &UnitHandle) -> runtime::Result<()> {
            self.calls.lock().unwrap().push(format!("{verb} {}", h.name));
            if self.limited.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            let r = if verb == "restart" { self.inner.restart(h) } else { self.inner.start_installed(h) };
            if self.crashes {
                self.fail();
            }
            r
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Units for StartLimited {
        fn start(&self, spec: &runtime::units::UnitSpec) -> runtime::Result<UnitHandle> {
            self.inner.start(spec)
        }
        fn stop(&self, h: &UnitHandle) -> runtime::Result<()> {
            self.inner.stop(h)
        }
        fn restart(&self, h: &UnitHandle) -> runtime::Result<()> {
            self.go("restart", h)
        }
        fn facts(&self, h: &UnitHandle) -> runtime::Result<UnitFacts> {
            self.inner.facts(h)
        }
        fn detail(&self, h: &UnitHandle) -> runtime::Result<runtime::units::UnitDetail> {
            self.inner.detail(h)
        }
        fn start_installed(&self, h: &UnitHandle) -> runtime::Result<()> {
            self.go("start", h)
        }
        fn reset_failed(&self, h: &UnitHandle) -> runtime::Result<()> {
            self.calls.lock().unwrap().push(format!("reset-failed {}", h.name));
            self.limited.store(false, std::sync::atomic::Ordering::SeqCst);
            self.inner.reset_failed(h)
        }
    }

    #[derive(Default)]
    struct Bodies(RefCell<Vec<String>>, RefCell<bool>);
    impl Pager for Bodies {
        fn page(&self, subject: &str, body: &str) -> Result<(), String> {
            if *self.1.borrow() {
                return Err("inbox unwritable".into());
            }
            self.0.borrow_mut().push(format!("{subject}\n{body}"));
            Ok(())
        }
    }

    fn run_pass(c: &Config, clock: &VirtualClock, units: &dyn Units, status: &Fixed, pages: &dyn Pager) -> Outcome {
        pass(c, &Deps { clock, units, status, marker: &Marker(None), pager: pages }).unwrap()
    }

    #[test]
    fn a_unit_failed_at_its_start_limit_is_reset_then_started() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let clock = VirtualClock::new(1_000_000);
        let units = StartLimited::new(&clock, false);
        let status = Fixed(RefCell::new(Reading::Unreachable("connection refused".into())));
        let pages = Bodies::default();
        run_pass(&c, &clock, &units, &status, &pages);
        let calls = units.calls();
        let reset = calls.iter().position(|c| c == &format!("reset-failed {UNIT}"));
        let start = calls.iter().position(|c| c == &format!("start {UNIT}"));
        assert!(matches!((reset, start), (Some(r), Some(s)) if r < s), "reset-failed, then start: {calls:?}");
        assert_eq!(units.inner.facts(&UnitHandle::named(UNIT)).unwrap().state, UnitState::Active);
        clock.advance(60_000);
        *status.0.borrow_mut() = Reading::Turn { at_ms: clock.now_ms() };
        assert_eq!(run_pass(&c, &clock, &units, &status, &pages), Outcome::Recovered { since_ms: 1_000_000 });
        assert!(pages.0.borrow().is_empty(), "a start that worked first time pages no one: {:?}", pages.0.borrow());
    }

    #[test]
    fn a_unit_that_keeps_failing_is_retried_with_backoff_and_paged_once_with_its_log() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        std::fs::write(tmp.path().join("spira-runtime.log"), "spira-runtime: serve: starting\nspira-runtime: SPIRA_TASK_MODES gives no mode for task watch.throughput\n").unwrap();
        let clock = VirtualClock::new(1_000_000);
        let units = StartLimited::new(&clock, true);
        let status = Fixed(RefCell::new(Reading::Unreachable("connection refused".into())));
        let pages = Bodies::default();
        let mut starts_at = Vec::new();
        for _ in 0..=30 {
            let before = units.calls().iter().filter(|c| c.starts_with("start ")).count();
            assert!(run_pass(&c, &clock, &units, &status, &pages).scheduler_down());
            if units.calls().iter().filter(|c| c.starts_with("start ")).count() > before {
                starts_at.push((clock.now_ms() - 1_000_000) / 1000);
            }
            clock.advance(60_000);
        }
        assert_eq!(starts_at, vec![0, 60, 180, 420, 900, 1800], "starts back off, doubling to a cap");
        assert_eq!(units.calls().iter().filter(|c| c.starts_with("reset-failed ")).count(), starts_at.len(), "every start is preceded by reset-failed");
        let pages = pages.0.borrow();
        assert_eq!(pages.len(), 1, "one page per incident: {pages:?}");
        for want in ["SPIRA_TASK_MODES gives no mode for task watch.throughput", "start-limit-hit", UNIT] {
            assert!(pages[0].contains(want), "the page carries {want:?}: {}", pages[0]);
        }
    }

    #[test]
    fn a_status_document_yields_its_turn_time() {
        assert_eq!(turn_of(r#"{"schema":1,"at":1234,"halted":false,"tasks":[]}"#), Ok(1234));
        assert!(turn_of(r#"{"schema":1}"#).is_err());
        assert!(turn_of("nope").is_err());
    }

    #[test]
    fn the_marker_age_is_the_files_mtime() {
        let tmp = testkit::TempDir::new("watchdog");
        let p = tmp.path().join("runtime-inflight.json");
        assert_eq!(FileMarkerAge(p.clone()).in_flight_since(), None);
        std::fs::write(&p, "{}").unwrap();
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_000);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        assert_eq!(FileMarkerAge(p).in_flight_since(), Some(1_700_000_000_000));
    }

    #[test]
    fn an_unanswering_socket_reads_unreachable() {
        assert!(matches!(SocketStatus(PathBuf::from("/nonexistent/control.sock")).read(), Reading::Unreachable(_)));
    }
}
