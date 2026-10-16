//! `/rounds` and `/rounds/<repo>/<batch>`: the forge repos' batch PRs. Membership and outcome
//! come from the batcher's records under `<run>/queue/<repo>/`; the PR's CI run, jobs, failing
//! step and first error lines come from `broker read run-jobs`, held per batch for
//! [`CI_TTL`]. The list never calls the broker; a detail page whose fetch fails shows the last
//! result marked stale, or says the detail is unavailable.

use crate::shell::shell;
use crate::stuck::{dur, esc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const CI_TTL: Duration = Duration::from_secs(60);
const CLOSED_SHOWN: usize = 5;

#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub repo: String,
    pub key: String,
    pub branch: String,
    pub pr: String,
    pub state: String,
    pub members: Vec<(String, String)>,
    pub opened: i64,
    pub ci: String,
    pub run: String,
    pub step: String,
    pub wall: i64,
    pub local_verdict: String,
    pub local_detail: String,
    pub reason: String,
}

fn kv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn from_record(repo: &str, state: &str, r: &BTreeMap<String, String>) -> Option<Batch> {
    let get = |k: &str| r.get(k).cloned().unwrap_or_default();
    let branch = r.get("batch_id").or_else(|| r.get("branch"))?.clone();
    let pr = get("pr");
    if pr.is_empty() {
        return None;
    }
    Some(Batch {
        repo: repo.to_string(),
        key: branch.rsplit('/').next().unwrap_or(&branch).to_string(),
        branch,
        pr,
        state: state.to_string(),
        members: get("members")
            .split_whitespace()
            .map(|m| m.split_once(':').map_or((m.to_string(), String::new()), |(b, s)| (b.to_string(), s.to_string())))
            .collect(),
        opened: get("opened").parse().unwrap_or(0),
        reason: get("reason"),
        ..Batch::default()
    })
}

/// Every forge batch on record, per repo: the open one first, then the newest closed ones.
pub fn read_all(queue: &Path) -> Vec<Batch> {
    let mut repos: Vec<_> = std::fs::read_dir(queue).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    repos.sort();
    let mut out = Vec::new();
    for dir in repos {
        let repo = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if let Some(mut b) = from_record(&repo, "open", &kv(&dir.join("open"))) {
            let fs = kv(&dir.join("forge-state"));
            if fs.get("batch_id").is_some_and(|id| *id == b.branch) {
                let get = |k: &str| fs.get(k).cloned().unwrap_or_default();
                b.ci = get("ci");
                b.run = get("run");
                b.step = get("step");
                b.wall = get("wall").parse().unwrap_or(0);
                if get("pr_state") == "merged" {
                    b.state = "merged".into();
                }
            }
            let mut lv = kv(&dir.join("local-verdict"));
            b.local_verdict = lv.remove("verdict").unwrap_or_default();
            b.local_detail = lv.remove("detail").unwrap_or_default();
            out.push(b);
        }
        let mut closed: Vec<Batch> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("closed-pr")).map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default()
            .iter()
            .filter_map(|p| from_record(&repo, "closed", &kv(p)))
            .collect();
        closed.sort_by(|a, b| b.opened.cmp(&a.opened));
        out.extend(closed);
    }
    out
}

fn badge(b: &Batch) -> (&'static str, String) {
    match (b.state.as_str(), b.ci.as_str()) {
        ("closed", _) => ("dim", "closed".into()),
        ("merged", _) => ("green", "merged".into()),
        (_, "red") => ("red", "CI red".into()),
        (_, "green") => ("green", "CI green".into()),
        (_, "") => ("dim", "open".into()),
        (_, c) => ("amber", format!("CI {c}")),
    }
}

const CSS: &str = "<style>.rc{display:block;padding:.6rem .8rem;border-bottom:1px solid #1d232b}.rc .head{display:flex;gap:.5rem;align-items:baseline;flex-wrap:wrap}\
.red{color:#ff6b6d}.green{color:#4cc38a}.amber{color:#e0a43a}h2{font-size:.85rem;margin:.8rem .8rem .2rem;color:#7d8896;font-weight:600}\
pre{margin:.3rem 0;padding:.5rem .6rem;background:#161b22;border-radius:4px;white-space:pre-wrap;word-break:break-word;font-size:.8rem}\
.card{padding:.6rem .8rem;border-bottom:1px solid #1d232b}.card b{display:inline-block;min-width:5rem}\
@media(min-width:640px){.rc,.card{max-width:60rem}}</style>";

pub fn render_list(now: i64, batches: &[Batch]) -> String {
    let mut b = format!("{CSS}<h2>Forge batches</h2>");
    if batches.is_empty() {
        b.push_str("<p class=dim style=\"padding:.4rem .8rem\">no forge batch on record</p>");
    }
    let mut repo = "";
    let mut shown_closed = 0;
    for x in batches {
        if x.repo != repo {
            repo = &x.repo;
            shown_closed = 0;
            b.push_str(&format!("<h2>{}</h2>", esc(repo)));
        }
        if x.state == "closed" {
            shown_closed += 1;
            if shown_closed > CLOSED_SHOWN {
                continue;
            }
        }
        let (cls, label) = badge(x);
        let step = if x.step.is_empty() { String::new() } else { format!(" <span class=dim>{}</span>", esc(&x.step)) };
        b.push_str(&format!(
            "<a class=rc href=\"/rounds/{r}/{k}\"><div class=head><span class=id>PR #{pr}</span><span class=\"{cls}\">{label}</span><span class=dim>{n} members · {age}</span></div><div class=dim>{k}{step}</div></a>",
            r = esc(&x.repo),
            k = esc(&x.key),
            pr = esc(&x.pr),
            n = x.members.len(),
            age = if x.opened > 0 { format!("{} ago", dur(now - x.opened)) } else { "—".into() },
        ));
    }
    b
}

#[derive(Debug, Clone)]
pub struct Ci {
    pub json: Value,
    pub taken: Instant,
}

pub enum CiView {
    Fresh(Ci),
    Stale(Ci, String),
    Refreshing(Ci),
    Loading,
    Unavailable(String),
    NoRun,
}

/// How long a page waits on the broker before answering with what it holds; the fetch keeps going behind it.
pub const CI_WAIT: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Entry {
    ci: Option<Arc<Ci>>,
    err: Option<String>,
    fetching: bool,
}

#[derive(Default, Clone)]
pub struct Cache(Arc<Mutex<HashMap<(String, String), Entry>>>);

impl Cache {
    pub async fn ci(&self, b: &Batch, fetch: impl std::future::Future<Output = Result<Value, String>> + Send + 'static) -> CiView {
        if b.run.is_empty() {
            return CiView::NoRun;
        }
        let key = (b.repo.clone(), b.key.clone());
        let rx = {
            let mut map = self.0.lock().await;
            let e = map.entry(key.clone()).or_default();
            if let Some(c) = e.ci.as_ref().filter(|c| c.taken.elapsed() < CI_TTL) {
                return CiView::Fresh((**c).clone());
            }
            if e.fetching {
                None
            } else {
                e.fetching = true;
                let (tx, rx) = tokio::sync::oneshot::channel();
                let cache = self.0.clone();
                let key = key.clone();
                tokio::spawn(async move {
                    let res = fetch.await;
                    let mut map = cache.lock().await;
                    let e = map.entry(key).or_default();
                    e.fetching = false;
                    match &res {
                        Ok(json) => {
                            e.ci = Some(Arc::new(Ci { json: json.clone(), taken: Instant::now() }));
                            e.err = None;
                        }
                        Err(err) => e.err = Some(err.clone()),
                    }
                    let _ = tx.send(res);
                });
                Some(rx)
            }
        };
        if let Some(rx) = rx {
            if let Ok(Ok(res)) = tokio::time::timeout(CI_WAIT, rx).await {
                return match res {
                    Ok(json) => CiView::Fresh(Ci { json, taken: Instant::now() }),
                    Err(e) => self.fallback(&key, Some(e)).await,
                };
            }
        }
        self.fallback(&key, None).await
    }

    async fn fallback(&self, key: &(String, String), err: Option<String>) -> CiView {
        let map = self.0.lock().await;
        let e = map.get(key);
        let held = e.and_then(|e| e.ci.clone());
        let err = err.or_else(|| e.and_then(|e| e.err.clone()));
        match (held, err) {
            (Some(c), Some(err)) => CiView::Stale((*c).clone(), err),
            (Some(c), None) => CiView::Refreshing((*c).clone()),
            (None, Some(err)) => CiView::Unavailable(err),
            (None, None) => CiView::Loading,
        }
    }
}

/// The PR's link, derived from the run's: `…/o/r/actions/runs/N` → `…/o/r/pull/<pr>`.
fn pr_url(run_url: &str, pr: &str) -> Option<String> {
    run_url.split_once("/actions/").map(|(base, _)| format!("{base}/pull/{pr}"))
}

pub fn render_detail(now: i64, b: &Batch, ci: &CiView, forge_base: &str) -> String {
    let (cls, label) = badge(b);
    let sv = |v: &Value| v.as_str().unwrap_or("").to_string();
    let json = match ci {
        CiView::Fresh(c) | CiView::Stale(c, _) | CiView::Refreshing(c) => Some(&c.json),
        _ => None,
    };
    let run_url = json.map(|j| sv(&j["run"]["url"])).unwrap_or_default();
    let pr_href = pr_url(&run_url, &b.pr).or_else(|| (!forge_base.is_empty()).then(|| format!("{forge_base}/{}/pull/{}", b.repo, b.pr)));
    let pr = match pr_href {
        Some(u) => format!("<a href=\"{}\">PR #{}</a>", esc(&u), esc(&b.pr)),
        None => format!("PR #{}", esc(&b.pr)),
    };
    let mut h = format!("{CSS}<header><h1>{repo} · {pr} <span class=\"{cls}\">{label}</span> <span class=dim>— <a href=/rounds>rounds</a></span></h1></header>", repo = esc(&b.repo));
    h.push_str(&format!("<div class=card><b>batch</b>{} <span class=dim>opened {} ago</span></div>", esc(&b.branch), dur(now - b.opened)));
    if !b.local_verdict.is_empty() {
        h.push_str(&format!("<div class=card><b>local check</b>{}{}</div>", esc(&b.local_verdict), if b.local_detail.is_empty() { String::new() } else { format!(" — {}", esc(&b.local_detail)) }));
    }
    if !b.reason.is_empty() {
        h.push_str(&format!("<div class=card><b>outcome</b>closed without merging — {}</div>", esc(&b.reason)));
    }
    let outcome = if b.state == "closed" { "dropped with the batch" } else if b.state == "merged" { "landed" } else { "in the batch" };
    h.push_str(&format!("<h2>Members ({})</h2>", b.members.len()));
    for (bead, sha) in &b.members {
        h.push_str(&format!("<a class=rc href=\"/stuck/{0}\"><span class=id>{0}</span> <span class=dim>{1} · {outcome}</span></a>", esc(bead), esc(sha.get(..9).unwrap_or(sha))));
    }
    h.push_str("<h2>CI</h2>");
    match ci {
        CiView::NoRun => h.push_str("<div class=card><span class=dim>no CI run recorded for this batch — the verdict above is the local check's</span></div>"),
        CiView::Loading => h.push_str("<div class=card><span class=amber>CI detail loading — reload in a few seconds</span></div>"),
        CiView::Unavailable(e) => h.push_str(&format!("<div class=card><span class=red>CI detail unavailable:</span> {}</div>", esc(e))),
        CiView::Fresh(c) | CiView::Stale(c, _) | CiView::Refreshing(c) => {
            if let CiView::Refreshing(c) = ci {
                h.push_str(&format!("<div class=card><span class=amber>fetched {} ago, refreshing</span></div>", dur(c.taken.elapsed().as_secs() as i64)));
            }
            if let CiView::Stale(c, e) = ci {
                h.push_str(&format!("<div class=card><span class=amber>stale — fetched {} ago, refresh failed:</span> {}</div>", dur(c.taken.elapsed().as_secs() as i64), esc(e)));
            }
            let run = &c.json["run"];
            h.push_str(&format!(
                "<div class=card><b>run</b><a href=\"{}\">#{}</a> {} {} <span class=dim>{}</span></div>",
                esc(&sv(&run["url"])),
                esc(&b.run),
                esc(&sv(&run["status"])),
                esc(&sv(&run["conclusion"])),
                esc(&b.step),
            ));
            for j in c.json["jobs"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let concl = sv(&j["conclusion"]);
                let c2 = match concl.as_str() { "success" => "green", "failure" => "red", _ => "dim" };
                let status = if concl.is_empty() { sv(&j["status"]) } else { concl };
                h.push_str(&format!("<div class=card><a href=\"{}\"><b>{}</b></a> <span class=\"{c2}\">{}</span>", esc(&sv(&j["url"])), esc(&sv(&j["name"])), esc(&status)));
                if let Some(step) = j["failed_step"].as_str() {
                    h.push_str(&format!("<div class=red>failed step: {}</div>", esc(step)));
                }
                let lines: Vec<String> = j["error_lines"].as_array().map(|a| a.iter().map(sv).collect()).unwrap_or_default();
                if !lines.is_empty() {
                    h.push_str(&format!("<pre>{}</pre>", esc(&lines.join("\n"))));
                }
                h.push_str("</div>");
            }
        }
    }
    shell("rounds", &format!("{} PR {}", b.repo, b.pr), 60, &format!("<main>{h}</main>"))
}
