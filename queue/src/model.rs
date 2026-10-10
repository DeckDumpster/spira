//! The data queue handles, as types (DESIGN.md §3).

use serde::{Deserialize, Serialize};

/// A repository's land mode as lib.sh `repo_land` reports it: `queue.forge` is normalised
/// to `queue`, an empty column is `push`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LandMode {
    Push,
    Pr,
    Hold,
    /// queue.forge — a batch PR per round.
    Queue,
    /// queue.local — a round fast-forwards a local landing ref.
    QueueLocal,
}

impl LandMode {
    pub fn parse(s: &str) -> Option<LandMode> {
        match s.trim() {
            "" | "push" => Some(LandMode::Push),
            "pr" => Some(LandMode::Pr),
            "hold" => Some(LandMode::Hold),
            "queue" | "queue.forge" => Some(LandMode::Queue),
            "queue.local" => Some(LandMode::QueueLocal),
            _ => None,
        }
    }

    /// The word lib.sh prints (and queue.sh's messages quote).
    pub fn as_str(self) -> &'static str {
        match self {
            LandMode::Push => "push",
            LandMode::Pr => "pr",
            LandMode::Hold => "hold",
            LandMode::Queue => "queue",
            LandMode::QueueLocal => "queue.local",
        }
    }

    /// The spelling a transition writes into the config (spira-config's LandMode).
    pub fn config_word(self) -> &'static str {
        match self {
            LandMode::Queue => "queue.forge",
            m => m.as_str(),
        }
    }

    pub fn is_queued(self) -> bool {
        matches!(self, LandMode::Queue | LandMode::QueueLocal)
    }
}

/// `id:tip`. A bare id (no colon) carries an empty tip; land-local reads that as "the head".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub tip: String,
}

impl Member {
    pub fn parse(tok: &str) -> Member {
        match tok.split_once(':') {
            Some((id, tip)) => Member { id: id.into(), tip: tip.into() },
            None => Member { id: tok.into(), tip: String::new() },
        }
    }

    /// `queue.sh`'s own split for open-record members: id before the FIRST colon, tip after
    /// the LAST (`${m%%:*}` / `${m##*:}`). Identical to [`Member::parse`] for any tip that
    /// is a SHA; kept separate so the record parser says which rule it uses.
    pub fn parse_record(tok: &str) -> Member {
        let id = tok.split(':').next().unwrap_or("").to_string();
        let tip = tok.rsplit(':').next().unwrap_or("").to_string();
        if !tok.contains(':') {
            return Member { id: tok.into(), tip: tok.into() };
        }
        Member { id, tip }
    }

    pub fn render(&self) -> String {
        format!("{}:{}", self.id, self.tip)
    }
}

/// Parse a member list: space- or comma-separated tokens, empties skipped.
pub fn parse_members(s: &str) -> Vec<Member> {
    s.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(Member::parse).collect()
}

pub fn render_members(ms: &[Member]) -> String {
    ms.iter().map(Member::render).collect::<Vec<_>>().join(" ")
}

/// Why an eject took a member out — the bd `reopen` cause row spira-claim classifies
/// (spira-claim/DESIGN.md §3 ReturnClass). DESIGN.md §8 D1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EjectCause {
    /// The harness or a rebase took it out: not a judgement of the work, never charged.
    Eject,
    /// A red test: judged, counted as a requeue and an attempt.
    EjectRed,
}

impl EjectCause {
    pub fn as_str(self) -> &'static str {
        match self {
            EjectCause::Eject => "eject",
            EjectCause::EjectRed => "eject-red",
        }
    }

    /// `--harness-fault` wins over everything: the round or the harness was at fault, so the
    /// member is not charged even when it names the suites that went red. Otherwise `--red` or
    /// a non-empty `--suites` means a red test; anything else is a harness/rebase eject.
    pub fn decide(red_flag: bool, harness_fault: bool, suites: &str) -> EjectCause {
        if harness_fault {
            EjectCause::Eject
        } else if red_flag || !suites.trim().is_empty() {
            EjectCause::EjectRed
        } else {
            EjectCause::Eject
        }
    }
}

/// One commit of a publish range, as `git log --format=%H%x1f%P%x1f%s -z` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub subject: String,
}

/// A bead row as `bd show --json` returns it (only the fields read).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeadRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub priority: Option<i64>,
}

/// A `spira-lc list` / `show-batch` row, string-valued columns tolerated.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LcBeadRow {
    #[serde(default)]
    pub bead_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default, deserialize_with = "opt_string_any")]
    pub tip: Option<String>,
    #[serde(default, deserialize_with = "opt_u64_any")]
    pub since: Option<u64>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

fn opt_u64_any<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn opt_string_any<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_mode_normalises_forge_and_defaults_push() {
        assert_eq!(LandMode::parse("queue.forge"), Some(LandMode::Queue));
        assert_eq!(LandMode::parse(""), Some(LandMode::Push));
        assert_eq!(LandMode::parse("queue.local"), Some(LandMode::QueueLocal));
        assert_eq!(LandMode::parse("bogus"), None);
        assert_eq!(LandMode::Queue.config_word(), "queue.forge");
    }

    #[test]
    fn members_parse_both_separators() {
        let m = parse_members("a:1, b:2 c");
        assert_eq!(m.len(), 3);
        assert_eq!(m[2], Member { id: "c".into(), tip: String::new() });
        assert_eq!(render_members(&m[..2]), "a:1 b:2");
    }

    #[test]
    fn eject_cause_red_when_flagged_or_suites_named() {
        assert_eq!(EjectCause::decide(false, false, ""), EjectCause::Eject);
        assert_eq!(EjectCause::decide(true, false, ""), EjectCause::EjectRed);
        assert_eq!(EjectCause::decide(false, false, "test-a.sh"), EjectCause::EjectRed);
        assert_eq!(EjectCause::decide(true, true, "test-a.sh"), EjectCause::Eject);
        assert_eq!(EjectCause::EjectRed.as_str(), "eject-red");
    }
}
