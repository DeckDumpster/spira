//! The gate as a state machine: a run moves queued → admitted → rebased → fences → trial →
//! verdict, and every move is an event with a reason. `gate status <bead>` reads the record
//! back and nothing else; `gate cancel <bead>` leaves a request the run honours, so no
//! operator tool scans processes or signals a pid.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    Queued,
    Admitted,
    Rebased,
    Fences,
    Trial,
    Verdict,
}

impl GateState {
    pub const ALL: [GateState; 6] = [
        GateState::Queued,
        GateState::Admitted,
        GateState::Rebased,
        GateState::Fences,
        GateState::Trial,
        GateState::Verdict,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GateState::Queued => "queued",
            GateState::Admitted => "admitted",
            GateState::Rebased => "rebased",
            GateState::Fences => "fences",
            GateState::Trial => "trial",
            GateState::Verdict => "verdict",
        }
    }

    pub fn parse(s: &str) -> Option<GateState> {
        Self::ALL.into_iter().find(|g| g.as_str() == s)
    }

    /// A suites composition runs fences and suites as one gate command, so it goes
    /// rebased → trial; any run can end at its verdict.
    pub fn can_go(self, to: GateState) -> bool {
        use GateState::*;
        matches!(
            (self, to),
            (Queued, Admitted | Verdict)
                | (Admitted, Rebased | Verdict)
                | (Rebased, Fences | Trial | Verdict)
                | (Fences, Trial | Verdict)
                | (Trial, Verdict)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEvent {
    pub to: GateState,
    pub reason: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub branch: String,
    pub repo: String,
    pub events: Vec<GateEvent>,
}

impl Run {
    pub fn new(id: &str, branch: &str, repo: &str) -> Run {
        Run { id: id.into(), branch: branch.into(), repo: repo.into(), events: Vec::new() }
    }

    pub fn state(&self) -> Option<GateState> {
        self.events.last().map(|e| e.to)
    }

    /// Applies a move, or refuses naming the state the run is in. Nothing is written on a refusal.
    pub fn record(&mut self, to: GateState, reason: &str, at: u64) -> Result<(), String> {
        let ok = match self.state() {
            None => to == GateState::Queued,
            Some(from) => from.can_go(to),
        };
        if !ok {
            let from = self.state().map_or("unstarted", GateState::as_str);
            return Err(format!("gate: {} is {from}; it cannot become {} ({reason})", self.id, to.as_str()));
        }
        self.events.push(GateEvent { to, reason: reason.into(), at });
        Ok(())
    }

    /// `record`, except that being in `to` already is not a move.
    pub fn enter(&mut self, to: GateState, reason: &str, at: u64) -> Result<bool, String> {
        if self.state() == Some(to) {
            return Ok(false);
        }
        self.record(to, reason, at).map(|()| true)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "branch": self.branch,
            "repo": self.repo,
            "events": self.events.iter().map(|e| json!({"to": e.to.as_str(), "reason": e.reason, "at": e.at})).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(text: &str) -> Result<Run, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not a gate record: {e}"))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("gate record has no {k}"));
        let mut events = Vec::new();
        for e in v.get("events").and_then(Value::as_array).ok_or("gate record has no events")? {
            let to = e.get("to").and_then(Value::as_str).and_then(GateState::parse).ok_or("gate record has an event with an unknown state")?;
            events.push(GateEvent {
                to,
                reason: e.get("reason").and_then(Value::as_str).unwrap_or_default().to_string(),
                at: e.get("at").and_then(Value::as_u64).unwrap_or(0),
            });
        }
        Ok(Run { id: s("id")?, branch: s("branch")?, repo: s("repo")?, events })
    }

    /// The read: exactly the recorded state, how long it has held, and the events behind it.
    pub fn status_json(&self, now: u64) -> Value {
        let last = self.events.last();
        json!({
            "id": self.id,
            "branch": self.branch,
            "repo": self.repo,
            "state": self.state().map_or("unstarted", GateState::as_str),
            "reason": last.map(|e| e.reason.as_str()).unwrap_or(""),
            "since": last.map_or(0, |e| e.at),
            "age_s": last.map_or(0, |e| now.saturating_sub(e.at)),
            "cancel_requested": false,
            "events": self.to_json()["events"].clone(),
        })
    }
}

/// The file name a bead or branch name gets: one path component.
pub fn id_for(bead: &str, branch: &str) -> String {
    let raw = if bead.is_empty() { branch } else { bead };
    raw.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') { c } else { '_' }).collect()
}

pub fn dir(run: &Path) -> PathBuf {
    run.join("gate-machine")
}

pub fn record_path(run: &Path, id: &str) -> PathBuf {
    dir(run).join(format!("{id}.json"))
}

pub fn cancel_path(run: &Path, id: &str) -> PathBuf {
    dir(run).join(format!("{id}.cancel"))
}

pub fn load(run: &Path, id: &str) -> Result<Option<Run>, String> {
    match std::fs::read_to_string(record_path(run, id)) {
        Ok(t) => Run::from_json(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read the record of {id}: {e}")),
    }
}

pub fn save(run: &Path, r: &Run) -> Result<(), String> {
    let d = dir(run);
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    let tmp = d.join(format!(".{}.{}", r.id, std::process::id()));
    std::fs::write(&tmp, format!("{}\n", r.to_json())).map_err(|e| format!("cannot record {}: {e}", r.id))?;
    std::fs::rename(&tmp, record_path(run, &r.id)).map_err(|e| format!("cannot record {}: {e}", r.id))
}

pub fn cancel_requested(run: &Path, id: &str) -> bool {
    cancel_path(run, id).is_file()
}

pub fn clear_cancel(run: &Path, id: &str) {
    let _ = std::fs::remove_file(cancel_path(run, id));
}

/// Records the cancel request. `Err` names the state that refuses it.
pub fn request_cancel(run: &Path, id: &str, why: &str) -> Result<String, String> {
    let Some(r) = load(run, id)? else {
        return Err(format!("no gate run is recorded for {id}"));
    };
    let state = r.state().map_or("unstarted", GateState::as_str);
    if r.state() == Some(GateState::Verdict) || r.state().is_none() {
        return Err(format!("gate {id} is {state}; there is nothing to cancel"));
    }
    let why = if why.is_empty() { "cancelled by the operator" } else { why };
    std::fs::write(cancel_path(run, id), format!("{why}\n")).map_err(|e| format!("cannot record the cancel: {e}"))?;
    Ok(format!("gate {id} is {state}; it will end NO_VERDICT cancelled at its next check"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `gate status <bead>`: prints the record as JSON. Exit 0 read, 1 no record, 2 usage.
pub fn status_main(run: &Path, args: &[String]) -> i32 {
    let [id] = args else {
        eprintln!("usage: gate status <bead>");
        return 2;
    };
    let id = id_for(id, "");
    match load(run, &id) {
        Ok(Some(r)) => {
            let mut j = r.status_json(now_secs());
            j["cancel_requested"] = Value::Bool(cancel_requested(run, &id));
            println!("{j}");
            0
        }
        Ok(None) => {
            eprintln!("gate status: no gate run is recorded for {id}");
            1
        }
        Err(e) => {
            eprintln!("gate status: {e}");
            1
        }
    }
}

/// `gate cancel <bead> [--why <text>]`. Exit 0 requested, 1 refused (names the state), 2 usage.
pub fn cancel_main(run: &Path, args: &[String]) -> i32 {
    let (mut id, mut why) = (None, String::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--why" => match it.next() {
                Some(w) => why = w.clone(),
                None => return cancel_usage(),
            },
            b if !b.starts_with('-') && id.is_none() => id = Some(id_for(b, "")),
            _ => return cancel_usage(),
        }
    }
    let Some(id) = id else { return cancel_usage() };
    match request_cancel(run, &id, &why) {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("gate cancel: {e}");
            1
        }
    }
}

fn cancel_usage() -> i32 {
    eprintln!("usage: gate cancel <bead> [--why <text>]");
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use GateState::*;

    const LEGAL: [(GateState, GateState); 10] = [
        (Queued, Admitted),
        (Queued, Verdict),
        (Admitted, Rebased),
        (Admitted, Verdict),
        (Rebased, Fences),
        (Rebased, Trial),
        (Rebased, Verdict),
        (Fences, Trial),
        (Fences, Verdict),
        (Trial, Verdict),
    ];

    fn at(state: GateState) -> Run {
        let mut r = Run::new("sp-1", "b", "spira");
        r.events.push(GateEvent { to: state, reason: "fixture".into(), at: 0 });
        r
    }

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(tag)
    }

    #[test]
    fn every_legal_transition_applies_and_is_recorded() {
        for (from, to) in LEGAL {
            let mut r = at(from);
            r.record(to, "because", 9).unwrap_or_else(|e| panic!("{from:?}->{to:?}: {e}"));
            let last = r.events.last().unwrap();
            assert_eq!((last.to, last.reason.as_str(), last.at), (to, "because", 9));
            assert_eq!(r.state(), Some(to));
        }
    }

    #[test]
    fn every_illegal_transition_is_refused_naming_the_state() {
        for from in GateState::ALL {
            for to in GateState::ALL {
                if LEGAL.contains(&(from, to)) {
                    continue;
                }
                let mut r = at(from);
                let e = r.record(to, "because", 9).unwrap_err();
                assert!(e.contains(from.as_str()) && e.contains(to.as_str()), "{e}");
                assert_eq!(r.events.len(), 1, "a refusal writes nothing");
            }
        }
    }

    #[test]
    fn a_run_can_only_begin_queued() {
        let mut r = Run::new("sp-1", "b", "spira");
        assert!(r.record(Trial, "x", 1).unwrap_err().contains("unstarted"));
        r.record(Queued, "x", 1).unwrap();
    }

    #[test]
    fn entering_the_state_already_held_is_not_a_move() {
        let mut r = at(Trial);
        assert_eq!(r.enter(Trial, "again", 5), Ok(false));
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.enter(Verdict, "done", 5), Ok(true));
    }

    #[test]
    fn the_read_returns_exactly_the_recorded_state() {
        let t = tmp("read");
        let mut r = Run::new("sp-1", "spira/sp-1", "spira");
        for (s, why) in [(Queued, "q"), (Admitted, "a"), (Rebased, "r"), (Trial, "t")] {
            r.record(s, why, 100).unwrap();
            save(t.path(), &r).unwrap();
            let got = load(t.path(), "sp-1").unwrap().unwrap();
            assert_eq!(got, r);
            let j = got.status_json(130);
            assert_eq!((j["state"].as_str(), j["reason"].as_str(), j["since"].as_u64(), j["age_s"].as_u64()), (Some(s.as_str()), Some(why), Some(100), Some(30)));
            assert_eq!(j["events"].as_array().unwrap().len(), r.events.len());
        }
    }

    #[test]
    fn status_of_an_unrecorded_run_is_refused() {
        let t = tmp("none");
        assert_eq!(status_main(t.path(), &["sp-9".into()]), 1);
        assert_eq!(status_main(t.path(), &[]), 2);
    }

    #[test]
    fn cancel_is_refused_naming_the_state_unless_the_run_is_live() {
        let t = tmp("cancel");
        let e = request_cancel(t.path(), "sp-1", "").unwrap_err();
        assert!(e.contains("no gate run"), "{e}");
        let mut r = Run::new("sp-1", "b", "spira");
        r.record(Queued, "q", 1).unwrap();
        r.record(Verdict, "done", 2).unwrap();
        save(t.path(), &r).unwrap();
        let e = request_cancel(t.path(), "sp-1", "").unwrap_err();
        assert!(e.contains("verdict") && !cancel_requested(t.path(), "sp-1"), "{e}");
        let mut r = Run::new("sp-2", "b", "spira");
        r.record(Queued, "q", 1).unwrap();
        save(t.path(), &r).unwrap();
        let m = request_cancel(t.path(), "sp-2", "stop").unwrap();
        assert!(m.contains("queued"), "{m}");
        assert!(cancel_requested(t.path(), "sp-2"));
        clear_cancel(t.path(), "sp-2");
        assert!(!cancel_requested(t.path(), "sp-2"));
    }

    #[test]
    fn an_id_is_one_path_component() {
        assert_eq!(id_for("", "spira/sp-1"), "spira_sp-1");
        assert_eq!(id_for("sp-2", "x"), "sp-2");
        assert_eq!(id_for("../../etc", ""), ".._.._etc");
    }
}
