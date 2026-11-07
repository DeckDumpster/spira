//! One record per stop of the loop, world or lane, and one per release of the world: the
//! `loop-stop` family. The kind and the reason are closed sets that refuse what they cannot
//! place; `detail` is prose for a reader and is never read by a consumer.

use serde_json::Value;
use std::path::Path;

pub const FAMILY: &str = "loop-stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The world halted: `world stop` or `world drain`.
    Halt,
    /// The world was released from a halt: `world start` or `world resume`.
    Release,
    /// Landing, claims or rounds stopped on one repo.
    Pause,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Halt, Kind::Release, Kind::Pause];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Halt => "halt",
            Kind::Release => "release",
            Kind::Pause => "pause",
        }
    }

    pub fn parse(s: &str) -> Result<Kind, String> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| format!("stop kind {s:?} is not one of {}", names(&Kind::ALL.map(Kind::as_str))))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Operator,
    Incident,
    Deploy,
    Reboot,
    Drain,
    Landing,
}

impl Reason {
    pub const ALL: [Reason; 6] = [Reason::Operator, Reason::Incident, Reason::Deploy, Reason::Reboot, Reason::Drain, Reason::Landing];

    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Operator => "operator",
            Reason::Incident => "incident",
            Reason::Deploy => "deploy",
            Reason::Reboot => "reboot",
            Reason::Drain => "drain",
            Reason::Landing => "landing",
        }
    }

    pub fn parse(s: &str) -> Result<Reason, String> {
        Reason::ALL.into_iter().find(|r| r.as_str() == s).ok_or_else(|| format!("stop reason {s:?} is not one of {}", names(&Reason::ALL.map(Reason::as_str))))
    }
}

fn names(all: &[&str]) -> String {
    all.join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRecord {
    pub kind: Kind,
    pub reason: Reason,
    /// Who stopped it: the owner a stop names, or `world` for the world's own verbs.
    pub actor: String,
    pub repo: Option<String>,
    pub bead: Option<String>,
    pub detail: String,
}

impl StopRecord {
    pub fn validate(&self) -> Result<(), String> {
        if self.actor.trim().is_empty() {
            return Err("a stop record needs an actor".into());
        }
        if self.kind == Kind::Pause && self.repo.is_none() {
            return Err("a pause record needs the repo it stopped".into());
        }
        Ok(())
    }

    pub fn fields(&self) -> Vec<(String, Value)> {
        let opt = |v: &Option<String>| v.clone().map_or(Value::Null, Value::from);
        vec![
            ("kind".into(), self.kind.as_str().into()),
            ("reason".into(), self.reason.as_str().into()),
            ("actor".into(), self.actor.clone().into()),
            ("repo".into(), opt(&self.repo)),
            ("bead".into(), opt(&self.bead)),
            ("detail".into(), self.detail.clone().into()),
        ]
    }

    pub fn from_row(row: &Value) -> Result<StopRecord, String> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
        let need = |k: &str| s(k).ok_or_else(|| format!("row has no {k}"));
        let rec = StopRecord {
            kind: Kind::parse(&need("kind")?)?,
            reason: Reason::parse(&need("reason")?)?,
            actor: need("actor")?,
            repo: s("repo"),
            bead: s("bead"),
            detail: s("detail").unwrap_or_default(),
        };
        rec.validate()?;
        Ok(rec)
    }

    /// Appends this stop's one row to `<root>/tsd/loop-stop.jsonl`.
    pub fn write(&self, root: &Path, host: &str, ts: &str) -> Result<(), String> {
        self.validate()?;
        let line = crate::build_row(ts, host, FAMILY, &self.fields())?;
        crate::append_line(&crate::family_path(root, FAMILY), &line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn halt() -> StopRecord {
        StopRecord { kind: Kind::Halt, reason: Reason::Incident, actor: "world".into(), repo: None, bead: None, detail: "free text".into() }
    }

    #[test]
    fn a_reason_or_kind_outside_the_closed_set_is_refused() {
        assert!(Reason::parse("because").is_err());
        assert!(Kind::parse("freeze").is_err());
        for r in Reason::ALL {
            assert_eq!(Reason::parse(r.as_str()), Ok(r));
        }
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Ok(k));
        }
    }

    #[test]
    fn a_record_round_trips_through_its_row_and_a_pause_needs_its_repo() {
        let tmp = testkit::TempDir::new("stop-record");
        halt().write(tmp.path(), "h", "2026-11-01T00:00:00Z").unwrap();
        let text = std::fs::read_to_string(crate::family_path(tmp.path(), FAMILY)).unwrap();
        let row: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(StopRecord::from_row(&row), Ok(halt()));
        let pause = StopRecord { kind: Kind::Pause, ..halt() };
        assert!(pause.write(tmp.path(), "h", "2026-11-01T00:00:00Z").is_err());
    }
}
