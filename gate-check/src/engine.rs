//! The pure decision surface (DESIGN.md "Design"): every JSON payload this binary reads is
//! parsed here with `serde_json`, and every dedup rule, extraction regex and bead
//! title/body is built here, with no `bd`/`gh`/filesystem call in this module at all.

use serde_json::Value;

/// `bd gate list --json`'s open, unbound (`await_id` empty) `gh:run` gates, reduced to the
/// distinct branches named in `metadata.branch`, in first-encounter order — the same
/// tolerance the bash's inline `python3` had: anything not shaped as expected (not a list, a
/// null entry, a missing field) is skipped, never a hard error (DESIGN.md "Non-goals").
pub fn discover_branches(gate_list_json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(gate_list_json) else { return Vec::new() };
    let gates: Vec<Value> = match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    let mut seen = Vec::new();
    for g in gates {
        if g.is_null() {
            continue;
        }
        if g.get("await_type").and_then(Value::as_str) != Some("gh:run") {
            continue;
        }
        let has_await_id = g.get("await_id").and_then(Value::as_str).map(|s| !s.is_empty()).unwrap_or(false);
        if has_await_id {
            continue;
        }
        let branch = g.get("metadata").and_then(|m| m.get("branch")).and_then(Value::as_str).unwrap_or("");
        if !branch.is_empty() && !seen.iter().any(|s| s == branch) {
            seen.push(branch.to_string());
        }
    }
    seen
}

/// `no run ID specified` lines in `bd gate check`'s output — gates `bd` cannot resolve and
/// never will, counted separately so "0 resolved" is never ambiguous between nothing to do
/// and every gate being permanently wedged.
pub fn stuck_count(check_output: &str) -> usize {
    check_output.lines().filter(|l| l.contains("no run ID specified")).count()
}

/// `⚠ <gate-id>: ESCALATE ...` lines from `bd gate check`'s output.
pub fn escalate_lines(check_output: &str) -> Vec<&str> {
    check_output.lines().filter(|l| l.contains("ESCALATE")).collect()
}

/// The id in an ESCALATE line — `[a-z][a-z0-9]*-[a-z0-9][a-z0-9]*`, first match, no assumed
/// prefix (`law-never-derive-an-id-from-output`).
pub fn gate_id_in(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let is_lower_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i].is_ascii_lowercase() {
            let start = i;
            let mut j = i + 1;
            while j < n && is_lower_alnum(bytes[j]) {
                j += 1;
            }
            if j < n && bytes[j] == b'-' {
                let dash = j;
                let mut k = dash + 1;
                if k < n && is_lower_alnum(bytes[k]) {
                    k += 1;
                    while k < n && is_lower_alnum(bytes[k]) {
                        k += 1;
                    }
                    return Some(line[start..k].to_string());
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    None
}

/// The bead a gate's own `bd show --json` description names as blocked:
/// `"blocking <id>"` inside the description of the first (only) record.
pub fn blocked_bead(show_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(show_json).ok()?;
    let first = match &v {
        Value::Array(a) => a.first()?,
        other => other,
    };
    let desc = first.get("description").and_then(Value::as_str).unwrap_or("");
    find_blocking_id(desc)
}

fn find_blocking_id(desc: &str) -> Option<String> {
    let needle = "blocking ";
    let idx = desc.find(needle)?;
    let rest = &desc[idx + needle.len()..];
    // `[a-z]+-\w+`: lowercase letters, a hyphen, then word characters (letters, digits, `_`).
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_lowercase() {
        i += 1;
    }
    if i == 0 || i >= bytes.len() || bytes[i] != b'-' {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if j == i + 1 {
        return None;
    }
    Some(rest[..j].to_string())
}

/// True when `bd list --json` already carries an `open`/`in_progress` bead with this exact
/// title (the flaky-suite dedup: "a suite stays quiet once a bead is open").
pub fn has_open_bead(list_json: &str, title: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(list_json) else { return false };
    let beads: &[Value] = match &v {
        Value::Array(a) => a,
        _ => return false,
    };
    beads.iter().any(|b| {
        matches!(b.get("status").and_then(Value::as_str), Some("open") | Some("in_progress")) && b.get("title").and_then(Value::as_str) == Some(title)
    })
}

/// The first open/in_progress bead carrying this exact title, and its priority (default 2
/// when absent, matching the bash's `.get('priority', 2)`).
pub fn find_open_bead(list_json: &str, title: &str) -> Option<(String, i64)> {
    let v: Value = serde_json::from_str(list_json).ok()?;
    let beads = v.as_array()?;
    for b in beads {
        let status_ok = matches!(b.get("status").and_then(Value::as_str), Some("open") | Some("in_progress"));
        if status_ok && b.get("title").and_then(Value::as_str) == Some(title) {
            let id = b.get("id").and_then(Value::as_str)?.to_string();
            let pri = b.get("priority").and_then(Value::as_i64).unwrap_or(2);
            return Some((id, pri));
        }
    }
    None
}

/// `gh api .../jobs`'s `.jobs[].id`.
pub fn job_ids(jobs_json: &str) -> Vec<i64> {
    let Ok(v) = serde_json::from_str::<Value>(jobs_json) else { return Vec::new() };
    v.get("jobs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|j| j.get("id").and_then(Value::as_i64)).collect())
        .unwrap_or_default()
}

/// A `flaky suite` warning annotation's suite name: the message text up to (not including)
/// the first `" was red"` — the bash's `msg.find(" was red")`, only when found at a
/// non-zero index (an annotation whose message starts with the literal marker has no suite
/// name to extract and is skipped, exactly as the bash's `if idx > 0` skipped it).
pub fn flaky_suites(annotations_json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(annotations_json) else { return Vec::new() };
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for a in arr {
        if a.get("annotation_level").and_then(Value::as_str) != Some("warning") {
            continue;
        }
        if a.get("title").and_then(Value::as_str) != Some("flaky suite") {
            continue;
        }
        let msg = a.get("message").and_then(Value::as_str).unwrap_or("");
        if let Some(idx) = msg.find(" was red") {
            if idx > 0 {
                out.push(msg[..idx].to_string());
            }
        }
    }
    out
}

/// A `red-twice suite` failure annotation's full message (the suite name, untruncated).
pub fn red_twice_suites(annotations_json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(annotations_json) else { return Vec::new() };
    let Some(arr) = v.as_array() else { return Vec::new() };
    arr.iter()
        .filter(|a| a.get("annotation_level").and_then(Value::as_str) == Some("failure") && a.get("title").and_then(Value::as_str) == Some("red-twice suite"))
        .map(|a| a.get("message").and_then(Value::as_str).unwrap_or("").to_string())
        .collect()
}

pub fn flaky_title(suite: &str) -> String {
    format!("flaky suite: {suite}")
}

pub fn red_twice_title(suite: &str) -> String {
    format!("suite red on main: {suite}")
}

pub fn flaky_body(run_id: &str, suite: &str) -> String {
    format!("Flaky suite in run {run_id}.\n\n{suite} was red on a parallel run then green on a serial re-run.\n")
}

pub fn red_twice_body(suite: &str, run_id: &str, sha: &str, last_green: &str, fail_lines: &str, commits: &str) -> String {
    let sha = if sha.is_empty() { "?" } else { sha };
    let last_green = if last_green.is_empty() { "(unknown)" } else { last_green };
    let fail_lines = if fail_lines.is_empty() { "(none captured)" } else { fail_lines };
    let commits = if commits.is_empty() { "(range unknown)" } else { commits };
    format!(
        "{suite} is red on main and blocks the release.\n\nRun: {run_id}\nFirst red commit: {sha}\nLast green commit: {last_green}\n\nFailing assertions:\n{fail_lines}\n\nCommits in range:\n{commits}\n"
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
