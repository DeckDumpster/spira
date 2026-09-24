use std::path::PathBuf;
use std::process::Command;
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
            "broker read: unknown verb '{}'; allowed: run-view artifact-download",
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
        Some(p) => p,
        None => return Err(format!("broker read: repo '{}' not in repo-map", repo)),
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
        ReadVerb::ArtifactDownload => {
            let name = artifact.ok_or_else(|| {
                "broker read artifact-download: --artifact is required".to_string()
            })?;
            let dir = match output_dir {
                Some(d) => d,
                None => {
                    let run = std::env::var("SPIRA_RUN").unwrap_or_else(|_| "/tmp".to_string());
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

fn gh_run_view(repo_path: &str, run_id: &str) -> Result<String, String> {
    let output = Command::new(gh_bin())
        .args(["run", "view", run_id, "--json", RUN_VIEW_FIELDS])
        .current_dir(repo_path)
        .envs(crate::token::gh_env())
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

fn gh_artifact_download(repo_path: &str, run_id: &str, name: &str, dir: &str) -> Result<(), String> {
    let output = Command::new(gh_bin())
        .args(["run", "download", run_id, "-n", name, "-D", dir])
        .current_dir(repo_path)
        .envs(crate::token::gh_env())
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

fn gh_bin() -> String {
    std::env::var("SPIRA_BROKER_GH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "gh".to_string())
}

fn repo_map_lookup(repo: &str) -> Option<String> {
    let map_path: PathBuf = {
        let v = std::env::var("SPIRA_REPO_MAP").unwrap_or_default();
        if !v.is_empty() {
            PathBuf::from(v)
        } else {
            let home = std::env::var("SPIRA_HOME").unwrap_or_default();
            PathBuf::from(home).join("repo-map")
        }
    };
    let content = std::fs::read_to_string(&map_path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() { continue; }
        let fields: Vec<&str> = line.splitn(6, '|').map(str::trim).collect();
        if fields.len() >= 2 && fields[0] == repo {
            return Some(fields[1].to_string());
        }
    }
    None
}
