//! The per-bead verdicts the dispatcher reached on its last pass. The dispatcher hands them to
//! `spira-lc claim-verdicts`, which stores them on the bead rows; a reader gets them back
//! with the live rows, so it never execs the claim tool.

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
    OutOfScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub verdict: Kind,
    #[serde(default)]
    pub blocker: Option<String>,
    /// The rune partitions that would take this bead; only a claimable bead has any.
    #[serde(default)]
    pub runes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictFile {
    pub computed_at: String,
    pub entries: Vec<Entry>,
}

impl Kind {
    /// The kind a stored word names; `None` for a word this build does not know.
    pub fn from_word(word: &str) -> Option<Kind> {
        serde_json::from_value(serde_json::Value::String(word.to_string())).ok()
    }
}

impl VerdictFile {
    /// The `spira-lc claim-verdicts` payload.
    pub fn payload(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
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

    #[test]
    fn a_stamp_reads_back_as_the_epoch_it_was_made_from() {
        for t in [0, 951_782_400, 1_793_398_800, 1_709_251_199] {
            assert_eq!(epoch(&stamp(t)), Some(t), "{}", stamp(t));
        }
        assert_eq!(stamp(1_793_398_800), "2026-10-30T22:20:00Z");
        assert_eq!(epoch("yesterday"), None);
    }

    #[test]
    fn a_kind_round_trips_through_the_word_the_store_keeps() {
        for k in [Kind::Claimable, Kind::Blocked, Kind::Held, Kind::Snoozed, Kind::TooDeep, Kind::StackUnchanged, Kind::Carried, Kind::Stopped, Kind::OutOfScope] {
            let word = serde_json::to_value(k).unwrap();
            assert_eq!(Kind::from_word(word.as_str().unwrap()), Some(k), "{word}");
        }
        assert_eq!(Kind::from_word("a-future-kind"), None);
    }

    #[test]
    fn the_payload_carries_the_stamp_and_every_entry() {
        let f = VerdictFile {
            computed_at: stamp(1_793_398_800),
            entries: vec![Entry { id: "sp-p.1".into(), verdict: Kind::Blocked, blocker: Some("sp-p".into()), runes: vec![] }],
        };
        let v: serde_json::Value = serde_json::from_str(&f.payload()).unwrap();
        assert_eq!((v["computed_at"].as_str(), v["entries"][0]["verdict"].as_str(), v["entries"][0]["blocker"].as_str()), (Some("2026-10-30T22:20:00Z"), Some("blocked"), Some("sp-p")));
    }
}
