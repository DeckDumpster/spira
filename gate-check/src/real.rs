//! The production [`crate::ports::World`]: `bd`, `gh`, `git`, `bead.sh`, `tsd-ingest.sh`,
//! and the `lib.sh` repository-map seam (the same one-shot context call `gate`/`gate-run`
//! use).

use crate::ports::World;
use std::cell::OnceCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Real {
    pub home: PathBuf,
    pub db: Option<String>,
    /// `SPIRA_BD`, resolved once at the process's top level (`spira_config::process::cfg`)
    /// and handed down — no literal `"bd"` fallback here; an unset/empty value is
    /// `spira.toml`'s own answer (see `spira/conf.d/SPIRA_BD`), not this crate's to invent.
    bd: String,
    /// `SPIRA_FLAKY_GH_REPO`, resolved the same way — empty means "no scan" per
    /// `spira/conf.d/SPIRA_FLAKY_GH_REPO`.
    flaky_repo: Option<String>,
    registry: OnceCell<spira_config::repos::Registry>,
}

impl Real {
    pub fn new(home: PathBuf, db: Option<String>, bd: String, flaky_repo: Option<String>) -> Real {
        Real { home, db, bd, flaky_repo, registry: OnceCell::new() }
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"), built once per process, in-process — no `bash -c '. lib.sh; <fn>'`
    /// subprocess per `repo_names`/`repo_land`/`repo_root`/`home_repo` call, and no
    /// one-shot snapshot subprocess either: `from_env` resolves
    /// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` the same way
    /// conf.sh does, in-process, when this (bare, unit-launched) process's own
    /// environment lacks them (sp-z3eyk).
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home))
    }

    fn bd(&self) -> Command {
        let mut c = Command::new(&self.bd);
        if let Some(db) = &self.db {
            c.arg("-C").arg(db);
        }
        c.stdin(Stdio::null());
        c
    }

    fn combined(mut c: Command) -> String {
        c.stderr(Stdio::null());
        c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(". \"$0\" >/dev/null 2>&1 || exit 96\n{body}");
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .args(args)
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }
}

impl World for Real {
    fn repo_names(&self) -> Vec<String> {
        self.registry().names()
    }

    fn repo_land(&self, name: &str) -> String {
        self.registry().land(name)
    }

    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.registry().root(name).map(PathBuf::from)
    }

    fn home_repo(&self) -> String {
        self.registry().home_repo().to_string()
    }

    fn bd_gate_list_json(&self) -> String {
        let mut c = self.bd();
        c.arg("gate").arg("list").arg("--json");
        Self::combined(c)
    }

    fn bd_gate_discover(&self, repo: &Path, branch: &str) {
        let mut c = self.bd();
        c.current_dir(repo).arg("gate").arg("discover").arg("--branch").arg(branch);
        let _ = c.stderr(Stdio::null()).stdout(Stdio::null()).status();
    }

    fn bd_gate_check(&self) -> String {
        // `2>&1` in the bash: stdout and stderr interleaved, captured as one stream — the
        // ESCALATE/stuck lines this leg parses can land on either.
        let mut c = self.bd();
        c.arg("gate").arg("check").arg("--type=gh:run");
        c.stderr(Stdio::piped());
        let Ok(out) = c.output() else { return String::new() };
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn bd_show_json(&self, id: &str) -> String {
        let mut c = self.bd();
        c.arg("show").arg(id).arg("--json");
        Self::combined(c)
    }

    fn bd_gate_resolve(&self, id: &str) {
        let mut c = self.bd();
        c.arg("gate").arg("resolve").arg(id);
        let _ = c.stderr(Stdio::null()).stdout(Stdio::null()).status();
    }

    fn spira_event(&self, kind: &str, subject: &str, summary: &str, detail: &str) {
        let _ = self.seam("spira_event \"$1\" \"$2\" \"$3\" \"$4\"", &[kind, subject, summary, detail]);
    }

    fn bd_list_json(&self) -> String {
        let mut c = self.bd();
        c.arg("list").arg("--all").arg("--json");
        Self::combined(c)
    }

    fn lc_rows(&self) -> Result<crate::engine::Lc, String> {
        spira_config::lc_state::list().map(spira_config::lc_state::index)
    }

    fn bd_priority(&self, id: &str, p: i64) {
        let mut c = self.bd();
        c.arg("priority").arg(id).arg(p.to_string());
        let _ = c.stderr(Stdio::null()).stdout(Stdio::null()).status();
    }

    fn bd_note(&self, id: &str, text: &str) {
        let mut c = self.bd();
        c.arg("note").arg(id).arg(text);
        let _ = c.stderr(Stdio::null()).stdout(Stdio::null()).status();
    }

    fn file_bead(&self, title: &str, repo: &str, priority: i64, body: &str) {
        // bead.sh by name on the launcher's PATH (sp-gypjk).
        let mut c = Command::new("bead.sh");
        c.arg("file").arg(title).arg("--for").arg("builder").arg("--repo").arg(repo).arg("-p").arg(priority.to_string()).arg("--body-file").arg("-");
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = c.spawn() {
            if let Some(mut w) = child.stdin.take() {
                let _ = w.write_all(body.as_bytes());
            }
            let _ = child.wait();
        }
    }

    fn flaky_repo(&self) -> Option<String> {
        let repo = self.flaky_repo.clone()?;
        let has_gh = Command::new("sh").arg("-c").arg("command -v gh").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if has_gh { Some(repo) } else { None }
    }

    fn gh_recent_run_ids(&self, repo: &str, limit: u32) -> Vec<String> {
        let out = Command::new("gh")
            .args(["run", "list", "--repo", repo, "--status", "completed", "--limit", &limit.to_string(), "--json", "databaseId", "--jq", ".[].databaseId"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default()
    }

    fn gh_jobs_json(&self, repo: &str, run_id: &str) -> String {
        let out = Command::new("gh").args(["api", &format!("repos/{repo}/actions/runs/{run_id}/jobs")]).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn gh_annotations_json(&self, repo: &str, job_id: &str) -> String {
        let out = Command::new("gh").args(["api", &format!("repos/{repo}/check-runs/{job_id}/annotations")]).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn gh_last_green_main_sha(&self, repo: &str) -> Option<String> {
        let out = Command::new("gh")
            .args(["run", "list", "--repo", repo, "--branch", "main", "--status", "success", "--limit", "1", "--json", "headSha", "--jq", ".[0].headSha"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    fn gh_failed_main_runs(&self, repo: &str, limit: u32) -> Vec<(String, String)> {
        let out = Command::new("gh")
            .args([
                "run",
                "list",
                "--repo",
                repo,
                "--branch",
                "main",
                "--status",
                "failure",
                "--limit",
                &limit.to_string(),
                "--json",
                "databaseId,headSha",
                "--jq",
                ".[] | [(.databaseId|tostring), .headSha] | @tsv",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        out.ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn gh_fail_lines(&self, repo: &str, run_id: &str) -> String {
        let out = Command::new("gh").args(["run", "view", run_id, "--repo", repo, "--log-failed"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
        let text = out.ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        for l in text.lines() {
            if !l.contains("FAIL") {
                continue;
            }
            // `sed 's/^[^\t]*\t[^\t]*\t//'` — strip the first two tab-separated fields.
            let stripped = l.splitn(3, '\t').last().unwrap_or(l);
            lines.push(stripped.to_string());
            if lines.len() >= 10 {
                break;
            }
        }
        lines.join("\n")
    }

    fn git_log_range(&self, repo_root: &Path, from: &str, to: &str) -> String {
        let out = Command::new("git").arg("-C").arg(repo_root).arg("log").arg("--oneline").arg(format!("{from}..{to}")).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }

    fn tsd_ingest(&self, _home: &Path, repo: &str, run_id: &str) {
        let _ = Command::new("tsd-ingest.sh").arg(repo).arg(run_id).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    fn print(&self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}
