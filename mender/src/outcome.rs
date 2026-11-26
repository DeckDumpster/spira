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
    /// The members a return sends back, and the members of the round it may name.
    pub members: &'a [&'a str],
    pub round: &'a [&'a str],
}

/// The diagnosis to record, or why the outcome cannot be recorded yet.
pub fn validate(a: &Answer) -> Result<String, String> {
    let d = a.diagnosis.trim();
    if d.is_empty() {
        return Err("an outcome carries its diagnosis on stdin: what the failure is and why".into());
    }
    match a.kind {
        Kind::Return if !cites_file_line(d) => Err("return: the diagnosis must cite the defect as <file>:<line>, so the builder never starts blind".into()),
        Kind::Return => match a.members.iter().find(|m| !a.round.contains(m)) {
            _ if a.members.is_empty() => Err(format!("return: name the member(s) it sends back with --member <bead>; the spec's members: {}", a.round.join(", "))),
            Some(m) => Err(format!("return: {m} is not a member of the round; the spec's members: {}", a.round.join(", "))),
            None if a.checks.is_some() || a.ask.is_some() => Err("--checks and --ask belong to mended alone".into()),
            None => Ok(d.to_string()),
        },
        _ if !a.members.is_empty() => Err("--member belongs to return alone".into()),
        Kind::Mended => match a.checks.map(str::trim).filter(|c| !c.is_empty()) {
            None => Err("mended: --checks must say what ran (each touched crate's `tools/test-sandbox/runner --around cargo test -p`, the suites naming touched non-Rust files) and that it passed; a failing check is a return".into()),
            Some(c) => Ok(format!("{d}\nchecks: {c}")),
        },
        _ if a.checks.is_some() || a.ask.is_some() => Err("--checks and --ask belong to mended alone".into()),
        _ => Ok(d.to_string()),
    }
}

/// The terminal mending event for every outcome but a mend, which goes through `mending mend`.
/// The clock is the machine's: `at` is overwritten there.
pub fn terminal_event(kind: Kind, diagnosis: &str, members: &[&str]) -> Value {
    match kind {
        Kind::Mended => unreachable!("a mend is assessed by `mending mend`, never sent as an event"),
        Kind::Environment => json!({"Environment": {"at": 0}}),
        Kind::Flaky => json!({"Flaky": {"at": 0}}),
        Kind::Unsure => json!({"Unsure": {"at": 0}}),
        Kind::Return => json!({"Return": {"at": 0, "diagnosis": diagnosis, "members": members}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a<'a>(kind: Kind, diagnosis: &'a str, checks: Option<&'a str>) -> Answer<'a> {
        Answer { kind, diagnosis, checks, ask: None, members: if kind == Kind::Return { &["sp-a"] } else { &[] }, round: &["sp-a", "sp-b"] }
    }

    #[test_id(tmy4uci)]
    #[test]
    fn every_name_parses_and_nothing_else_does() {
        for (s, k) in [("mended", Kind::Mended), ("environment", Kind::Environment), ("flaky", Kind::Flaky), ("return", Kind::Return), ("unsure", Kind::Unsure)] {
            assert_eq!(Kind::parse(s), Some(k));
        }
        assert_eq!(Kind::parse("fixed"), None);
        assert_eq!(Kind::parse("Mended"), None);
    }

    #[test_id(tdzd12u)]
    #[test]
    fn file_line_is_recognised_and_prose_is_not() {
        assert!(cites_file_line("wrong approach at spira-lc/src/work.rs:412: reads bd directly"));
        assert!(cites_file_line("see (spira/lib.sh:88)."));
        assert!(!cites_file_line("the approach in work.rs is wrong"));
        assert!(!cites_file_line("took 12:30 minutes"));
        assert!(!cites_file_line("ratio a:1"));
    }

    #[test_id(tlltvue)]
    #[test]
    fn a_return_without_file_line_is_refused_and_with_one_is_kept() {
        assert!(validate(&a(Kind::Return, "it is wrong somewhere", None)).is_err());
        assert_eq!(validate(&a(Kind::Return, "bad at x/y.rs:9", None)).unwrap(), "bad at x/y.rs:9");
    }

    #[test_id(t32xpb5)]
    #[test]
    fn mended_needs_the_checks_and_records_them_in_the_diagnosis() {
        assert!(validate(&a(Kind::Mended, "fixed", None)).is_err());
        assert!(validate(&a(Kind::Mended, "fixed", Some("  "))).is_err());
        assert_eq!(validate(&a(Kind::Mended, "fixed", Some("cargo test -p spira-lc ok"))).unwrap(), "fixed\nchecks: cargo test -p spira-lc ok");
    }

    #[test_id(tsng6e8)]
    #[test]
    fn checks_on_any_other_outcome_and_an_empty_diagnosis_are_refused() {
        assert!(validate(&a(Kind::Flaky, "ok", Some("x"))).is_err());
        for k in [Kind::Mended, Kind::Environment, Kind::Flaky, Kind::Return, Kind::Unsure] {
            assert!(validate(&a(k, "  \n", Some("x"))).is_err());
        }
    }

    #[test_id(t44cm5g)]
    #[test]
    fn an_ask_on_any_outcome_but_a_mend_is_refused() {
        let with_ask = |kind| validate(&Answer { kind, diagnosis: "bad at x/y.rs:9", checks: None, ask: Some("a1"), members: &["sp-a"], round: &["sp-a"] });
        assert!(with_ask(Kind::Return).is_err());
        assert!(with_ask(Kind::Flaky).is_err());
    }

    #[test_id(t8j31c2)]
    #[test]
    fn a_return_names_a_member_of_the_round_or_is_refused_naming_the_round() {
        let ret = |members: &[&str]| validate(&Answer { kind: Kind::Return, diagnosis: "bad at x/y.rs:9", checks: None, ask: None, members, round: &["sp-a", "sp-b"] });
        assert!(ret(&["sp-b"]).is_ok());
        for bad in [ret(&[]), ret(&["sp-z"]), ret(&["sp-a", "sp-z"])] {
            assert!(bad.unwrap_err().contains("sp-a, sp-b"));
        }
    }

    #[test_id(tvn7ozu)]
    #[test]
    fn a_member_on_any_other_outcome_is_refused() {
        let flaky = validate(&Answer { kind: Kind::Flaky, diagnosis: "ok", checks: None, ask: None, members: &["sp-a"], round: &["sp-a"] });
        assert!(flaky.is_err());
    }

    #[test_id(tgdwhx2)]
    #[test]
    fn the_return_event_carries_its_members() {
        assert_eq!(terminal_event(Kind::Return, "d", &["sp-b"]), json!({"Return": {"at": 0, "diagnosis": "d", "members": ["sp-b"]}}));
    }
}
