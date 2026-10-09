//! The skip contract (DESIGN.md §3.7): fail closed, not open. A SKIP or SKIP-REQ is green
//! only when its requirement is declared — checked into `spira/skip-allowlist.tsv` on the
//! tree under test — and the requirement is genuinely outside testenv's own environment.
//! Undeclared is red. sp-gjx1b: sp-cln99 broke the server-mode testdb template lookup and 13
//! suites SKIPped with no red anywhere, because a SKIP counted as green unconditionally.
//!
//! A requirement testenv itself must provide — a fixture, a binary in the artifact set, a
//! template, a testdb — can never be declared here, on purpose: declaring it would hide
//! testenv's own defect behind "genuinely external" cover, exactly the failure mode this
//! module exists to close. [`SkipGate::load`] refuses the whole file if it tries.

use crate::record::{ResultRecord, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    pub suite: String,
    /// The exact `ResultRecord.fingerprint` this declaration covers: `skip:<reason>` or
    /// `requires:<tok,...>`, normalized the same way the record was.
    pub requirement: String,
    /// Free text, for the human reading the file — not read by the loader.
    pub why: String,
}

/// `<suite>\t<requirement>\t<why>`. `#` comments and blank lines dropped. A malformed line
/// (fewer than two tab-separated fields) is dropped rather than panicking — a truncated file
/// then declares less, which is the fail-closed direction.
pub fn parse(text: &str) -> Vec<AllowEntry> {
    text.lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            let suite = f.next()?.trim().to_string();
            let requirement = f.next()?.trim().to_string();
            if suite.is_empty() || requirement.is_empty() {
                return None;
            }
            let why = f.next().unwrap_or("").trim().to_string();
            Some(AllowEntry {
                suite,
                requirement,
                why,
            })
        })
        .collect()
}

/// Categories testenv itself is responsible for providing (sp-gjx1b's acceptance): a
/// fixture, a binary in the artifact set, a template, a testdb. Matched case-insensitively
/// as a substring of the requirement text, deliberately broad — a new reserved word is added
/// here, never worked around at the call site.
const RESERVED_WORDS: [&str; 4] = ["fixture", "template", "testdb", "binary"];

pub fn reserved_word(requirement: &str) -> Option<&'static str> {
    let low = requirement.to_ascii_lowercase();
    RESERVED_WORDS.iter().copied().find(|w| low.contains(w))
}

#[derive(Debug, Clone)]
pub struct SkipGate {
    entries: Vec<AllowEntry>,
}

impl SkipGate {
    /// Parses `text` and refuses it outright if any entry declares a reserved requirement —
    /// the allow list itself must fail closed, not merely the suites that consult it.
    pub fn load(text: &str) -> Result<SkipGate, String> {
        let entries = parse(text);
        let bad: Vec<String> = entries
            .iter()
            .filter_map(|e| {
                reserved_word(&e.requirement)
                    .map(|w| format!("{} declares {:?} (reserved word {w:?})", e.suite, e.requirement))
            })
            .collect();
        if !bad.is_empty() {
            return Err(format!(
                "skip-allowlist.tsv declares a requirement testenv itself must provide — never declarable: {}",
                bad.join("; ")
            ));
        }
        Ok(SkipGate { entries })
    }

    pub fn declared(&self, suite: &str, requirement: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.suite == suite && e.requirement == requirement)
    }
}

/// The allow-list key a record's SKIP/SKIP-REQ names — the fingerprint field is already that
/// key (`skip:<reason>` or `requires:<tok,...>`, record.rs). `None` for every other status:
/// there is nothing to gate.
pub fn requirement_of(rec: &ResultRecord) -> Option<&str> {
    match rec.status {
        Status::Skip | Status::SkipReq => Some(rec.fingerprint.as_str()),
        _ => None,
    }
}

/// Reclassify one suite's record under the contract. Declared (or not a skip at all) passes
/// through unchanged. Undeclared becomes a plain `Status::Red` (or `QuarantinedRed` when the
/// suite is quarantined, matching how `ResultRecord::from_exit` already treats a real
/// failure there) — reusing the status every existing reader (gate-diag.sh, round.sh) already
/// treats as blocking, rather than teaching them a new word — with the fingerprint (and so
/// the reason) kept exactly as it was.
pub fn apply(gate: &SkipGate, suite: &str, quarantined: bool, rec: ResultRecord) -> ResultRecord {
    match requirement_of(&rec) {
        Some(requirement) if !gate.declared(suite, requirement) => ResultRecord {
            status: if quarantined {
                Status::QuarantinedRed
            } else {
                Status::Red
            },
            ..rec
        },
        _ => rec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Mode, Producer};

    fn skip_rec(fingerprint: &str) -> ResultRecord {
        ResultRecord {
            status: Status::Skip,
            epoch: 0,
            secs: 1,
            fingerprint: fingerprint.into(),
            mode: Some(Mode::Parallel),
            producer: Some(Producer::Diff),
            rc: Some(77),
        }
    }

    fn skip_req_rec(fingerprint: &str) -> ResultRecord {
        ResultRecord {
            status: Status::SkipReq,
            epoch: 0,
            secs: 0,
            fingerprint: fingerprint.into(),
            mode: Some(Mode::Parallel),
            producer: Some(Producer::Diff),
            rc: None,
        }
    }

    #[test]
    fn parses_tab_separated_entries_and_drops_comments_and_blanks() {
        let text = "# comment\n\ntest-a.sh\tskip:no docker\treason a\ntest-b.sh\trequires:claude\t\n";
        let e = parse(text);
        assert_eq!(
            e,
            vec![
                AllowEntry {
                    suite: "test-a.sh".into(),
                    requirement: "skip:no docker".into(),
                    why: "reason a".into()
                },
                AllowEntry {
                    suite: "test-b.sh".into(),
                    requirement: "requires:claude".into(),
                    why: "".into()
                },
            ]
        );
    }

    #[test]
    fn a_malformed_line_is_dropped_not_fatal() {
        let e = parse("just-one-field\ntest-a.sh\tskip:x\tok\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].suite, "test-a.sh");
    }

    #[test]
    fn reserved_words_cover_the_four_testenv_owned_categories() {
        assert_eq!(reserved_word("server testdb not available"), Some("testdb"));
        assert_eq!(reserved_word("no fixture database reachable"), Some("fixture"));
        assert_eq!(reserved_word("template lookup failed"), Some("template"));
        assert_eq!(
            reserved_word("cargo not found — spira-config binary cannot be built"),
            Some("binary")
        );
        assert_eq!(reserved_word("no systemd --user session"), None);
    }

    #[test]
    fn load_refuses_a_reserved_declaration() {
        let text = "test-poison.sh\tskip:server testdb not available\tsneaking it past the gate\n";
        let err = SkipGate::load(text).unwrap_err();
        assert!(err.contains("test-poison.sh"));
        assert!(err.contains("testdb"));
    }

    #[test]
    fn load_accepts_a_clean_file_and_declared_lookup_is_exact() {
        let gate = SkipGate::load("test-a.sh\tskip:no docker on this host\thost-only\n").unwrap();
        assert!(gate.declared("test-a.sh", "skip:no docker on this host"));
        assert!(!gate.declared("test-a.sh", "skip:something else"));
        assert!(!gate.declared("test-b.sh", "skip:no docker on this host"));
    }

    #[test]
    fn apply_leaves_a_declared_skip_alone() {
        let gate = SkipGate::load("test-a.sh\tskip:no docker\thost-only\n").unwrap();
        let rec = skip_rec("skip:no docker");
        let out = apply(&gate, "test-a.sh", false, rec.clone());
        assert_eq!(out, rec);
    }

    #[test]
    fn apply_reclassifies_an_undeclared_skip_to_red_keeping_the_fingerprint() {
        let gate = SkipGate::load("").unwrap();
        let rec = skip_rec("skip:server testdb not available");
        let out = apply(&gate, "test-poison.sh", false, rec);
        assert_eq!(out.status, Status::Red);
        assert_eq!(out.fingerprint, "skip:server testdb not available");
        assert!(out.status.blocking());
    }

    #[test]
    fn apply_reclassifies_an_undeclared_skip_req_to_red() {
        let gate = SkipGate::load("").unwrap();
        let rec = skip_req_rec("requires:claude");
        let out = apply(&gate, "test-concierge.sh", false, rec);
        assert_eq!(out.status, Status::Red);
        assert_eq!(out.fingerprint, "requires:claude");
    }

    #[test]
    fn apply_downgrades_an_undeclared_skip_to_quarantined_red_when_quarantined() {
        let gate = SkipGate::load("").unwrap();
        let rec = skip_rec("skip:flaky widget");
        let out = apply(&gate, "test-flaky.sh", true, rec);
        assert_eq!(out.status, Status::QuarantinedRed);
        assert!(!out.status.blocking());
    }

    #[test]
    fn apply_ignores_every_other_status() {
        let gate = SkipGate::load("").unwrap();
        let ok = ResultRecord {
            status: Status::Ok,
            epoch: 0,
            secs: 1,
            fingerprint: "-".into(),
            mode: Some(Mode::Parallel),
            producer: Some(Producer::Diff),
            rc: Some(0),
        };
        assert_eq!(apply(&gate, "test-a.sh", false, ok.clone()), ok);
    }
}
