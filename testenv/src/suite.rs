//! Suite headers and lifecycle state, read from the tree under test (DESIGN.md §3.6).

use serde::{Deserialize, Serialize};

/// The `# key: value` header lines a suite declares before its first `set -` line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteHeaders {
    /// `# requires: a, b` — tokens checked with `command -v` inside the container.
    pub requires: Vec<String>,
    /// `# host-requires: cmd py:module` — what the machine running testenv must have, checked
    /// by `testenv preflight` before anything is built.
    #[serde(default)]
    pub host_requires: Vec<String>,
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
        h.host_requires = text
            .lines()
            .take_while(|l| !l.starts_with("set -"))
            .find_map(|l| header_value(l, "host-requires"))
            .map(|v| v.replace(',', " ").split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
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

    /// Whether the suite declares `# requires: testenv`: it uses the installed instance's
    /// systemd user units, so its batch runs the install (sp-vkmgmx).
    pub fn needs_install(&self) -> bool {
        self.requires.iter().any(|t| t == "testenv")
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
    fn host_requires_is_read_before_set_and_is_not_requires() {
        let h = SuiteHeaders::parse("# host-requires: jq, py:jwt\n# requires: dolt\nset -u\n# host-requires: late\n");
        assert_eq!(h.host_requires, vec!["jq", "py:jwt"]);
        assert_eq!(h.requires, vec!["dolt"]);
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
}
