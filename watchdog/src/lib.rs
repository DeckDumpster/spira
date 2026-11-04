//! spira-watchdog (S15): one pass per invocation, run by the platform's timer. It asks the
//! scheduler for its last turn (`scheduler status` on the control socket) and the service
//! manager for the scheduler unit's facts (the `Units` port); when the scheduler is absent or its
//! last turn is older than the bound, it restarts the unit and pages the operator, once per
//! incident.

pub mod render;

use std::path::{Path, PathBuf};

use runtime::ports::Clock;
use runtime::units::{UnitFacts, UnitHandle, UnitState, Units};
use serde::{Deserialize, Serialize};

pub const KEY_BOUND: &str = "SPIRA_WATCHDOG_BOUND_SECS";
pub const KEY_SOCKET: &str = "SPIRA_RUNTIME_SOCKET";
pub const KEY_UNIT: &str = "SPIRA_WATCHDOG_UNIT";
pub const KEY_RUN: &str = "SPIRA_RUN";
pub const KEYS: [&str; 4] = [KEY_BOUND, KEY_SOCKET, KEY_UNIT, KEY_RUN];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub bound_ms: u64,
    pub socket: PathBuf,
    pub unit: String,
    /// The scheduler's in-flight marker, read when the socket does not answer.
    pub marker: PathBuf,
    /// The open incident, so one incident pages once across passes.
    pub incident: PathBuf,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    pub since_ms: u64,
    pub reason: String,
    pub restarted: bool,
    pub paged: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Healthy,
    Unknown(String),
    /// A new incident: the restart's result and whether the page went out.
    Acted { reason: String, restart: Result<(), String>, paged: bool },
    /// The incident is already open and paged; nothing more this pass.
    Open { since_ms: u64 },
    /// The page that failed last pass, sent now.
    Repaged { paged: bool },
    Recovered { since_ms: u64 },
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Healthy => write!(f, "healthy"),
            Outcome::Unknown(why) => write!(f, "cannot tell: {why}"),
            Outcome::Acted { reason, restart, paged } => write!(f, "incident: {reason}; restart {}; paged {paged}", restart.as_ref().map_or_else(|e| format!("failed ({e})"), |_| "requested".into())),
            Outcome::Open { since_ms } => write!(f, "incident open since {since_ms}; already paged"),
            Outcome::Repaged { paged } => write!(f, "incident open; page retried, paged {paged}"),
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

/// One pass. `Err` is a pass that could not look (the unit's facts unreadable, the incident file
/// unwritable); it changes nothing.
pub fn pass(cfg: &Config, d: &Deps) -> Result<Outcome, String> {
    let now = d.clock.now_ms();
    let h = UnitHandle::named(&cfg.unit);
    let facts = d.units.facts(&h).map_err(|e| format!("{}: facts: {e}", cfg.unit))?;
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
    if let Some(mut i) = open {
        if i.paged {
            return Ok(Outcome::Open { since_ms: i.since_ms });
        }
        i.paged = d.pager.page(&subject(cfg), &body(cfg, &i.reason, i.restarted)).is_ok();
        save(&cfg.incident, &i)?;
        return Ok(Outcome::Repaged { paged: i.paged });
    }
    let restart = d.units.restart(&h).map_err(|e| e.0);
    let paged = d.pager.page(&subject(cfg), &body(cfg, &reason, restart.is_ok())).is_ok();
    save(&cfg.incident, &Incident { since_ms: now, reason: reason.clone(), restarted: restart.is_ok(), paged })?;
    Ok(Outcome::Acted { reason, restart, paged })
}

fn subject(cfg: &Config) -> String {
    format!("spira-watchdog: scheduler {} wedged or absent", cfg.unit)
}

fn body(cfg: &Config, reason: &str, restarted: bool) -> String {
    let what = if restarted { "restart requested through the service manager" } else { "the restart request FAILED; the scheduler is still down" };
    format!("## Watchdog\n{reason}.\n\n{what} for {}. This incident pages once; the next healthy pass closes it.\n", cfg.unit)
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
        let m: HashMap<&str, String> = [(KEY_BOUND, "90".to_string()), (KEY_SOCKET, "/nonexistent/control.sock".into()), (KEY_UNIT, UNIT.into()), (KEY_RUN, dir.display().to_string())].into();
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
        let full: HashMap<&str, &str> = [(KEY_BOUND, "90"), (KEY_SOCKET, "/s"), (KEY_UNIT, "u"), (KEY_RUN, "/r")].into();
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

    #[test]
    fn a_wedged_scheduler_is_restarted_and_paged_once_per_incident() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(1_000_000, active(0), Reading::Turn { at_ms: 1_000_000 - 91_000 });
        assert!(matches!(w.pass(&c), Outcome::Acted { restart: Ok(()), paged: true, .. }));
        assert_eq!(w.units.calls(), vec![format!("restart {UNIT}")]);
        assert_eq!(w.pages.0.borrow().len(), 1);
        w.clock.advance(60_000);
        w.units.install(UNIT, active(0));
        assert_eq!(w.pass(&c), Outcome::Open { since_ms: 1_000_000 });
        assert_eq!(w.units.calls().len(), 1, "no second restart in the same incident");
        assert_eq!(w.pages.0.borrow().len(), 1, "no second page in the same incident");
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
    fn a_healthy_pass_closes_the_incident_and_the_next_wedge_pages_again() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(500_000, UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(Exit::Code(1)), since_ms: Some(1) }, Reading::Unreachable("refused".into()));
        assert!(matches!(w.pass(&c), Outcome::Acted { .. }));
        *w.status.0.borrow_mut() = Reading::Turn { at_ms: 500_000 };
        assert_eq!(w.pass(&c), Outcome::Recovered { since_ms: 500_000 });
        w.clock.advance(200_000);
        assert!(matches!(w.pass(&c), Outcome::Acted { .. }));
        assert_eq!(w.pages.0.borrow().len(), 2);
        assert_eq!(w.units.calls().len(), 2);
    }

    #[test]
    fn a_failed_page_is_retried_without_a_second_restart() {
        let tmp = testkit::TempDir::new("watchdog");
        let c = cfg(tmp.path());
        let w = World::new(500_000, UnitFacts::absent(), Reading::Unreachable("refused".into()));
        *w.pages.1.borrow_mut() = true;
        assert!(matches!(w.pass(&c), Outcome::Acted { paged: false, .. }));
        w.units.install(UNIT, UnitFacts::absent());
        *w.pages.1.borrow_mut() = false;
        assert_eq!(w.pass(&c), Outcome::Repaged { paged: true });
        assert_eq!(w.pass(&c), Outcome::Open { since_ms: 500_000 });
        assert_eq!((w.units.calls().len(), w.pages.0.borrow().len()), (1, 1));
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
