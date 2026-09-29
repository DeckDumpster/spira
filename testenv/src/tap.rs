//! A tolerant TAP reader for a suite's captured output (DESIGN.md §3.5). Informational: the
//! verdict is the suite's exit status; this names *which* assertions failed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapSummary {
    pub plan: Option<u64>,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub todo: u64,
    pub bail_out: Option<String>,
    /// Every `not ok` line that is not a TODO, verbatim (trimmed).
    pub not_ok: Vec<String>,
}

fn directive(desc: &str) -> Option<&'static str> {
    let (_, d) = desc.split_once('#')?;
    let d = d.trim_start().to_ascii_uppercase();
    if d.starts_with("SKIP") {
        Some("skip")
    } else if d.starts_with("TODO") {
        Some("todo")
    } else {
        None
    }
}

/// The reason testlib.sh's `skip <reason>` names, from its TAP skip-all form
/// (`1..0 # SKIP <reason>`, DESIGN.md §3.7). `None` when the suite exited 77 without
/// following the convention — the caller then has no requirement to check.
pub fn skip_all_reason(output: &str) -> Option<String> {
    for raw in output.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("1..0") else {
            continue;
        };
        let Some(directive) = rest.trim_start().strip_prefix('#') else {
            continue;
        };
        let directive = directive.trim_start();
        if directive.len() >= 4 && directive[..4].eq_ignore_ascii_case("skip") {
            return Some(directive[4..].trim_start().to_string());
        }
    }
    None
}

pub fn parse(output: &str) -> TapSummary {
    let mut t = TapSummary::default();
    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("1..") {
            if let Ok(n) = rest.split_whitespace().next().unwrap_or("").parse() {
                t.plan = Some(n);
            }
        } else if let Some(rest) = line.strip_prefix("Bail out!") {
            t.bail_out = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("not ok") {
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                match directive(rest) {
                    Some("todo") => t.todo += 1,
                    Some("skip") => t.skipped += 1,
                    _ => {
                        t.failed += 1;
                        t.not_ok.push(line.to_string());
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix("ok") {
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                match directive(rest) {
                    Some("skip") => t.skipped += 1,
                    Some("todo") => t.todo += 1,
                    _ => t.passed += 1,
                }
            }
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_names_failures() {
        let out = "1..5\nok 1 - a\nnot ok 2 - b\n  ---\nok 3 - c # SKIP no dolt\nnot ok 4 - d # TODO later\nnot ok 5 - launcher's --model came from persona.modeltest.model\n";
        let t = parse(out);
        assert_eq!(t.plan, Some(5));
        assert_eq!((t.passed, t.failed, t.skipped, t.todo), (1, 2, 1, 1));
        assert_eq!(
            t.not_ok,
            vec![
                "not ok 2 - b",
                "not ok 5 - launcher's --model came from persona.modeltest.model"
            ]
        );
    }

    #[test]
    fn bail_out_and_non_tap_noise() {
        let t = parse("okay then\nnot okay\nBail out! requires: testenv\n");
        assert_eq!(t.bail_out.as_deref(), Some("requires: testenv"));
        assert_eq!((t.passed, t.failed), (0, 0));
    }

    #[test]
    fn skip_all_reason_reads_testlibs_skip_all_form() {
        assert_eq!(
            skip_all_reason("1..0 # SKIP no dolt on this host\n"),
            Some("no dolt on this host".to_string())
        );
        // case-insensitive directive word, and text logged before the plan line is ignored.
        assert_eq!(
            skip_all_reason("configuring...\n1..0 # skip lowercase directive\n"),
            Some("lowercase directive".to_string())
        );
    }

    #[test]
    fn skip_all_reason_is_none_without_the_convention() {
        assert_eq!(skip_all_reason("1..3\nok 1 - a\n"), None);
        assert_eq!(skip_all_reason("some noise\nno plan line here\n"), None);
        // a per-case SKIP directive is not the whole-suite form (plan is not "1..0")
        assert_eq!(skip_all_reason("1..1\nok 1 - a # SKIP not applicable\n"), None);
    }
}
