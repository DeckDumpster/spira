//! cert-sweep core: verdicts, the history row, and the classification that turns a new result
//! plus the history into at most one event. Pure; the IO seam is main.rs.

use serde_json::Value;

pub const FAMILY: &str = "cert-history";
pub const EVENT_FAMILY: &str = "cert-event";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Red,
    /// The suite ran and podman lost its exit status: no attribution signal either way.
    Fault,
    Skip,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Red => "red",
            Verdict::Fault => "fault",
            Verdict::Skip => "skip",
        }
    }

    pub fn parse(s: &str) -> Option<Verdict> {
        match s {
            "ok" => Some(Verdict::Ok),
            "red" => Some(Verdict::Red),
            "fault" => Some(Verdict::Fault),
            "skip" => Some(Verdict::Skip),
            _ => None,
        }
    }

    /// testenv's own status vocabulary (a `.result` first word, or the stdout column) folded
    /// to the four states the history keeps. A status that executed nothing is a skip; an
    /// unknown word is None rather than a guess.
    pub fn from_status(s: &str) -> Option<Verdict> {
        match s.to_ascii_lowercase().as_str() {
            "ok" => Some(Verdict::Ok),
            "red" | "timeout" | "quarantined-red" => Some(Verdict::Red),
            "fault" => Some(Verdict::Fault),
            "skip" | "skipped" | "disabled" | "skip-req" | "unreached" | "deferred" => Some(Verdict::Skip),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub suite: String,
    pub verdict: Verdict,
    pub secs: Option<u64>,
}

/// `<suite>.result` is `<status> <ran> <secs> ...`; `name` is the file's stem.
pub fn parse_result_file(name: &str, text: &str) -> Option<Outcome> {
    let mut f = text.split_whitespace();
    let verdict = Verdict::from_status(f.next()?)?;
    let _ran = f.next();
    let secs = f.next().and_then(|s| s.parse().ok());
    Some(Outcome { suite: name.to_string(), verdict, secs })
}

/// testenv's per-suite stdout lines: `  test-x.sh    ok      7s`, `RED     rc=1 after 3s`,
/// `FAULT   ... after 246s`, `SKIPPED`. Anything else is not a result line. A suite that
/// printed no seconds has none recorded; a fault line's `after Ns` is read the same way.
pub fn parse_testenv_stdout(text: &str) -> Vec<Outcome> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(suite), Some(status)) = (f.next(), f.next()) else { continue };
        if !(suite.starts_with("test-") && suite.ends_with(".sh")) {
            continue;
        }
        let Some(verdict) = Verdict::from_status(status) else { continue };
        let secs = line
            .split_whitespace()
            .filter_map(|w| w.strip_suffix('s'))
            .filter_map(|n| n.parse().ok())
            .last();
        out.push(Outcome { suite: suite.to_string(), verdict, secs });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub commit: String,
    pub round: String,
    pub suite: String,
    pub verdict: Verdict,
    pub secs: Option<u64>,
    pub mode: String,
}

impl Row {
    pub fn fields(&self) -> Vec<(String, Value)> {
        let mut v = vec![
            ("commit".to_string(), Value::String(self.commit.clone())),
            ("round".to_string(), Value::String(self.round.clone())),
            ("suite".to_string(), Value::String(self.suite.clone())),
            ("verdict".to_string(), Value::String(self.verdict.as_str().to_string())),
            ("mode".to_string(), Value::String(self.mode.clone())),
        ];
        if let Some(s) = self.secs {
            v.push(("secs".to_string(), Value::from(s)));
        }
        v
    }

    /// A row that does not parse is skipped by the reader: history is append-only and a
    /// reader must survive a line it cannot place.
    pub fn from_json(line: &str) -> Option<Row> {
        let v: Value = serde_json::from_str(line).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Row {
            commit: s("commit")?,
            round: s("round")?,
            suite: s("suite")?,
            verdict: Verdict::parse(&s("verdict")?)?,
            secs: v.get("secs").and_then(Value::as_u64),
            mode: s("mode")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A suite went red: the culprit window is (last_green, first_red].
    NewRed { suite: String, last_green_round: String, last_green_commit: String, first_red_round: String, first_red_commit: String },
    /// Red with no green anywhere in the history: no window to name.
    RedUnbounded { suite: String, round: String, commit: String },
    /// Green and red on one commit: the suite's verdict is not a function of the code.
    Flip { suite: String, round: String, commit: String },
    /// A verdict podman lost. Never part of a red window.
    Fault { suite: String, round: String, commit: String },
    /// The runner produced no results at all.
    SweepFault { mode: String, round: String, why: String },
    Summary { mode: String, round: String, ran: usize, red: usize, fault: usize, wall_secs: u64 },
}

fn short(c: &str) -> &str {
    &c[..c.len().min(9)]
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::NewRed { .. } => "new-red",
            Event::RedUnbounded { .. } => "red-unbounded",
            Event::Flip { .. } => "flip",
            Event::Fault { .. } => "fault",
            Event::SweepFault { .. } => "sweep-fault",
            Event::Summary { .. } => "summary",
        }
    }

    pub fn suite(&self) -> Option<&str> {
        match self {
            Event::NewRed { suite, .. } | Event::RedUnbounded { suite, .. } | Event::Flip { suite, .. } | Event::Fault { suite, .. } => Some(suite),
            _ => None,
        }
    }

    pub fn render(&self) -> String {
        match self {
            Event::NewRed { suite, last_green_round, last_green_commit, first_red_round, first_red_commit } => format!(
                "NEW RED {suite}: last green round {last_green_round} ({}), first red round {first_red_round} ({})",
                short(last_green_commit),
                short(first_red_commit)
            ),
            Event::RedUnbounded { suite, round, commit } => {
                format!("RED {suite} at round {round} ({}); no earlier result", short(commit))
            }
            Event::Flip { suite, round, commit } => {
                format!("FLIP {suite}: green and red on the same commit {} (round {round})", short(commit))
            }
            Event::Fault { suite, round, commit } => {
                format!("FAULT {suite} at round {round} ({}): verdict lost, not a red", short(commit))
            }
            Event::SweepFault { mode, round, why } => format!("SWEEP FAULT mode={mode} round={round}: {why}"),
            Event::Summary { mode, round, ran, red, fault, wall_secs } => {
                format!("{} round={round} ran={ran} red={red} fault={fault} wall={wall_secs}s", if mode == "full" { "FULL" } else { "SAMPLE" })
            }
        }
    }
}

/// What one new result means against the history before it. A red is an event once: when it
/// is the first red after a green. A suite already red says nothing more until it recovers.
/// Faults and skips never move a window — neither is evidence about the code.
pub fn classify(history: &[Row], new: &Row) -> Option<Event> {
    let same_suite = |r: &&Row| r.suite == new.suite;
    let on_commit = |r: &&Row| r.commit == new.commit;
    let mk = |row: &Row| (row.suite.clone(), row.round.clone(), row.commit.clone());
    match new.verdict {
        Verdict::Skip => None,
        Verdict::Fault => {
            let (suite, round, commit) = mk(new);
            Some(Event::Fault { suite, round, commit })
        }
        Verdict::Ok | Verdict::Red => {
            let opposite = if new.verdict == Verdict::Ok { Verdict::Red } else { Verdict::Ok };
            let has = |v: Verdict| history.iter().filter(same_suite).filter(on_commit).any(|r| r.verdict == v);
            if has(opposite) {
                if has(new.verdict) {
                    return None;
                }
                let (suite, round, commit) = mk(new);
                return Some(Event::Flip { suite, round, commit });
            }
            if new.verdict == Verdict::Ok {
                return None;
            }
            let last = history.iter().filter(same_suite).rfind(|r| matches!(r.verdict, Verdict::Ok | Verdict::Red));
            match last {
                Some(r) if r.verdict == Verdict::Red => None,
                Some(r) => Some(Event::NewRed {
                    suite: new.suite.clone(),
                    last_green_round: r.round.clone(),
                    last_green_commit: r.commit.clone(),
                    first_red_round: new.round.clone(),
                    first_red_commit: new.commit.clone(),
                }),
                None => {
                    let (suite, round, commit) = mk(new);
                    Some(Event::RedUnbounded { suite, round, commit })
                }
            }
        }
    }
}

/// Fold a pass's results into the history, one at a time, so a result sees the ones before
/// it. Returns the rows to append and the events they raised.
pub fn record(history: &[Row], results: &[Outcome], commit: &str, round: &str, mode: &str) -> (Vec<Row>, Vec<Event>) {
    let mut all: Vec<Row> = history.to_vec();
    let mut rows = Vec::new();
    let mut events = Vec::new();
    for r in results {
        let row = Row { commit: commit.into(), round: round.into(), suite: r.suite.clone(), verdict: r.verdict, secs: r.secs, mode: mode.into() };
        if let Some(e) = classify(&all, &row) {
            events.push(e);
        }
        all.push(row.clone());
        rows.push(row);
    }
    (rows, events)
}

/// `div`-th of `all` (at least one), chosen by a seeded shuffle: the same seed picks the
/// same suites, a fresh seed per pass resamples.
pub fn pick_subset(all: &[String], div: usize, seed: u64) -> Vec<String> {
    let n = if all.is_empty() { 0 } else { (all.len() / div.max(1)).max(1) };
    let mut v = all.to_vec();
    let mut s = seed | 1;
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v.truncate(n);
    v.sort();
    v
}

pub fn bead_title(e: &Event) -> Option<String> {
    match e {
        Event::NewRed { suite, first_red_round, .. } => Some(format!("cert-sweep: {suite} went red at round {first_red_round}")),
        Event::RedUnbounded { suite, .. } => Some(format!("cert-sweep: {suite} is red with no known-green round")),
        Event::Flip { suite, .. } => Some(format!("cert-sweep: {suite} flips on one commit — delete it")),
        _ => None,
    }
}

pub fn bead_body(e: &Event) -> String {
    let tail = match e {
        Event::Flip { .. } => "\nA suite that is green and red on one commit asserts nothing: delete it, or replace it with a deterministic check (law-a-test-that-flips-is-deleted).\n",
        Event::NewRed { .. } => "\nThe culprit is in the rounds between last green and first red. History: the cert-history tsd family.\n",
        _ => "\nHistory: the cert-history tsd family.\n",
    };
    format!("{}\n{tail}", e.render())
}

#[cfg(test)]
mod tests;
