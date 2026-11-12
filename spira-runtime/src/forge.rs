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

    fn get(&self, caller: &str, path: &str) -> Option<Value> {
        self.try_get(caller, path).ok()
    }

    /// One GET, conditional on the ETag of the last answer for the same URL: a 304 returns that
    /// answer and is not charged against the budget. A spent budget is reported once per reset
    /// and no request is made until it passes.
    fn try_get(&self, caller: &str, path: &str) -> Result<Value, String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Some(reset) = api_budget::exhausted_until(&self.api, now) {
            api_budget::count(caller, api_budget::REFUSED);
            let why = format!("the GitHub API budget is spent until {} (epoch {reset}); {caller} made no request", api_budget::clock_of(reset));
            if api_budget::first_report_of(&self.api, reset) {
                eprintln!("spira-runtime: {why}");
            }
            return Err(why);
        }
        let token = self.credential()?;
        let url = format!("{}{path}", self.api.trim_end_matches('/'));
        let mut req = ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build().get(&url).set("Accept", "application/vnd.github+json").set("User-Agent", "spira-runtime").set("Authorization", &format!("Bearer {token}"));
        if let (Some(tag), Some(_)) = (api_budget::etag(&url), api_budget::body_for(&url)) {
            req = req.set("If-None-Match", &tag);
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => {
                api_budget::count(caller, api_budget::FAILED);
                if let ureq::Error::Status(_, r) = &e {
                    self.note_rate(r);
                }
                return Err(e.to_string());
            }
        };
        self.note_rate(&resp);
        if resp.status() == 304 {
            if let Some(v) = api_budget::body_for(&url) {
                api_budget::count(caller, api_budget::CACHED);
                return Ok(v);
            }
        }
        let tag = resp.header("ETag").map(str::to_string);
        let mut body = String::new();
        resp.into_reader().take(16 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_str(&body).map_err(|e| format!("{path}: {e}"))?;
        api_budget::count(caller, api_budget::FRESH);
        if let Some(tag) = tag {
            api_budget::keep(&url, tag, value.clone());
        }
        Ok(value)
    }

    fn note_rate(&self, r: &ureq::Response) {
        let num = |h: &str| r.header(h).and_then(|v| v.trim().parse::<u64>().ok());
        if let (Some(limit), Some(remaining), Some(reset)) = (num("x-ratelimit-limit"), num("x-ratelimit-remaining"), num("x-ratelimit-reset")) {
            api_budget::note_rate(&self.api, api_budget::Rate { limit, remaining, reset });
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

    /// Every open PR with its checks classified; `None` when any call failed.
    pub fn open_prs(&self, slug: &str) -> Option<Vec<(OpenPr, String)>> {
        let prs = self.listed("open_prs", &format!("/repos/{slug}/pulls?state=open&{PAGE}"))?;
        prs.iter()
            .map(|p| {
                let sha = p["head"]["sha"].as_str()?;
                let checks = self.get("open_prs", &format!("/repos/{slug}/commits/{sha}/check-runs?{PAGE}"))?;
                let runs = self.get("open_prs", &format!("/repos/{slug}/actions/runs?head_sha={sha}&{PAGE}"))?;
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
        let p = self.get("final_state", &format!("/repos/{slug}/pulls/{n}"))?;
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
        let runs = self.get("deploy_run", &format!("/repos/{slug}/actions/workflows/deploy.yml/runs?head_sha={sha}&{PAGE}"))?;
        let r = runs["workflow_runs"].as_array()?.iter().find(|r| r["status"] == "completed")?;
        let secs = epoch(r["updated_at"].as_str()?).zip(epoch(r["created_at"].as_str()?)).map_or(0, |(u, c)| u.saturating_sub(c));
        Some(Run { conclusion: r["conclusion"].as_str().unwrap_or("unknown").to_string(), secs, url: text(r, "html_url") })
    }

    /// One `STRANDED run=<id> job="<name>" label=<labels> queued=<n>s runner=<name> vmid=<id>` line per
    /// queued job older than `min_age` whose per-run label no online runner carries; the lines
    /// `forge stranded-runners` prints. A call that fails reads as nothing stranded, as it does there.
    pub fn stranded_runners(&self, slug: &str, now: u64, min_age: u64) -> Vec<String> {
        const GENERIC: [&str; 9] = ["self-hosted", "linux", "macos", "windows", "x64", "arm64", "x86_64", "arm", "ubuntu-latest"];
        let Some(runs) = self.get("stranded_runners", &format!("/repos/{slug}/actions/runs?per_page=50")) else { return vec![] };
        let ids: Vec<String> = runs["workflow_runs"].as_array().map(|a| a.iter().filter(|r| r["status"] != "completed").filter_map(|r| r.get("id").map(|i| i.to_string())).collect()).unwrap_or_default();
        if ids.is_empty() {
            return vec![];
        }
        let runners: Vec<Value> = self.get("stranded_runners", &format!("/repos/{slug}/actions/runners?per_page=100")).and_then(|v| v["runners"].as_array().cloned()).unwrap_or_default();
        let mut lines = vec![];
        for id in ids {
            let Some(jobs) = self.get("stranded_runners", &format!("/repos/{slug}/actions/runs/{id}/jobs")) else { continue };
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
        let branches = self.listed("stray_branches", &format!("/repos/{slug}/branches?{PAGE}"))?;
        let mut out = vec![];
        for b in branches.iter().filter_map(|b| b["name"].as_str()).filter(|b| b.starts_with("spira/")) {
            let ahead = self.get("stray_branches", &format!("/repos/{slug}/compare/{base}...{b}"))?["ahead_by"].as_u64()?;
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

    /// Serves `answers` in order, one connection each; returns the api base and the requests seen.
    fn stub(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
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

    fn answer(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn forge_at(api: String) -> Forge {
        Forge { api, token: "t".into(), ..Forge::default() }
    }

    #[test]
    fn an_unchanged_poll_is_revalidated_by_etag_and_answered_from_the_kept_body() {
        let (api, seen) = stub(vec![answer("200 OK", "ETag: \"v1\"\r\n", "[]"), answer("304 Not Modified", "", "")]);
        let forge = forge_at(api);
        assert_eq!(forge.listed("etag-caller", "/repos/a/b/pulls"), Some(vec![]));
        assert_eq!(forge.listed("etag-caller", "/repos/a/b/pulls"), Some(vec![]));
        let seen = seen.join().unwrap();
        assert!(!seen[0].contains("if-none-match"));
        assert!(seen[1].contains("if-none-match: \"v1\""), "{}", seen[1]);
        assert_eq!((api_budget::calls("etag-caller", api_budget::FRESH), api_budget::calls("etag-caller", api_budget::CACHED)), (1, 1));
    }

    #[test]
    fn the_budget_the_forge_reports_is_kept_and_a_spent_one_stops_requests_until_its_reset() {
        let reset = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
        let spent = format!("X-RateLimit-Limit: 5000\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {reset}\r\n");
        let (api, seen) = stub(vec![answer("200 OK", &spent, "[]")]);
        let forge = forge_at(api.clone());
        assert!(forge.try_get("spend-caller", "/repos/a/b/pulls").is_ok());
        assert_eq!(api_budget::rate(&api), Some(api_budget::Rate { limit: 5000, remaining: 0, reset }));
        let why = forge.try_get("spend-caller", "/repos/a/b/pulls").unwrap_err();
        assert!(why.contains(&api_budget::clock_of(reset)) && why.contains("spend-caller"), "{why}");
        assert_eq!(seen.join().unwrap().len(), 1, "no second request while the budget is spent");
        assert_eq!(api_budget::calls("spend-caller", api_budget::REFUSED), 1);
    }

    #[test]
    fn a_spent_budget_is_reported_once_per_reset() {
        assert!(api_budget::first_report_of("once-api", 100));
        assert!(!api_budget::first_report_of("once-api", 100));
        assert!(api_budget::first_report_of("once-api", 200));
    }

    #[test]
    fn stamps_convert_to_epoch_seconds() {
        assert_eq!(epoch("2026-10-28T05:45:43Z"), Some(1_793_166_343));
        assert_eq!(epoch("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(epoch("nonsense"), None);
    }
}
