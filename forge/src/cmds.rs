//! One function per verb (DESIGN.md §3.1), each returning the lines to print and the exit
//! code — ported from forge.sh's case branches. Fail-closed answers (`pending`, `UNKNOWN`,
//! `unknown`, `unprotected`, `?`, empty) are the same word forge.sh printed for "cannot tell".

use crate::ports::{Gh, GhOut, Proc};
use crate::time::epoch;
use crate::zipread;
use serde_json::Value;
use std::path::Path;

pub struct Out {
    pub code: i32,
    pub lines: Vec<String>,
}

fn ok(lines: Vec<String>) -> Out {
    Out { code: 0, lines }
}
fn fail(lines: Vec<String>) -> Out {
    Out { code: 1, lines }
}

fn jstr(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}
fn first_elem(v: &Value) -> Option<&Value> {
    v.as_array().and_then(|a| a.first())
}
fn parse(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

// ── pr-create ────────────────────────────────────────────────────────────────────────────

pub fn pr_create(gh: &dyn Gh, repo: &Path, head: &str, base: &str, title: &str, body: &[u8]) -> Out {
    let r = gh.call_merged(Some(repo), &["pr", "create", "--head", head, "--base", base, "--title", title, "--body-file", "-"], Some(body));
    if !r.ok() {
        return ok(vec![String::new()]);
    }
    let n = gh.call(Some(repo), &["pr", "view", head, "--json", "number", "-q", ".number"]);
    ok(vec![n.text().trim().to_string()])
}

// ── pr-number ────────────────────────────────────────────────────────────────────────────

pub fn pr_number(gh: &dyn Gh, repo: &Path, head: &str) -> Out {
    let n = gh.call(Some(repo), &["pr", "view", head, "--json", "number", "-q", ".number"]);
    ok(vec![n.text().trim().to_string()])
}

// ── pr-list-queue ────────────────────────────────────────────────────────────────────────

pub fn pr_list_queue(gh: &dyn Gh, repo: &Path) -> Out {
    let r = gh.call(Some(repo), &["pr", "list", "--state", "open", "--json", "number,headRefName"]);
    let mut lines = Vec::new();
    if let Some(Value::Array(items)) = parse(&r.stdout) {
        for pr in items {
            let h = jstr(&pr, "headRefName").unwrap_or_default();
            if h.starts_with("spira/queue/") {
                if let Some(n) = pr.get("number") {
                    lines.push(n.to_string());
                }
            }
        }
    }
    ok(lines)
}

// ── pr-list-open (new) ──────────────────────────────────────────────────────────────────

/// `<number> <headRefName>` per open PR — the superset `land_pr`'s dedup scan needs
/// (DESIGN.md §3.1, §6).
pub fn pr_list_open(gh: &dyn Gh, repo: &Path) -> Out {
    let r = gh.call(Some(repo), &["pr", "list", "--state", "open", "--json", "number,headRefName"]);
    let mut lines = Vec::new();
    if let Some(Value::Array(items)) = parse(&r.stdout) {
        for pr in items {
            let h = jstr(&pr, "headRefName").unwrap_or_default();
            if let Some(n) = pr.get("number").and_then(Value::as_i64) {
                lines.push(format!("{n} {h}"));
            }
        }
    }
    ok(lines)
}

// ── pr-mergeability ──────────────────────────────────────────────────────────────────────

pub fn pr_mergeability(gh: &dyn Gh, repo: &Path, pr_n: &str) -> Out {
    let r = gh.call(Some(repo), &["pr", "view", pr_n, "--json", "mergeable,mergeStateStatus"]);
    let word = (|| {
        let v = parse(&r.stdout)?;
        let m = jstr(&v, "mergeable").unwrap_or_default().to_uppercase();
        let ms = jstr(&v, "mergeStateStatus").unwrap_or_default().to_uppercase();
        if m == "CONFLICTING" || ms == "DIRTY" {
            Some("DIRTY")
        } else if m == "MERGEABLE" {
            Some("CLEAN")
        } else {
            Some("UNKNOWN")
        }
    })()
    .unwrap_or("UNKNOWN");
    ok(vec![word.to_string()])
}

// ── pr-state ─────────────────────────────────────────────────────────────────────────────

pub fn pr_state(gh: &dyn Gh, repo: &Path, selector: &str) -> Out {
    let r = gh.call(Some(repo), &["pr", "view", selector, "--json", "state", "-q", ".state"]);
    let st = r.text().trim().to_string();
    let word = match st.as_str() {
        "OPEN" => "open",
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ => "unknown",
    };
    ok(vec![word.to_string()])
}

// ── pr-automerge (new) ───────────────────────────────────────────────────────────────────

/// Arms squash auto-merge — `land_pr`'s `ghq pr merge --auto --squash` (DESIGN.md §6).
/// Silent; exit 0 armed, 1 could not be armed (an open PR with a red required check stays
/// open, not a failure worth surfacing beyond the exit code — the caller logs it).
pub fn pr_automerge(gh: &dyn Gh, repo: &Path, selector: &str) -> Out {
    let r = gh.call(Some(repo), &["pr", "merge", "--auto", "--squash", selector]);
    if r.ok() {
        ok(vec![])
    } else {
        fail(vec![])
    }
}

// ── check-status ─────────────────────────────────────────────────────────────────────────

pub fn check_status(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, branch: &str) -> Out {
    if branch.is_empty() {
        // Without a branch there is no way to tell two PRs sharing a head commit apart
        // (law-a-control-that-cannot-check-must-refuse): report pending, never a guess.
        return ok(vec!["pending".into()]);
    }
    let run_list = gh.call(
        Some(repo),
        &["run", "list", "--branch", branch, "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
    );
    let rl = parse(&run_list.stdout).unwrap_or(Value::Array(vec![]));
    let first = first_elem(&rl);
    let run_id = first.and_then(|r| r.get("databaseId")).map(|v| v.to_string());
    let run_status = first.and_then(|r| jstr(r, "status")).unwrap_or_default();
    let run_conclusion = first.and_then(|r| jstr(r, "conclusion")).unwrap_or_default();
    let head_sha = first.and_then(|r| jstr(r, "headSha")).unwrap_or_default();
    let run_url = first.and_then(|r| jstr(r, "url")).unwrap_or_default();

    let mut status = if run_id.is_none() || run_status != "completed" {
        "pending".to_string()
    } else {
        match run_conclusion.as_str() {
            "success" => "green".to_string(),
            "skipped" | "neutral" | "stale" | "cancelled" => "harness_fault".to_string(),
            _ => "red".to_string(),
        }
    };

    let mut jobs_json = String::new();
    let mut build_err_lines: Vec<String> = Vec::new();
    let mut run_attributable = false;
    if status == "green" || status == "red" {
        run_attributable = true;
        if let Some(rid) = &run_id {
            let jr = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{rid}/jobs")]);
            jobs_json = jr.text().into_owned();
        }
        let jv = parse(jobs_json.as_bytes());
        if status == "red" {
            let prov_failed = jv.as_ref().map(|v| job_bad(v, "provision")).unwrap_or(false);
            if prov_failed {
                status = "provision_fault".to_string();
            }
        }
        if status == "red" {
            if let Some(build_job_id) = jv.as_ref().and_then(|v| bad_job_id(v, "build")) {
                let logs = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/jobs/{build_job_id}/logs")]);
                build_err_lines = extract_build_errors(&logs.text());
            }
        }
        if status == "red" {
            if let Some(v) = &jv {
                if suites_job_faulted(v) {
                    status = "harness_fault".to_string();
                }
            }
        }
    }

    let mut artifact_out = String::new();
    if status == "red" {
        if let Some(rid) = &run_id {
            artifact_out = zipread::red_suites_artifact(gh, proc, repo, rid);
            if let Some(rest) = artifact_out.strip_prefix("artifact-truncated:") {
                let _ = rest;
                status = "harness_fault".to_string();
                artifact_out.clear();
            }
        }
    }

    let mut lines = vec![status.clone()];
    if !head_sha.is_empty() {
        lines.push(format!("head-sha: {head_sha}"));
    }
    if run_attributable && !run_url.is_empty() {
        lines.push(format!("run-url: {run_url}"));
    }
    for l in &build_err_lines {
        if !l.is_empty() {
            lines.push(format!("build-error: {l}"));
        }
    }
    if status != "green" && status != "red" {
        return ok(lines);
    }
    if run_id.is_none() {
        return ok(lines);
    }
    if !artifact_out.is_empty() {
        lines.extend(artifact_out.lines().map(str::to_string));
    } else {
        lines.extend(zipread::red_suites_annotations(gh, proc, repo, &jobs_json));
    }
    ok(lines)
}

/// A job named `name` exists and its conclusion is neither empty nor `success`.
fn job_bad(v: &Value, name: &str) -> bool {
    let Some(jobs) = v.get("jobs").and_then(Value::as_array) else { return false };
    for j in jobs {
        if jstr(j, "name").as_deref() == Some(name) {
            let concl = jstr(j, "conclusion").unwrap_or_default().to_lowercase();
            if !concl.is_empty() && concl != "success" {
                return true;
            }
        }
    }
    false
}

fn bad_job_id(v: &Value, name: &str) -> Option<String> {
    let jobs = v.get("jobs")?.as_array()?;
    for j in jobs {
        if jstr(j, "name").as_deref() == Some(name) {
            let concl = jstr(j, "conclusion").unwrap_or_default().to_lowercase();
            if !concl.is_empty() && concl != "success" {
                return j.get("id").map(|id| id.to_string());
            }
        }
    }
    None
}

fn suites_job_faulted(v: &Value) -> bool {
    const FAULT: [&str; 2] = ["cancelled", "timed_out"];
    let Some(jobs) = v.get("jobs").and_then(Value::as_array) else { return false };
    for j in jobs {
        if jstr(j, "name").as_deref() != Some("suites") {
            continue;
        }
        let concl = jstr(j, "conclusion").unwrap_or_default().to_lowercase();
        if FAULT.contains(&concl.as_str()) {
            return true;
        }
        if let Some(steps) = j.get("steps").and_then(Value::as_array) {
            for s in steps {
                if jstr(s, "name").as_deref() == Some("Suites") {
                    let sc = jstr(s, "conclusion").unwrap_or_default().to_lowercase();
                    if FAULT.contains(&sc.as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// `grep -E '^error(\[E[0-9]+\])?:|^error: could not compile|failed to load manifest for
/// (workspace member|package)' | awk '!seen[$0]++' | head -5`, after stripping GitHub's
/// per-line UTC timestamp prefix.
fn extract_build_errors(log: &str) -> Vec<String> {
    // sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z //' — GitHub's per-line UTC timestamp
    // (variable-length fractional seconds), stripped when present; the line is unchanged
    // otherwise.
    fn ts_prefixed(l: &str) -> &str {
        let b = l.as_bytes();
        let digit = |i: usize| b.get(i).map(u8::is_ascii_digit).unwrap_or(false);
        if !(digit(0) && digit(1) && digit(2) && digit(3) && b.get(4) == Some(&b'-')) {
            return l;
        }
        if !(digit(5) && digit(6) && b.get(7) == Some(&b'-')) {
            return l;
        }
        if !(digit(8) && digit(9) && b.get(10) == Some(&b'T')) {
            return l;
        }
        let mut i = 11;
        while matches!(b.get(i), Some(c) if c.is_ascii_digit() || *c == b':' || *c == b'.') {
            i += 1;
        }
        if i == 11 || b.get(i) != Some(&b'Z') || b.get(i + 1) != Some(&b' ') {
            return l;
        }
        &l[i + 2..]
    }
    let is_error_line = |l: &str| -> bool {
        if let Some(rest) = l.strip_prefix("error") {
            if rest.starts_with(':') {
                return true;
            }
            if let Some(rest2) = rest.strip_prefix('[') {
                if let Some(close) = rest2.find(']') {
                    let code = &rest2[..close];
                    if code.starts_with('E') && code[1..].chars().all(|c| c.is_ascii_digit()) && rest2[close + 1..].starts_with(':') {
                        return true;
                    }
                }
            }
            return false;
        }
        l.starts_with("error: could not compile")
            || l.starts_with("failed to load manifest for workspace member")
            || l.starts_with("failed to load manifest for package")
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in log.lines() {
        let l = ts_prefixed(raw);
        if is_error_line(l) && seen.insert(l.to_string()) {
            out.push(l.to_string());
            if out.len() == 5 {
                break;
            }
        }
    }
    out
}

// ── run-id ───────────────────────────────────────────────────────────────────────────────

pub fn run_id(gh: &dyn Gh, repo: &Path, branch: &str) -> Out {
    let r = gh.call(Some(repo), &["run", "list", "--branch", branch, "--json", "databaseId", "--limit", "1", "-q", ".[0].databaseId"]);
    ok(vec![r.text().trim().to_string()])
}

// ── runs-for-branch ──────────────────────────────────────────────────────────────────────

pub fn runs_for_branch(gh: &dyn Gh, repo: &Path, branch: &str) -> Out {
    if branch.is_empty() {
        return ok(vec![]);
    }
    let r = gh.call(Some(repo), &["run", "list", "--branch", branch, "--workflow", "Gate", "--json", "databaseId,status", "--limit", "20"]);
    let mut lines = Vec::new();
    if let Some(Value::Array(items)) = parse(&r.stdout) {
        for run in items {
            let st = jstr(&run, "status").unwrap_or_default();
            if st != "completed" {
                if let Some(id) = run.get("databaseId") {
                    lines.push(format!("{id} {st}"));
                }
            }
        }
    }
    ok(lines)
}

// ── runs-queue-branches ──────────────────────────────────────────────────────────────────

pub fn runs_queue_branches(gh: &dyn Gh, repo: &Path) -> Out {
    let r = gh.call(Some(repo), &["run", "list", "--workflow", "Gate", "--json", "databaseId,headBranch,status", "--limit", "100"]);
    let mut lines = Vec::new();
    if let Some(Value::Array(items)) = parse(&r.stdout) {
        for run in items {
            let hb = jstr(&run, "headBranch").unwrap_or_default();
            let st = jstr(&run, "status").unwrap_or_default();
            if hb.starts_with("spira/queue/") && st != "completed" {
                if let Some(id) = run.get("databaseId") {
                    lines.push(format!("{id} {hb} {st}"));
                }
            }
        }
    }
    ok(lines)
}

// ── batch-ci-status ──────────────────────────────────────────────────────────────────────

pub fn batch_ci_status(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, branch: &str) -> Out {
    let run_list = gh.call(
        Some(repo),
        &["run", "list", "--branch", branch, "--workflow", "Gate", "--json", "databaseId,conclusion,status,updatedAt,headSha,url", "--limit", "1"],
    );
    let rl = parse(&run_list.stdout).unwrap_or(Value::Array(vec![]));
    let Some(first) = first_elem(&rl) else { return ok(vec![]) };
    let Some(run_id) = first.get("databaseId").map(|v| v.to_string()) else { return ok(vec![]) };
    let mut lines = vec![format!("run-id: {run_id}")];
    let conclusion = jstr(first, "conclusion").unwrap_or_default();
    if !conclusion.is_empty() {
        lines.push(format!("run-conclusion: {conclusion}"));
        let e = epoch(&jstr(first, "updatedAt").unwrap_or_default());
        if e > 0 {
            lines.push(format!("run-completed-at: {e}"));
        }
    }
    if let Some(sha) = jstr(first, "headSha") {
        if !sha.is_empty() {
            lines.push(format!("head-sha: {sha}"));
        }
    }
    if let Some(url) = jstr(first, "url") {
        if !url.is_empty() {
            lines.push(format!("run-url: {url}"));
        }
    }
    let jobs_raw = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs")]);
    if !jobs_raw.ok() {
        return ok(lines);
    }
    if let Some(qs) = queued_since_of(&jobs_raw.stdout) {
        lines.push(format!("queued-since: {qs}"));
    }
    if conclusion == "failure" {
        let mut artifact_out = zipread::red_suites_artifact(gh, proc, repo, &run_id);
        if artifact_out.starts_with("artifact-truncated:") {
            artifact_out.clear();
        }
        if !artifact_out.is_empty() {
            lines.extend(artifact_out.lines().map(str::to_string));
        } else {
            lines.extend(zipread::red_suites_annotations(gh, proc, repo, &jobs_raw.text()));
        }
    }
    ok(lines)
}

fn queued_since_of(jobs_json: &[u8]) -> Option<u64> {
    let v = parse(jobs_json)?;
    let jobs = v.get("jobs")?.as_array()?;
    let mut earliest = 0u64;
    for j in jobs {
        if jstr(j, "status").as_deref() == Some("queued") {
            let e = epoch(&jstr(j, "created_at").unwrap_or_default());
            if e > 0 && (earliest == 0 || e < earliest) {
                earliest = e;
            }
        }
    }
    if earliest > 0 {
        Some(earliest)
    } else {
        None
    }
}

// ── queued-since ─────────────────────────────────────────────────────────────────────────

pub fn queued_since(gh: &dyn Gh, repo: &Path, branch: &str) -> Out {
    let r = gh.call(Some(repo), &["run", "list", "--branch", branch, "--json", "databaseId", "--limit", "1", "-q", ".[0].databaseId"]);
    let run_id = r.text().trim().to_string();
    if run_id.is_empty() {
        return ok(vec![]);
    }
    let jr = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs")]);
    if !jr.ok() {
        return ok(vec![]);
    }
    match queued_since_of(&jr.stdout) {
        Some(e) => ok(vec![e.to_string()]),
        None => ok(vec![]),
    }
}

// ── runs-active ──────────────────────────────────────────────────────────────────────────

pub fn runs_active(gh: &dyn Gh, repo: &Path) -> Out {
    let runs = gh.call(Some(repo), &["api", "repos/{owner}/{repo}/actions/runs?per_page=100"]);
    if !runs.ok() || runs.stdout.is_empty() {
        return ok(vec!["?".into()]);
    }
    let Some(d) = parse(&runs.stdout) else { return ok(vec!["?".into()]) };
    let Some(all) = d.get("workflow_runs").and_then(Value::as_array) else { return ok(vec!["?".into()]) };

    let open_prs = gh.call(Some(repo), &["pr", "list", "--state", "open", "--json", "number"]);
    // None (unparseable / failed) means "cannot tell which PRs are open" — every run then
    // counts (law-absence-needs-a-positive-control); Some(empty set) is a real empty answer.
    let open_numbers: Option<std::collections::HashSet<i64>> = if open_prs.ok() {
        parse(&open_prs.stdout).and_then(|v| v.as_array().map(|a| a.iter().filter_map(|p| p.get("number").and_then(Value::as_i64)).collect()))
    } else {
        None
    };

    let mut n = 0u64;
    for r in all {
        if jstr(r, "event").as_deref() != Some("pull_request") {
            continue;
        }
        let status = jstr(r, "status").unwrap_or_default();
        if !["queued", "in_progress", "waiting", "requested", "pending"].contains(&status.as_str()) {
            continue;
        }
        let belongs = match &open_numbers {
            None => true,
            Some(open) => {
                let prs = r.get("pull_requests").and_then(Value::as_array).cloned().unwrap_or_default();
                prs.is_empty() || prs.iter().any(|p| p.get("number").and_then(Value::as_i64).map(|n| open.contains(&n)).unwrap_or(false))
            }
        };
        if belongs {
            n += 1;
        }
    }
    ok(vec![n.to_string()])
}

// ── run-metadata ─────────────────────────────────────────────────────────────────────────

pub fn run_metadata(gh: &dyn Gh, repo: &Path, run_id: &str) -> Out {
    if run_id.is_empty() {
        return fail(vec![]);
    }
    let mut lines = Vec::new();
    let run_json = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}")]);
    if let Some(v) = parse(&run_json.stdout) {
        let started = jstr(&v, "run_started_at").filter(|s| !s.is_empty()).or_else(|| jstr(&v, "created_at")).unwrap_or_default();
        let e = epoch(&started);
        if e > 0 {
            lines.push(format!("started-at: {e}"));
        }
        // run.updated_at advances while CI runs; job/step timestamps stall for the duration
        // of a single-step job until it completes — the jobs-derived line below can refine
        // (and print again after) this one; a reader takes the last `last-activity:` line.
        let u = epoch(&jstr(&v, "updated_at").unwrap_or_default());
        if u > 0 {
            lines.push(format!("last-activity: {u}"));
        }
    }
    let jobs_json = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs")]);
    if let Some(v) = parse(&jobs_json.stdout) {
        if let Some(jobs) = v.get("jobs").and_then(Value::as_array) {
            let mut latest = 0u64;
            for j in jobs {
                for f in ["started_at", "completed_at"] {
                    let e = epoch(&jstr(j, f).unwrap_or_default());
                    latest = latest.max(e);
                }
                if let Some(steps) = j.get("steps").and_then(Value::as_array) {
                    for s in steps {
                        for f in ["started_at", "completed_at"] {
                            let e = epoch(&jstr(s, f).unwrap_or_default());
                            latest = latest.max(e);
                        }
                    }
                }
            }
            if latest > 0 {
                lines.push(format!("last-activity: {latest}"));
            }
        }
    }
    ok(lines)
}

// ── run-cancel / workflow-rerun / pr-close / pr-comment / dispatch ─────────────────────────

pub fn run_cancel(gh: &dyn Gh, repo: &Path, run_id: &str) -> Out {
    let r = gh.call(Some(repo), &["run", "cancel", run_id]);
    ok(passthrough(&r))
}

pub fn workflow_rerun(gh: &dyn Gh, repo: &Path, run_id: &str) -> Out {
    let st = gh.call(Some(repo), &["run", "view", run_id, "--json", "status", "-q", ".status"]);
    let status = st.text().trim().to_string();
    if status == "in_progress" || status == "queued" {
        let _ = gh.call(Some(repo), &["run", "cancel", run_id]);
    }
    let r = gh.call(Some(repo), &["run", "rerun", run_id]);
    Out { code: r.code, lines: passthrough(&r) }
}

pub fn pr_close(gh: &dyn Gh, repo: &Path, pr_n: &str) -> Out {
    let r = gh.call(Some(repo), &["pr", "close", pr_n]);
    ok(passthrough(&r))
}

pub fn pr_comment(gh: &dyn Gh, repo: &Path, pr_n: &str, body: &str) -> Out {
    let r = gh.call(Some(repo), &["pr", "comment", pr_n, "--body", body]);
    ok(passthrough(&r))
}

pub fn dispatch(gh: &dyn Gh, repo: &Path, ref_: &str, suites: &str) -> Out {
    if ref_.is_empty() {
        eprintln!("forge: dispatch: ref required");
        return fail(vec![]);
    }
    let r = gh.call(Some(repo), &["workflow", "run", "Gate", "--ref", ref_, "-f", &format!("suites={suites}")]);
    Out { code: r.code, lines: passthrough(&r) }
}

fn passthrough(r: &GhOut) -> Vec<String> {
    let t = r.text();
    if t.is_empty() {
        vec![]
    } else {
        t.lines().map(str::to_string).collect()
    }
}

// ── fail-lines ───────────────────────────────────────────────────────────────────────────

pub fn fail_lines(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, run_id: &str, suites: &str) -> Out {
    if run_id.is_empty() {
        return ok(vec![]);
    }
    let list: Vec<&str> = suites.split_whitespace().collect();
    ok(zipread::fail_lines_artifact(gh, proc, repo, run_id, &list))
}

// ── branch-protect / branch-protection-status ───────────────────────────────────────────

pub fn branch_protect(gh: &dyn Gh, repo: &Path, base: &str, app_id: &str) -> Out {
    if base.is_empty() {
        eprintln!("forge: branch-protect: base branch required");
        return fail(vec![]);
    }
    let body = format!(
        r#"{{"required_status_checks":{{"strict":false,"checks":[{{"context":"gate","app_id":{app_id}}}]}},"enforce_admins":true,"required_pull_request_reviews":null,"restrictions":null,"allow_force_pushes":false,"allow_deletions":false,"block_creations":false}}"#
    );
    let r = gh.call_merged(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/branches/{base}/protection"), "--method", "PUT", "--input", "-"], Some(body.as_bytes()));
    ok(passthrough(&r))
}

pub fn branch_protection_status(gh: &dyn Gh, repo: &Path, base: &str) -> Out {
    if base.is_empty() {
        return ok(vec!["unprotected".into()]);
    }
    let r = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/branches/{base}")]);
    let word = parse(&r.stdout).and_then(|v| v.get("protected").and_then(Value::as_bool)).map(|p| if p { "protected" } else { "unprotected" }).unwrap_or("unprotected");
    ok(vec![word.to_string()])
}
