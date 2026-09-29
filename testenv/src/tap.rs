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
}
