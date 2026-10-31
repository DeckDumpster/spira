//! The forge over HTTP. Each question is one REST call with a short timeout; a call that fails is
//! `None`, never an empty answer, so a forge that is down cannot look like a repository with
//! nothing open.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::watchers::pr_notify::{classify, Final, OpenPr, Run};
use crate::watchers::repo_events;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const PAGE_SIZE: usize = 100;
const PAGE: &str = "per_page=100";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Forge {
    pub api: String,
    pub token: String,
}

impl fmt::Debug for Forge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Forge").field("api", &self.api).field("token", &(!self.token.is_empty())).finish()
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

impl Forge {
    fn get(&self, path: &str) -> Option<Value> {
        self.try_get(path).ok()
    }

    fn try_get(&self, path: &str) -> Result<Value, String> {
        let mut req = ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build().get(&format!("{}{path}", self.api.trim_end_matches('/'))).set("Accept", "application/vnd.github+json").set("User-Agent", "spira-runtime");
        if !self.token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.token));
        }
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
                let runs = self.get(&format!("/repos/{slug}/commits/{}/check-runs?{PAGE}", p["head"]["sha"].as_str()?))?;
                let checks: Vec<(String, String, String)> = runs["check_runs"].as_array()?.iter().map(|c| (text(c, "name"), text(c, "status").to_uppercase(), text(c, "conclusion").to_uppercase())).collect();
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
    fn stamps_convert_to_epoch_seconds() {
        assert_eq!(epoch("2026-10-28T05:45:43Z"), Some(1_793_166_343));
        assert_eq!(epoch("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(epoch("nonsense"), None);
    }
}
