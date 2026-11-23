//! The facts of a unit's history the cover note states: commits, reverts, fixes, line ages, lost tests.

use crate::ports::{Beads, CommitInfo, Registry, RemovedTest, SourceTree};
use std::collections::BTreeSet;

const DAY_MS: u64 = 86_400_000;

/// The bead a commit names: `spira: land <id> — …` or `<id>: …`.
pub fn bead_of(subject: &str) -> Option<String> {
    let id_at = |s: &str| -> Option<String> {
        let n: String = s.strip_prefix("sp-")?.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
        let n = n.trim_end_matches('.');
        (!n.is_empty()).then(|| format!("sp-{n}"))
    };
    if let Some(rest) = subject.strip_prefix("spira: land ") {
        return id_at(rest);
    }
    let (head, _) = subject.split_once(':')?;
    id_at(head).filter(|id| id.len() == head.len())
}

/// The distinct commits touching any of `paths`, newest first within each path's log.
pub fn unit_commits(paths: &[String], tree: &dyn SourceTree) -> Vec<CommitInfo> {
    let mut seen = BTreeSet::new();
    paths.iter().flat_map(|p| tree.log(p)).filter(|c| seen.insert(c.sha.clone())).collect()
}

/// The first-commit and commit-count lines, every figure drawn from `commits`. The windowed count
/// is omitted when the first commit is inside it, which would only repeat the total.
pub fn commits_lines(commits: &[CommitInfo], beads: &dyn Beads, now_ms: u64, staged_at: u64, churn_days: u32) -> String {
    let first = commits.iter().map(|c| c.at_ms).min();
    let since = now_ms.saturating_sub(u64::from(churn_days) * DAY_MS);
    let windowed = commits.iter().filter(|c| c.at_ms >= since).count();
    let mut m = match first {
        Some(t) => format!("- First commit: {} days before this review was staged\n", staged_at.saturating_sub(t) / DAY_MS),
        None => "- First commit: unknown\n".to_string(),
    };
    let reverts = commits.iter().filter(|c| c.reverts).count();
    let named: BTreeSet<String> = commits.iter().filter_map(|c| bead_of(&c.subject)).collect();
    let fixes = if named.is_empty() {
        Ok("0".to_string())
    } else {
        beads.types(&named.iter().cloned().collect::<Vec<_>>()).map(|types| {
            let bug = |c: &&CommitInfo| bead_of(&c.subject).is_some_and(|id| types.get(&id).is_some_and(|t| t == "bug"));
            commits.iter().filter(bug).count().to_string()
        })
    };
    let fixes = match fixes {
        Ok(n) => format!("{n} fixes"),
        Err(e) => format!("fixes: unknown ({e})"),
    };
    let window = if first.is_some_and(|t| t >= since) { String::new() } else { format!(" ({windowed} in the last {churn_days} days)") };
    m.push_str(&format!("- Commits: {} ever{window}, {reverts} reverts, {fixes}\n", commits.len()));
    m
}

/// The body of the incidents section: each incident bead named by a commit touching `paths`, or why it is unknown.
pub fn incident_lines(paths: &[String], tree: &dyn SourceTree, beads: &dyn Beads) -> String {
    let named: BTreeSet<String> = paths.iter().flat_map(|p| tree.log(p)).filter_map(|c| bead_of(&c.subject)).collect();
    if named.is_empty() {
        return "None found.\n".to_string();
    }
    match beads.incidents(&named.into_iter().collect::<Vec<_>>()) {
        Err(e) => format!("- Incidents: unknown ({e})\n"),
        Ok(found) if found.is_empty() => "None found.\n".to_string(),
        Ok(found) => found.iter().map(|(id, title)| format!("- `{id}`: {title}\n")).collect(),
    }
}

pub fn rewritten_line(paths: &[String], tree: &dyn SourceTree, now_ms: u64, days: u32) -> String {
    let mut ages: Vec<u64> = paths.iter().flat_map(|p| tree.line_times(p)).map(|t| now_ms.saturating_sub(t)).collect();
    if ages.is_empty() {
        return format!("- Rewritten in the last {days} days: unknown (no line ages)\n");
    }
    ages.sort_unstable();
    let recent = ages.iter().filter(|a| **a <= u64::from(days) * DAY_MS).count();
    let n = ages.len();
    let pct = (recent * 100 + n / 2) / n;
    let median = if n % 2 == 1 { ages[n / 2] } else { (ages[n / 2 - 1] + ages[n / 2]) / 2 };
    format!("- Rewritten in the last {days} days: {pct}% of {n} lines; median line age {} days\n", median / DAY_MS)
}

/// `(count line, listing)` of tests removed from the unit and absent today.
pub fn deleted_lines(removed: &[RemovedTest]) -> String {
    let mut m = format!("- Deleted later: {}\n", removed.len());
    for r in removed {
        m.push_str(&format!("  - `{}::{}` removed in `{}`\n", r.path, r.name, r.sha.chars().take(7).collect::<String>()));
    }
    m
}

/// Registry ids whose test path ends in `::<fn>` for a fn of the unit's tests, with the date first disabled.
pub fn disabled_lines(registry: &dyn Registry, test_fns: &BTreeSet<String>) -> String {
    let rows = match registry.disabled() {
        Ok(r) => r,
        Err(e) => return format!("- Disabled as flaky: unknown ({e})\n"),
    };
    let ours: Vec<_> = rows.iter().filter(|(id, _)| id.split_once(' ').is_some_and(|(_, path)| test_fns.iter().any(|f| path.ends_with(&format!("::{f}"))))).collect();
    let mut m = format!("- Disabled as flaky: {}\n", ours.len());
    for (id, at) in ours {
        m.push_str(&format!("  - `{id}` {}\n", date(*at)));
    }
    m
}

fn date(epoch_secs: i64) -> String {
    let z = epoch_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{month:02}-{day:02}", yoe + era * 400 + i64::from(month <= 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(te6riu6)]
    #[test]
    fn bead_of_reads_a_landed_commit_and_a_dwarf_commit_and_nothing_else() {
        assert_eq!(bead_of("spira: land sp-ab12 — the title").as_deref(), Some("sp-ab12"));
        assert_eq!(bead_of("sp-aflyc.2: sim world").as_deref(), Some("sp-aflyc.2"));
        assert_eq!(bead_of("repoint sp-x: later"), None);
        assert_eq!(bead_of("gate: cache"), None);
    }

    #[test_id(txswzch)]
    #[test]
    fn date_is_the_utc_calendar_day() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_763_596_800), "2025-11-20");
    }
}
