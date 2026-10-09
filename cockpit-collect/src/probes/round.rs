//! `round_keys` — the round in flight, read only from the lifecycle batch machine
//! (`spira-lc list --batches`) and the bead rows. `?` is CANNOT TELL, never an idle pane.

use super::{push, Kv};
use crate::quoting::sanitize;
use serde_json::Value;
use std::process::{Command, Stdio};

const MAX_ROWS: usize = 12;
const MAX_ROUNDS: usize = 4;

pub fn round_keys(cap_secs: i64) -> Kv {
    let batches = list_batches().and_then(|raw| parse_batches(&raw));
    let pool = spira_config::lc_state::list().ok().map(|rows| pool_waiting(&rows));
    round_keys_from(batches.as_deref(), pool, cap_secs)
}

fn list_batches() -> Option<String> {
    let bin = spira_config::lifecycle_row::lc_bin();
    let o = Command::new("timeout").arg("5").arg(bin).args(["list", "--batches"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub struct Batch {
    pub id: String,
    pub state: String,
    pub reason: String,
    pub opened_at: i64,
    pub last_at: Option<i64>,
    pub members: Vec<(String, String)>,
    pub ejected: Vec<(String, String)>,
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn pairs(v: &Value, k: &str, a: &str, b: &str) -> Vec<(String, String)> {
    v.get(k).and_then(Value::as_array).map(|rows| rows.iter().map(|r| (text(r, a), text(r, b))).collect()).unwrap_or_default()
}

pub fn parse_batches(raw: &str) -> Option<Vec<Batch>> {
    let Value::Array(rows) = serde_json::from_str::<Value>(raw).ok()? else { return None };
    Some(
        rows.iter()
            .map(|r| Batch {
                id: text(r, "batch_id"),
                state: text(r, "state"),
                reason: text(r, "reason"),
                opened_at: int(r.get("opened_at")).unwrap_or(0),
                last_at: int(r.get("last_at")),
                members: pairs(r, "members", "bead_id", "outcome"),
                ejected: pairs(r, "ejected", "bead_id", "reason"),
            })
            .collect(),
    )
}

pub fn pool_waiting(rows: &[spira_config::lc_state::Row]) -> usize {
    rows.iter().filter(|r| matches!(r.state.as_str(), "SUBMITTED" | "CERTIFIED") && r.holds.is_empty()).count()
}

fn terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SETTLED" | "ABANDONED")
}

pub fn phase(state: &str) -> &'static str {
    match state {
        "STAGED" => "staged",
        "OPEN" => "merging",
        "CI_RUNNING" => "certifying",
        "GREEN" => "landing",
        "ATTRIBUTING" => "attributing",
        "REBUILDING" => "rebuilding",
        _ => "?",
    }
}

fn member_state(outcome: &str) -> &str {
    if outcome.is_empty() { "in" } else { outcome }
}

pub fn round_keys_from(batches: Option<&[Batch]>, pool: Option<usize>, cap_secs: i64) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_ROUND_POOL", pool.map_or("?".to_string(), |n| n.to_string()));
    push(&mut out, "SP_ROUND_CAP", cap_secs.to_string());
    let Some(batches) = batches else {
        push(&mut out, "SP_ROUND_STATE", "?");
        return out;
    };
    let live: Vec<&Batch> = batches.iter().filter(|b| !terminal(&b.state)).collect();
    if !live.is_empty() {
        push(&mut out, "SP_ROUND_STATE", "open");
        push(&mut out, "SP_ROUNDS_N", live.len().to_string());
        for (r, b) in live.iter().take(MAX_ROUNDS).enumerate() {
            let k = format!("SP_ROUNDS{r}");
            push(&mut out, &format!("{k}_NAME"), sanitize(&b.id));
            push(&mut out, &format!("{k}_PHASE"), phase(&b.state));
            push(&mut out, &format!("{k}_OPENED"), b.opened_at.to_string());
            let ejected: Vec<&str> = b.ejected.iter().map(|(id, _)| id.as_str()).collect();
            let members: Vec<_> = b.members.iter().filter(|(id, _)| !ejected.contains(&id.as_str())).collect();
            push(&mut out, &format!("{k}_N"), members.len().to_string());
            for (i, (id, outcome)) in members.iter().take(MAX_ROWS).enumerate() {
                push(&mut out, &format!("{k}_MEMBER{i}"), format!("{}|{}", sanitize(id), sanitize(member_state(outcome))));
            }
            push(&mut out, &format!("{k}_EJECT_N"), b.ejected.len().to_string());
            for (i, (id, why)) in b.ejected.iter().take(MAX_ROWS).enumerate() {
                push(&mut out, &format!("{k}_EJECT{i}"), format!("{}|{}", sanitize(id), sanitize(why)));
            }
        }
        return out;
    }
    push(&mut out, "SP_ROUND_STATE", "idle");
    if let Some(b) = batches.first() {
        let green = b.state != "ABANDONED";
        push(&mut out, "SP_ROUND_LAST_NAME", sanitize(&b.id));
        push(&mut out, "SP_ROUND_LAST_VERDICT", if green { "GREEN" } else { "RED" });
        push(&mut out, "SP_ROUND_LAST_AT", b.last_at.unwrap_or(b.opened_at).to_string());
        let why = if green { String::new() } else { b.reason.clone() };
        push(&mut out, "SP_ROUND_LAST_WHY", sanitize(&why));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(kv: &'a Kv, k: &str) -> Option<&'a str> {
        kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }

    const RAW: &str = r#"[
      {"batch_id":"r-9","repo":"spira","state":"CI_RUNNING","reason":null,"opened_at":"1000","last_at":1100,
       "members":[{"batch_id":"r-9","bead_id":"sp-a","outcome":null},{"batch_id":"r-9","bead_id":"sp-b","outcome":null},{"batch_id":"r-9","bead_id":"sp-c","outcome":null}],
       "ejected":[{"batch_id":"r-9","bead_id":"sp-c","reason":"red: test-x.sh"}]},
      {"batch_id":"r-8","repo":"spira","state":"LANDED","reason":null,"opened_at":500,"last_at":700,"members":[],"ejected":[]}]"#;

    #[test]
    fn an_open_batch_reports_phase_opened_members_and_ejections() {
        let b = parse_batches(RAW).unwrap();
        let kv = round_keys_from(Some(&b), Some(4), 900);
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("open"));
        assert_eq!(get(&kv, "SP_ROUNDS_N"), Some("1"));
        assert_eq!(get(&kv, "SP_ROUNDS0_NAME"), Some("r-9"));
        assert_eq!(get(&kv, "SP_ROUNDS0_PHASE"), Some("certifying"));
        assert_eq!(get(&kv, "SP_ROUNDS0_OPENED"), Some("1000"));
        assert_eq!(get(&kv, "SP_ROUND_CAP"), Some("900"));
        assert_eq!(get(&kv, "SP_ROUNDS0_N"), Some("2"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER1"), Some("sp-b|in"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER2"), None);
        assert_eq!(get(&kv, "SP_ROUNDS0_EJECT0"), Some("sp-c|red: test-x.sh"));
        assert_eq!(get(&kv, "SP_ROUND_POOL"), Some("4"));
    }

    #[test]
    fn two_live_rounds_are_both_reported_and_a_landed_one_is_not() {
        let raw = r#"[
          {"batch_id":"r-11","state":"STAGED","opened_at":2000,"members":[{"bead_id":"sp-z","outcome":"skipped: unlanded dependency"}],"ejected":[]},
          {"batch_id":"r-10","state":"CI_RUNNING","opened_at":1000,"members":[{"bead_id":"sp-y","outcome":null}],"ejected":[]},
          {"batch_id":"r-9","state":"LANDED","opened_at":500,"members":[{"bead_id":"sp-x","outcome":null}],"ejected":[]}]"#;
        let kv = round_keys_from(Some(&parse_batches(raw).unwrap()), Some(0), 900);
        assert_eq!(get(&kv, "SP_ROUNDS_N"), Some("2"));
        assert_eq!(get(&kv, "SP_ROUNDS0_PHASE"), Some("staged"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER0"), Some("sp-z|skipped: unlanded dependency"));
        assert_eq!(get(&kv, "SP_ROUNDS1_NAME"), Some("r-10"));
        assert!(!kv.iter().any(|(_, v)| v == "r-9"));
    }

    #[test]
    fn each_open_state_has_a_phase() {
        for (s, p) in [("OPEN", "merging"), ("CI_RUNNING", "certifying"), ("GREEN", "landing"), ("ATTRIBUTING", "attributing"), ("REBUILDING", "rebuilding")] {
            assert_eq!(phase(s), p);
        }
    }

    #[test]
    fn no_open_batch_reports_the_last_verdict() {
        let b = parse_batches(RAW).unwrap();
        let kv = round_keys_from(Some(&b[1..]), Some(0), 900);
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("idle"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_VERDICT"), Some("GREEN"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_AT"), Some("700"));
        let red = parse_batches(r#"[{"batch_id":"r-7","state":"ABANDONED","reason":"red: test-a.sh test-b.sh","opened_at":1,"last_at":2,"members":[],"ejected":[]}]"#).unwrap();
        let kv = round_keys_from(Some(&red), None, 900);
        assert_eq!(get(&kv, "SP_ROUND_LAST_VERDICT"), Some("RED"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_WHY"), Some("red: test-a.sh test-b.sh"));
        assert_eq!(get(&kv, "SP_ROUND_POOL"), Some("?"));
    }

    #[test]
    fn an_unreadable_store_is_cannot_tell_not_idle() {
        let kv = round_keys_from(None, None, 900);
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("?"));
        assert!(parse_batches("cannot tell: x").is_none());
    }

    #[test]
    fn the_pool_counts_submitted_and_certified_without_a_hold() {
        use spira_config::lc_state::Row;
        let row = |state: &str, holds: &[&str]| Row { bead_id: "x".into(), state: state.into(), holds: holds.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let rows = [row("SUBMITTED", &[]), row("CERTIFIED", &[]), row("CERTIFIED", &["wait"]), row("WORKING", &[]), row("LANDED", &[])];
        assert_eq!(pool_waiting(&rows), 2);
    }
}
