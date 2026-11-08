//! The `batcher attribute` verb's outcome: one line on stdout, read back by the scheduler's
//! batcher tasks from the leg's captured output (`unit.ran`'s stdout tail). The verb runs
//! `batcher::drive::attribute_round` on a round whose corpus was red: each red is attributed,
//! its owners ejected (or handed to their menders), and only the owners' suites re-run on the
//! survivors; the line says what is left to land, or why nothing does.

use serde_json::{json, Value};

/// The outcome line starts with this, so other output around it is never mistaken for it.
pub const PREFIX: &str = "attribution\t";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
    /// The survivors land: their ids and tips in round order, the longest per-red attribution
    /// wall, and whether a mender pass held the round's reds (`settle_mending` answered).
    Land { members: Vec<(String, String)>, attribution_secs: Option<u64>, mended: bool },
    /// Nothing lands; why.
    Blocked(String),
    /// The harness failed (no VM, a dead container): the round was not judged, red or green.
    Fault(String),
}

impl Attribution {
    /// The outcome as the verb prints it: [`PREFIX`] and one JSON object.
    pub fn line(&self) -> String {
        let body = match self {
            Attribution::Land { members, attribution_secs, mended } => json!({ "end": "land", "members": members, "attribution_secs": attribution_secs, "mended": mended }),
            Attribution::Blocked(why) => json!({ "end": "blocked", "why": why }),
            Attribution::Fault(why) => json!({ "end": "fault", "why": why }),
        };
        format!("{PREFIX}{body}")
    }

    /// The last outcome line in `stdout`, or `None` when the verb printed none (it failed before
    /// deciding, or its output was lost).
    pub fn parse(stdout: &str) -> Option<Attribution> {
        let body: Value = serde_json::from_str(stdout.lines().rev().find_map(|l| l.strip_prefix(PREFIX))?).ok()?;
        match body.get("end")?.as_str()? {
            "land" => {
                let members = body.get("members")?.as_array()?.iter().map(|m| Some((m.get(0)?.as_str()?.to_string(), m.get(1)?.as_str()?.to_string()))).collect::<Option<Vec<_>>>()?;
                Some(Attribution::Land { members, attribution_secs: body.get("attribution_secs").and_then(Value::as_u64), mended: body.get("mended").and_then(Value::as_bool).unwrap_or(false) })
            }
            "blocked" => Some(Attribution::Blocked(body.get("why")?.as_str()?.to_string())),
            "fault" => Some(Attribution::Fault(body.get("why")?.as_str()?.to_string())),
            _ => None,
        }
    }
}

/// `id:tip,id:tip`, as `--members` takes it.
pub fn parse_members(csv: &str) -> Result<Vec<(String, String)>, String> {
    csv.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().split_once(':').map(|(i, t)| (i.to_string(), t.to_string())).ok_or_else(|| format!("--members: {s:?} is not id:tip"))).collect()
}

/// `--members`'s value for `members`.
pub fn members_arg(members: &[(String, String)]) -> String {
    members.iter().map(|(i, t)| format!("{i}:{t}")).collect::<Vec<_>>().join(",")
}

pub fn is_round_vm_fault(err: &str) -> bool {
    err.starts_with("round-vm:") && !err.contains("not a harness fault")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_reads_back_from_the_last_line_among_other_output() {
        let land = Attribution::Land { members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], attribution_secs: Some(42), mended: true };
        let out = format!("batcher spira: red test-x.sh → owner sp-c\n{}\nnoise after\n", land.line());
        assert_eq!(Attribution::parse(&out), Some(land));
        let blocked = Attribution::Blocked("unattributed red test-y.sh".into());
        assert_eq!(Attribution::parse(&format!("{}\n{}", Attribution::Blocked("first".into()).line(), blocked.line())), Some(blocked), "the last line wins");
    }

    #[test]
    fn a_fault_reads_back_as_a_fault_not_a_block() {
        let fault = Attribution::Fault("round-vm: harness fault".into());
        assert_eq!(Attribution::parse(&fault.line()), Some(fault));
    }

    #[test]
    fn only_a_round_vm_harness_fault_is_a_fault() {
        assert!(is_round_vm_fault("round-vm: harness fault — the round VM did not come up"));
        assert!(is_round_vm_fault("round-vm: infrastructure fault — the round VM (h) was unreachable"));
        assert!(!is_round_vm_fault("round-vm: suite list mismatch, not a harness fault — x"));
        assert!(!is_round_vm_fault("refresh failed after round-vm refresh"));
    }

    #[test]
    fn no_line_or_a_malformed_one_is_none() {
        assert_eq!(Attribution::parse("batcher: spira-lc unreachable\n"), None);
        assert_eq!(Attribution::parse(&format!("{PREFIX}{{\"end\":\"land\"}}")), None, "a land with no members is not an outcome");
        assert_eq!(Attribution::parse(&format!("{PREFIX}{{\"end\":\"maybe\"}}")), None);
        assert_eq!(Attribution::parse(&format!("{PREFIX}not json")), None);
    }

    #[test]
    fn members_round_trip_and_a_bad_one_refuses() {
        let m = vec![("sp-a".to_string(), "t1".to_string()), ("sp-b".to_string(), "t2".to_string())];
        assert_eq!(parse_members(&members_arg(&m)), Ok(m));
        assert!(parse_members("sp-a").is_err());
        assert_eq!(parse_members(""), Ok(vec![]));
    }
}
