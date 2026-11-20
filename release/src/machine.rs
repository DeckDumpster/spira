//! The release as a state machine: a release moves cut → accepted → published → activated, and
//! activated → rolled_back when rollback moves `current` off it. Every move is an event with a
//! reason, refused when illegal (naming the state). `release status --json` reads the record
//! back and nothing else, so no operator tool reads the history or hotfix files.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    Cut,
    Accepted,
    Published,
    Activated,
    RolledBack,
}

impl ReleaseState {
    pub const ALL: [ReleaseState; 5] = [ReleaseState::Cut, ReleaseState::Accepted, ReleaseState::Published, ReleaseState::Activated, ReleaseState::RolledBack];

    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseState::Cut => "cut",
            ReleaseState::Accepted => "accepted",
            ReleaseState::Published => "published",
            ReleaseState::Activated => "activated",
            ReleaseState::RolledBack => "rolled_back",
        }
    }

    pub fn parse(s: &str) -> Option<ReleaseState> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// A rolled-back release may be published again.
    pub fn can_go(self, to: ReleaseState) -> bool {
        use ReleaseState::*;
        matches!(
            (self, to),
            (Cut, Accepted) | (Accepted, Published) | (Published, Activated) | (Published, RolledBack) | (Activated, RolledBack) | (RolledBack, Published)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEvent {
    pub to: ReleaseState,
    pub reason: String,
    pub at: u64,
    pub restarted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub sha: String,
    pub events: Vec<ReleaseEvent>,
}

fn event_json(e: &ReleaseEvent) -> Value {
    let mut v = json!({"to": e.to.as_str(), "reason": e.reason, "at": e.at});
    if !e.restarted.is_empty() {
        v["restarted"] = json!(e.restarted);
    }
    v
}

impl Release {
    pub fn new(sha: &str) -> Release {
        Release { sha: sha.into(), events: Vec::new() }
    }

    pub fn state(&self) -> Option<ReleaseState> {
        self.events.last().map(|e| e.to)
    }

    /// Applies a move, or refuses naming the state the release is in. Nothing is written on a refusal.
    pub fn record(&mut self, to: ReleaseState, reason: &str, at: u64) -> Result<(), String> {
        self.record_with(to, reason, at, &[])
    }

    pub fn record_with(&mut self, to: ReleaseState, reason: &str, at: u64, restarted: &[String]) -> Result<(), String> {
        let ok = match self.state() {
            None => to == ReleaseState::Cut,
            Some(from) => from.can_go(to),
        };
        if !ok {
            let from = self.state().map_or("unrecorded", ReleaseState::as_str);
            return Err(format!("release {} is {from}; it cannot become {} ({reason})", self.sha, to.as_str()));
        }
        self.events.push(ReleaseEvent { to, reason: reason.into(), at, restarted: restarted.to_vec() });
        Ok(())
    }

    fn events_json(&self) -> Value {
        Value::Array(self.events.iter().map(event_json).collect())
    }

    pub fn to_json(&self) -> Value {
        json!({"sha": self.sha, "events": self.events_json()})
    }

    pub fn from_json(text: &str) -> Result<Release, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not a release record: {e}"))?;
        let sha = v.get("sha").and_then(Value::as_str).ok_or("release record has no sha")?.to_string();
        let mut events = Vec::new();
        for e in v.get("events").and_then(Value::as_array).ok_or("release record has no events")? {
            let to = e.get("to").and_then(Value::as_str).and_then(ReleaseState::parse).ok_or("release record has an event with an unknown state")?;
            events.push(ReleaseEvent {
                to,
                reason: e.get("reason").and_then(Value::as_str).unwrap_or_default().to_string(),
                at: e.get("at").and_then(Value::as_u64).unwrap_or(0),
                restarted: e.get("restarted").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
            });
        }
        Ok(Release { sha, events })
    }

    /// The read: exactly the recorded state, how long it has held, and the events behind it.
    pub fn status_json(&self, now: u64) -> Value {
        let last = self.events.last();
        json!({
            "sha": self.sha,
            "state": self.state().map_or("unrecorded", ReleaseState::as_str),
            "reason": last.map(|e| e.reason.as_str()).unwrap_or(""),
            "since": last.map_or(0, |e| e.at),
            "age_s": last.map_or(0, |e| now.saturating_sub(e.at)),
            "events": self.events_json(),
        })
    }
}

pub fn dir(state: &Path) -> PathBuf {
    state.join("machine")
}

pub fn record_path(state: &Path, sha: &str) -> PathBuf {
    dir(state).join(format!("{sha}.json"))
}

pub fn load(state: &Path, sha: &str) -> Result<Option<Release>, String> {
    if !crate::is_sha(sha) {
        return Err(format!("{sha:?} is not a full commit sha"));
    }
    match std::fs::read_to_string(record_path(state, sha)) {
        Ok(t) => Release::from_json(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read the record of {sha}: {e}")),
    }
}

pub fn save(state: &Path, r: &Release) -> Result<(), String> {
    let d = dir(state);
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    let tmp = d.join(format!(".{}.{}", r.sha, std::process::id()));
    std::fs::write(&tmp, format!("{}\n", r.to_json())).map_err(|e| format!("cannot record {}: {e}", r.sha))?;
    std::fs::rename(&tmp, record_path(state, &r.sha)).map_err(|e| format!("cannot record {}: {e}", r.sha))
}

pub fn all(state: &Path) -> Result<Vec<Release>, String> {
    let rd = match std::fs::read_dir(dir(state)) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", dir(state).display())),
    };
    let mut out = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|e| format!("cannot read {}: {e}", dir(state).display()))?;
        let name = ent.file_name().to_string_lossy().to_string();
        let Some(sha) = name.strip_suffix(".json") else { continue };
        if let Some(r) = load(state, sha)? {
            out.push(r);
        }
    }
    out.sort_by(|a, b| b.events.last().map(|e| e.at).cmp(&a.events.last().map(|e| e.at)).then_with(|| a.sha.cmp(&b.sha)));
    Ok(out)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Re-verifying or re-activating a release that already passed `to` (rollback re-enters an
/// earlier release) confirms the record; it is not a move.
fn already_passed(from: ReleaseState, to: ReleaseState) -> bool {
    use ReleaseState::*;
    let rank = |s| match s {
        Cut => 0,
        Accepted => 1,
        Published => 2,
        Activated | RolledBack => 3,
    };
    match to {
        RolledBack => false,
        Published if from == RolledBack => false,
        _ => rank(to) < rank(from),
    }
}

/// Records `sha` entering `to`. Being in `to` already is not a move. A release the machine
/// never saw (built before it existed) is brought up to `to` through the states it must have
/// passed, each event saying it was backfilled; a move that is illegal from the recorded state
/// is refused and nothing is written.
pub fn enter(state: &Path, sha: &str, to: ReleaseState, reason: &str) -> Result<(), String> {
    enter_with(state, sha, to, reason, &[])
}

/// `enter`, the event also recording which services the move restarted.
pub fn enter_with(state: &Path, sha: &str, to: ReleaseState, reason: &str, restarted: &[String]) -> Result<(), String> {
    let mut r = load(state, sha)?.unwrap_or_else(|| Release::new(sha));
    if r.state() == Some(to) || r.state().is_some_and(|from| already_passed(from, to)) {
        return Ok(());
    }
    let now = now_secs();
    if r.state().is_none() {
        for s in ReleaseState::ALL.into_iter().take_while(|s| *s != to).filter(|s| *s != ReleaseState::RolledBack && !(to == ReleaseState::RolledBack && *s == ReleaseState::Activated)) {
            r.record(s, "backfilled: the release predates its record", now)?;
        }
    }
    r.record_with(to, reason, now, restarted)?;
    save(state, &r)
}

/// `release status --json`: `current` and every recorded release, newest first.
pub fn status_all_json(state: &Path, current: Option<&str>) -> Result<Value, String> {
    let now = now_secs();
    Ok(json!({
        "current": current,
        "releases": all(state)?.iter().map(|r| r.status_json(now)).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReleaseState::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const LEGAL: [(ReleaseState, ReleaseState); 6] = [(Cut, Accepted), (Accepted, Published), (Published, Activated), (Published, RolledBack), (Activated, RolledBack), (RolledBack, Published)];

    fn at(state: ReleaseState) -> Release {
        let mut r = Release::new(SHA);
        r.events.push(ReleaseEvent { to: state, reason: "fixture".into(), at: 0, restarted: Vec::new() });
        r
    }

    #[test]
    fn a_recorded_restart_list_reads_back() {
        let t = testkit::TempDir::new("machine-restarted");
        enter(t.path(), SHA, Published, "p").unwrap();
        enter_with(t.path(), SHA, Activated, "up", &["lc-serve.service".to_string()]).unwrap();
        let r = load(t.path(), SHA).unwrap().unwrap();
        assert_eq!(r.events[0].restarted, Vec::<String>::new());
        assert_eq!(r.events.last().unwrap().restarted, vec!["lc-serve.service".to_string()]);
        assert!(!r.to_json()["events"][0].as_object().unwrap().contains_key("restarted"), "an empty list is not written");
    }

    #[test]
    fn a_record_in_the_format_before_restart_lists_loads_with_an_empty_list() {
        let old = format!(r#"{{"sha":"{SHA}","events":[{{"to":"cut","reason":"x","at":5}}]}}"#);
        let r = Release::from_json(&old).unwrap();
        assert_eq!(r.events[0].restarted, Vec::<String>::new());
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
        for from in ReleaseState::ALL {
            for to in ReleaseState::ALL {
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
    fn a_release_can_only_begin_cut() {
        let mut r = Release::new(SHA);
        assert!(r.record(Activated, "x", 1).unwrap_err().contains("unrecorded"));
        r.record(Cut, "x", 1).unwrap();
    }

    #[test]
    fn the_read_returns_exactly_the_recorded_state() {
        let t = testkit::TempDir::new("rel-read");
        let mut r = Release::new(SHA);
        for (s, why) in [(Cut, "c"), (Accepted, "a"), (Published, "p"), (Activated, "v"), (RolledBack, "b")] {
            r.record(s, why, 100).unwrap();
            save(t.path(), &r).unwrap();
            let got = load(t.path(), SHA).unwrap().unwrap();
            assert_eq!(got, r);
            let j = got.status_json(130);
            assert_eq!((j["state"].as_str(), j["reason"].as_str(), j["since"].as_u64(), j["age_s"].as_u64()), (Some(s.as_str()), Some(why), Some(100), Some(30)));
            assert_eq!(j["events"].as_array().unwrap().len(), r.events.len());
            let all = status_all_json(t.path(), Some(SHA)).unwrap();
            assert_eq!((all["current"].as_str(), all["releases"][0]["state"].as_str()), (Some(SHA), Some(s.as_str())));
        }
    }

    #[test]
    fn enter_backfills_an_unrecorded_release_and_refuses_an_illegal_move() {
        let t = testkit::TempDir::new("rel-enter");
        enter(t.path(), SHA, Published, "activating").unwrap();
        let r = load(t.path(), SHA).unwrap().unwrap();
        let states: Vec<_> = r.events.iter().map(|e| e.to).collect();
        assert_eq!(states, vec![Cut, Accepted, Published]);
        assert!(r.events[0].reason.contains("backfilled") && r.events[2].reason == "activating");
        enter(t.path(), SHA, Published, "again").unwrap();
        assert_eq!(load(t.path(), SHA).unwrap().unwrap().events.len(), 3);
        let other = "b".repeat(40);
        enter(t.path(), &other, Accepted, "built").unwrap();
        let e = enter(t.path(), &other, RolledBack, "no").unwrap_err();
        assert!(e.contains("accepted") && e.contains("rolled_back"), "{e}");
        assert_eq!(load(t.path(), SHA).unwrap().unwrap().events.len(), 3, "a refusal writes nothing");
        enter(t.path(), SHA, Activated, "up").unwrap();
        enter(t.path(), SHA, RolledBack, "down").unwrap();
        enter(t.path(), SHA, Published, "again").unwrap();
    }

    #[test]
    fn rollback_reentering_a_passed_release_is_not_a_move() {
        let t = testkit::TempDir::new("rel-reenter");
        enter(t.path(), SHA, Activated, "up").unwrap();
        let n = load(t.path(), SHA).unwrap().unwrap().events.len();
        for to in [Cut, Accepted, Published] {
            enter(t.path(), SHA, to, "again").unwrap();
        }
        assert_eq!(load(t.path(), SHA).unwrap().unwrap().events.len(), n);
        enter(t.path(), SHA, RolledBack, "down").unwrap();
        enter(t.path(), SHA, Accepted, "verified").unwrap();
        assert_eq!(load(t.path(), SHA).unwrap().unwrap().state(), Some(RolledBack));
        enter(t.path(), SHA, Published, "again").unwrap();
    }

    #[test]
    fn a_sha_is_one_path_component() {
        let t = testkit::TempDir::new("rel-sha");
        assert!(load(t.path(), "../../etc/passwd").is_err());
    }
}
