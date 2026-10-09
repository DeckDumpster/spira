//! The pure decisions landing-lib.sh held: the certification order, whether a branch is the
//! green fix for its repository's red base, and the suite set a recorded PASS covered.

use crate::model::BeadRow;

/// One branch, as certify_order reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderRow {
    pub id: String,
    pub branch: String,
    pub priority: i64,
    pub closed_at: String,
    pub external_ref: Option<String>,
    pub express: bool,
}

impl OrderRow {
    pub fn of(branch: &str, bead: Option<&BeadRow>) -> OrderRow {
        let id = branch.strip_prefix("spira/").unwrap_or(branch).to_string();
        OrderRow {
            id,
            branch: branch.to_string(),
            priority: bead.map(|b| b.priority).unwrap_or(9999),
            closed_at: bead.map(|b| b.closed_at.clone()).unwrap_or_else(|| "9999-99-99".into()),
            external_ref: bead.and_then(|b| b.external_ref.clone()),
            express: bead.is_some_and(|b| b.express),
        }
    }
}

pub fn is_base_fix(external_ref: Option<&str>, name: &str) -> bool {
    external_ref.map(|r| r.starts_with(&format!("basefail:{name}:"))).unwrap_or(false)
}

fn bucket(r: &OrderRow, name: &str) -> u8 {
    if is_base_fix(r.external_ref.as_deref(), name) {
        0
    } else if r.express {
        1
    } else {
        2
    }
}

/// certify_order: base-fix branches first (a budget cut must never defer the fix that
/// unblocks every held branch), express second, everyone else last; each bucket by priority
/// then oldest close. Ties fall to the branch name, as `sort`'s last-resort comparison did.
pub fn certify_order(name: &str, rows: &[OrderRow]) -> Vec<String> {
    let mut v: Vec<&OrderRow> = rows.iter().collect();
    v.sort_by(|a, b| {
        bucket(a, name)
            .cmp(&bucket(b, name))
            .then(a.priority.cmp(&b.priority))
            .then(a.closed_at.cmp(&b.closed_at))
            .then(a.branch.cmp(&b.branch))
    });
    v.into_iter().map(|r| r.branch.clone()).collect()
}

/// basefail_fix_decision: true (a green base-fix) when `external_ref` names THIS
/// repository's base-fix suite and the branch's own section of the gate transcript does not
/// show that suite red. A fix for repository A never certifies a branch in repository B.
///
/// The base can also be red with no suite named at all — a FENCE failure (gate output
/// suite='-'), which fails before any suite runs and so never appears in a "name.sh RED"
/// line. There is no single suite to re-check there, so the fix is instead a branch whose
/// own section names no red suite at all.
pub fn basefail_fix_decision(external_ref: Option<&str>, name: &str, gate_out: &str) -> bool {
    let Some(r) = external_ref else { return false };
    let Some(suite) = r.strip_prefix(&format!("basefail:{name}:")) else { return false };
    if suite.is_empty() {
        return false;
    }
    let fence = suite == "-" || suite.starts_with("-@");
    let mut in_branch = false;
    for line in gate_out.split('\n') {
        if line.starts_with("--- this branch") {
            in_branch = true;
            continue;
        }
        if in_branch && (line.starts_with("---") || line.starts_with("gate:")) {
            in_branch = false;
        }
        if !in_branch {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        for i in 0..f.len().saturating_sub(1) {
            if fence {
                if !f[i].ends_with(".sh") {
                    continue;
                }
            } else if f[i] != suite {
                continue;
            }
            let next = f[i + 1];
            let killed = next == "was" && f.get(i + 2) == Some(&"killed");
            if matches!(next, "RED" | "TIMEOUT" | "FAILED") || killed {
                return false;
            }
        }
    }
    true
}

/// prior_pass_suites: the suite list a recorded PASS covered, from `gate-run.sh --status`.
pub fn prior_pass_suites(status_text: &str) -> String {
    status_text
        .split('\n')
        .filter_map(|l| l.strip_prefix("gate-run: gate PASS covered suites: "))
        .last()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, pri: i64, cat: &str, ext: Option<&str>, express: bool) -> OrderRow {
        OrderRow {
            id: id.into(),
            branch: format!("spira/{id}"),
            priority: pri,
            closed_at: cat.into(),
            external_ref: ext.map(String::from),
            express,
        }
    }

    #[test]
    // covers: UC-landing-merge-queue-04 UC-landing-merge-queue-11
    fn base_fix_then_express_then_rest_each_by_priority_then_age() {
        let rows = vec![
            row("sp-c", 1, "2026-09-01", None, false),
            row("sp-a", 2, "2026-09-02", None, true),
            row("sp-b", 3, "2026-09-03", Some("basefail:spira:test-x.sh"), false),
            row("sp-d", 1, "2026-08-01", None, false),
            row("sp-e", 0, "2026-09-01", Some("basefail:other:test-x.sh"), false),
            row("sp-f", 1, "2026-09-01", None, true),
        ];
        assert_eq!(
            certify_order("spira", &rows),
            vec!["spira/sp-b", "spira/sp-f", "spira/sp-a", "spira/sp-e", "spira/sp-d", "spira/sp-c"]
        );
    }

    #[test]
    // covers: UC-landing-merge-queue-11
    fn basefix_green_only_for_its_own_repository_and_suite() {
        let out = "--- base\ntest-x.sh RED\n--- this branch\ntest-y.sh RED\ngate: VERDICT=BASE_FAIL";
        assert!(basefail_fix_decision(Some("basefail:spira:test-x.sh"), "spira", out));
        assert!(!basefail_fix_decision(Some("basefail:spira:test-y.sh"), "spira", out));
        assert!(!basefail_fix_decision(Some("basefail:other:test-x.sh"), "spira", out));
        // the branch section is red (test-y.sh), so a fence-red base-fix is not green either
        assert!(!basefail_fix_decision(Some("basefail:spira:-"), "spira", out));
        assert!(!basefail_fix_decision(None, "spira", out));
        let killed = "--- this branch\ntest-x.sh was killed at 300s\n";
        assert!(!basefail_fix_decision(Some("basefail:spira:test-x.sh"), "spira", killed));
        // a line naming the suite outside the branch section does not count
        let outside = "test-x.sh TIMEOUT\n--- this branch\nall green\n";
        assert!(basefail_fix_decision(Some("basefail:spira:test-x.sh"), "spira", outside));
    }

    #[test]
    fn basefix_for_a_fence_red_base_checks_the_branch_is_fully_green() {
        // suite='-': the base failed a fence, before any suite ran — there is no single
        // suite name to re-check, so the fix is a branch whose own section names no red at
        // all, of any suite.
        let clean = "--- base\ninventory.sh FAILED\n--- this branch\ntest-x.sh ok\ntest-y.sh ok\ngate: VERDICT=BASE_FAIL";
        assert!(basefail_fix_decision(Some("basefail:spira:-"), "spira", clean));
        let still_red = "--- base\ninventory.sh FAILED\n--- this branch\ntest-x.sh ok\ntest-y.sh RED\n";
        assert!(!basefail_fix_decision(Some("basefail:spira:-"), "spira", still_red));
        let timed_out = "--- this branch\ntest-x.sh was killed at 300s\n";
        assert!(!basefail_fix_decision(Some("basefail:spira:-"), "spira", timed_out));
        // a red suite outside the branch section still does not count
        let outside = "test-x.sh RED\n--- this branch\nall green\n";
        assert!(basefail_fix_decision(Some("basefail:spira:-"), "spira", outside));
    }

    #[test]
    fn prior_pass_reads_the_last_record() {
        let t = "gate-run: gate PASS covered suites: a.sh\nx\ngate-run: gate PASS covered suites: a.sh b.sh\n";
        assert_eq!(prior_pass_suites(t), "a.sh b.sh");
        assert_eq!(prior_pass_suites("nothing"), "");
    }
}
