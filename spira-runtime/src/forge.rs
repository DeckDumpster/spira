//! The forge over HTTP. Each question is one REST call with a short timeout; a call that fails is
//! `None`, never an empty answer, so a forge that is down cannot look like a repository with
//! nothing open.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::watchers::pr_notify::{classify, Final, OpenPr, Run};
use crate::watchers::repo_events;
use crate::wiki_git::gh_auth_token;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const PAGE_SIZE: usize = 100;
const PAGE: &str = "per_page=100";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Forge {
    pub api: String,
    pub token: String,
    pub gh_config_dir: String,
    pub gh: String,
}

pub const NO_CREDENTIAL: &str = "no forge credential";

impl fmt::Debug for Forge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Forge").field("api", &self.api).field("token", &(!self.token.is_empty())).field("gh_config_dir", &self.gh_config_dir).finish()
    }
}

/// `owner/repo` of a checkout's forge remote: `origin` when it has one, else the first remote.
pub fn slug(repo_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(repo_dir.join(".git/config")).ok()?;
    let mut remotes: Vec<(String, String)> = vec![];
    let mut current: Option<String> = None;
    for l in text.lines().map(str::trim) {
        if let Some(h) = l.strip_prefix('[') {
            current = h.strip_prefix("remote \"").and_then(|r| r.strip_suffix("\"]")).map(str::to_string);
        } else if let (Some(name), Some(url)) = (&current, l.strip_prefix("url").map(|r| r.trim_start().trim_start_matches('=').trim())) {
            remotes.push((name.clone(), url.to_string()));
        }
    }
    let (_, url) = remotes.iter().find(|(n, _)| n == "origin").or(remotes.first())?;
    let path = url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).take(2).collect::<Vec<_>>();
    match path[..] {
        [repo, owner] if !repo.is_empty() && !owner.is_empty() => Some(format!("{owner}/{repo}")),
        _ => None,
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn epoch(stamp: &str) -> Option<u64> {
    let (date, time) = stamp.strip_suffix('Z')?.split_once('T')?;
    let d: Vec<i64> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    let t: Vec<i64> = time.split(':').filter_map(|p| p.parse().ok()).collect();
    let [y, m, day] = d[..] else { return None };
    let [h, mi, s] = t[..] else { return None };
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from(((era * 146_097 + doe - 719_468) * 86_400) + h * 3600 + mi * 60 + s).ok()
}

/// The first run of two or more digits in a runner's name, else the name itself.
fn vmid_of(name: &str) -> String {
    let first = name.split(|c: char| !c.is_ascii_digit()).find(|run| run.len() >= 2);
    match first {
        Some(d) => d.to_string(),
        None if name.is_empty() => "?".into(),
        None => name.to_string(),
    }
}

/// The check runs that judge a PR on `branch`, each `(name, STATUS, CONCLUSION)`. A commit's check
/// runs are every run at that head, whichever branch it ran for: a head published once as a PR
/// that went red and again as a new PR carries the old PR's failures too. `runs` are the commit's
/// workflow runs; a check a workflow run owns counts only when that run is the latest of its
/// workflow on `branch`, and a check no workflow run owns (an app's own check) always counts.
pub fn current_checks(check_runs: &[Value], runs: &[Value], branch: &str) -> Vec<(String, String, String)> {
    let suite = |v: &Value, key: &str| v.pointer(key).and_then(Value::as_u64);
    let owned: BTreeSet<u64> = runs.iter().filter_map(|r| suite(r, "/check_suite_id")).collect();
    let mut latest: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for r in runs.iter().filter(|r| r["head_branch"].as_str() == Some(branch)) {
        let (Some(id), Some(s)) = (r["id"].as_u64(), suite(r, "/check_suite_id")) else { continue };
        let workflow = r["workflow_id"].as_u64().map_or_else(|| text(r, "name"), |w| w.to_string());
        if latest.get(&workflow).is_none_or(|(newest, _)| id > *newest) {
            latest.insert(workflow, (id, s));
        }
    }
    let current: BTreeSet<u64> = latest.values().map(|(_, s)| *s).collect();
    check_runs
        .iter()
        .filter(|c| suite(c, "/check_suite/id").is_none_or(|s| !owned.contains(&s) || current.contains(&s)))
        .map(|c| (text(c, "name"), text(c, "status").to_uppercase(), text(c, "conclusion").to_uppercase()))
        .collect()
}

impl Forge {
    /// A static token when one is set, else whatever `gh auth token` answers now under the
    /// configured gh config dir. Read per call, so a rotated login is picked up; never logged.
    pub fn credential(&self) -> Result<String, String> {
        if !self.token.is_empty() {
            return Ok(self.token.clone());
        }
        let (code, out) = gh_auth_token(if self.gh.is_empty() { "gh" } else { &self.gh }, &self.gh_config_dir, CALL_TIMEOUT).map_err(|e| format!("{NO_CREDENTIAL}: gh: {e}"))?;
        let token = String::from_utf8_lossy(&out).trim().to_string();
        if code == Some(0) && !token.is_empty() {
            Ok(token)
        } else {
            Err(format!("{NO_CREDENTIAL}: SPIRA_BROKER_GH_TOKEN is empty and gh has no login"))
        }
    }

    fn get(&self, path: &str) -> Option<Value> {
        self.try_get(path).ok()
    }

    fn try_get(&self, path: &str) -> Result<Value, String> {
        let token = self.credential()?;
        let req = ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build().get(&format!("{}{path}", self.api.trim_end_matches('/'))).set("Accept", "application/vnd.github+json").set("User-Agent", "spira-runtime").set("Authorization", &format!("Bearer {token}"));
        let mut body = String::new();
        req.call().map_err(|e| e.to_string())?.into_reader().take(16 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_str(&body).map_err(|e| format!("{path}: {e}"))
    }

    /// A list the forge may have cut short at one page reads as not answered, never as the whole.
    fn listed(&self, path: &str) -> Option<Vec<Value>> {
        self.get(path)?.as_array().filter(|a| a.len() < PAGE_SIZE).cloned()
    }

    /// Every open PR with its checks classified; `None` when any call failed.
    pub fn open_prs(&self, slug: &str) -> Option<Vec<(OpenPr, String)>> {
        let prs = self.listed(&format!("/repos/{slug}/pulls?state=open&{PAGE}"))?;
        prs.iter()
            .map(|p| {
                let sha = p["head"]["sha"].as_str()?;
                let checks = self.get(&format!("/repos/{slug}/commits/{sha}/check-runs?{PAGE}"))?;
                let runs = self.get(&format!("/repos/{slug}/actions/runs?head_sha={sha}&{PAGE}"))?;
                let runs = runs["workflow_runs"].as_array().filter(|a| a.len() < PAGE_SIZE)?;
                let checks = current_checks(checks["check_runs"].as_array()?, runs, &text(&p["head"], "ref"));
                let (status, fail) = classify(&checks);
                let title = text(p, "title").replace(['\n', '\t'], " ");
                Some((OpenPr { number: p["number"].as_u64()?, status: status.into(), title, fail }, text(&p["head"], "ref")))
            })
            .collect()
    }

    /// How a PR that left the open list ended; `None` when the forge could not say.
    pub fn final_state(&self, slug: &str, n: u64) -> Option<Final> {
        let p = self.get(&format!("/repos/{slug}/pulls/{n}"))?;
        let state = match (p["state"].as_str()?, p["merged"].as_bool().unwrap_or(false)) {
            (_, true) => "MERGED",
            ("closed", false) => "CLOSED",
            _ => "OPEN",
        };
        let beads = text(&p, "body").lines().filter(|l| l.starts_with("- ")).map(str::to_string).collect();
        Some(Final { state: state.into(), sha: text(&p, "merge_commit_sha"), beads })
    }

    /// The first completed deploy run for a merge commit; `None` while there is none.
    pub fn deploy_run(&self, slug: &str, sha: &str) -> Option<Run> {
        let runs = self.get(&format!("/repos/{slug}/actions/workflows/deploy.yml/runs?head_sha={sha}&{PAGE}"))?;
        let r = runs["workflow_runs"].as_array()?.iter().find(|r| r["status"] == "completed")?;
        let secs = epoch(r["updated_at"].as_str()?).zip(epoch(r["created_at"].as_str()?)).map_or(0, |(u, c)| u.saturating_sub(c));
        Some(Run { conclusion: r["conclusion"].as_str().unwrap_or("unknown").to_string(), secs, url: text(r, "html_url") })
    }

    /// The newest finished workflow runs of a repository, newest first; the forge's reason when it
    /// could not say, never an empty list.
    pub fn completed_runs(&self, slug: &str) -> Result<Vec<repo_events::Run>, String> {
        let v = self.try_get(&format!("/repos/{slug}/actions/runs?per_page=30"))?;
        let runs = v["workflow_runs"].as_array().ok_or_else(|| format!("/repos/{slug}/actions/runs: no workflow_runs"))?;
        Ok(runs
            .iter()
            .filter(|r| r["status"] == "completed")
            .filter_map(|r| {
                Some(repo_events::Run { id: r["id"].as_u64()?, workflow: text(r, "name"), conclusion: text(r, "conclusion"), branch: text(r, "head_branch"), event: text(r, "event"), title: repo_events::title_of(&text(r, "display_title")) })
            })
            .collect())
    }

    /// One `STRANDED run=<id> job="<name>" label=<labels> queued=<n>s runner=<name> vmid=<id>` line per
    /// queued job older than `min_age` whose per-run label no online runner carries; the lines
    /// `forge stranded-runners` prints. A call that fails reads as nothing stranded, as it does there.
    pub fn stranded_runners(&self, slug: &str, now: u64, min_age: u64) -> Vec<String> {
        const GENERIC: [&str; 9] = ["self-hosted", "linux", "macos", "windows", "x64", "arm64", "x86_64", "arm", "ubuntu-latest"];
        let Some(runs) = self.get(&format!("/repos/{slug}/actions/runs?per_page=50")) else { return vec![] };
        let ids: Vec<String> = runs["workflow_runs"].as_array().map(|a| a.iter().filter(|r| r["status"] != "completed").filter_map(|r| r.get("id").map(|i| i.to_string())).collect()).unwrap_or_default();
        if ids.is_empty() {
            return vec![];
        }
        let runners: Vec<Value> = self.get(&format!("/repos/{slug}/actions/runners?per_page=100")).and_then(|v| v["runners"].as_array().cloned()).unwrap_or_default();
        let mut lines = vec![];
        for id in ids {
            let Some(jobs) = self.get(&format!("/repos/{slug}/actions/runs/{id}/jobs")) else { continue };
            for j in jobs["jobs"].as_array().cloned().unwrap_or_default() {
                if j["status"] != "queued" {
                    continue;
                }
                let created = epoch(j["created_at"].as_str().unwrap_or("")).unwrap_or(0);
                let age = now.saturating_sub(created);
                if created == 0 || age < min_age {
                    continue;
                }
                let labels: Vec<String> = j["labels"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).filter(|l| !GENERIC.contains(&l.to_lowercase().as_str())).collect()).unwrap_or_default();
                if labels.is_empty() {
                    continue;
                }
                let carrying: Vec<&Value> = runners.iter().filter(|r| r["labels"].as_array().is_some_and(|ls| ls.iter().any(|l| l["name"].as_str().is_some_and(|n| labels.iter().any(|x| x == n))))).collect();
                if carrying.iter().any(|r| r["status"] == "online") {
                    continue;
                }
                let (runner, vmid) = carrying.first().map_or(("none".to_string(), "?".to_string()), |r| {
                    let name = text(r, "name");
                    let vmid = vmid_of(&name);
                    (name, vmid)
                });
                lines.push(format!("STRANDED run={id} job=\"{}\" label={} queued={age}s runner={runner} vmid={vmid}", text(&j, "name"), labels.join(",")));
            }
        }
        lines
    }

    /// `spira/*` branches ahead of `base` that none of `open_heads` is the head of.
    pub fn stray_branches(&self, slug: &str, base: &str, open_heads: &[String]) -> Option<Vec<String>> {
        let branches = self.listed(&format!("/repos/{slug}/branches?{PAGE}"))?;
        let mut out = vec![];
        for b in branches.iter().filter_map(|b| b["name"].as_str()).filter(|b| b.starts_with("spira/")) {
            let ahead = self.get(&format!("/repos/{slug}/compare/{base}...{b}"))?["ahead_by"].as_u64()?;
            if ahead > 0 && !open_heads.iter().any(|h| h == b) {
                out.push(b.to_string());
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(url: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new("forge-slug");
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/config"), format!("[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/a/b.git\n[remote \"origin\"]\n\turl = {url}\n")).unwrap();
        d
    }

    #[test]
    fn every_remote_spelling_names_owner_and_repo() {
        for url in ["https://github.com/acme/widgets.git", "git@github.com:acme/widgets.git", "ssh://git@host:22/acme/widgets", "file:///x/acme/widgets/"] {
            assert_eq!(slug(checkout(url).path()).as_deref(), Some("acme/widgets"), "{url}");
        }
        assert_eq!(slug(Path::new("/nonexistent")), None);
    }

    #[test]
    fn without_an_origin_the_first_remote_is_used() {
        let d = testkit::TempDir::new("forge-slug-first");
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/config"), "[remote \"fork\"]\n\turl = git@h:o/r.git\n").unwrap();
        assert_eq!(slug(d.path()).as_deref(), Some("o/r"));
    }

    #[test]
    fn a_runner_is_known_by_the_first_run_of_two_digits_in_its_name_else_its_name() {
        assert_eq!((vmid_of("ci-vm-9001-a7").as_str(), vmid_of("r1").as_str(), vmid_of("").as_str()), ("9001", "r1", "?"));
    }

    /// sp-hw3qio: PR #989's head had also been published as PR #988, whose Gate runs failed at
    /// provision. The commit's check runs carry both PRs' runs, so pr-notify said FAIL RED for a
    /// PR whose own latest runs were green. A PR is judged by the latest run of each workflow on
    /// its own branch; a check that no workflow run owns (an app's check) still counts.
    #[test]
    fn a_pr_is_judged_by_the_latest_run_of_each_workflow_on_its_own_branch() {
        let check = |name: &str, conclusion: &str, suite: u64| serde_json::json!({"name": name, "status": "completed", "conclusion": conclusion, "check_suite": {"id": suite}});
        let run = |id: u64, workflow: &str, branch: &str, suite: u64| serde_json::json!({"id": id, "name": workflow, "head_branch": branch, "check_suite_id": suite});
        let checks = [
            check("provision", "failure", 1), // PR 988's Gate run, same head
            check("gate", "failure", 1),
            check("provision", "failure", 2), // PR 989's first Gate run, superseded
            check("provision", "success", 3), // PR 989's latest Gate run
            check("gate", "success", 3),
            check("tag", "success", 4), // PR 989's Test image run
            check("lint-app", "success", 9), // an app's check, no workflow run
        ];
        let runs = [run(10, "Gate", "spira/publish/20261107T001222Z", 1), run(11, "Gate", "spira/publish/1794013814", 2), run(12, "Gate", "spira/publish/1794013814", 3), run(13, "Test image", "spira/publish/1794013814", 4)];
        let judged = current_checks(&checks, &runs, "spira/publish/1794013814");
        assert_eq!(classify(&judged), ("green", String::new()), "{judged:?}");
        let names: Vec<&str> = judged.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, ["provision", "gate", "tag", "lint-app"]);
        // On PR 988's own branch the same commit is red.
        assert_eq!(classify(&current_checks(&checks, &runs, "spira/publish/20261107T001222Z")).0, "red");
    }

    #[test]
    fn stamps_convert_to_epoch_seconds() {
        assert_eq!(epoch("2026-10-28T05:45:43Z"), Some(1_793_166_343));
        assert_eq!(epoch("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(epoch("nonsense"), None);
    }
}
