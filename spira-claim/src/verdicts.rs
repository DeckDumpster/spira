//! The per-bead verdicts the dispatcher reached on its last pass; they feed `publish-readiness`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Claimable,
    Blocked,
    Held,
    Snoozed,
    TooDeep,
    StackUnchanged,
    Carried,
    Stopped,
    TakenOver,
    /// A live process has its working directory in the bead's worktree.
    Occupied,
    OutOfScope,
    /// A record the view is missing an input for.
    CannotTell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub verdict: Kind,
    #[serde(default)]
    pub blocker: Option<String>,
}

impl Kind {
    /// The kind a stored word names; `None` for a word this build does not know.
    pub fn from_word(word: &str) -> Option<Kind> {
        serde_json::from_value(serde_json::Value::String(word.to_string())).ok()
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for an epoch.
pub fn stamp(epoch: i64) -> String {
    bead::claimdesc::iso_of(epoch)
}

/// Epoch seconds of a [`stamp`]; `None` for anything else.
pub fn epoch(s: &str) -> Option<i64> {
    (s.len() == 20 && s.ends_with('Z')).then(|| bead::claimdesc::parse_iso(s)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tsdi0m9)]
    #[test]
    fn a_stamp_reads_back_as_the_epoch_it_was_made_from() {
        for t in [0, 951_782_400, 1_793_398_800, 1_709_251_199] {
            assert_eq!(epoch(&stamp(t)), Some(t), "{}", stamp(t));
        }
        assert_eq!(stamp(1_793_398_800), "2026-10-30T22:20:00Z");
        assert_eq!(epoch("yesterday"), None);
    }

    #[test_id(tt81twb)]
    #[test]
    fn a_kind_round_trips_through_the_word_the_store_keeps() {
        for k in [Kind::Claimable, Kind::Blocked, Kind::Held, Kind::Snoozed, Kind::TooDeep, Kind::StackUnchanged, Kind::Carried, Kind::Stopped, Kind::TakenOver, Kind::Occupied, Kind::OutOfScope] {
            let word = serde_json::to_value(k).unwrap();
            assert_eq!(Kind::from_word(word.as_str().unwrap()), Some(k), "{word}");
        }
        assert_eq!(Kind::from_word("a-future-kind"), None);
    }
}
