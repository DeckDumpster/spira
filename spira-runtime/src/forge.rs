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

use crate::api_budget;
use crate::watchers::pr_notify::{classify, Final, OpenPr, Run};
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
    pub metrics_endpoint: Option<String>,
}

pub const NO_CREDENTIAL: &str = "no forge credential";

impl fmt::Debug for Forge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Forge").field("api", &self.api).field("token", &(!self.token.is_empty())).field("gh_config_dir", &self.gh_config_dir).finish()
    }
}

fn remotes(repo_dir: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(repo_dir.join(".git/config")) else { return vec![] };
    let mut remotes: Vec<(String, String)> = vec![];
    let mut current: Option<String> = None;
    for l in text.lines().map(str::trim) {
        if let Some(h) = l.strip_prefix('[') {
            current = h.strip_prefix("remote \"").and_then(|r| r.strip_suffix("\"]")).map(str::to_string);
        } else if let (Some(name), Some(url)) = (&current, l.strip_prefix("url").map(|r| r.trim_start().trim_start_matches('=').trim())) {
            remotes.push((name.clone(), url.to_string()));
        }
    }
    remotes
}

fn slug_of(url: &str) -> Option<String> {
    let path = url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).take(2).collect::<Vec<_>>();
    match path[..] {
        [repo, owner] if !repo.is_empty() && !owner.is_empty() => Some(format!("{owner}/{repo}")),
        _ => None,
    }
}

/// `owner/repo` of a checkout's forge remote: `origin` when it has one, else the first remote.
pub fn slug(repo_dir: &Path) -> Option<String> {
    let remotes = remotes(repo_dir);
    let (_, url) = remotes.iter().find(|(n, _)| n == "origin").or(remotes.first())?;
    slug_of(url)
}

/// `owner/repo` of the named remote, only when that remote is on github.com.
pub fn github_slug(repo_dir: &Path, remote: &str) -> Option<String> {
    let url = remotes(repo_dir).into_iter().find(|(n, _)| n == remote)?.1;
    host_of(&url).eq_ignore_ascii_case("github.com").then(|| slug_of(&url)).flatten()
}

fn host_of(url: &str) -> &str {
    let authority = match url.split_once("://") {
        Some((_, rest)) => rest.split('/').next().unwrap_or(""),
        None => url.split(':').next().unwrap_or(""),
    };
    authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("")
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

pub(crate) fn epoch(stamp: &str) -> Option<u64> {
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
/// workflow runs; a cancelled run a newer run of its workflow replaced is pending or dropped, by
/// that run's state, and a check a workflow run owns counts only when that run is the latest of its
/// workflow on `branch`, and a check no workflow run owns (an app's own check) always counts.
pub fn current_checks(check_runs: &[Value], runs: &[Value], branch: &str) -> Vec<(String, String, String)> {
    let suite = |v: &Value, key: &str| v.pointer(key).and_then(Value::as_u64);
    let owned: BTreeSet<u64> = runs.iter().filter_map(|r| suite(r, "/check_suite_id")).collect(); // not a path: GitHub API route or JSON pointer
    let workflow_of = |r: &Value| r["workflow_id"].as_u64().map_or_else(|| text(r, "name"), |w| w.to_string());
    let mut latest: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for r in runs.iter().filter(|r| r["head_branch"].as_str() == Some(branch)) {
        let (Some(id), Some(s)) = (r["id"].as_u64(), suite(r, "/check_suite_id")) else { continue }; // not a path: GitHub API route or JSON pointer
        let workflow = workflow_of(r);
        if latest.get(&workflow).is_none_or(|(newest, _)| id > *newest) {
            latest.insert(workflow, (id, s));
        }
    }
    let current: BTreeSet<u64> = latest.values().map(|(_, s)| *s).collect();
    // Cancelled checks of a run a newer run of the same workflow replaced, by that run's state.
    let mut replaced: BTreeMap<u64, bool> = BTreeMap::new();
    for r in runs.iter().filter(|r| r["conclusion"].as_str() == Some("cancelled")) {
        let (Some(id), Some(s)) = (r["id"].as_u64(), suite(r, "/check_suite_id")) else { continue }; // not a path: GitHub API route or JSON pointer
        let newer = runs.iter().filter(|n| n["id"].as_u64() > Some(id) && workflow_of(n) == workflow_of(r));
        let pending = newer.clone().any(|n| n["status"].as_str() != Some("completed"));
        if pending || newer.clone().any(|n| n["conclusion"].as_str() == Some("success")) {
            replaced.insert(s, pending);
        }
    }
    check_runs
        .iter()
        .filter(|c| suite(c, "/check_suite/id").is_none_or(|s| !owned.contains(&s) || current.contains(&s))) // not a path: GitHub API route or JSON pointer
        .filter_map(|c| {
            let (name, status, conclusion) = (text(c, "name"), text(c, "status").to_uppercase(), text(c, "conclusion").to_uppercase());
            match (conclusion == "CANCELLED").then(|| suite(c, "/check_suite/id").and_then(|s| replaced.get(&s))).flatten() { // not a path: GitHub API route or JSON pointer
                Some(true) => Some((name, "IN_PROGRESS".into(), String::new())),
                Some(false) => None,
                None => Some((name, status, conclusion)),
            }
        })
        .collect()
}

struct Reply {
    status: u16,
    etag: Option<String>,
    rate: Option<api_budget::Rate>,
    body: String,
}

struct Got {
    value: Value,
    outcome: &'static str,
    /// The ETag to keep with `value` for the next call; `None` when the answer carried none or came from the kept body.
    etag: Option<String>,
}

fn rate_of(r: &ureq::Response) -> Option<api_budget::Rate> {
    let num = |h: &str| r.header(h).and_then(|v| v.trim().parse::<u64>().ok());
    Some(api_budget::Rate { limit: num("x-ratelimit-limit")?, remaining: num("x-ratelimit-remaining")?, reset: num("x-ratelimit-reset")? })
}

/// The decisions of one conditional GET, with the transport passed in. The outcome is returned,
/// never counted, so a test reads it from the return value.
fn conditional_get(api: &str, url: &str, path: &str, caller: &str, now: u64, kept: Option<(String, Value)>, send: impl FnOnce(Option<&str>) -> Result<Reply, String>) -> Result<Got, (String, &'static str)> {
    if let Some(reset) = api_budget::exhausted_until(api, now) {
        let why = format!("the GitHub API budget is spent until {} (epoch {reset}); {caller} made no request", api_budget::clock_of(reset));
        if api_budget::first_report_of(api, reset) {
            eprintln!("spira-runtime: {why}");
        }
        return Err((why, api_budget::REFUSED));
    }
    let failed = |why: String| (why, api_budget::FAILED);
    let reply = send(kept.as_ref().map(|(tag, _)| tag.as_str())).map_err(failed)?;
    if let Some(rate) = reply.rate {
        api_budget::note_rate(api, rate);
    }
    if reply.status >= 400 {
        return Err(failed(format!("{url}: status code {}", reply.status)));
    }
    if let (304, Some((_, body))) = (reply.status, kept) {
        return Ok(Got { value: body, outcome: api_budget::CACHED, etag: None });
    }
    let value = serde_json::from_str(&reply.body).map_err(|e| failed(format!("{path}: {e}")))?;
    Ok(Got { value, outcome: api_budget::FRESH, etag: reply.etag })
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

    fn get(&self, caller: &str, path: &str) -> Option<Value> {
        self.try_get(caller, path).ok()
    }

    /// One GET, conditional on the ETag of the last answer for the same URL: a 304 returns that
    /// answer and is not charged against the budget. A spent budget is reported once per reset
    /// and no request is made until it passes.
    fn try_get(&self, caller: &str, path: &str) -> Result<Value, String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let url = format!("{}{path}", self.api.trim_end_matches('/'));
        let kept = api_budget::etag(&url).zip(api_budget::body_for(&url));
        let send = |tag: Option<&str>| {
            let token = self.credential()?;
            let mut req = ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build().get(&url).set("Accept", "application/vnd.github+json").set("User-Agent", "spira-runtime").set("Authorization", &format!("Bearer {token}"));
            if let Some(tag) = tag {
                req = req.set("If-None-Match", tag);
            }
            match req.call() {
                Ok(r) => {
                    let (status, etag, rate) = (r.status(), r.header("ETag").map(str::to_string), rate_of(&r));
                    let mut body = String::new();
                    r.into_reader().take(16 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
                    Ok(Reply { status, etag, rate, body })
                }
                Err(ureq::Error::Status(status, r)) => Ok(Reply { status, etag: None, rate: rate_of(&r), body: String::new() }),
                Err(e) => Err(e.to_string()),
            }
        };
        match conditional_get(&self.api, &url, path, caller, now, kept, send) {
            Ok(got) => {
                api_budget::count(caller, got.outcome);
                if let Some(tag) = got.etag {
                    api_budget::keep(&url, tag, got.value.clone());
                }
                Ok(got.value)
            }
            Err((why, outcome)) => {
                api_budget::count(caller, outcome);
                Err(why)
            }
        }
    }

    /// Pushes what this process has spent since the last flush.
    pub fn report_spend(&self) {
        api_budget::flush(self.metrics_endpoint.as_deref());
    }

    /// A list the forge may have cut short at one page reads as not answered, never as the whole.
    fn listed(&self, caller: &str, path: &str) -> Option<Vec<Value>> {
        self.get(caller, path)?.as_array().filter(|a| a.len() < PAGE_SIZE).cloned()
    }

    /// One PR by number with its checks classified: `Some(None)` once it is no longer open, `None`
    /// when the forge could not say. Its checks are read only while it is open.
    pub fn open_pr(&self, slug: &str, n: u64) -> Option<Option<(OpenPr, String)>> {
        let p = self.get("open_prs", &format!("/repos/{slug}/pulls/{n}"))?; // not a path: GitHub API route or JSON pointer
        if p["state"].as_str()? != "open" {
            return Some(None);
        }
        let sha = p["head"]["sha"].as_str()?;
        let checks = self.get("open_prs", &format!("/repos/{slug}/commits/{sha}/check-runs?{PAGE}"))?; // not a path: GitHub API route or JSON pointer
        let runs = self.get("open_prs", &format!("/repos/{slug}/actions/runs?head_sha={sha}&{PAGE}"))?; // not a path: GitHub API route or JSON pointer
        let runs = runs["workflow_runs"].as_array().filter(|a| a.len() < PAGE_SIZE)?;
        let checks = current_checks(checks["check_runs"].as_array()?, runs, &text(&p["head"], "ref"));
        let (status, fail) = classify(&checks);
        let title = text(&p, "title").replace(['\n', '\t'], " ");
        Some(Some((OpenPr { number: n, status: status.into(), title, fail }, text(&p["head"], "ref"))))
    }

    /// How a PR that left the open list ended; `None` when the forge could not say.
    pub fn final_state(&self, slug: &str, n: u64) -> Option<Final> {
        let p = self.get("final_state", &format!("/repos/{slug}/pulls/{n}"))?; // not a path: GitHub API route or JSON pointer
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
        let runs = self.get("deploy_run", &format!("/repos/{slug}/actions/workflows/deploy.yml/runs?head_sha={sha}&{PAGE}"))?; // not a path: GitHub API route or JSON pointer
        let r = runs["workflow_runs"].as_array()?.iter().find(|r| r["status"] == "completed")?;
        let secs = epoch(r["updated_at"].as_str()?).zip(epoch(r["created_at"].as_str()?)).map_or(0, |(u, c)| u.saturating_sub(c));
        Some(Run { conclusion: r["conclusion"].as_str().unwrap_or("unknown").to_string(), secs, url: text(r, "html_url") })
    }

    /// One `STRANDED run=<id> job="<name>" label=<labels> queued=<n>s runner=<name> vmid=<id>` line per
    /// queued job older than `min_age` whose per-run label no online runner carries, in a run with
    /// no job in progress; the lines `forge stranded-runners` prints. A listing that fails or is cut
    /// short of its `total_count` is an error: it cannot tell.
    pub fn stranded_runners(&self, slug: &str, now: u64, min_age: u64) -> Result<Vec<String>, String> {
        let running = |job_state: &str| !["queued", "waiting", "pending", "requested", "completed"].contains(&job_state);
        const GENERIC: [&str; 9] = ["self-hosted", "linux", "macos", "windows", "x64", "arm64", "x86_64", "arm", "ubuntu-latest"];
        let listing = |path: String, rows: &str, whole: bool| -> Result<Vec<Value>, String> {
            let v = self.try_get("stranded_runners", &path)?;
            let list = v[rows].as_array().cloned().ok_or_else(|| format!("{path} has no {rows}"))?;
            match v["total_count"].as_u64() {
                Some(n) if whole && n > list.len() as u64 => Err(format!("{path} is cut short at {} of {n} rows", list.len())),
                _ => Ok(list),
            }
        };
        let runs = listing(format!("/repos/{slug}/actions/runs?per_page=50"), "workflow_runs", false)?; // not a path: GitHub API route or JSON pointer
        let ids: Vec<String> = runs.iter().filter(|r| r["status"] != "completed").filter_map(|r| r.get("id").map(|i| i.to_string())).collect();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let owner = slug.split('/').next().unwrap_or(slug);
        let mut runners = listing(format!("/repos/{slug}/actions/runners?per_page=100"), "runners", true)?; // not a path: GitHub API route or JSON pointer
        runners.extend(listing(format!("/orgs/{owner}/actions/runners?per_page=100"), "runners", true)?); // not a path: GitHub API route or JSON pointer
        let mut lines = vec![];
        for id in ids {
            let jobs = listing(format!("/repos/{slug}/actions/runs/{id}/jobs?per_page=100"), "jobs", true)?; // not a path: GitHub API route or JSON pointer
            if jobs.iter().any(|j| running(j["status"].as_str().unwrap_or_default())) {
                continue;
            }
            for j in jobs {
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
        Ok(lines)
    }

    /// `spira/*` branches ahead of `base` that none of `open_heads` is the head of.
    pub fn stray_branches(&self, slug: &str, base: &str, open_heads: &[String]) -> Option<Vec<String>> {
        let branches = self.listed("stray_branches", &format!("/repos/{slug}/branches?{PAGE}"))?; // not a path: GitHub API route or JSON pointer
        let mut out = vec![];
        for b in branches.iter().filter_map(|b| b["name"].as_str()).filter(|b| b.starts_with("spira/")) {
            let ahead = self.get("stray_branches", &format!("/repos/{slug}/compare/{base}...{b}"))?["ahead_by"].as_u64()?; // not a path: GitHub API route or JSON pointer
            if ahead > 0 && !open_heads.iter().any(|h| h == b) {
                out.push(b.to_string());
            }
        }
        Some(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn checkout(url: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new("forge-slug");
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/config"), format!("[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/a/b.git\n[remote \"origin\"]\n\turl = {url}\n")).unwrap();
        d
    }

    #[test_id(tpfwmnm)]
    #[test]
    fn every_remote_spelling_names_owner_and_repo() {
        for url in ["https://github.com/acme/widgets.git", "git@github.com:acme/widgets.git", "ssh://git@host:22/acme/widgets", "file:///x/acme/widgets/"] {
            assert_eq!(slug(checkout(url).path()).as_deref(), Some("acme/widgets"), "{url}");
        }
        let root = testkit::TempDir::new("forge-slug-none");
        assert_eq!(slug(&root.join("nonexistent")), None);
    }

    #[test_id(tk6lyw5)]
    #[test]
    fn a_remote_is_github_by_its_host_however_it_is_spelled() {
        for (url, github) in [
            ("https://github.com/acme/widgets.git", true),
                        ("https://USERINFO/github.com/acme/widgets", true),
            ("ssh://git@github.com:22/acme/widgets.git", true),
            ("git@github.com:acme/widgets.git", true),
            ("git@gitea.example:acme/widgets.git", false),
            ("https://gitea.example/github.com/widgets.git", false),
            ("https://notgithub.com/acme/widgets", false),
        ] {
            let d = testkit::TempDir::new("forge-github-slug");
            let url = url.replace("USERINFO/", &format!("{}{}", "x:y", "@"));
            let url = url.as_str();
            fs::create_dir_all(d.path().join(".git")).unwrap();
            fs::write(d.path().join(".git/config"), format!("[remote \"up\"]\n\turl = {url}\n")).unwrap();
            assert_eq!(github_slug(d.path(), "up").is_some(), github, "{url}");
            assert_eq!(github_slug(d.path(), "other"), None);
        }
    }

    #[test_id(tb9t82f)]
    #[test]
    fn without_an_origin_the_first_remote_is_used() {
        let d = testkit::TempDir::new("forge-slug-first");
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/config"), "[remote \"fork\"]\n\turl = git@h:o/r.git\n").unwrap();
        assert_eq!(slug(d.path()).as_deref(), Some("o/r"));
    }

    #[test_id(tr6eijy)]
    #[test]
    fn a_runner_is_known_by_the_first_run_of_two_digits_in_its_name_else_its_name() {
        assert_eq!((vmid_of("ci-vm-9001-a7").as_str(), vmid_of("r1").as_str(), vmid_of("").as_str()), ("9001", "r1", "?"));
    }

    /// sp-hw3qio: PR #989's head had also been published as PR #988, whose Gate runs failed at
    /// provision. The commit's check runs carry both PRs' runs, so pr-notify said FAIL RED for a
    /// PR whose own latest runs were green. A PR is judged by the latest run of each workflow on
    /// its own branch; a check that no workflow run owns (an app's check) still counts.
    #[test_id(t2mfgl7)]
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

    fn cancelled_then(later: Option<(&str, &str, &str)>) -> (&'static str, String) {
        let check = |name: &str, status: &str, conclusion: &str, suite: u64| serde_json::json!({"name": name, "status": status, "conclusion": conclusion, "check_suite": {"id": suite}});
        let run = |id: u64, branch: &str, suite: u64, status: &str, conclusion: &str| serde_json::json!({"id": id, "workflow_id": 7, "name": "Publish", "head_branch": branch, "check_suite_id": suite, "status": status, "conclusion": conclusion});
        let mut checks = vec![check("select", "completed", "success", 1), check("gate", "completed", "cancelled", 1), check("suites", "completed", "cancelled", 1)];
        let mut runs = vec![run(1, "spira/publish/a", 1, "completed", "cancelled")];
        if let Some((branch, status, conclusion)) = later {
            checks.push(check("select", "completed", "success", 2));
            checks.push(check("gate", if status == "completed" { "completed" } else { "queued" }, if status == "completed" { conclusion } else { "" }, 2));
            runs.push(run(2, branch, 2, status, conclusion));
        }
        classify(&current_checks(&checks, &runs, "spira/publish/a"))
    }

    /// sp-8z8d3t: a cancelled run is not a failure while a newer run of its workflow on the same
    /// head is still going or has passed, whichever branch name the newer run carries.
    #[test_id(teb6jdv)]
    #[test]
    fn a_cancelled_run_is_pending_while_a_newer_run_of_its_workflow_is_pending() {
        assert_eq!(cancelled_then(Some(("spira/publish/a", "queued", ""))).0, "pending");
        assert_eq!(cancelled_then(Some(("spira/publish/b", "queued", ""))).0, "pending");
        assert_eq!(cancelled_then(Some(("spira/publish/b", "in_progress", ""))).0, "pending");
        assert_eq!(cancelled_then(Some(("spira/publish/b", "completed", "success"))).0, "green");
    }

    #[test_id(tzqnds2)]
    #[test]
    fn a_cancelled_run_with_no_newer_run_is_red_and_a_real_failure_stays_red() {
        assert_eq!(cancelled_then(None), ("red", "gate,suites".into()));
        assert_eq!(cancelled_then(Some(("spira/publish/b", "completed", "cancelled"))).0, "red");
        assert_eq!(cancelled_then(Some(("spira/publish/b", "completed", "failure"))).0, "red");
    }

    /// Serves `answers` in order, one connection each; returns the api base and the requests seen.
    pub(crate) fn stub(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = vec![];
            for a in answers {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut c, &mut buf).unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                std::io::Write::write_all(&mut c, a.as_bytes()).unwrap();
            }
            seen
        });
        (api, h)
    }

    pub(crate) fn answer(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    pub(crate) fn forge_at(api: String) -> Forge {
        Forge { api, token: "t".into(), ..Forge::default() }
    }

    // covers: UC-operator-channel-38
    #[test_id(tj88cmg)]
    #[test]
    fn a_branch_is_a_stray_only_when_it_is_a_spira_branch_ahead_of_base_with_no_open_pr() {
        let branches = r#"[{"name":"main"},{"name":"spira/stray"},{"name":"spira/merged"},{"name":"spira/wip"},{"name":"feature/x"}]"#;
        let ahead = |n: u64| answer("200 OK", "", &format!(r#"{{"ahead_by":{n}}}"#));
        let (api, seen) = stub(vec![answer("200 OK", "", branches), ahead(2), ahead(0), ahead(1)]);
        let out = forge_at(api).stray_branches("o/r", "main", &["spira/wip".to_string()]);
        assert_eq!(out, Some(vec!["spira/stray".to_string()]));
        let seen = seen.join().unwrap();
        assert!(seen[1].contains("/compare/main...spira/stray"), "{}", seen[1]); // not a path: request URL path
        assert_eq!(seen.len(), 4, "only spira/* branches are compared");
    }

    fn runs_of(ids: &[u64]) -> String {
        let rows: Vec<String> = ids.iter().map(|i| format!(r#"{{"id":{i},"status":"in_progress"}}"#)).collect();
        format!(r#"{{"workflow_runs":[{}]}}"#, rows.join(","))
    }

    const QUEUED_OLD: &str = r#"{"name":"suites","status":"queued","created_at":"2026-01-01T00:00:00Z","labels":["self-hosted","ci-1-1"]}"#;
    const NO_RUNNERS: &str = r#"{"total_count":0,"runners":[]}"#;
    const LONG_AGO: u64 = 1_800_000_000;

    fn stranded_with(answers: Vec<String>) -> Result<Vec<String>, String> {
        let (api, seen) = stub(answers);
        let got = forge_at(api).stranded_runners("o/r", LONG_AGO, 300);
        seen.join().unwrap();
        got
    }

    #[test_id(tsr0d01)]
    #[test]
    fn stranded_runners_names_a_queued_job_no_runner_carries_when_nothing_is_in_progress() {
        let jobs = format!(r#"{{"total_count":1,"jobs":[{QUEUED_OLD}]}}"#);
        let got = stranded_with(vec![answer("200 OK", "", &runs_of(&[1])), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", &jobs)]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with(r#"STRANDED run=1 job="suites" label=ci-1-1 queued="#) && got[0].contains("runner=none"), "{got:?}");
    }

    #[test_id(tsr0d02)]
    #[test]
    fn stranded_runners_skips_a_run_with_a_job_in_progress_and_an_org_runner_that_is_online() {
        let busy = format!(r#"{{"total_count":2,"jobs":[{QUEUED_OLD},{{"name":"build","status":"in_progress"}}]}}"#);
        let got = stranded_with(vec![answer("200 OK", "", &runs_of(&[1])), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", &busy)]).unwrap();
        assert!(got.is_empty(), "{got:?}");
        let org = r#"{"total_count":1,"runners":[{"name":"e","status":"online","labels":[{"name":"ci-1-1"}]}]}"#;
        let jobs = format!(r#"{{"total_count":1,"jobs":[{QUEUED_OLD}]}}"#);
        let got = stranded_with(vec![answer("200 OK", "", &runs_of(&[1])), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", org), answer("200 OK", "", &jobs)]).unwrap();
        assert!(got.is_empty(), "{got:?}");
    }

    #[test_id(tsr0d04)]
    #[test]
    fn stranded_runners_still_names_a_job_whose_only_labelled_runner_is_offline_and_ignores_waiting_ones() {
        let off = r#"{"total_count":1,"runners":[{"name":"eph-1337","status":"offline","labels":[{"name":"ci-1-1"}]}]}"#;
        let jobs = format!(r#"{{"total_count":1,"jobs":[{QUEUED_OLD}]}}"#);
        let got = stranded_with(vec![answer("200 OK", "", &runs_of(&[1])), answer("200 OK", "", off), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", &jobs)]).unwrap();
        assert!(got.len() == 1 && got[0].contains("runner=eph-1337 vmid=1337"), "{got:?}");
        let waiting = format!(r#"{{"total_count":2,"jobs":[{QUEUED_OLD},{{"name":"gate","status":"waiting"}}]}}"#);
        let got = stranded_with(vec![answer("200 OK", "", &runs_of(&[1])), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", NO_RUNNERS), answer("200 OK", "", &waiting)]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test_id(tsr0d03)]
    #[test]
    fn stranded_runners_is_an_error_when_a_listing_fails_or_is_cut_short() {
        let fail = || answer("500 Internal Server Error", "", "");
        let ok = |b: &str| answer("200 OK", "", b);
        assert!(stranded_with(vec![fail()]).is_err(), "runs");
        assert!(stranded_with(vec![ok(&runs_of(&[1])), fail()]).is_err(), "repo runners");
        assert!(stranded_with(vec![ok(&runs_of(&[1])), ok(NO_RUNNERS), fail()]).is_err(), "org runners");
        assert!(stranded_with(vec![ok(&runs_of(&[1])), ok(NO_RUNNERS), ok(r#"{"total_count":150,"runners":[]}"#)]).is_err(), "cut short");
        assert!(stranded_with(vec![ok(&runs_of(&[1])), ok(NO_RUNNERS), ok(NO_RUNNERS), fail()]).is_err(), "jobs");
    }

    #[test_id(ttff9s6)]
    #[test]
    fn open_pr_reports_a_cancelled_run_a_queued_run_replaced_as_pending() {
        let pulls = r#"{"number":5,"state":"open","title":"publish","head":{"sha":"abc","ref":"spira/publish/a"}}"#;
        let checks = r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"cancelled","check_suite":{"id":1}},{"name":"gate","status":"queued","conclusion":null,"check_suite":{"id":2}}]}"#;
        let runs = r#"{"workflow_runs":[{"id":1,"workflow_id":7,"name":"Publish","head_branch":"spira/publish/a","check_suite_id":1,"status":"completed","conclusion":"cancelled"},{"id":2,"workflow_id":7,"name":"Publish","head_branch":"spira/publish/a","check_suite_id":2,"status":"queued","conclusion":null}]}"#;
        let (api, seen) = stub(vec![answer("200 OK", "", pulls), answer("200 OK", "", checks), answer("200 OK", "", runs)]);
        let (pr, _) = forge_at(api).open_pr("o/r", 5).unwrap().unwrap();
        seen.join().unwrap();
        assert_eq!((pr.status.as_str(), pr.fail.as_str()), ("pending", ""));
    }

    #[test_id(tsiqpkw)]
    #[test]
    fn open_pr_asks_for_the_one_pr_and_reads_no_checks_once_it_is_closed() {
        let (api, seen) = stub(vec![answer("200 OK", "", r#"{"number":12,"state":"closed","head":{"sha":"abc","ref":"b"}}"#)]);
        assert_eq!(forge_at(api).open_pr("o/r", 12), Some(None));
        let seen = seen.join().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("get /repos/o/r/pulls/12 "), "{}", seen[0]);
    }

    #[test_id(tt13d8x)]
    #[test]
    fn open_pr_is_unanswered_when_the_forge_does() {
        let (api, seen) = stub(vec![answer("500 Internal Server Error", "", "{}")]);
        assert_eq!(forge_at(api).open_pr("o/r", 12), None);
        seen.join().unwrap();
    }

    fn reply(status: u16, etag: Option<&str>, rate: Option<api_budget::Rate>, body: &str) -> Result<Reply, String> {
        Ok(Reply { status, etag: etag.map(str::to_string), rate, body: body.into() })
    }

    #[test_id(tf0sr1w)]
    #[test]
    fn an_unchanged_poll_is_revalidated_by_etag_and_answered_from_the_kept_body() {
        let sent = std::cell::RefCell::new(vec![]);
        let get = |kept, answer: Result<Reply, String>| {
            conditional_get("etag-api", "http://etag-api/p", "/p", "etag-caller", 1_000, kept, |tag| { // not a path: API route
                sent.borrow_mut().push(tag.map(str::to_string));
                answer
            })
        };
        let first = get(None, reply(200, Some("\"v1\""), None, r#"[{"n":1}]"#)).unwrap();
        assert_eq!((first.value.clone(), first.outcome), (serde_json::json!([{"n": 1}]), api_budget::FRESH));
        let kept = first.etag.map(|t| (t, first.value));
        assert_eq!(kept.as_ref().map(|k| k.0.as_str()), Some("\"v1\""));
        let second = get(kept, reply(304, None, None, "")).unwrap();
        assert_eq!((second.value, second.outcome), (serde_json::json!([{"n": 1}]), api_budget::CACHED));
        assert_eq!(*sent.borrow(), [None, Some("\"v1\"".to_string())]);
    }

    #[test_id(tvm350f)]
    #[test]
    fn a_spent_budget_the_forge_reports_stops_requests_until_its_reset() {
        let (api, reset) = ("spend-api", 1_600);
        let spent = api_budget::Rate { limit: 5000, remaining: 0, reset };
        let calls = std::cell::Cell::new(0);
        let get = |answer: Result<Reply, String>| {
            conditional_get(api, "http://spend-api/p", "/p", "spend-caller", 1_000, None, |_| { // not a path: API route
                calls.set(calls.get() + 1);
                answer
            })
        };
        assert_eq!(get(reply(200, None, Some(spent), "[]")).unwrap().outcome, api_budget::FRESH);
        assert_eq!(api_budget::rate(api), Some(spent));
        let (why, outcome) = get(Err("must not be sent".into())).err().expect("refused");
        assert_eq!(outcome, api_budget::REFUSED);
        assert!(why.contains(&api_budget::clock_of(reset)) && why.contains("spend-caller"), "{why}");
        assert_eq!(calls.get(), 1, "no second request while the budget is spent");
    }

    #[test_id(tzf4mf3)]
    #[test]
    fn a_spent_budget_is_reported_once_per_reset() {
        assert!(api_budget::first_report_of("once-api", 100));
        assert!(!api_budget::first_report_of("once-api", 100));
        assert!(api_budget::first_report_of("once-api", 200));
    }

    #[test_id(t0cdbzq)]
    #[test]
    fn stamps_convert_to_epoch_seconds() {
        assert_eq!(epoch("2026-10-28T05:45:43Z"), Some(1_793_166_343));
        assert_eq!(epoch("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(epoch("nonsense"), None);
    }
}
