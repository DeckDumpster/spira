//! Checkpoint holds across a `world.sh stop`: an aeon slain by the stop leaves its bead under
//! a `wait` hold with a `checkpoint-until:` reason, and `world.sh start` lifts them.

use serde_json::Value;

/// The beads in `spira-lc list --hold wait` output whose hold is a checkpoint.
pub fn checkpointed(list_json: &str) -> Vec<String> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(list_json.trim()) else { return Vec::new() };
    rows.iter()
        .filter(|r| r.get("reason").and_then(Value::as_str).is_some_and(spira_config::lc_state::is_checkpoint))
        .filter_map(|r| r.get("bead_id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Lifts every checkpoint hold; returns the beads lifted and the beads that refused.
pub fn lift_all() -> (Vec<String>, Vec<String>) {
    let list = spira_config::bounded::bounded("spira-lc").args(["list", "--hold", "wait"]).output();
    let Ok(list) = list else { return (Vec::new(), vec!["(spira-lc list could not run)".into()]) };
    if !list.status.success() {
        return (Vec::new(), vec!["(spira-lc list refused)".into()]);
    }
    let (mut lifted, mut failed) = (Vec::new(), Vec::new());
    for bead in checkpointed(&String::from_utf8_lossy(&list.stdout)) {
        let ok = spira_config::bounded::bounded("spira-lc")
            .args(["unhold", &bead, "wait", "world"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok { lifted.push(bead) } else { failed.push(bead) }
    }
    (lifted, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_checkpoint_holds_are_lifted_not_snoozes_or_other_waits() {
        let j = format!(
            r#"[{{"bead_id":"sp-a","reason":"{}"}},{{"bead_id":"sp-b","reason":"{}"}},{{"bead_id":"sp-c","reason":"unlanded-blocker"}},{{"bead_id":"sp-d"}}]"#,
            spira_config::lc_state::checkpoint_reason(9),
            spira_config::lc_state::snooze_reason(9)
        );
        assert_eq!(checkpointed(&j), vec!["sp-a".to_string()]);
        assert!(checkpointed("not json").is_empty());
    }
}
