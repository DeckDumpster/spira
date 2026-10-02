//! A tolerant TAP reader for a suite's captured output (DESIGN.md §3.5). Informational: the
//! verdict is the suite's exit status; this names *which* assertions failed.
//!
//! `jsonl_rows` (DESIGN.md §3.5a) is a second, unrelated reader of the same captured output:
//! `spira/tap-jsonl.sh`'s `tap_jsonl_rows`, ported here because this is testenv's reporting
//! module and gate-diag (its only remaining caller) already depends on this crate for
//! `crate::suite`. It replaces the bash's `. tap-jsonl.sh` shell-out (sp-9gd4e).

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

// ------------------------------------------------------------------- jsonl_rows

/// `_tap_json_escape` (full form): backslash, quote, newline and tab. Used for the suite name
/// and tier, and for the whole fallback row — every string that is not itself extracted from
/// a TAP line.
fn esc_full(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// The awk `esc()` inside the TAP branch: backslash and quote only. TAP case names and
/// failure details keep any other character verbatim, including newlines — the same
/// asymmetry the bash carried (its `esc()` is a different, narrower function from
/// `_tap_json_escape`).
fn esc_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

/// `suite_uc_of`'s effective (second, overriding) definition: a `# covers:` token matching
/// the shell glob `UC-*-[0-9][0-9]` — starts with `UC-`, ends with `-` then two ASCII digits.
fn is_uc_token(tok: &str) -> bool {
    let b = tok.as_bytes();
    let n = b.len();
    n >= 6 && tok.starts_with("UC-") && b[n - 3] == b'-' && b[n - 2].is_ascii_digit() && b[n - 1].is_ascii_digit()
}

/// `[]`, or a JSON array of the tokens verbatim (double-quoted, unescaped — as
/// `_tap_uc_json` printed them; UC ids are `[A-Za-z0-9-]` and need none).
fn uc_json(uc: &[String]) -> String {
    if uc.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[");
    for (i, t) in uc.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(t);
        out.push('"');
    }
    out.push(']');
    out
}

/// One pending TAP case row, held until the next transition or EOF closes it.
struct Pending {
    case: String,
    status: &'static str,
    detail: String,
}

/// `tap_jsonl_rows <suite> <src> <out> <fallback-status> <secs>` (`spira/tap-jsonl.sh`,
/// DESIGN.md §3.5a): one JSONL row per line, `\n`-terminated; `""` when there is nothing to
/// emit. `suite_source` is the suite's own script text (`""` when it could not be read —
/// tier and covers/UC then read as undeclared, exactly as the bash's header accessors do on
/// a missing file). `out_text` is `None` when the captured output is unreadable, which is
/// the same "no TAP" case as output that does not open with a `TAP version 14` line.
pub fn jsonl_rows(suite: &str, suite_source: &str, out_text: Option<&str>, fallback: &str, secs: &str) -> String {
    let tier = suite_select::header::tier_of(suite_source).unwrap_or_default();
    let uc: Vec<String> = suite_select::header::covers_of(suite_source)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| is_uc_token(t))
        .collect();
    let uc_j = uc_json(&uc);

    if let Some(out) = out_text {
        if out.lines().next() == Some("TAP version 14") {
            let mut rows = String::new();
            let mut pending: Option<Pending> = None;
            let emit = |rows: &mut String, p: Pending| {
                rows.push_str(&format!(
                    "{{\"suite\":\"{}\",\"tier\":\"{}\",\"case\":\"{}\",\"status\":\"{}\",\"seconds\":{},\"uc\":{},\"detail\":\"{}\"}}\n",
                    esc_full(suite),
                    esc_full(&tier),
                    esc_case(&p.case),
                    p.status,
                    secs,
                    uc_j,
                    esc_case(&p.detail),
                ));
            };
            for line in out.lines() {
                if let Some(case) = strip_case_prefix(line, "ok") {
                    if let Some(p) = pending.take() {
                        emit(&mut rows, p);
                    }
                    pending = Some(Pending { case, status: "pass", detail: String::new() });
                } else if let Some(case) = strip_case_prefix(line, "not ok") {
                    if let Some(p) = pending.take() {
                        emit(&mut rows, p);
                    }
                    pending = Some(Pending { case, status: "fail", detail: String::new() });
                } else if let Some(rest) = line.strip_prefix("# ") {
                    if let Some(p) = pending.as_mut() {
                        if p.status == "fail" && p.detail.is_empty() {
                            p.detail = rest.to_string();
                        }
                    }
                } else if let Some(detail) = line.strip_prefix("1..0 # SKIP ") {
                    if let Some(p) = pending.take() {
                        emit(&mut rows, p);
                    }
                    emit(&mut rows, Pending { case: "(suite)".into(), status: "skip", detail: detail.to_string() });
                } else if let Some(detail) = line.strip_prefix("Bail out! ") {
                    if let Some(p) = pending.take() {
                        emit(&mut rows, p);
                    }
                    emit(&mut rows, Pending { case: "(suite)".into(), status: "bail", detail: detail.to_string() });
                } else if let Some(p) = pending.take() {
                    emit(&mut rows, p);
                }
            }
            if let Some(p) = pending.take() {
                emit(&mut rows, p);
            }
            return rows;
        }
    }

    let rstatus = match fallback {
        "ok" => "pass",
        "skip" => "skip",
        "unreached" => "unreached",
        "timeout" | "red" | "quarantined-red" => "fail",
        _ => "fail",
    };
    format!(
        "{{\"suite\":\"{}\",\"tier\":\"{}\",\"case\":\"(suite)\",\"status\":\"{}\",\"seconds\":{},\"uc\":{},\"detail\":\"not migrated to testlib.sh\"}}\n",
        esc_full(suite),
        esc_full(&tier),
        rstatus,
        secs,
        uc_j,
    )
}

pub fn case_of(line: &str, word: &str) -> Option<String> {
    strip_case_prefix(line, word)
}

/// `^(not )?ok [0-9]+ - ` — the leading digits and the `" - "` separator, literally (no
/// regex crate needed for this one shape). `word` is `"ok"` or `"not ok"`.
fn strip_case_prefix(line: &str, word: &str) -> Option<String> {
    let rest = line.strip_prefix(word)?.strip_prefix(' ')?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digits == 0 {
        return None;
    }
    rest[digits..].strip_prefix(" - ").map(str::to_string)
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

    // --------------------------------------------------------------- jsonl_rows

    #[test]
    fn jsonl_rows_one_row_per_tap_case_with_a_failure_detail() {
        let src = "#!/usr/bin/env bash\n# covers: a.sh UC-gate-01\n# tier: T1\nset -u\n";
        let out = "TAP version 14\n1..2\nok 1 - first\nnot ok 2 - second\n# assertion: expected 1 got 2\n";
        let rows = jsonl_rows("test-x.sh", src, Some(out), "red", "3");
        let lines: Vec<&str> = rows.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            r#"{"suite":"test-x.sh","tier":"T1","case":"first","status":"pass","seconds":3,"uc":["UC-gate-01"],"detail":""}"#
        );
        assert_eq!(
            lines[1],
            r#"{"suite":"test-x.sh","tier":"T1","case":"second","status":"fail","seconds":3,"uc":["UC-gate-01"],"detail":"assertion: expected 1 got 2"}"#
        );
    }

    #[test]
    fn jsonl_rows_skip_all_and_bail_out_are_suite_level() {
        let skip = jsonl_rows("test-s.sh", "", Some("TAP version 14\n1..0 # SKIP no dolt on this host\n"), "red", "1");
        assert_eq!(
            skip.trim_end(),
            r#"{"suite":"test-s.sh","tier":"","case":"(suite)","status":"skip","seconds":1,"uc":[],"detail":"no dolt on this host"}"#
        );
        let bail = jsonl_rows("test-b.sh", "", Some("TAP version 14\nok 1 - a\nBail out! requires: testenv\n"), "red", "2");
        let lines: Vec<&str> = bail.lines().collect();
        assert_eq!(lines.len(), 2, "the pending pass row is flushed before the bail row: {lines:?}");
        assert!(lines[1].contains(r#""status":"bail""#));
        assert!(lines[1].contains(r#""detail":"requires: testenv""#));
    }

    #[test]
    fn jsonl_rows_falls_back_to_one_suite_row_when_not_migrated() {
        assert_eq!(
            jsonl_rows("test-old.sh", "", None, "red", "5").trim_end(),
            r#"{"suite":"test-old.sh","tier":"","case":"(suite)","status":"fail","seconds":5,"uc":[],"detail":"not migrated to testlib.sh"}"#
        );
        // output exists but does not open with the TAP version line: same fallback.
        assert_eq!(
            jsonl_rows("test-old.sh", "", Some("some noise\nTAP version 14\n"), "ok", "0").trim_end(),
            r#"{"suite":"test-old.sh","tier":"","case":"(suite)","status":"pass","seconds":0,"uc":[],"detail":"not migrated to testlib.sh"}"#
        );
        for (fallback, status) in [
            ("ok", "pass"),
            ("skip", "skip"),
            ("unreached", "unreached"),
            ("timeout", "fail"),
            ("red", "fail"),
            ("quarantined-red", "fail"),
            ("something-else", "fail"),
        ] {
            let row = jsonl_rows("s.sh", "", None, fallback, "0");
            assert!(row.contains(&format!("\"status\":\"{status}\"")), "{fallback} -> {row}");
        }
    }

    #[test]
    fn jsonl_rows_escapes_full_for_suite_and_tier_but_minimal_for_case_and_detail() {
        // Suite/tier: backslash, quote, newline and tab all escaped.
        let row = jsonl_rows("weird\"na\\me.sh", "# tier: T\t2\n", None, "red", "0");
        assert!(row.contains(r#""suite":"weird\"na\\me.sh""#));
        assert!(row.contains(r#""tier":"T\t2""#));
        // Case/detail from TAP lines: only backslash and quote — a tab stays a literal tab
        // (esc_full would have turned it into the two characters `\t`).
        let out = "TAP version 14\nnot ok 1 - has \"quotes\" and \\slashes\\\n# detail\twith a tab\n";
        let rows = jsonl_rows("t.sh", "", Some(out), "red", "0");
        assert!(rows.contains(r#""case":"has \"quotes\" and \\slashes\\""#));
        assert!(rows.contains("\"detail\":\"detail\twith a tab\""), "{rows}");
    }

    #[test]
    fn jsonl_rows_reads_tier_and_uc_from_the_suite_source_never_the_output() {
        let src = "# covers: a.sh UC-x-01 b/* UC-y-02\n# tier: T3\nset -u\n";
        let row = jsonl_rows("t.sh", src, None, "red", "0");
        assert!(row.contains(r#""tier":"T3""#));
        assert!(row.contains(r#""uc":["UC-x-01","UC-y-02"]"#), "{row}");
        // An unreadable suite source (empty string) reads as undeclared, not an error.
        let row = jsonl_rows("t.sh", "", None, "red", "0");
        assert!(row.contains(r#""tier":"""#));
        assert!(row.contains(r#""uc":[]"#));
    }

    #[test]
    fn uc_token_matches_the_bash_glob_exactly() {
        assert!(is_uc_token("UC-x-01"));
        assert!(is_uc_token("UC--01"));
        assert!(!is_uc_token("UC-1"));
        assert!(!is_uc_token("uc-x-01"));
        assert!(!is_uc_token("UCX-01"));
        assert!(!is_uc_token("UC-x-1"));
        assert!(!is_uc_token("a.sh"));
    }
}
