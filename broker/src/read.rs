use crate::policy::ReadVerb;

// Safe JSON fields for run-view: status and timing only, no log content.
// gh run view --log returns whatever the run printed; secrets leak there.
// This set covers what the loop needs to measure CI timing without touching logs.
const RUN_VIEW_FIELDS: &str =
    "status,conclusion,databaseId,name,event,createdAt,updatedAt,headBranch,headSha,workflowName";

pub fn run(raw: &[String]) -> Result<(), String> {
    if raw.is_empty() {
        return Err("broker read: usage: broker read <verb> <repo>/<id> [flags]".to_string());
    }
    let verb = match ReadVerb::parse(&raw[0]) {
        Some(v) => v,
        None => return Err(format!(
            "broker read: unknown verb '{}'; allowed: run-view run-jobs artifact-download",
            raw[0]
        )),
    };
    if raw.len() < 2 {
        return Err(format!("broker read {}: <repo>/<id> required", raw[0]));
    }
    let target = &raw[1];
    let (repo, number) = match target.rfind('/') {
        Some(i) => (target[..i].to_string(), target[i+1..].to_string()),
        None => return Err(format!(
            "broker read: target must be <repo>/<id>, got '{}'",
            target
        )),
    };
    if repo.is_empty() || number.is_empty() {
        return Err(format!("broker read: target must be <repo>/<id>, got '{}'", target));
    }

    let repo_path = match repo_map_lookup(&repo) {
        Ok(Some(p)) => p,
        Ok(None) => return Err(format!("broker read: repo '{}' not in repo-map", repo)),
        Err(e) => return Err(format!("broker read: repo-map lookup failed: {}", e)),
    };

    let mut artifact: Option<String> = None;
    let mut output_dir: Option<String> = None;
    let mut i = 2usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--artifact"   => { i += 1; if i < raw.len() { artifact   = Some(raw[i].clone()); } }
            "--output-dir" => { i += 1; if i < raw.len() { output_dir = Some(raw[i].clone()); } }
            f => return Err(format!("broker read: unknown flag '{}'", f)),
        }
        i += 1;
    }

    match verb {
        ReadVerb::RunView => {
            let out = gh_run_view(&repo_path, &number)?;
            println!("{}", out);
        }
        ReadVerb::RunJobs => {
            let out = gh_run_jobs(&repo_path, &number)?;
            println!("{}", out);
        }
        ReadVerb::ArtifactDownload => {
            let name = artifact.ok_or_else(|| {
                "broker read artifact-download: --artifact is required".to_string()
            })?;
            let dir = match output_dir {
                Some(d) => d,
                None => {
                    // SPIRA_RUN is a registered key — resolved the same way every other
                    // SPIRA_RUN read in this crate resolves it, not via the env-var-first
                    // helper spira-config also exposes.
                    let run = spira_config::process::cfg("SPIRA_RUN")
                        .map_err(|e| format!("broker read artifact-download: {e}"))?;
                    format!("{}/broker/artifacts/{}", run, number)
                }
            };
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("broker read: cannot create output dir '{}': {e}", dir))?;
            gh_artifact_download(&repo_path, &number, &name, &dir)?;
            println!("{}", dir);
        }
    }
    Ok(())
}

// run-view's argv, pulled out so the "safe fields only, never --log" property is a value
// a test can inspect rather than something only provable by spawning gh.
fn run_view_args(run_id: &str) -> Vec<String> {
    vec!["run".into(), "view".into(), run_id.into(), "--json".into(), RUN_VIEW_FIELDS.into()]
}

fn artifact_download_args(run_id: &str, name: &str, dir: &str) -> Vec<String> {
    vec!["run".into(), "download".into(), run_id.into(), "-n".into(), name.into(), "-D".into(), dir.into()]
}

fn gh_run_view(repo_path: &str, run_id: &str) -> Result<String, String> {
    let output = spira_config::bounded::bounded(gh_bin())
        .args(run_view_args(run_id))
        .current_dir(repo_path)
        .envs(crate::token::gh_env()?)
        .output()
        .map_err(|e| format!("gh failed: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "gh run view failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

const ERROR_LINES: usize = 5;
const ERROR_LINE_CHARS: usize = 300;

fn gh_api(repo_path: &str, endpoint: &str) -> Result<String, String> {
    let output = spira_config::bounded::bounded(gh_bin())
        .args(["api", endpoint])
        .current_dir(repo_path)
        .envs(crate::token::gh_env()?)
        .output()
        .map_err(|e| format!("gh failed: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("gh api {endpoint} failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// The first error lines of a job log, GitHub's per-line timestamp stripped.
pub(crate) fn error_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(|l| l.split_once(' ').filter(|(t, _)| t.ends_with('Z') && t.contains('T')).map_or(l, |(_, r)| r))
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().contains("error") && !l.starts_with("0 errors"))
        .take(ERROR_LINES)
        .map(|l| l.trim_start_matches("##[error]").chars().take(ERROR_LINE_CHARS).collect())
        .collect()
}

/// Run and job results for one run: per job its conclusion, link and failing step, and for a
/// failed job the first error lines of its log fetched by `log_of(job id)`.
pub(crate) fn jobs_summary(run: &serde_json::Value, jobs: &serde_json::Value, log_of: &dyn Fn(&str) -> String) -> serde_json::Value {
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let jobs: Vec<serde_json::Value> = jobs
        .get("jobs")
        .and_then(|j| j.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|j| {
            let failed = s(j, "conclusion") == "failure";
            let step = j.get("steps").and_then(|x| x.as_array()).and_then(|ss| ss.iter().find(|st| s(st, "conclusion") == "failure")).map(|st| s(st, "name"));
            let id = j.get("id").map(|i| i.to_string()).unwrap_or_default();
            serde_json::json!({
                "name": s(j, "name"),
                "conclusion": s(j, "conclusion"),
                "status": s(j, "status"),
                "url": s(j, "html_url"),
                "failed_step": step,
                "error_lines": if failed && !id.is_empty() { error_lines(&log_of(&id)) } else { Vec::new() },
            })
        })
        .collect();
    serde_json::json!({
        "run": { "id": run.get("id"), "url": s(run, "html_url"), "status": s(run, "status"), "conclusion": s(run, "conclusion"), "head_sha": s(run, "head_sha") },
        "jobs": jobs,
    })
}

fn gh_run_jobs(repo_path: &str, run_id: &str) -> Result<String, String> {
    if !run_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("broker read run-jobs: run id must be numeric, got '{run_id}'"));
    }
    let parse = |t: String| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("gh api returned non-JSON: {e}"));
    let run = parse(gh_api(repo_path, &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}"))?)?;
    let jobs = parse(gh_api(repo_path, &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs"))?)?;
    let log_of = |id: &str| gh_api(repo_path, &format!("repos/{{owner}}/{{repo}}/actions/jobs/{id}/logs")).unwrap_or_default();
    Ok(jobs_summary(&run, &jobs, &log_of).to_string())
}

fn gh_artifact_download(repo_path: &str, run_id: &str, name: &str, dir: &str) -> Result<(), String> {
    let output = spira_config::bounded::bounded(gh_bin())
        .args(artifact_download_args(run_id, name, dir))
        .current_dir(repo_path)
        .envs(crate::token::gh_env()?)
        .output()
        .map_err(|e| format!("gh failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh run download failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
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

fn repo_map_lookup(repo: &str) -> Result<Option<String>, String> {
    crate::repo_map::lookup(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_view_asks_for_safe_fields_only() {
        let args = run_view_args("12345");
        assert_eq!(args, vec!["run", "view", "12345", "--json", RUN_VIEW_FIELDS]);
    }

    #[test]
    fn run_view_never_requests_log_output() {
        // gh run view --log returns whatever the run printed; secrets leak there. The field
        // list is a const, so this is really a guard against someone appending to it.
        assert!(!run_view_args("12345").iter().any(|a| a == "--log"));
        assert!(!RUN_VIEW_FIELDS.contains("log"));
    }

    #[test]
    fn jobs_summary_names_the_failing_step_and_its_first_error_lines_only_for_failed_jobs() {
        let run = serde_json::json!({"id": 7, "html_url": "https://github.com/o/r/actions/runs/7", "status": "completed", "conclusion": "failure", "head_sha": "abc"});
        let jobs = serde_json::json!({"jobs": [
            {"id": 1, "name": "lint", "conclusion": "success", "html_url": "u1", "steps": [{"name": "a", "conclusion": "success"}]},
            {"id": 2, "name": "gate", "conclusion": "failure", "html_url": "u2", "steps": [{"name": "setup", "conclusion": "success"}, {"name": "svelte-check", "conclusion": "failure"}]},
        ]});
        let asked = std::cell::RefCell::new(Vec::new());
        let log_of = |id: &str| {
            asked.borrow_mut().push(id.to_string());
            "2026-10-15T01:02:03.1234567Z ok\n2026-10-15T01:02:04.1Z Error: Property 'x' does not exist\n2026-10-15T01:02:05Z ##[error]Process completed with exit code 1.\n".to_string()
        };
        let v = jobs_summary(&run, &jobs, &log_of);
        assert_eq!(*asked.borrow(), vec!["2"], "a green job's log is never fetched");
        assert_eq!(v["jobs"][0]["failed_step"], serde_json::Value::Null);
        assert_eq!(v["jobs"][1]["failed_step"], "svelte-check");
        assert_eq!(v["jobs"][1]["error_lines"][0], "Error: Property 'x' does not exist");
        assert_eq!(v["jobs"][1]["error_lines"][1], "Process completed with exit code 1.");
        assert_eq!(v["run"]["url"], "https://github.com/o/r/actions/runs/7");
    }

    #[test]
    fn artifact_download_names_the_artifact_and_output_dir() {
        let args = artifact_download_args("12345", "batch-results", "/tmp/out");
        assert_eq!(
            args,
            vec!["run", "download", "12345", "-n", "batch-results", "-D", "/tmp/out"]
        );
    }
}
