//! The production [`crate::ports::World`]: the store's content door, `gh`, `git`, `bead.sh`, `tsd-ingest.sh`,
//! and the `lib.sh` repository-map seam (the same one-shot context call `gate`/`gate-run`
//! use).

use crate::ports::World;
use std::cell::OnceCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use spira_otel::metrics::MetricsSink;
use spira_store::lc_call::{LcClient, Reply};

pub struct Real {
    pub home: PathBuf,
    lc: Result<LcClient, String>,
    /// `SPIRA_FLAKY_GH_REPO`, resolved the same way — empty means "no scan" per
    /// `spira/conf.d/SPIRA_FLAKY_GH_REPO`.
    flaky_repo: Option<String>,
    registry: OnceCell<spira_config::repos::Registry>,
}

impl Real {
    pub fn new(home: PathBuf, lc: Result<LcClient, String>, flaky_repo: Option<String>) -> Real {
        Real { home, lc, flaky_repo, registry: OnceCell::new() }
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"), built once per process, in-process — no `bash -c '. lib.sh; <fn>'`
    /// subprocess per `repo_root`/`home_repo` call, and no
    /// one-shot snapshot subprocess either: `from_env` resolves
    /// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` the same way
    /// conf.sh does, in-process, when this (bare, unit-launched) process's own
    /// environment lacks them (sp-z3eyk).
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home))
    }

    /// One `content` request; `None` when the door gave no answer.
    fn door(&self, args: &[&str], body: Option<&str>) -> Option<Reply> {
        let lc = self.lc.as_ref().ok()?;
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
        match body {
            Some(b) => lc.call_stdin(&argv, b),
            None => lc.call(&argv),
        }
        .ok()
    }

    fn door_out(&self, args: &[&str]) -> String {
        self.door(args, None).map(|r| r.out).unwrap_or_default()
    }

    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(". \"$0\" >/dev/null 2>&1 || exit 96\n{body}");
        let envs = match spira_config::release_env::child_path_env_for_process() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("gate-check: {e}");
                return String::new();
            }
        };
        // batch-job: runs a gate, build or forge script that takes as long as its work
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .args(args)
            .envs(envs)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }
}

/// One `gh` process, counted under `gate-check:<verb>` as it exits. `spawn` is the port the
/// tests replace.
pub(crate) struct Gh<'a> {
    pub sink: &'a MetricsSink,
    pub spawn: &'a dyn Fn(&[&str]) -> std::io::Result<std::process::Output>,
}

fn spawn_gh(args: &[&str]) -> std::io::Result<std::process::Output> {
    spira_config::bounded::bounded("gh").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output()
}

impl Gh<'_> {
    fn out(&self, verb: &str, args: &[&str]) -> Option<std::process::Output> {
        forge::spend::counted_output(self.sink, &format!("gate-check:{verb}"), args, || (self.spawn)(args)).ok()
    }
}

pub(crate) fn recent_run_ids(gh: &Gh, repo: &str, limit: u32) -> Vec<String> {
    let limit = limit.to_string();
    let out = gh.out("run-list", &["run", "list", "--repo", repo, "--status", "completed", "--limit", &limit, "--json", "databaseId", "--jq", ".[].databaseId"]);
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default()
}

fn with_real_gh<T>(f: impl FnOnce(&Gh) -> T) -> T {
    let sink = forge::telemetry::sink();
    f(&Gh { sink: &sink, spawn: &spawn_gh })
}

impl World for Real {
    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.registry().root(name).map(PathBuf::from)
    }

    fn home_repo(&self) -> String {
        self.registry().home_repo().to_string()
    }

    fn bd_gate_check(&self) -> String {
        // The ESCALATE/stuck lines this leg parses can land on either stream.
        self.door(&["gate", "check", "--type=gh:run"], None).map(|r| format!("{}{}", r.out, r.err)).unwrap_or_default()
    }

    fn bd_show_json(&self, id: &str) -> String {
        self.door_out(&["show", id, "--json"])
    }

    fn bd_gate_resolve(&self, id: &str) {
        let _ = self.door(&["gate", "resolve", id], None);
    }

    fn spira_event(&self, kind: &str, subject: &str, summary: &str, detail: &str) {
        let _ = self.seam("spira_event \"$1\" \"$2\" \"$3\" \"$4\"", &[kind, subject, summary, detail]);
    }

    fn bd_list_json(&self) -> String {
        self.door_out(&["list", "--all", "--limit", "0", "--json"])
    }

    fn lc_rows(&self) -> Result<crate::engine::Lc, String> {
        spira_store::lc_state::list_live().map(spira_store::lc_state::index)
    }

    fn bd_priority(&self, id: &str, p: i64) {
        let _ = self.door(&["update", id, "--priority", &p.to_string()], None);
    }

    fn bd_note(&self, id: &str, text: &str) {
        let _ = self.door(&["note", id, "--stdin"], Some(text));
    }

    fn file_bead(&self, title: &str, repo: &str, priority: i64, body: &str) {
        // bead.sh by name on the launcher's PATH (sp-gypjk).
        let mut c = spira_config::bounded::bounded("bead.sh");
        c.arg("file").arg(title).arg("--for").arg("builder").arg("--repo").arg(repo).arg("-p").arg(priority.to_string()).arg("--body-file").arg("-");
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = c.spawn() {
            if let Some(mut w) = child.stdin.take() {
                let _ = w.write_all(format!("{body}\n\n## Build\n\nFix the gate failure described above in this repository and land the fix.\n").as_bytes());
            }
            let _ = child.wait();
        }
    }

    fn flaky_repo(&self) -> Option<String> {
        let repo = self.flaky_repo.clone()?;
        let has_gh = spira_config::bounded::bounded("sh").arg("-c").arg("command -v gh").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if has_gh { Some(repo) } else { None }
    }

    fn gh_recent_run_ids(&self, repo: &str, limit: u32) -> Vec<String> {
        with_real_gh(|gh| recent_run_ids(gh, repo, limit))
    }

    fn gh_jobs_json(&self, repo: &str, run_id: &str) -> String {
        let out = with_real_gh(|gh| gh.out("jobs", &["api", &format!("repos/{repo}/actions/runs/{run_id}/jobs")]));
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn gh_annotations_json(&self, repo: &str, job_id: &str) -> String {
        let out = with_real_gh(|gh| gh.out("annotations", &["api", &format!("repos/{repo}/check-runs/{job_id}/annotations")]));
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn gh_last_green_main_sha(&self, repo: &str) -> Option<String> {
        let out = with_real_gh(|gh| gh.out("run-list", &["run", "list", "--repo", repo, "--branch", "main", "--status", "success", "--limit", "1", "--json", "headSha", "--jq", ".[0].headSha"]))?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    fn gh_failed_main_runs(&self, repo: &str, limit: u32) -> Vec<(String, String)> {
        let limit = limit.to_string();
        let out = with_real_gh(|gh| gh.out("run-list", &["run", "list", "--repo", repo, "--branch", "main", "--status", "failure", "--limit", &limit, "--json", "databaseId,headSha"]));
        out.filter(|o| o.status.success()).and_then(|o| failed_runs_of(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default()
    }

    fn gh_fail_lines(&self, repo: &str, run_id: &str) -> String {
        let out = with_real_gh(|gh| gh.out("run-view", &["run", "view", run_id, "--repo", repo, "--log-failed"]));
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        crate::engine::fail_lines(&text).join("\n")
    }

    fn git_log_range(&self, repo_root: &Path, from: &str, to: &str) -> String {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo_root).arg("log").arg("--oneline").arg(format!("{from}..{to}")).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }

    fn tsd_ingest(&self, _home: &Path, repo: &str, run_id: &str) {
        let _ = spira_config::bounded::bounded("tsd-ingest.sh").arg(repo).arg(run_id).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    fn print(&self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}

/// `gh run list --json databaseId,headSha` as (run id, head sha); `None` when the answer is not
/// that array.
pub fn failed_runs_of(json: &str) -> Option<Vec<(String, String)>> {
    let rows = serde_json::from_str::<serde_json::Value>(json).ok()?;
    rows.as_array()?
        .iter()
        .map(|r| Some((r.get("databaseId")?.as_u64()?.to_string(), r.get("headSha")?.as_str()?.to_string())))
        .collect()
}

#[cfg(test)]
mod failed_runs_tests {
    use super::failed_runs_of;

    #[test_id(tibtx8c)]
    #[test]
    fn failed_runs_are_read_from_the_json_array() {
        let got = failed_runs_of(r#"[{"databaseId":42,"headSha":"abc"},{"databaseId":7,"headSha":"def"}]"#);
        assert_eq!(got, Some(vec![("42".into(), "abc".into()), ("7".into(), "def".into())]));
        assert_eq!(failed_runs_of("42\tabc\n"), None, "tab-separated text is not the answer");
        assert_eq!(failed_runs_of("[]"), Some(vec![]));
    }
}

#[cfg(test)]
mod door_tests {
    use super::*;
    use spira_store::lc_call::fake::Content;
    use std::sync::Arc;

    struct Stderr;

    impl spira_store::lc_call::Transport for Stderr {
        fn exchange(&self, _: &[String], _: Option<&str>, _: std::time::Duration) -> Result<Reply, String> {
            Ok(Reply { code: 0, out: "out\n".into(), err: "⚠ zq-9k2: ESCALATE red\n".into(), read_ended: false })
        }
    }

    #[test_id(tl4617v)]
    #[test]
    fn a_gate_check_answer_carries_both_streams() {
        let world = Real::new(PathBuf::new(), Ok(LcClient::over(Arc::new(Stderr), true)), None);
        assert_eq!(world.bd_gate_check(), "out\n⚠ zq-9k2: ESCALATE red\n");
    }

    #[test_id(tsbo4sr)]
    #[test]
    fn the_stores_verbs_are_content_requests() {
        let door = Arc::new(Content::new(vec![]));
        let world = Real::new(PathBuf::new(), Ok(LcClient::over(door.clone(), true)), None);
        world.bd_priority("sp-a", 1);
        world.bd_note("sp-a", "text");
        world.bd_gate_resolve("sp-g");
        let _ = world.bd_gate_check();
        let _ = world.bd_list_json();
        let seen: Vec<String> = door.requests().iter().map(|r| r.join(" ")).collect();
        assert_eq!(
            seen,
            [
                "content update sp-a --priority 1",
                "content note sp-a --stdin",
                "content gate resolve sp-g",
                "content gate check --type=gh:run",
                "content list --all --limit 0 --json",
            ]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;
    use std::os::unix::process::ExitStatusExt;

    #[test_id(tdr4qe6)]
    #[test]
    fn a_run_list_through_a_fake_gh_is_counted_once_under_gate_check_run_list() {
        let fake = |_: &[&str]| {
            Ok(std::process::Output { status: std::process::ExitStatus::from_raw(0), stdout: b"11\n12\n".to_vec(), stderr: Vec::new() })
        };
        let (ids, got) = fixture::capture(|sink| recent_run_ids(&Gh { sink, spawn: &fake }, "o/r", 20));
        assert_eq!(ids, ["11", "12"]);
        let series: Vec<_> = got.iter().filter(|s| s.name == forge::spend::CALLS).collect();
        assert_eq!(series.len(), 1, "{got:?}");
        assert_eq!((series[0].label("caller"), series[0].label("outcome"), series[0].value as u64), (Some("gate-check:run-list"), Some("fresh"), 1));
    }
}
