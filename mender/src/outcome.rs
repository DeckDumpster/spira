//! The five triage outcomes and what each must carry before the machine is asked to apply it.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mended,
    Environment,
    Flaky,
    Return,
    Unsure,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "mended" => Kind::Mended,
            "environment" => Kind::Environment,
            "flaky" => Kind::Flaky,
            "return" => Kind::Return,
            "unsure" => Kind::Unsure,
            _ => return None,
        })
    }
}

/// `path.ext:LINE` — the shape a returned member's builder can act on without starting blind.
pub fn cites_file_line(text: &str) -> bool {
    text.split_whitespace().any(|tok| {
        let tok = tok.trim_matches(|c: char| matches!(c, '`' | '(' | ')' | ',' | ';' | ':' | '"' | '\'' | '.'));
        let Some((path, line)) = tok.rsplit_once(':') else { return false };
        let file = path.rsplit('/').next().unwrap_or("");
        !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) && file.contains('.') && !file.starts_with('.') && !file.ends_with('.')
    })
}

pub struct Answer<'a> {
    pub kind: Kind,
    pub diagnosis: &'a str,
    pub checks: Option<&'a str>,
    pub ask: Option<&'a str>,
}

/// The diagnosis to record, or why the outcome cannot be recorded yet.
pub fn validate(a: &Answer) -> Result<String, String> {
    let d = a.diagnosis.trim();
    if d.is_empty() {
        return Err("an outcome carries its diagnosis on stdin: what the failure is and why".into());
    }
    match a.kind {
        Kind::Return if !cites_file_line(d) => Err("return: the diagnosis must cite the defect as <file>:<line>, so the builder never starts blind".into()),
        Kind::Mended => match a.checks.map(str::trim).filter(|c| !c.is_empty()) {
            None => Err("mended: --checks must say what ran (each touched crate's `cargo test -p`, the suites naming touched non-Rust files) and that it passed; a failing check is a return".into()),
            Some(c) => Ok(format!("{d}\nchecks: {c}")),
        },
        _ if a.checks.is_some() || a.ask.is_some() => Err("--checks and --ask belong to mended alone".into()),
        _ => Ok(d.to_string()),
    }
}

/// The terminal mending event for every outcome but a mend, which goes through `mending mend`.
/// The clock is the machine's: `at` is overwritten there.
pub fn terminal_event(kind: Kind, diagnosis: &str) -> Value {
    match kind {
        Kind::Mended => unreachable!("a mend is assessed by `mending mend`, never sent as an event"),
        Kind::Environment => json!({"Environment": {"at": 0}}),
        Kind::Flaky => json!({"Flaky": {"at": 0}}),
        Kind::Unsure => json!({"Unsure": {"at": 0}}),
        Kind::Return => json!({"Return": {"at": 0, "diagnosis": diagnosis}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a<'a>(kind: Kind, diagnosis: &'a str, checks: Option<&'a str>) -> Answer<'a> {
        Answer { kind, diagnosis, checks, ask: None }
    }

    #[test]
    fn every_name_parses_and_nothing_else_does() {
        for (s, k) in [("mended", Kind::Mended), ("environment", Kind::Environment), ("flaky", Kind::Flaky), ("return", Kind::Return), ("unsure", Kind::Unsure)] {
            assert_eq!(Kind::parse(s), Some(k));
        }
        assert_eq!(Kind::parse("fixed"), None);
        assert_eq!(Kind::parse("Mended"), None);
    }

    #[test]
    fn file_line_is_recognised_and_prose_is_not() {
        assert!(cites_file_line("wrong approach at spira-lc/src/work.rs:412: reads bd directly"));
        assert!(cites_file_line("see (spira/lib.sh:88)."));
        assert!(!cites_file_line("the approach in work.rs is wrong"));
        assert!(!cites_file_line("took 12:30 minutes"));
        assert!(!cites_file_line("ratio a:1"));
    }

    #[test]
    fn a_return_without_file_line_is_refused_and_with_one_is_kept() {
        assert!(validate(&a(Kind::Return, "it is wrong somewhere", None)).is_err());
        assert_eq!(validate(&a(Kind::Return, "bad at x/y.rs:9", None)).unwrap(), "bad at x/y.rs:9");
    }

    #[test]
    fn mended_needs_the_checks_and_records_them_in_the_diagnosis() {
        assert!(validate(&a(Kind::Mended, "fixed", None)).is_err());
        assert!(validate(&a(Kind::Mended, "fixed", Some("  "))).is_err());
        assert_eq!(validate(&a(Kind::Mended, "fixed", Some("cargo test -p spira-lc ok"))).unwrap(), "fixed\nchecks: cargo test -p spira-lc ok");
    }

    #[test]
    fn checks_on_any_other_outcome_and_an_empty_diagnosis_are_refused() {
        assert!(validate(&a(Kind::Flaky, "ok", Some("x"))).is_err());
        for k in [Kind::Mended, Kind::Environment, Kind::Flaky, Kind::Return, Kind::Unsure] {
            assert!(validate(&a(k, "  \n", Some("x"))).is_err());
        }
    }

    #[test]
    fn an_ask_on_any_outcome_but_a_mend_is_refused() {
        let with_ask = |kind| validate(&Answer { kind, diagnosis: "bad at x/y.rs:9", checks: None, ask: Some("a1") });
        assert!(with_ask(Kind::Return).is_err());
        assert!(with_ask(Kind::Flaky).is_err());
    }

    #[test]
    fn the_terminal_event_names_a_lifecycle_event_with_no_clock_of_its_own() {
        assert_eq!(terminal_event(Kind::Environment, "d"), json!({"Environment": {"at": 0}}));
        assert_eq!(terminal_event(Kind::Return, "d"), json!({"Return": {"at": 0, "diagnosis": "d"}}));
    }
}
