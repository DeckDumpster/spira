use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::{json, Value};
use crate::policy::{self, Verb};

const RATE_LIMIT_WINDOW_S: u64 = 60;
const RATE_LIMIT_MAX: u64 = 10;

pub fn run() -> Result<(), String> {
    // Top-level config reads for this process: SPIRA_RUN, SPIRA_BD, SPIRA_DB are all
    // registered keys (spira/conf.d) — resolved once here and threaded down as
    // parameters so the rest of this module stays pure/testable.
    let run_dir = spira_config::process::cfg("SPIRA_RUN")
        .map_err(|e| format!("broker execute: {e}"))?;
    let bd = spira_config::process::cfg("SPIRA_BD")
        .map_err(|e| format!("broker execute: {e}"))?;
    let db = spira_config::process::cfg("SPIRA_DB")
        .map_err(|e| format!("broker execute: {e}"))?;

    let broker_dir = Path::new(&run_dir).join("broker");
    let inbox   = broker_dir.join("inbox");
    let done    = broker_dir.join("done");
    let refused = broker_dir.join("refused");
    let audit   = broker_dir.join("audit.jsonl");

    for d in [&inbox, &done, &refused] {
        std::fs::create_dir_all(d)
            .map_err(|e| format!("broker execute: cannot create dir {}: {e}", d.display()))?;
    }

    // SPIRA_HOME is not a registered config key (spira/conf.d has no entry) — left on
    // the process environment.
    let spira_home = std::env::var("SPIRA_HOME").unwrap_or_default();

    let entries = std::fs::read_dir(&inbox)
        .map_err(|e| format!("broker execute: cannot read inbox: {e}"))?;

    let mut refusal_count = 0u64;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let intent: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let (outcome, detail) = process_intent(&intent, &spira_home, &run_dir, &audit);
        let moved_to = if outcome == "DONE" { &done } else { &refused };
        if outcome != "DONE" { refusal_count += 1; }

        let filename = path.file_name().unwrap();
        let _ = std::fs::rename(&path, moved_to.join(filename));

        let executed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = json!({
            "id":          intent["id"],
            "verb":        intent["verb"],
            "repo":        intent["repo"],
            "number":      intent["number"],
            "reason":      intent["reason"],
            "bead":        intent["bead"],
            "fayth":       intent["fayth"],
            "aeon":        intent["aeon"],
            "class":       intent["class"],
            "submitted_at":intent["submitted_at"],
            "executed_at": executed_at,
            "outcome":     outcome,
            "detail":      detail,
        });
        append_audit(&audit, &record);

        if let (Some(bead), false) = (intent["bead"].as_str(), intent["bead"].is_null()) {
            if !bead.is_empty() {
                post_bead_note(&bd, &db, bead, &record);
            }
        }
    }

    // Count total refusals from audit for cockpit.
    let total_refusals = count_audit_refusals(&audit);
    write_cockpit_fragment(&run_dir, total_refusals);

    Ok(())
}

fn process_intent(
    intent: &Value,
    spira_home: &str,
    run_dir: &str,
    audit: &Path,
) -> (String, String) {
    let verb_str = intent["verb"].as_str().unwrap_or("");
    let fayth    = intent["fayth"].as_str().unwrap_or("");
    let repo     = intent["repo"].as_str().unwrap_or("");
    let number   = intent["number"].as_str().unwrap_or("");
    let reason   = intent["reason"].as_str().unwrap_or("");
    let class    = intent["class"].as_str().unwrap_or("");

    // 1. Verb must be in the v1 table.
    let verb = match Verb::parse(verb_str) {
        Some(v) => v,
        None => return (
            "REFUSED".to_string(),
            format!("verb '{}' is not in the policy table", verb_str),
        ),
    };

    // 2. Fayth allowed for this verb.
    match policy::check_fayth(&verb, fayth) {
        policy::PolicyResult::Refused(r) => return ("REFUSED".to_string(), r),
        policy::PolicyResult::Allowed => {}
    }

    // 3. Repo must be in the repo-map; get local path.
    let repo_path = match repo_map_lookup(repo) {
        Ok(Some(p)) => p,
        Ok(None) => return (
            "REFUSED".to_string(),
            format!("repo '{}' is not in the repo-map", repo),
        ),
        Err(e) => return (
            "REFUSED".to_string(),
            format!("repo-map lookup failed: {}", e),
        ),
    };

    // 4. Rate limit per fayth.
    if over_rate_limit(audit, fayth) {
        return (
            "REFUSED".to_string(),
            format!("rate limit exceeded for fayth '{}'", fayth),
        );
    }

    // 5. For czar verbs: check czar-fence (shadow/act).
    if verb.needs_czar_fence() {
        if class.is_empty() {
            return (
                "REFUSED".to_string(),
                "czar verb requires --class for czar-fence check".to_string(),
            );
        }
        match czar_fence_check(spira_home, class) {
            Ok(true) => {}  // act
            Ok(false) => return (
                "CZAR-WOULD".to_string(),
                format!("class '{}' is shadow — czar-fence refused", class),
            ),
            Err(e) => return (
                "REFUSED".to_string(),
                format!("czar-fence error: {}", e),
            ),
        }
    }

    // 6. For pr-close: verify it's a batch PR.
    if verb.requires_batch_pr() {
        match check_batch_pr(&repo_path, number) {
            Ok(true) => {}
            Ok(false) => return (
                "REFUSED".to_string(),
                format!("PR {} is not a batch PR (branch must start with spira/queue/)", number),
            ),
            Err(e) => return (
                "REFUSED".to_string(),
                format!("cannot verify batch PR: {}", e),
            ),
        }
    }

    // 7. Execute.
    match gh_exec(&verb, &repo_path, number, reason) {
        Ok(out) => ("DONE".to_string(), out),
        Err(e)  => ("REFUSED".to_string(), format!("gh exec failed: {}", e)),
    }
}

fn repo_map_lookup(repo: &str) -> Result<Option<String>, String> {
    crate::repo_map::lookup(repo)
}

fn czar_fence_check(_spira_home: &str, class: &str) -> Result<bool, String> {
    // czar-fence.sh by name on the launcher's PATH (sp-gypjk).
    let status = Command::new("czar-fence.sh")
        .arg(class)
        .status()
        .map_err(|e| format!("cannot run czar-fence.sh: {e}"))?;
    Ok(status.success())
}

fn check_batch_pr(repo_path: &str, number: &str) -> Result<bool, String> {
    let gh = gh_bin();
    let output = Command::new(&gh)
        .args(["pr", "view", number, "--json", "headRefName", "-q", ".headRefName"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("gh pr view failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(branch.starts_with("spira/queue/"))
}

// The gh argv for each verb. Pulled out of gh_exec so it can be asserted without spawning a
// process — a wrong flag here (e.g. --body instead of --comment) fails silently against the
// gh stub used elsewhere, since both are accepted CLI flags on different subcommands.
fn build_gh_args(verb: &Verb, number: &str, reason: &str) -> Vec<String> {
    match verb {
        Verb::RunRerun     => vec!["run".into(), "rerun".into(), number.into()],
        Verb::RunCancel    => vec!["run".into(), "cancel".into(), number.into()],
        Verb::PrClose      => vec!["pr".into(), "close".into(), number.into(), "--comment".into(), reason.into()],
        Verb::PrComment    => vec!["pr".into(), "comment".into(), number.into(), "--body".into(), reason.into()],
        Verb::IssueComment => vec!["issue".into(), "comment".into(), number.into(), "--body".into(), reason.into()],
        Verb::IssueClose   => vec!["issue".into(), "close".into(), number.into(), "--comment".into(), reason.into()],
    }
}

fn gh_exec(verb: &Verb, repo_path: &str, number: &str, reason: &str) -> Result<String, String> {
    let gh = gh_bin();
    let mut cmd = Command::new(&gh);
    cmd.current_dir(repo_path);
    cmd.envs(crate::token::gh_env()?);
    cmd.args(build_gh_args(verb, number, reason));

    let output = cmd.output()
        .map_err(|e| format!("gh failed: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

// SPIRA_BROKER_GH is not a registered config key (spira/conf.d has no entry) — a
// test-only override of the gh binary, left on the process environment.
fn gh_bin() -> String {
    std::env::var("SPIRA_BROKER_GH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "gh".to_string())
}

fn over_rate_limit(audit: &Path, fayth: &str) -> bool {
    let content = match std::fs::read_to_string(audit) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let window_start = now.saturating_sub(RATE_LIMIT_WINDOW_S);
    let count = content.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            v["fayth"].as_str() == Some(fayth)
                && v["submitted_at"].as_u64().map(|t| t >= window_start).unwrap_or(false)
        })
        .count() as u64;
    count >= RATE_LIMIT_MAX
}

fn append_audit(audit: &Path, record: &Value) {
    let line = format!("{}\n", record);
    let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(audit) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = f.write_all(line.as_bytes());
}

fn post_bead_note(bd: &str, db: &str, bead: &str, record: &Value) {
    let outcome = record["outcome"].as_str().unwrap_or("?");
    let detail  = record["detail"].as_str().unwrap_or("");
    let verb    = record["verb"].as_str().unwrap_or("?");
    let msg = format!("broker {}: {} — {}", outcome, verb, detail);

    let mut cmd = Command::new(bd);
    if !db.is_empty() { cmd.args(["-C", db]); }
    cmd.args(["note", bead, &msg]);
    let _ = cmd.status();
}

fn count_audit_refusals(audit: &Path) -> u64 {
    let content = match std::fs::read_to_string(audit) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    content.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            matches!(v["outcome"].as_str(), Some("REFUSED") | Some("CZAR-WOULD"))
        })
        .count() as u64
}

fn write_cockpit_fragment(run_dir: &str, refusal_count: u64) {
    let cockpit_d = Path::new(run_dir).join("cockpit.d");
    if !cockpit_d.exists() { return; }
    let _ = std::fs::write(
        cockpit_d.join("broker.env"),
        format!("BROKER_REFUSALS={}\n", refusal_count),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn run_rerun_argv_has_no_reason() {
        assert_eq!(build_gh_args(&Verb::RunRerun, "42", "ignored"), vec!["run", "rerun", "42"]);
    }

    #[test]
    fn run_cancel_argv_has_no_reason() {
        assert_eq!(build_gh_args(&Verb::RunCancel, "42", "ignored"), vec!["run", "cancel", "42"]);
    }

    #[test]
    fn pr_close_argv_uses_comment_flag() {
        assert_eq!(
            build_gh_args(&Verb::PrClose, "7", "batch red"),
            vec!["pr", "close", "7", "--comment", "batch red"]
        );
    }

    #[test]
    fn pr_comment_argv_uses_body_flag_not_comment() {
        assert_eq!(
            build_gh_args(&Verb::PrComment, "7", "hello"),
            vec!["pr", "comment", "7", "--body", "hello"]
        );
    }

    #[test]
    fn issue_comment_argv_uses_body_flag() {
        assert_eq!(
            build_gh_args(&Verb::IssueComment, "9", "hello"),
            vec!["issue", "comment", "9", "--body", "hello"]
        );
    }

    #[test]
    fn issue_close_argv_uses_comment_flag() {
        assert_eq!(
            build_gh_args(&Verb::IssueClose, "9", "done"),
            vec!["issue", "close", "9", "--comment", "done"]
        );
    }

    fn write_audit_lines(path: &Path, lines: &[Value]) {
        let body: String = lines.iter().map(|l| format!("{}\n", l)).collect();
        std::fs::write(path, body).unwrap();
    }

    /// The scratch dir (hold it: dropping it removes the file) and the audit file in it.
    fn scratch_audit_path(name: &str) -> (testkit::TempDir, PathBuf) {
        let dir = testkit::TempDir::new(&format!("broker-execute-test-{name}"));
        let path = dir.join("audit.jsonl");
        (dir, path)
    }

    #[test]
    fn rate_limit_counts_only_the_same_fayth_in_the_window() {
        let (_d, path) = scratch_audit_path("rate-same-fayth");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut lines = Vec::new();
        for i in 0..RATE_LIMIT_MAX {
            lines.push(json!({"fayth": "czar", "submitted_at": now - i}));
        }
        // A different fayth's activity must not count toward czar's limit.
        lines.push(json!({"fayth": "builder", "submitted_at": now}));
        write_audit_lines(&path, &lines);
        assert!(over_rate_limit(&path, "czar"));
        assert!(!over_rate_limit(&path, "builder"));
    }

    #[test]
    fn rate_limit_ignores_entries_outside_the_window() {
        let (_d, path) = scratch_audit_path("rate-window");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut lines = Vec::new();
        for _ in 0..RATE_LIMIT_MAX {
            lines.push(json!({"fayth": "czar", "submitted_at": now.saturating_sub(RATE_LIMIT_WINDOW_S + 3600)}));
        }
        write_audit_lines(&path, &lines);
        assert!(!over_rate_limit(&path, "czar"));
    }

    #[test]
    fn rate_limit_missing_audit_file_is_not_over_limit() {
        let d = testkit::TempDir::new("broker-execute-test-no-audit");
        let path = d.join("no-such-audit.jsonl");
        assert!(!over_rate_limit(&path, "czar"));
    }

    #[test]
    fn refusal_count_includes_refused_and_czar_would_not_done() {
        let (_d, path) = scratch_audit_path("refusal-count");
        write_audit_lines(&path, &[
            json!({"outcome": "REFUSED"}),
            json!({"outcome": "CZAR-WOULD"}),
            json!({"outcome": "DONE"}),
        ]);
        assert_eq!(count_audit_refusals(&path), 2);
    }
}
