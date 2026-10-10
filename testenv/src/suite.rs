//! Suite headers and lifecycle state, read from the tree under test (DESIGN.md §3.6).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The `# key: value` header lines a suite declares before its first `set -` line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteHeaders {
    /// `# requires: a, b` — tokens checked with `command -v` inside the container.
    pub requires: Vec<String>,
    /// `# exclusive: <reason>` — drains the pool and runs alone.
    pub exclusive: Option<String>,
    /// `# tier: T0..T4`, empty when undeclared.
    pub tier: String,
    /// A `# testdb-mode: server` line anywhere in the suite (spira-lint's testdb-mode-lint
    /// rule requires it of every server-mode suite): testenv pre-builds the server template
    /// (DESIGN-testdb.md).
    #[serde(default)]
    pub testdb_server: bool,
    /// A `# testdb-mode: embedded — <reason>` line: the suite needs the embedded engine
    /// specifically (e.g. a local directory it can break the permissions of — server mode's
    /// unreachability is a TCP port, not a local directory) and must be exempted from the
    /// batch-wide server fixture (sp-gjx1b; DESIGN-testdb.md §2.4 note). testenv strips
    /// `SPIRA_TESTDB_MODE=server` from just this suite's environment so its own
    /// `_testdb_embedded_check` runs as it would with no server template built at all.
    #[serde(default)]
    pub testdb_embedded: bool,
    /// `# pids: N` — peak processes the suite holds; the batch admits suites while the
    /// running weights fit the container's pids budget.
    #[serde(default)]
    pub pids: Option<u32>,
    /// `# lane: <name>` — suites of a lane share their own slot budget (sim worlds).
    #[serde(default)]
    pub lane: Option<String>,
}

fn header_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix('#')?;
    let rest = rest.trim_start_matches(' ');
    let rest = rest.strip_prefix(key)?.strip_prefix(':')?;
    Some(rest.trim_start_matches(' '))
}

impl SuiteHeaders {
    pub fn parse(text: &str) -> Self {
        let mut h = SuiteHeaders::default();
        let (mut got_req, mut got_excl, mut got_tier) = (false, false, false);
        for line in text.lines() {
            if line.starts_with("set -") {
                break;
            }
            if !got_req {
                if let Some(v) = header_value(line, "requires") {
                    got_req = true;
                    h.requires = v
                        .replace(',', " ")
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                }
            }
            if !got_excl {
                if let Some(v) = header_value(line, "exclusive") {
                    got_excl = true;
                    if !v.is_empty() {
                        h.exclusive = Some(v.to_string());
                    }
                }
            }
            if !got_tier {
                if let Some(v) = header_value(line, "tier") {
                    got_tier = true;
                    h.tier = v.trim().to_string();
                }
            }
        }
        fn mode_word(l: &str) -> Option<&str> {
            let v = header_value(l, "testdb-mode")?;
            v.split(|c: char| !c.is_ascii_alphanumeric()).next()
        }
        h.pids = text
            .lines()
            .take_while(|l| !l.starts_with("set -"))
            .find_map(|l| header_value(l, "pids"))
            .and_then(|v| v.trim().parse().ok());
        h.lane = text
            .lines()
            .take_while(|l| !l.starts_with("set -"))
            .find_map(|l| header_value(l, "lane"))
            .and_then(|v| v.split_whitespace().next().map(str::to_string));
        h.testdb_server = text.lines().any(|l| mode_word(l) == Some("server"));
        h.testdb_embedded = text.lines().any(|l| mode_word(l) == Some("embedded"));
        h
    }

    /// Requirement tokens that need a check; `testenv` is met by being in the container.
    pub fn checkable_requires(&self) -> impl Iterator<Item = &str> {
        self.requires
            .iter()
            .map(String::as_str)
            .filter(|t| *t != "testenv")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuiteState {
    Active,
    Quarantined,
    Disabled,
}

impl SuiteState {
    /// The word the state file and suites' messages use.
    pub fn as_str(self) -> &'static str {
        match self {
            SuiteState::Active => "active",
            SuiteState::Quarantined => "quarantined",
            SuiteState::Disabled => "disabled",
        }
    }
}

/// One row of `spira/suite-state`: `<suite> | <state> | <since> | <bead> | <reason>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteStateRow {
    pub suite: String,
    pub state: SuiteState,
    pub since: String,
    pub bead: String,
    pub reason: String,
    /// `until=<iso>` after the bead id: the quarantine stops counting at this instant.
    pub until: Option<u64>,
}

/// The parsed suite-state file. Absent file, malformed lines and unknown states all read as
/// active — fail-closed: an unreadable file makes every suite blocking.
#[derive(Debug, Clone, Default)]
pub struct SuiteStates {
    rows: BTreeMap<String, SuiteStateRow>,
}

impl SuiteStates {
    pub fn parse(text: &str) -> Self {
        let mut rows = BTreeMap::new();
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
            if parts.len() < 5 {
                continue;
            }
            let state = match parts[1] {
                "active" => SuiteState::Active,
                "quarantined" => SuiteState::Quarantined,
                "disabled" => SuiteState::Disabled,
                _ => continue,
            };
            if parts[0].is_empty() {
                continue;
            }
            let (bead, until) = split_bead(parts[3]);
            // First row for a suite wins, as suite_state_of's first-match read does.
            rows.entry(parts[0].to_string()).or_insert(SuiteStateRow {
                suite: parts[0].to_string(),
                state,
                since: parts[2].to_string(),
                bead,
                reason: parts[4].to_string(),
                until,
            });
        }
        SuiteStates { rows }
    }

    /// Every row, one per suite (the first a file names), sorted by suite.
    pub fn rows(&self) -> impl Iterator<Item = &SuiteStateRow> {
        self.rows.values()
    }

    pub fn state_of(&self, suite: &str) -> SuiteState {
        self.rows
            .get(suite)
            .map(|r| r.state)
            .unwrap_or(SuiteState::Active)
    }

    /// `state_of` at `now`: a quarantine past its `until`, or past `max_age` from `since`
    /// when it names no `until`, reads as active — a forgotten quarantine blocks again.
    pub fn state_at(&self, suite: &str, now: u64, max_age: u64) -> SuiteState {
        let Some(r) = self.rows.get(suite) else { return SuiteState::Active };
        if r.state == SuiteState::Quarantined && r.expired(now, max_age) {
            return SuiteState::Active;
        }
        r.state
    }
}

impl SuiteStateRow {
    pub fn expired(&self, now: u64, max_age: u64) -> bool {
        let end = self.until.or_else(|| {
            crate::suites::model::parse_iso_utc(&self.since).map(|s| s.saturating_add(max_age))
        });
        end.is_some_and(|e| now >= e)
    }
}

/// `sp-x until=2026-10-10T00:00:00Z` -> (`sp-x`, Some(epoch)). An `until` that does not
/// parse is dropped, so the max age governs instead of the row never expiring.
fn split_bead(field: &str) -> (String, Option<u64>) {
    let mut bead = Vec::new();
    let mut until = None;
    for tok in field.split_whitespace() {
        match tok.strip_prefix("until=") {
            Some(t) => until = crate::suites::model::parse_iso_utc(t),
            None => bead.push(tok),
        }
    }
    (bead.join(" "), until)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_stop_at_the_first_set_line() {
        let t = "#!/usr/bin/env bash\n# requires: jq, dolt testenv\n# exclusive: builds cargo\n# tier: T2 \nset -uo pipefail\n# requires: late\n";
        let h = SuiteHeaders::parse(t);
        assert_eq!(h.requires, vec!["jq", "dolt", "testenv"]);
        assert_eq!(h.exclusive.as_deref(), Some("builds cargo"));
        assert_eq!(h.tier, "T2");
        assert_eq!(
            h.checkable_requires().collect::<Vec<_>>(),
            vec!["jq", "dolt"]
        );
    }

    #[test]
    fn headers_after_set_are_ignored_and_first_wins() {
        let h = SuiteHeaders::parse("set -e\n# requires: jq\n");
        assert!(h.requires.is_empty());
        let h = SuiteHeaders::parse("#requires: a\n# requires: b\n");
        assert_eq!(h.requires, vec!["a"]);
    }

    #[test]
    fn testdb_server_mode_is_read_from_anywhere_in_the_suite() {
        let t = "#!/usr/bin/env bash\nset -uo pipefail\n# testdb-mode: server — needs bd sql\nexport SPIRA_TESTDB_MODE=server\n";
        assert!(SuiteHeaders::parse(t).testdb_server);
        assert!(!SuiteHeaders::parse("# testdb-mode: default (embedded).\n").testdb_server);
        assert!(!SuiteHeaders::parse("# testdb-mode: servers\n").testdb_server);
        assert!(!SuiteHeaders::parse("export SPIRA_TESTDB_MODE=server\n").testdb_server, "the lint's header is the signal");
    }

    #[test]
    fn testdb_embedded_mode_opts_a_suite_out_of_the_batch_wide_server_fixture() {
        let t = "# testdb-mode: embedded — needs a local store it can make unreadable\n";
        let h = SuiteHeaders::parse(t);
        assert!(h.testdb_embedded);
        assert!(!h.testdb_server);
        assert!(!SuiteHeaders::parse("# testdb-mode: server — x\n").testdb_embedded);
        assert!(!SuiteHeaders::parse("").testdb_embedded);
    }

    #[test]
    fn pids_and_lane_are_read_from_the_header_only() {
        let h = SuiteHeaders::parse("# pids: 900\n# lane: sim — four worlds\nset -uo pipefail\n# pids: 5\n");
        assert_eq!((h.pids, h.lane.as_deref()), (Some(900), Some("sim")));
        let h = SuiteHeaders::parse("# pids: many\nset -e\n# lane: sim\n");
        assert_eq!((h.pids, h.lane), (None, None));
    }

    #[test]
    fn empty_exclusive_is_not_exclusive() {
        assert_eq!(SuiteHeaders::parse("# exclusive:\n").exclusive, None);
    }

    #[test]
    fn suite_state_reads_rows_comments_and_defaults() {
        let t = "# header\ntest-poison.sh | quarantined | 2026-09-28T12:35:54Z | sp-ytbma | slow # note\n\
                 test-off.sh | disabled | x | | gone\nbad line\ntest-odd.sh | weird | x | y | z\n";
        let s = SuiteStates::parse(t);
        assert_eq!(s.state_of("test-poison.sh"), SuiteState::Quarantined);
        assert_eq!(s.state_of("test-off.sh"), SuiteState::Disabled);
        assert_eq!(s.state_of("test-odd.sh"), SuiteState::Active);
        assert_eq!(s.state_of("test-none.sh"), SuiteState::Active);
        assert_eq!(SuiteStates::parse("").state_of("x"), SuiteState::Active);
    }

    #[test]
    fn a_quarantine_expires_at_its_until_or_its_max_age() {
        let t = "test-u.sh | quarantined | 2026-10-01T00:00:00Z | sp-1 until=2026-10-02T00:00:00Z | flaky\n\
                 test-m.sh | quarantined | 2026-10-01T00:00:00Z | sp-2 | flaky\n\
                 test-d.sh | disabled | 2026-01-01T00:00:00Z | | off\n";
        let s = SuiteStates::parse(t);
        let at = |iso: &str| crate::suites::model::parse_iso_utc(iso).unwrap();
        assert_eq!(s.rows().find(|r| r.suite == "test-u.sh").unwrap().bead, "sp-1");
        let day = 86_400;
        assert_eq!(s.state_at("test-u.sh", at("2026-10-01T23:59:59Z"), 7 * day), SuiteState::Quarantined);
        assert_eq!(s.state_at("test-u.sh", at("2026-10-02T00:00:00Z"), 7 * day), SuiteState::Active);
        assert_eq!(s.state_at("test-m.sh", at("2026-10-07T23:59:59Z"), 7 * day), SuiteState::Quarantined);
        assert_eq!(s.state_at("test-m.sh", at("2026-10-08T00:00:00Z"), 7 * day), SuiteState::Active);
        assert_eq!(s.state_at("test-d.sh", at("2030-01-01T00:00:00Z"), 7 * day), SuiteState::Disabled);
    }
}
