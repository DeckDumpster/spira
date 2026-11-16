//! Where a red gate failed: the one rule the gate's telemetry row, its lifecycle certify and the
//! landing pass's reopen note all read, so a red is named the same way everywhere.

/// The unit a red failed at (the verdict's `suite`, else the first red `gate-step`; empty when
/// the output does not say) and the last line of `out` that is not the gate's own bookkeeping.
pub fn red_at(suite: &str, out: &str) -> (String, String) {
    let step = match suite {
        "-" | "" => out
            .lines()
            .find_map(|l| l.trim().strip_prefix("gate-step RED ")?.split_once(" :: ").map(|(_, u)| u.trim().to_string()))
            .unwrap_or_default(),
        s => s.to_string(),
    };
    let message = out
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("gate: ") && !l.starts_with("gate-step "))
        .unwrap_or("")
        .to_string();
    (step, message)
}

#[cfg(test)]
mod tests {
    use super::red_at;

    #[test]
    fn a_named_suite_wins_and_bookkeeping_lines_are_skipped() {
        let out = "boom\ngate-step RED rc=1 :: fence\ngate: VERDICT=FAIL reason=branch-red suite=s\n";
        assert_eq!(red_at("s", out), ("s".into(), "boom".into()));
    }

    #[test]
    fn an_unnamed_suite_falls_back_to_the_first_red_step() {
        let out = "boom\ngate-step RED rc=1 :: fence\n";
        assert_eq!(red_at("-", out), ("fence".into(), "boom".into()));
        assert_eq!(red_at("-", ""), (String::new(), String::new()));
    }
}
