//! A testenv run as a state machine: the phases a run passes through, each suite's record and
//! the verdict, as events in `run.events` beside the results. An illegal move is refused
//! naming the state and nothing is written; `testenv status` reads the recorded events and
//! nothing else, so no reader parses `<suite>.result` or the run's output.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const EVENTS_FILE: &str = "run.events";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Phase { name: String },
    Suite { name: String, status: String },
    Verdict { word: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Started,
    Setup(String),
    Verdict(String),
}

impl RunState {
    pub fn name(&self) -> String {
        match self {
            RunState::Started => "started".into(),
            RunState::Setup(p) => format!("setup({p})"),
            RunState::Verdict(w) => format!("verdict({w})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub event: RunEvent,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub events: Vec<Recorded>,
}

impl Run {
    pub fn state(&self) -> RunState {
        let mut s = RunState::Started;
        for r in &self.events {
            match &r.event {
                RunEvent::Phase { name } => s = RunState::Setup(name.clone()),
                RunEvent::Verdict { word, .. } => s = RunState::Verdict(word.clone()),
                RunEvent::Suite { .. } => {}
            }
        }
        s
    }

    pub fn suites(&self) -> BTreeMap<String, String> {
        self.events
            .iter()
            .filter_map(|r| match &r.event {
                RunEvent::Suite { name, status } => Some((name.clone(), status.clone())),
                _ => None,
            })
            .collect()
    }

    fn phase_seen(&self, name: &str) -> bool {
        self.events.iter().any(|r| matches!(&r.event, RunEvent::Phase { name: n } if n == name))
    }

    pub fn refusal(&self, event: &RunEvent) -> Option<String> {
        let state = self.state();
        let bad = match (&state, event) {
            (RunState::Verdict(_), _) => true,
            (_, RunEvent::Phase { name }) => name.is_empty() || self.phase_seen(name),
            (_, RunEvent::Suite { name, status }) => name.is_empty() || status.is_empty() || self.suites().contains_key(name),
            (_, RunEvent::Verdict { word, .. }) => word.is_empty(),
        };
        bad.then(|| format!("run is {}; it cannot take {}", state.name(), describe(event)))
    }

    pub fn record(&mut self, event: RunEvent, at: u64) -> Result<(), String> {
        if let Some(e) = self.refusal(&event) {
            return Err(e);
        }
        self.events.push(Recorded { event, at });
        Ok(())
    }

    pub fn status_json(&self) -> Value {
        let suites: Value = self.suites().into_iter().map(|(k, v)| (k, Value::String(v))).collect::<serde_json::Map<_, _>>().into();
        let verdict = self.events.iter().rev().find_map(|r| match &r.event {
            RunEvent::Verdict { word, reason } => Some(json!({"word": word, "reason": reason, "at": r.at})),
            _ => None,
        });
        let phases: Vec<Value> = self
            .events
            .iter()
            .filter_map(|r| match &r.event {
                RunEvent::Phase { name } => Some(json!({"name": name, "at": r.at})),
                _ => None,
            })
            .collect();
        json!({
            "state": self.state().name(),
            "since": self.events.last().map_or(0, |r| r.at),
            "phases": phases,
            "suites": suites,
            "verdict": verdict,
        })
    }
}

fn describe(e: &RunEvent) -> String {
    match e {
        RunEvent::Phase { name } => format!("phase {name}"),
        RunEvent::Suite { name, .. } => format!("suite {name}"),
        RunEvent::Verdict { word, .. } => format!("verdict {word}"),
    }
}

fn clean(s: &str) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join("_");
    if t.is_empty() { "-".into() } else { t }
}

fn render(r: &Recorded) -> String {
    match &r.event {
        RunEvent::Phase { name } => format!("{} phase {}", r.at, clean(name)),
        RunEvent::Suite { name, status } => format!("{} suite {} {}", r.at, clean(name), clean(status)),
        RunEvent::Verdict { word, reason } => format!("{} verdict {} {}", r.at, clean(word), clean(reason)),
    }
}

fn parse_line(line: &str) -> Option<Recorded> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let at = f.first()?.parse().ok()?;
    let event = match (*f.get(1)?, f.get(2), f.get(3)) {
        ("phase", Some(n), _) => RunEvent::Phase { name: n.to_string() },
        ("suite", Some(n), Some(s)) => RunEvent::Suite { name: n.to_string(), status: s.to_string() },
        ("verdict", Some(w), r) => RunEvent::Verdict { word: w.to_string(), reason: r.map_or("-", |v| v).to_string() },
        _ => return None,
    };
    Some(Recorded { event, at })
}

/// A run's event file. Reading refuses a line it cannot parse rather than skipping it.
#[derive(Debug, Clone)]
pub struct RunLog {
    path: PathBuf,
}

impl RunLog {
    pub fn in_dir(results: &Path) -> RunLog {
        RunLog { path: results.join(EVENTS_FILE) }
    }

    /// Begins a run: whatever an earlier run of this key left is not this run's record.
    pub fn begin(results: &Path) -> RunLog {
        let log = RunLog::in_dir(results);
        let _ = fs::remove_file(&log.path);
        log
    }

    pub fn load(&self) -> Result<Run, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Run::default()),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let mut run = Run::default();
        for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            run.events.push(parse_line(l).ok_or_else(|| format!("{}:{}: not a run event", self.path.display(), i + 1))?);
        }
        Ok(run)
    }

    pub fn record(&self, event: RunEvent, at: u64) -> Result<(), String> {
        let mut run = self.load()?;
        run.record(event, at)?;
        let line = render(run.events.last().expect("just recorded"));
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        writeln!(f, "{line}").map_err(|e| e.to_string())
    }
}

const USAGE: &str = "usage: testenv status [--json] <results-dir>";

/// `testenv status [--json] <results-dir>`: the run's recorded state.
pub fn main(args: &[String]) -> i32 {
    let (mut json_out, mut dir) = (false, None);
    for a in args {
        match a.as_str() {
            "--json" => json_out = true,
            s if s.starts_with('-') || dir.is_some() => {
                eprintln!("{USAGE}");
                return 2;
            }
            _ => dir = Some(a.clone()),
        }
    }
    let Some(dir) = dir else {
        eprintln!("{USAGE}");
        return 2;
    };
    let run = match RunLog::in_dir(Path::new(&dir)).load() {
        Ok(r) if !r.events.is_empty() => r,
        Ok(_) => {
            eprintln!("testenv: {dir} has no recorded events");
            return 1;
        }
        Err(e) => {
            eprintln!("testenv: {e}");
            return 1;
        }
    };
    if json_out {
        println!("{}", run.status_json());
    } else {
        let v = run.status_json();
        println!("{} suites={}", v["state"].as_str().unwrap_or(""), run.suites().len());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(n: &str) -> RunEvent {
        RunEvent::Phase { name: n.into() }
    }
    fn suite(n: &str, s: &str) -> RunEvent {
        RunEvent::Suite { name: n.into(), status: s.into() }
    }
    fn verdict(w: &str) -> RunEvent {
        RunEvent::Verdict { word: w.into(), reason: "-".into() }
    }
    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(tag)
    }

    #[test]
    fn every_legal_move_applies_and_is_recorded() {
        let d = tmp("legal");
        let log = RunLog::begin(&d);
        for (i, e) in [phase("build"), phase("up"), suite("test-a.sh", "ok"), suite("test-b.sh", "red"), phase("suites"), verdict("RED")].into_iter().enumerate() {
            log.record(e, 100 + i as u64).unwrap();
        }
        let run = log.load().unwrap();
        assert_eq!(run.events.len(), 6);
        assert_eq!(run.state(), RunState::Verdict("RED".into()));
        assert_eq!(run.suites().get("test-b.sh").map(String::as_str), Some("red"));
    }

    #[test]
    fn an_illegal_move_is_refused_naming_the_state_and_writes_nothing() {
        let d = tmp("illegal");
        let log = RunLog::begin(&d);
        log.record(phase("build"), 1).unwrap();
        let e = log.record(phase("build"), 2).unwrap_err();
        assert!(e.contains("setup(build)"), "{e}");
        log.record(verdict("GREEN"), 3).unwrap();
        let before = fs::read_to_string(d.join(EVENTS_FILE)).unwrap();
        for ev in [phase("up"), suite("test-a.sh", "ok"), verdict("RED")] {
            let e = log.record(ev, 4).unwrap_err();
            assert!(e.contains("verdict(GREEN)"), "{e}");
        }
        assert_eq!(fs::read_to_string(d.join(EVENTS_FILE)).unwrap(), before);
    }

    #[test]
    fn the_read_is_exactly_the_recorded_state() {
        let d = tmp("read");
        let log = RunLog::begin(&d);
        assert_eq!(log.load().unwrap().state(), RunState::Started);
        log.record(phase("up"), 5).unwrap();
        log.record(suite("test-a.sh", "ok"), 6).unwrap();
        let v = log.load().unwrap().status_json();
        assert_eq!(v["state"], "setup(up)");
        assert_eq!(v["suites"]["test-a.sh"], "ok");
        assert_eq!(v["since"], 6);
        assert!(v["verdict"].is_null());
        assert_eq!(main(&["--json".into(), d.display().to_string()]), 0);
    }

    #[test]
    fn begin_discards_an_earlier_runs_record_and_a_garbled_line_refuses() {
        let d = tmp("begin");
        RunLog::begin(&d).record(verdict("GREEN"), 1).unwrap();
        let log = RunLog::begin(&d);
        assert!(log.load().unwrap().events.is_empty());
        fs::write(d.join(EVENTS_FILE), "garbage\n").unwrap();
        assert!(log.load().is_err());
        assert_eq!(main(&[d.display().to_string()]), 1);
    }
}
