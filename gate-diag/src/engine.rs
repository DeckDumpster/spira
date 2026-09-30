//! The pure decision surface (DESIGN.md "Design"): FAIL-line extraction, declared-timeout
//! lookup, retry classification, and every rendered string and JSON/XML shape. No file IO
//! happens here; `real.rs` gathers inputs and `main.rs` calls these.

use std::collections::BTreeMap;

/// One suite's `.result` file, first field only (`ok red timeout skip …`).
pub fn is_red(status: &str) -> bool {
    matches!(status, "red" | "timeout")
}

/// `grep -E '^  FAIL  |^FAIL[: ]|^not ok '` — the tight tier: a line the suite itself marked
/// as a failure in one of testlib.sh's own shapes.
fn tier1_fail_line(line: &str) -> bool {
    line.starts_with("  FAIL  ")
        || line.starts_with("FAIL:")
        || line.starts_with("FAIL ")
        || line.starts_with("not ok ")
}

/// `grep -E 'FAIL|not ok'` — the loose fallback tier, for a suite that never adopted
/// testlib.sh's exact prefixes but still printed the word somewhere.
fn tier2_fail_line(line: &str) -> bool {
    line.contains("FAIL") || line.contains("not ok")
}

/// The FAIL lines a red suite's raw output names, tier 1 if any matched, else tier 2.
pub fn fail_lines(raw_out: &str) -> Vec<String> {
    let t1: Vec<String> = raw_out.lines().filter(|l| tier1_fail_line(l)).map(String::from).collect();
    if !t1.is_empty() {
        return t1;
    }
    raw_out.lines().filter(|l| tier2_fail_line(l)).map(String::from).collect()
}

/// The one-line summary of why a suite is red: the first FAIL line (leading whitespace
/// trimmed), or — when the suite died before printing one — a fact-based line built from its
/// exit code, wall time and declared budget, exactly as informative as a human opening the
/// log would find (DESIGN.md, mirroring the bash's own comment).
pub fn first_fail(raw_out: &str, lines: &[String], rc: &str, secs: &str, decl_to: u64) -> String {
    if raw_out.trim().is_empty() {
        return format!("(died rc={rc} at {secs}s/{decl_to}s — no output)");
    }
    if let Some(l) = lines.first() {
        return l.trim_start().to_string();
    }
    let last = raw_out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    format!("(died rc={rc} at {secs}s/{decl_to}s — last: {last})")
}

/// `sed -n 's/^# *timeout: *//p' <suite-source> | head -1`, parsed as a plain integer;
/// `SPIRA_SUITE_TIMEOUT` (default 600) when absent or not a plain integer.
pub fn declared_timeout(suite_source: Option<&str>, default_timeout: u64) -> u64 {
    let Some(src) = suite_source else { return default_timeout };
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix("timeout:") {
                let v = v.trim();
                if let Ok(n) = v.parse::<u64>() {
                    return n;
                }
                return default_timeout;
            }
        }
    }
    default_timeout
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryClass {
    /// Red the first time, green on a serial rerun — charged to nobody, marked flaky.
    Flaky,
    /// Red both times — a real, reproducible defect.
    RedRed,
    /// No retry ran, or it produced something this binary does not interpret further.
    Uncertain,
}

/// `_retry_status`'s three-arm `case`, with no `*)` beyond `red` in the bash: `ok` → flaky,
/// `red`/`timeout` → red-red, anything else (including "no retry ran at all") → uncertain.
pub fn classify_retry(retry_status: Option<&str>) -> (RetryClass, &'static str) {
    match retry_status {
        Some("ok") => (RetryClass::Flaky, "red-green (flake)"),
        Some("red") | Some("timeout") => (RetryClass::RedRed, "red-red"),
        _ => (RetryClass::Uncertain, "red"),
    }
}

/// One line of the markdown summary table (`| suite | secs (rc) | verdict | first FAIL |`).
/// The FAIL text has `|` folded to `!` and newlines stripped, then truncated to 80 chars —
/// exactly the bash's own guard against a table row wrapping mid-cell.
pub fn summary_row(suite: &str, secs: &str, rc: &str, verdict: &str, fail: &str) -> String {
    let fs: String = fail.replace('|', "!").replace(['\n', '\r'], "");
    let fs: String = fs.chars().take(80).collect();
    format!("| {suite} | {secs}s (rc={rc}) | {verdict} | {fs} |")
}

/// The last `n` lines of `s`, newline-joined (`tail -n`).
pub fn tail_n(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub fn summary_header() -> &'static str {
    "| Suite | Duration | Verdict | First FAIL line |\n|-------|----------|---------|-----------------|"
}

/// A GitHub Actions annotation's message: the same text as the table's FAIL cell, newlines
/// stripped and truncated to 200 chars (`cut -c1-200` in the bash).
pub fn annotation_text(fail: &str) -> String {
    fail.chars().filter(|&c| c != '\n').take(200).collect()
}

/// The GitHub Actions per-suite block: a collapsible `::group::`, the output (or `(no
/// output)`), and one `::error` annotation naming the suite's own file path.
pub fn render_gha_block(suite: &str, verdict: &str, rc: &str, secs: &str, raw_out: &str, lines: &[String], tail: &str, tail_n: usize, annotation: &str) -> String {
    let mut o = format!("::group::{suite}  {verdict}  rc={rc}  {secs}s\n");
    if raw_out.trim().is_empty() {
        o += "(no output)\n";
    } else {
        if !lines.is_empty() {
            o += "--- FAIL lines ---\n";
            for l in lines {
                o += l;
                o.push('\n');
            }
        }
        o += &format!("--- last {tail_n} lines ---\n");
        o += tail;
        o.push('\n');
    }
    o += "::endgroup::\n";
    o += &format!("::error file=spira/{suite}::{annotation}\n");
    o
}

/// The plain-terminal per-suite block (no GitHub Actions in force).
pub fn render_plain_block(suite: &str, verdict: &str, rc: &str, secs: &str, raw_out: &str, lines: &[String], tail: &str, tail_n: usize) -> String {
    let mut o = format!("\n=== {suite}  {verdict}  rc={rc}  {secs}s ===\n");
    if raw_out.trim().is_empty() {
        o += "(no output)\n";
    } else {
        for l in lines {
            o += l;
            o.push('\n');
        }
        o += &format!("--- last {tail_n} lines ---\n");
        o += tail;
        o.push('\n');
    }
    o
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn json_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{}\"", json_escape(s))).collect();
    format!("[{}]", inner.join(","))
}

/// `red-suites.json` — the artifact `forge.sh` reads instead of per-suite annotations
/// (GitHub caps those at 10 per step).
pub fn red_suites_json(red: &[String], flaky: &[String]) -> String {
    format!(
        "{{\"red\":{},\"flaky\":{},\"red_count\":{}}}",
        json_str_array(red),
        json_str_array(flaky),
        red.len()
    )
}

/// The `(verdict)` rows appended to `results.jsonl`: one `red-red` per still-red suite, one
/// `red-green` per flaky one.
pub fn verdict_rows(red: &[String], flaky: &[String]) -> String {
    let mut out = String::new();
    for s in red {
        out += &format!(
            "{{\"suite\":\"{}\",\"tier\":\"\",\"case\":\"(verdict)\",\"status\":\"red-red\",\"seconds\":0,\"uc\":[],\"detail\":\"\"}}\n",
            json_escape(s)
        );
    }
    for s in flaky {
        out += &format!(
            "{{\"suite\":\"{}\",\"tier\":\"\",\"case\":\"(verdict)\",\"status\":\"red-green\",\"seconds\":0,\"uc\":[],\"detail\":\"\"}}\n",
            json_escape(s)
        );
    }
    out
}

/// One `results.jsonl` row, as `tap_jsonl_rows` (`spira/tap-jsonl.sh`, not ported here —
/// DESIGN.md "Non-goals") writes it.
#[derive(Clone, Debug, Default)]
pub struct TapRow {
    pub suite: String,
    pub case: String,
    pub status: String,
    pub seconds: serde_json::Value,
    pub detail: String,
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// The JUnit XML the bash's inline `python3` built from `results.jsonl`, ported directly
/// (pure text — no external tool needed). One difference from the bash, not a behavior
/// change: attribute values are escaped for `"` and `'` too, not just `&`/`<`/`>` — the
/// bash's `xml.sax.saxutils.escape` left quotes alone even though every use here is inside a
/// double-quoted attribute, which is invalid XML for a case name containing one.
pub fn junit_xml(rows: &[TapRow]) -> String {
    let mut by_suite: BTreeMap<&str, Vec<&TapRow>> = BTreeMap::new();
    for r in rows {
        by_suite.entry(r.suite.as_str()).or_default().push(r);
    }
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
    for (suite, rs) in by_suite {
        let failures = rs.iter().filter(|r| r.status == "fail").count();
        let skipped = rs.iter().filter(|r| matches!(r.status.as_str(), "skip" | "unreached" | "bail")).count();
        let seconds = rs.first().map(|r| r.seconds.clone()).unwrap_or(serde_json::Value::from(0));
        out += &format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" skipped=\"{}\" time=\"{}\">\n",
            xml_escape(suite),
            rs.len(),
            failures,
            skipped,
            seconds
        );
        for r in &rs {
            let case = if r.case.is_empty() { "(suite)" } else { r.case.as_str() };
            out += &format!(
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{}\">\n",
                xml_escape(case),
                xml_escape(suite),
                r.seconds
            );
            if r.status == "fail" {
                out += &format!("      <failure message=\"{}\"></failure>\n", xml_escape(&r.detail));
            } else if matches!(r.status.as_str(), "skip" | "unreached" | "bail") {
                out += &format!("      <skipped message=\"{}\"></skipped>\n", xml_escape(&r.detail));
            }
            out += "    </testcase>\n";
        }
        out += "  </testsuite>\n";
    }
    out += "</testsuites>\n";
    out
}

/// Parse `results.jsonl` (one JSON object per line, produced by `tap_jsonl_rows`) into rows
/// for the JUnit builder. A line that does not parse is skipped, not fatal — the same
/// tolerance the bash's own `try/except: sys.exit(0)` gave the whole file.
pub fn parse_tap_rows(jsonl: &str) -> Vec<TapRow> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let suite = v.get("suite").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let case = v.get("case").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let seconds = v.get("seconds").cloned().unwrap_or(serde_json::Value::from(0));
        let detail = v.get("detail").and_then(|x| x.as_str()).unwrap_or("").to_string();
        out.push(TapRow { suite, case, status, seconds, detail });
    }
    out
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
