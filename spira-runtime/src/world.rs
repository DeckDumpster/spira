//! Where the watchers' snapshots come from: files under the run directory and the lifecycle
//! store through spira-lc's socket. No process is launched here; the wiki's git is `wiki_git`.

use std::sync::{Arc, Mutex};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use runtime::ports::{Proc, SysProc};
use runtime::socket::lc_call;
use spira_config::nonwork::{status_args, Kind, Which};
use serde_json::Value;

use crate::forge::{self, slug, Forge};
use crate::watchers::cr_cadence;
use crate::watchers::pr_notify::{self, DeployRun, RepoObs};
use crate::watchers::asks;
use crate::watchers::design_asks::{self, Page};
use crate::hunk;
use crate::watchers::view;
use crate::wiki_git::{git, git_ok_with, exporter_run, overrides_apply, tmux};
use crate::watchers::brain_dirty;
use crate::watchers::claim_count;
use crate::watchers::supersede_duty::{self, Act, Held, Reading, Verb};
use crate::watchers::bead_sweep::{self, Bead, Repair, Repaired, Reservation, Row, Stop};
use crate::watchers::publish_backlog::{self, Read};
use crate::watchers::reconciler_flow::{FlowSnapshot, Params, Slots, Stage, Statuses};
use crate::watchers::halt::Halt;
use crate::watchers::round_duty::{RoundState, Snapshot};

const INBOX_WATCHER: &str = "concierge-inbox";
const STORE_ATTEMPTS: u32 = 3;
/// Ids per `plan items` call when the flow pass reads the live beads' type and labels.
const FLOW_CONTENT_CHUNK: usize = 200;
const REFRESH_FETCH: std::time::Duration = std::time::Duration::from_secs(20);
const TMUX_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);
const STORE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    pub run_dir: PathBuf,
    /// `SPIRA_CR_TZ` and `SPIRA_CR_OPEN_HOUR` as configured; each is empty while unset.
    pub cr_tz: String,
    pub cr_open_hour: String,
    pub queue_dir: PathBuf,
    pub repo_map: PathBuf,
    pub home_repo: String,
    pub inbox: PathBuf,
    /// `SPIRA_CONCIERGE_PUSH_WAKE`: empty, no inbox line wakes the session.
    pub push_wake: String,
    /// `SPIRA_CONCIERGE_PUSH_PATTERN`: an inbox line that matches it also wakes the session.
    pub push_pattern: String,
    /// The `inbox-triage` program whose `flush` verb delivers a pushed line; empty, the release's.
    pub inbox_flusher: PathBuf,
    /// Where an inbox append is announced as `concierge/inbox`.
    pub inbox_bus: inbox_rules::Bus,
    pub lc_socket: PathBuf,
    pub proc_dir: PathBuf,
    pub mail: PathBuf,
    pub ask_label: String,
    pub wiki: PathBuf,
    pub hunk_dir: PathBuf,
    /// `SPIRA_OVERRIDES`: where operator override specs are declared.
    pub overrides_dir: PathBuf,
    /// The status line's records in the user runtime directory; empty without one.
    pub ctx_status_dir: PathBuf,
    /// Where the cockpit ops document is kept (`spira_config::cockpit_view_dir`); empty without a runtime directory.
    pub cockpit_view_dir: PathBuf,
    pub cockpit_state: PathBuf,
    pub cockpit_session: String,
    pub tmux: PathBuf,
    pub metrics_endpoint: Option<String>,
    pub scope_label: String,
    pub desired_dir: PathBuf,
    pub export_bin: String,
    /// The batch machine is served from the control store, so a round the store does not hold is stale.
    pub batches_on_store: bool,
    /// When set, every store call of an observe shares this bound: (started, total).
    pub observe_bound: Option<(std::time::Instant, std::time::Duration)>,
    /// Git program for the landing-ref and log reads; `None` is `git` on `PATH`.
    pub git_bin: Option<PathBuf>,
}

#[derive(Default)]
pub struct PrTarget {
    pub name: String,
    /// `None`: a `queue.local` repo whose origin is not on GitHub, so no forge call is made for it.
    pub slug: Option<String>,
    /// The origin URL, without any login, when there is no slug.
    pub origin: String,
    pub path: PathBuf,
    pub land: String,
    pub base: String,
    /// A `queue.local` repo's publish ref as the checkout holds it.
    pub tip: Option<String>,
    /// The harness is live by release activation, so it gets no deploy verdict.
    pub home: bool,
}

/// Reads `<sha>:<file>` from a checkout; `None` when it is not there to read.
pub type Blob<'a> = dyn Fn(&Path, &str) -> Option<String> + 'a;

const DEPLOY_DECLARATION: &str = ".spira/deploy.toml";

fn declared_deploy(repo: &Path, sha: &str, blob: &Blob) -> Option<String> {
    let table: toml::Table = blob(repo, &format!("{sha}:{DEPLOY_DECLARATION}"))?.parse().ok()?;
    table.get("after_landing")?.as_str().map(str::to_string)
}

/// The mapped checkouts the forge can answer for, and every `queue.local` one whether or not it can.
fn pr_targets(rows: Vec<MapRow>, home_repo: &str, tip_of: &dyn Fn(&MapRow) -> Option<String>) -> Vec<PrTarget> {
    rows.into_iter()
        .filter(|r| Path::new(&r.path).join(".git").is_dir())
        .filter_map(|r| {
            let path = PathBuf::from(&r.path);
            let (local, home) = (r.land == "queue.local", r.name == home_repo);
            let (slug, origin) = match local.then(|| forge::origin_url(&path)).flatten() {
                Some(url) => match forge::github_slug_of(&url) {
                    Some(slug) => (Some(slug), String::new()),
                    None => (None, forge::without_login(&url)),
                },
                None => (slug(&path), String::new()),
            };
            if slug.is_none() && (home || origin.is_empty()) {
                return None;
            }
            let tip = (local && !home).then(|| tip_of(&r)).flatten();
            Some(PrTarget { name: r.name, slug, origin, path, land: r.land, base: r.base, tip, home })
        })
        .collect()
}

/// The publish ref's commit as the checkout holds it; nothing is fetched.
fn forge_tip(reg: &spira_config::repos::Registry, git: &Path, r: &MapRow) -> Option<String> {
    let (remote, branch) = spira_config::repos::publish_forge(reg, &r.name, &BTreeMap::new())?;
    crate::unpublished::rev(git, Path::new(&r.path), &format!("refs/remotes/{remote}/{branch}")).ok()
}

/// A repo absent from the map has no recorded PR; one mapped to `Err` could not be read.
pub type RecordedPrs = BTreeMap<String, Result<Vec<u64>, String>>;

fn pr_number(v: &str) -> Option<u64> {
    v.trim().trim_start_matches('#').parse().ok()
}

/// The PR the repo's publish record names; an unreadable record is an `Err`, an absent one `None`.
fn publish_pr(queue_dir: &Path, repo: &str) -> Result<Option<u64>, String> {
    queue::records::read_kv(&queue_dir.join(repo).join("publish")).map(|kv| kv.and_then(|kv| kv.get("pr").and_then(pr_number)))
}

/// The forge's word on each PR in `records` or already tracked in `carry`, asked by number.
/// A repo's open set is `None` when its records or any answer is missing; strays are looked for
/// only when a tracked PR left the open set.
pub fn pr_snapshot_from(records: &RecordedPrs, targets: &[PrTarget], forge: &Forge, carry: &pr_notify::Carry, blob: &Blob) -> Result<String, String> {
    let mut repos = vec![];
    for t in targets {
        let prev = carry.repos.get(&t.name).cloned().unwrap_or_default();
        let mut obs = RepoObs { tip: t.tip.clone(), home: t.home, ..RepoObs::default() };
        match &t.slug {
            Some(slug) => {
                obs.slug = slug.clone();
                observe_prs(records, t, slug, forge, &prev, &mut obs);
            }
            None => obs.origin = Some(t.origin.clone()),
        }
        if !t.home {
            let moved = t.tip.as_deref().filter(|tip| prev.moved_to(tip));
            for sha in prev.deploys.iter().map(|d| d.sha.as_str()).chain(moved) {
                if let Some(slug) = &t.slug {
                    let Ok(run) = forge.deploy_run(slug, sha) else { continue };
                    let completed = matches!(run, DeployRun::Completed(_));
                    obs.runs.insert(sha.to_string(), run);
                    if completed {
                        continue;
                    }
                }
                obs.declared.extend(declared_deploy(&t.path, sha, blob).map(|text| (sha.to_string(), text)));
            }
        }
        repos.push((t.name.clone(), obs));
    }
    forge.report_spend();
    serde_json::to_string(&pr_notify::Snapshot { repos }).map_err(|e| e.to_string())
}

fn observe_prs(records: &RecordedPrs, t: &PrTarget, slug: &str, forge: &Forge, prev: &pr_notify::RepoCarry, obs: &mut RepoObs) {
    let Ok(recorded) = records.get(&t.name).cloned().unwrap_or(Ok(vec![])) else { return };
    let asked: BTreeSet<u64> = recorded.into_iter().chain(prev.prs.keys().copied()).collect();
    let mut open = vec![];
    let mut left = vec![];
    let mut unanswered = false;
    for n in asked {
        match forge.open_pr(slug, n) {
            Some(Some(pr)) => open.push(pr),
            Some(None) => left.push(n),
            None => unanswered = true,
        }
    }
    if unanswered {
        return;
    }
    for n in left.iter().filter(|n| prev.prs.contains_key(n)) {
        obs.finals.extend(forge.final_state(slug, *n).map(|f| (*n, f)));
    }
    if t.land == "pr" && left.iter().any(|n| prev.prs.contains_key(n)) {
        let heads: Vec<String> = open.iter().map(|(_, h)| h.clone()).collect();
        let base = t.base.strip_prefix("origin/").unwrap_or(&t.base);
        obs.strays = forge.stray_branches(slug, if base.is_empty() { "main" } else { base }, &heads);
    }
    obs.open = Some(open.into_iter().map(|(p, _)| p).collect());
}

/// Each queue.local checkout's forge tip, fetched in parallel and best-effort: the measurement
/// that follows reads local refs and answers from the cached tip when this fails.
fn refresh_forge_tip(proc: &dyn Proc, git: &Path, reg: spira_config::repos::Registry, r: &MapRow) {
    use crate::tasks::a::queue_step::edge::git_through;
    let reg = &if git == Path::new("git") { reg } else { reg.with_git(git.to_path_buf()) };
    if let Some((remote, branch)) = spira_config::repos::publish_forge(reg, &r.name, &BTreeMap::new()) {
        if spira_config::repos::landref(reg, &r.name).is_some_and(|base| crate::unpublished::published(Path::new(&r.path), &base, &remote, &branch)) {
            return;
        }
        let _ = git_through(proc, git, Path::new(&r.path), &["fetch", "--quiet", &remote, &branch], REFRESH_FETCH);
    }
}

fn forge_refresh(proc: &dyn Proc, reg: Option<spira_config::repos::Registry>) -> impl Fn(&Path, &MapRow) + Sync + '_ {
    move |git, r| reg.iter().for_each(|reg| refresh_forge_tip(proc, git, reg.clone(), r))
}

/// Refreshes every queue.local checkout at once, waits for all of them, then measures each.
/// Nothing here reads the refresh outcome.
fn publish_backlog_snapshot_from(rows: &[MapRow], refresh: &(dyn Fn(&MapRow) + Sync), measure: &dyn Fn(&str) -> Result<Option<Read>, String>) -> Result<String, String> {
    let local = |r: &MapRow| !r.path.is_empty() && Path::new(&r.path).join(".git").is_dir() && r.land == "queue.local";
    let tally = spira_config::bounded::tally();
    std::thread::scope(|s| {
        let handles: Vec<_> = rows.iter().filter(|r| local(r)).map(|r| s.spawn({ let tally = tally.clone(); move || spira_config::bounded::in_tally(tally, || refresh(r)) })).collect();
        for h in handles {
            let _ = h.join();
        }
    });
    let mut repos = vec![];
    for r in rows {
        if r.path.is_empty() {
            repos.push((r.name.clone(), Read::Blind("the repo map has no checkout for it".into())));
            continue;
        }
        if !local(r) {
            continue;
        }
        match measure(&r.name) {
            Ok(None) => {}
            Ok(Some(read)) => repos.push((r.name.clone(), read)),
            Err(why) => repos.push((r.name.clone(), Read::Blind(format!("the unpublished range could not be computed: {why}")))),
        }
    }
    serde_json::to_string(&publish_backlog::Snapshot { repos }).map_err(|e| e.to_string())
}

struct MapRow {
    name: String,
    path: String,
    land: String,
    base: String,
}

fn map_rows(text: &str) -> Vec<MapRow> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').map(str::trim).collect();
            (f.len() >= 3 && !f[0].is_empty()).then(|| MapRow { name: f[0].into(), path: f[1].into(), land: f[2].into(), base: f.get(3).copied().unwrap_or("").into() })
        })
        .collect()
}

/// The work plane's stamp, if the plane is halted. Its age is its mtime; the present stands in
/// when the filesystem will not say.
pub fn read_halt(run_dir: &Path) -> Option<Halt> {
    let stamp = run_dir.join("world.halted");
    let meta = fs::metadata(&stamp).ok()?;
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let since = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or_else(now, |d| d.as_secs());
    let text = fs::read_to_string(&stamp).unwrap_or_default();
    let why = text.lines().find_map(|l| l.strip_prefix("why: ")).unwrap_or("unstated").to_string();
    Some(Halt { since, why })
}

fn lane_stopped(run_dir: &Path, repo: &str, lane: &str) -> bool {
    let Ok(text) = fs::read_to_string(run_dir.join("scoped-stops").join(format!("repo-{repo}.json"))) else { return false };
    serde_json::from_str::<Value>(&text).ok().and_then(|v| v["lanes"].as_array().map(|a| a.iter().any(|l| l == lane))).unwrap_or(false)
}

/// A top-level frontmatter value, unquoted; empty when the page has none.
fn frontmatter(text: &str, key: &str) -> String {
    let mut lines = text.split('\n');
    if lines.next() != Some("---") {
        return String::new();
    }
    for l in lines.take_while(|l| *l != "---") {
        if let Some(v) = l.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
            return v.trim_start_matches([' ', '\t', '"', '\'']).trim_end_matches([' ', '\t', '"', '\'', '\r']).to_string();
        }
    }
    String::new()
}

fn artifact_urls(text: &str) -> Vec<String> {
    const PREFIX: &str = "https://claude.ai/artifact/";
    text.match_indices(PREFIX)
        .map(|(i, _)| {
            let tail = &text[i + PREFIX.len()..];
            let id: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
            format!("{PREFIX}{id}")
        })
        .filter(|u| u.len() > PREFIX.len())
        .collect()
}

/// The newest Hold event of a `spira-lc history` answer: its seq and its whitespace-folded
/// detail, capped.
fn last_hold(history_json: &str) -> Option<(String, String)> {
    let rows: Vec<Value> = serde_json::from_str(history_json).ok()?;
    let r = rows.iter().rfind(|r| r["event"] == "Hold")?;
    let seq = r["seq"].as_str().map(str::to_string).unwrap_or_else(|| r["seq"].to_string());
    let detail = r["evidence"]["Hold"]["detail"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    Some((seq, detail.chars().take(400).collect()))
}

/// The `items` of the `{watermark, items}` object a `spira-lc plan items` door printed, skipping
/// any notice lines the store printed before it.
fn plan_items(out: &str) -> Option<Vec<Value>> {
    out.char_indices()
        .filter(|&(i, c)| c == '{' && (i == 0 || out.as_bytes()[i - 1] == b'\n'))
        .find_map(|(i, _)| match serde_json::from_str::<Value>(&out[i..]).ok()?.get_mut("items")?.take() {
            Value::Array(list) => Some(list),
            _ => None,
        })
}

impl World {
    fn lc(&self, argv: &[&str]) -> Option<String> {
        match store_view::held_read(argv) {
            Some(held) => held.ok(),
            None => lc_call(&self.lc_socket, argv).ok(),
        }
    }

    /// The home repo's checkout, from the repo map.
    pub fn home_path(&self) -> Result<PathBuf, String> {
        let text = fs::read_to_string(&self.repo_map).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        map_rows(&text).into_iter().find(|r| r.name == self.home_repo).map(|r| PathBuf::from(r.path)).ok_or_else(|| format!("repo map has no row for {}", self.home_repo))
    }

    /// Unheld beads of one repo in `state`; `Err` is the store's own refusal when it did not answer.
    fn unheld(&self, repo: &str, state: &str) -> Result<u64, String> {
        if let Some(views) = store_view::shared() {
            return views.unheld(state, repo).map(|r| r.value.len() as u64).map_err(|e| e.to_string());
        }
        let text = lc_call(&self.lc_socket, &["list", "--state", state, "--repo", repo]).map_err(|e| e.to_string())?;
        let rows: Value = serde_json::from_str(&text).map_err(|e| format!("cannot tell: list --state {state} --repo {repo}: not JSON: {e}"))?;
        let held = |r: &Value| r["holds"].as_array().is_some_and(|h| !h.is_empty());
        let rows = rows.as_array().ok_or_else(|| format!("cannot tell: list --state {state} --repo {repo}: not a list"))?;
        Ok(rows.iter().filter(|r| r.get("bead_id").is_some() && !held(r)).count() as u64)
    }

    /// The home repo's rounds the lifecycle has not ended, the source `queue round status` reads,
    /// and of those the ones still at phase `opened` with the epoch second they opened. `Err` is the
    /// store's refusal: no answer is not "no round open".
    fn open_round_facts(&self) -> Result<(u64, Vec<(String, u64)>), String> {
        if let Some(views) = store_view::shared() {
            let rounds: Vec<_> = views.round_view(&self.home_repo).map_err(|e| e.to_string())?.value.rounds.into_iter().filter(|r| round_unended(&r.record.state)).collect();
            let unadvanced = rounds.iter().filter(|r| r.record.state == "OPEN").filter_map(|r| Some((r.record.id.clone(), r.record.get("opened")?.parse().ok()?))).collect();
            return Ok((rounds.len() as u64, unadvanced));
        }
        let batches = self.lc_json(&["list", "--batches", "--window", "1"])?;
        let mut live = 0;
        let mut unadvanced = vec![];
        for b in batches.as_array().ok_or("cannot tell: list --batches: not a list")? {
            let (Some(repo), Some(state)) = (b["repo"].as_str(), b["state"].as_str()) else { continue };
            if repo != self.home_repo || !round_unended(state) {
                continue;
            }
            live += 1;
            let opened = b["opened_at"].as_str().and_then(|o| o.parse::<u64>().ok());
            if let (true, Some(at), Some(id)) = (state == "OPEN", opened, b["batch_id"].as_str()) {
                unadvanced.push((id.to_string(), at));
            }
        }
        Ok((live, unadvanced))
    }

    fn open_rounds(&self) -> Option<BTreeMap<String, u64>> {
        Some(open_rounds_by_repo(self.lc_json(&["list", "--batches"]).ok()?.as_array()?))
    }

    fn results(&self, known: &BTreeMap<String, u64>) -> (BTreeMap<String, u64>, Vec<(String, String)>) {
        let mut lines = BTreeMap::new();
        let mut fresh = vec![];
        let mut files: Vec<PathBuf> = fs::read_dir(self.run_dir.join("rounds")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "result")).collect();
        files.sort();
        for f in files {
            let Ok(text) = fs::read_to_string(&f) else { continue };
            let all: Vec<&str> = text.lines().collect();
            let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if let Some(&seen) = known.get(&name) {
                fresh.extend(all.iter().skip(seen as usize).map(|l| (name.clone(), l.to_string())));
            }
            lines.insert(name, all.len() as u64);
        }
        (lines, fresh)
    }

    pub fn round_snapshot(&self, input: &str) -> Result<String, String> {
        let known: BTreeMap<String, u64> = serde_json::from_str(input).unwrap_or_default();
        let rows = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        let (mut certified, mut submitted, mut by_repo, mut submitted_by_repo, mut stopped) = (0, 0, vec![], vec![], vec![]);
        for r in rows.iter().filter(|r| r.land == "queue.local") {
            if lane_stopped(&self.run_dir, &r.name, "rounds") {
                stopped.push(r.name.clone());
                continue;
            }
            let answer = |state| self.unheld(&r.name, state).map_err(|why| format!("lifecycle did not answer for {}: {why}", r.name));
            let (n, m) = (answer("CERTIFIED")?, answer("SUBMITTED")?);
            if n > 0 {
                certified += n;
                by_repo.push(format!("{}={n}", r.name));
            }
            if m > 0 {
                submitted += m;
                submitted_by_repo.push(format!("{}={m}", r.name));
            }
        }
        let open = self.open_round_facts();
        let (lines, fresh) = self.results(&known);
        let snap = Snapshot {
            certified,
            by_repo: by_repo.join(" "),
            stopped: stopped.join(","),
            submitted,
            submitted_by_repo: submitted_by_repo.join(" "),
            state: match &open {
                Err(_) => RoundState::Unknown,
                Ok((0, _)) => RoundState::None,
                Ok(_) => RoundState::Open,
            },
            why: open.as_ref().err().cloned().unwrap_or_default(),
            activation: crate::watchers::current_activation(&self.run_dir),
            maintenance: spira_world::freeze::read_maintenance(&self.run_dir),
            open: open.as_ref().map_or(0, |o| o.0),
            unadvanced: open.map(|o| o.1).unwrap_or_default(),
            lines,
            fresh,
            halt: read_halt(&self.run_dir),
            ..Snapshot::default()
        };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    /// Every review and the epoch the next scheduled one is due. An unset or malformed zone or hour is an
    /// `Err` naming its key.
    pub fn cr_cadence_snapshot(&self, input: &str) -> Result<String, String> {
        let carry: cr_cadence::Carry = serde_json::from_str(input).unwrap_or_default();
        let (tz, hour) = (self.cr_tz.trim(), self.cr_open_hour.trim());
        if tz.is_empty() {
            return Err("SPIRA_CR_TZ is not set; the review cadence has no zone".into());
        }
        let hour: u32 = hour.parse().ok().filter(|h| *h < 24).ok_or_else(|| format!("SPIRA_CR_OPEN_HOUR must be an hour 0 to 23, got {hour:?}"))?;
        let reviews = cr::record::all(&self.run_dir)?;
        let newest_closed = reviews.iter().filter(|r| r.kind == cr::record::Kind::Scheduled).max_by_key(|r| r.number).and_then(|r| r.closed_at);
        let due_at = match newest_closed {
            Some(closed) => cr_open_epoch(tz, hour, closed / 1000, 1)?,
            None => {
                let base = carry.anchor.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
                let today = cr_open_epoch(tz, hour, base, 0)?;
                if today > base { today } else { cr_open_epoch(tz, hour, base, 1)? }
            }
        };
        serde_json::to_string(&cr_cadence::Snapshot::from_records(&reviews, due_at)).map_err(|e| e.to_string())
    }

    /// Stages the next scheduled review with `cr`; its stdout is the answer and its stderr the failure.
    pub fn cr_open(&self) -> Result<String, String> {
        let cr = spira_config::release_env::tool("cr")?;
        let out = spira_config::bounded::bounded_for(cr, &cr_cadence::OPEN_CALL_SECS.to_string()).args(["open", "--count", "1", "--scheduled"]).output().map_err(|e| format!("cr: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("cr exited {}: {}", out.status, err.trim()))
    }

    /// Mails one comment of the operator's in the reader to the Concierge, read through the configured reader.
    pub fn margin_deliver(&self, input: &str) -> Result<String, String> {
        let cfg = &|k: &str| spira_config::process::cfg(k);
        let margin = margin_client::HttpMargin::configured(cfg)?;
        let delivery = crate::sources::smtp::Delivery::of(&mail::env::Env::load()?);
        self.margin_deliver_with(&margin, &margin.base, &delivery, input)
    }

    /// [`Self::margin_deliver`] over `margin`, whose address `base` names in an answer that has no `cr` review to go to.
    pub fn margin_deliver_with(&self, margin: &dyn margin_client::Margin, base: &str, delivery: &crate::sources::smtp::Delivery, input: &str) -> Result<String, String> {
        use crate::watchers::margin_comments::Deliver;
        let d: Deliver = serde_json::from_str(input).map_err(|e| format!("margin.deliver: bad input: {e}"))?;
        let raw = crate::margin_mail::build(margin, base, &self.run_dir, &d.collection, &d.thread, &d.comment)?;
        let delivered = crate::margin_mail::deliver_once(delivery, &raw)?;
        crate::sources::margin_changed::record_done(&self.run_dir, d.seq, &d.batch, d.through)?;
        Ok(format!("comment {} {}", d.comment, if delivered { "delivered" } else { "already delivered" }))
    }

    /// Runs `cr unseal` for the open review holding the published collection; `cr` refuses any other state, so a replay skips it.
    pub fn margin_publish(&self, input: &str) -> Result<String, String> {
        self.margin_publish_with(&|| {
            let margin = margin_client::HttpMargin::configured(|k: &str| spira_config::process::cfg(k))?;
            Ok((Box::new(margin), crate::sources::smtp::Delivery::of(&mail::env::Env::load()?)))
        }, input)
    }

    /// [`Self::margin_publish`] with the reader and delivery a design collection needs, built only when one is.
    pub fn margin_publish_with(&self, design_ports: &dyn Fn() -> Result<(Box<dyn margin_client::Margin>, crate::sources::smtp::Delivery), String>, input: &str) -> Result<String, String> {
        use crate::watchers::margin_comments::{Publish, PUBLISH_CALL_SECS};
        let p: Publish = serde_json::from_str(input).map_err(|e| format!("margin.publish: bad input: {e}"))?;
        let review = cr::record::all(&self.run_dir)?.into_iter().find(|r| r.collection.as_deref() == Some(p.collection.as_str()));
        let design = if review.is_none() { design_review::by_collection(&self.run_dir, &p.collection) } else { None };
        let note = match review {
            None if design.is_some() => {
                let (margin, delivery) = design_ports()?;
                self.design_published(margin.as_ref(), &delivery, design.as_ref().expect("checked"))?
            }
            None => "not a code review".to_string(),
            Some(r) if r.state != cr::record::State::Open => format!("review {:03} is {}; nothing to unseal", r.number, r.state.as_str()),
            Some(r) => {
                let cr = spira_config::release_env::tool("cr")?;
                let out = spira_config::bounded::bounded_for(cr, &PUBLISH_CALL_SECS.to_string()).args(["unseal", "--review", &format!("{:03}", r.number)]).output().map_err(|e| format!("cr: {e}"))?;
                if !out.status.success() {
                    return Err(format!("cr exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
                }
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
        };
        crate::sources::margin_changed::record_done(&self.run_dir, p.seq, &p.batch, p.through)?;
        Ok(note)
    }

    /// Mails the Concierge that his pass on a design page is done, once.
    pub fn design_published(&self, margin: &dyn margin_client::Margin, delivery: &crate::sources::smtp::Delivery, d: &design_review::Record) -> Result<String, String> {
        let sent = crate::margin_mail::publish_design(margin, d, delivery)?;
        Ok(format!("design {} {}", d.page, if sent { "publish mailed" } else { "publish already mailed" }))
    }

    /// The PRs the harness opened and recorded, asked of the forge by number, with the PRs already
    /// tracked in `input`. No repository's PR list is read.
    pub fn pr_snapshot(&self, forge: &Forge, input: &str) -> Result<String, String> {
        let carry: pr_notify::Carry = serde_json::from_str(input).unwrap_or_default();
        let rows = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        let git = self.git_bin.as_deref().unwrap_or(Path::new("git"));
        let reg = crate::tasks::a::queue_step::edge::registry().inspect_err(|e| eprintln!("pr-notify: no forge tip is read: {e}")).ok();
        let reg = reg.map(|r| match &self.git_bin {
            Some(bin) => r.with_git(bin.clone()),
            None => r,
        });
        let targets = pr_targets(rows, &self.home_repo, &|r| reg.as_ref().and_then(|reg| forge_tip(reg, git, r)));
        let records = self.recorded_prs(&targets);
        pr_snapshot_from(&records, &targets, forge, &carry, &|repo, spec| git_ok_with(git, repo, &["cat-file", "blob", spec]).ok().map(|out| String::from_utf8_lossy(&out).into_owned()))
    }

    /// Per repo, the PR numbers its batch record and its publish record name; an `Err` where
    /// either could not be read, which is not "no PR".
    fn recorded_prs(&self, targets: &[PrTarget]) -> RecordedPrs {
        let lc = spira_store::lc_call::LcClient::socket(&self.lc_socket).within(std::time::Duration::from_secs(5));
        let views = store_view::Projection::hot(lc).and_then(|v| v.batches_current().map(|()| v).map_err(|e| e.to_string()));
        let mut out = RecordedPrs::new();
        for t in targets {
            let batch = match &views {
                Ok(v) => v.batch_view(&t.name).map(|view| view.value.open.and_then(|r| r.get("pr").and_then(pr_number))).map_err(|e| e.to_string()),
                Err(e) => Err(e.clone()),
            };
            let publish = publish_pr(&self.queue_dir, &t.name);
            out.insert(t.name.clone(), batch.and_then(|b| publish.map(|p| b.into_iter().chain(p).collect())));
        }
        out
    }

    /// Per wanted bead, the mail files naming it as work bead, in mailbox order: every `new/` then
    /// every `cur/`. Each file is read once, however many beads are wanted.
    fn mails_by_bead(&self, ids: &BTreeSet<String>, read: &dyn Fn(&Path) -> Option<String>) -> BTreeMap<String, Vec<String>> {
        let mut boxes: Vec<PathBuf> = fs::read_dir(&self.mail).into_iter().flatten().flatten().map(|e| e.path()).collect();
        boxes.sort();
        let wanted: Vec<(&String, String)> = ids.iter().map(|id| (id, format!("X-Spira-Work-Bead: {id}"))).collect();
        let mut hits: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for sub in ["new", "cur"] {
            for b in &boxes {
                let mut files: Vec<PathBuf> = fs::read_dir(b.join(sub)).into_iter().flatten().flatten().map(|e| e.path()).collect();
                files.sort();
                for text in files.iter().filter_map(|f| read(f)) {
                    for (id, want) in &wanted {
                        if text.lines().any(|l| l == want) {
                            hits.entry((*id).clone()).or_default().push(text.clone());
                        }
                    }
                }
            }
        }
        hits
    }

    fn read_mail(p: &Path) -> Option<String> {
        fs::read_to_string(p).ok()
    }

    fn hold_line(&self, id: &str, last_mail: Option<&String>) -> String {
        if let Some(text) = last_mail {
            let header = |k: &str| text.lines().take_while(|l| !l.is_empty()).find_map(|l| l.strip_prefix(k)).unwrap_or("").to_string();
            return format!("NEW ASK HOLD {id}: {} | default: {} | message-id: {}", header("Subject: "), header("X-Spira-Default: "), header("Message-ID: "));
        }
        let last = self.lc(&["history", id, "--machine", "bead"]).and_then(|t| last_hold(&t));
        let (seq, q) = last.unwrap_or(("?".into(), String::new()));
        let q = if q.is_empty() { "<no text recorded>".to_string() } else { q };
        format!("NEW ASK HOLD {id}: {q} | NO MAIL for the question; answer with spira-lc reply {id} seq:{seq}")
    }

    /// One spira-lc call, retried so a store restart inside the tick is not an outage; an error
    /// names the command and what the store said.
    fn lc_named(&self, argv: &[&str]) -> Result<String, String> {
        if let Some((began, total)) = self.observe_bound {
            return lc_bounded(&spira_store::lc_call::LcClient::socket(&self.lc_socket), total, began.elapsed(), argv);
        }
        let mut last = String::new();
        for attempt in 0..STORE_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(STORE_BACKOFF);
            }
            match lc_call(&self.lc_socket, argv) {
                Ok(out) => return Ok(out),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("spira-lc {} failed after {STORE_ATTEMPTS} attempts: {}", argv.join(" "), last.trim()))
    }

    /// A lifecycle `list` from the store view when this process keeps one (a stale view refuses, it
    /// does not fall through to the store), else [`Self::lc_named`].
    fn lc_listed(&self, argv: &[&str]) -> Result<String, String> {
        match store_view::held_list(argv) {
            Some(held) => held.map_err(|e| e.to_string()),
            None => self.lc_named(argv),
        }
    }

    /// The beads the lifecycle holds on an ask, from the socket's `list` (the `list-held` caller verb is the CLI's composition of it).
    pub fn held_asks(&self) -> Result<Vec<String>, String> {
        let out = self.lc_listed(&["list", "--live", "--hold", "ask"])?;
        let rows: Vec<Value> = serde_json::from_str(&out).map_err(|e| format!("held asks unparsed: {e}"))?;
        Ok(rows.iter().filter_map(|r| r["bead_id"].as_str().map(str::to_string)).collect())
    }

    /// The open asks as a JSON list; lines before the list (a notice the store printed) are skipped.
    fn open_asks_json(&self) -> Result<String, String> {
        let [flag, filter] = status_args(Kind::Ask, Which::Open);
        let argv = ["plan", "items", "--label", &self.ask_label, &flag, &filter, "--full"];
        let out = self.lc_named(&argv)?;
        match plan_items(&out) {
            Some(list) => Ok(Value::Array(list).to_string()),
            None => Err(format!("spira-lc {} exited 0 but printed no item list: {:?}", argv.join(" "), out.chars().take(200).collect::<String>())),
        }
    }

    pub fn asks_snapshot(&self, input: &str) -> Result<String, String> {
        let seen: BTreeSet<String> = serde_json::from_str(input).unwrap_or_default();
        let listed = self.open_asks_json()?;
        let rows: Vec<Value> = serde_json::from_str(&listed).map_err(|e| format!("ask list unparsed: {e}"))?;
        let open = rows.iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), r["title"].as_str()?.split_whitespace().collect::<Vec<_>>().join(" ")))).collect();
        let unseen: Vec<String> = self.held_asks().unwrap_or_default().into_iter().filter(|id| !seen.contains(id)).collect();
        let held = self.hold_lines(unseen, &Self::read_mail);
        serde_json::to_string(&asks::Snapshot { open, held }).map_err(|e| e.to_string())
    }

    fn hold_lines(&self, ids: Vec<String>, read: &dyn Fn(&Path) -> Option<String>) -> Vec<(String, String)> {
        let mails = self.mails_by_bead(&ids.iter().cloned().collect(), read);
        ids.into_iter().map(|id| { let line = self.hold_line(&id, mails.get(&id).and_then(|m| m.last())); (id, line) }).collect()
    }

    /// Every open ask's JSON, then the mail of every held ask.
    fn asks_text(&self) -> Result<String, String> {
        let listed = self.open_asks_json()?;
        Ok(self.ask_text_from(listed, self.held_asks()?, &Self::read_mail))
    }

    fn ask_text_from(&self, listed: String, held: Vec<String>, read: &dyn Fn(&Path) -> Option<String>) -> String {
        let mails = self.mails_by_bead(&held.iter().cloned().collect(), read);
        let joined = held.iter().flat_map(|id| mails.get(id).cloned().unwrap_or_default());
        std::iter::once(listed).chain(joined).collect::<Vec<_>>().join("\n")
    }

    pub fn design_asks_snapshot(&self) -> Result<String, String> {
        let failed = |why: String| serde_json::to_string(&design_asks::Snapshot { failure: Some(why), ..Default::default() }).map_err(|e| e.to_string());
        if self.wiki.as_os_str().is_empty() {
            return failed("SPIRA_WIKI is not set".into());
        }
        let brain = self.wiki.join("wiki/projects");
        if !brain.is_dir() {
            return failed(format!("{} is not a directory", brain.display()));
        }
        let asks = match self.asks_text() {
            Ok(a) => a,
            Err(why) => return failed(format!("the ask store could not be read: {why}")),
        };
        let mut pages = vec![];
        let mut projects: Vec<PathBuf> = fs::read_dir(&brain).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| !p.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))).collect();
        projects.sort();
        for project in projects {
            let mut files: Vec<PathBuf> = fs::read_dir(project.join("designs")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md") && !p.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))).collect();
            files.sort();
            for f in files {
                let Ok(text) = fs::read_to_string(&f) else { continue };
                let Ok(rel) = f.strip_prefix(&brain) else { continue };
                let writer = ["bead", "session", "author"].iter().map(|k| frontmatter(&text, k)).find(|v| !v.is_empty()).unwrap_or_default();
                let urls = artifact_urls(&text);
                pages.push(Page { rel: format!("wiki/projects/{}", rel.display()), status: frontmatter(&text, "status"), writer, urls });
            }
        }
        serde_json::to_string(&design_asks::Snapshot { failure: None, asks, pages }).map_err(|e| e.to_string())
    }

    /// The readiness look: the head revision and the records with an event since `input`'s
    /// revision. With no revision yet it only reads the head, and names no keys.
    pub fn readiness_look(&self, input: &str) -> Result<String, String> {
        use crate::watchers::readiness_dependents::Pass;
        const BASELINE: &str = "18446744073709551615";
        let asked: Pass = serde_json::from_str(input).map_err(|e| format!("readiness-dependents input: {e}"))?;
        if spira_config::process::cfg("SPIRA_LC_STORE_PATH")?.is_empty() {
            return serde_json::to_string(&Pass { keys: vec![], ..asked }).map_err(|e| e.to_string());
        }
        let since = asked.through.map_or(BASELINE.to_string(), |t| t.to_string());
        let seen = self.lc_json(&["event-seq", "--since", &since])?;
        let through = seen["seq"].as_u64().ok_or("event-seq: no seq")?;
        let keys = seen["keys"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        serde_json::to_string(&Pass { through: Some(through), changed: 0, keys }).map_err(|e| e.to_string())
    }

    /// One readiness-dependents pass: the dependents of `input`'s keys are judged again.
    pub fn readiness_dependents(&self, input: &str) -> Result<String, String> {
        use crate::watchers::readiness_dependents::Pass;
        const CHUNK: usize = 100;
        let asked: Pass = serde_json::from_str(input).map_err(|e| format!("readiness-dependents input: {e}"))?;
        for chunk in asked.keys.chunks(CHUNK) {
            let argv = ["republish-dependents".to_string(), "--changed".into(), chunk.join(",")];
            let out = spira_claim::cli::dispatch_in(&argv, &mut std::io::empty(), &|k| std::env::var(k).ok());
            if out.code != 0 {
                return Err(format!("republish-dependents: exit {}: {}", out.code, out.err.lines().next().unwrap_or("no message")));
            }
        }
        serde_json::to_string(&Pass { through: asked.through, changed: asked.keys.len() as u64, keys: vec![] }).map_err(|e| e.to_string())
    }

    /// One `overrides.sh apply` pass; a failed override fails the pass, and its stdout and stderr say which.
    pub fn overrides_apply(&self, input: &str, held: &crate::wiki_git::Held) -> Result<String, String> {
        overrides_apply(&self.overrides_dir, input.trim().parse().ok(), held)
    }

    /// lc-serve's kept verdict on the content writer's grants, as the `writer-grants` verb answers it.
    pub fn writer_grants_snapshot(&self) -> Result<String, String> {
        lc_call(&self.lc_socket, &["writer-grants"]).map_err(|e| e.to_string())
    }

    /// The control store's newest revision, as lc-serve's `ship-status` verb answers it.
    pub fn ship_head(&self) -> Result<u64, String> {
        let text = lc_call(&self.lc_socket, &["ship-status"]).map_err(|e| e.to_string())?;
        let said: Value = serde_json::from_str(&text).map_err(|e| format!("ship-status: {e}: {text}"))?;
        said["head"].as_u64().ok_or_else(|| format!("ship-status: no head in {text}"))
    }

    /// The wiki checkout's changed paths and their mtimes. A path that no longer exists has none
    /// and is left out; an unset wiki observes nothing.
    pub fn brain_dirty_snapshot(&self) -> Result<String, String> {
        use std::os::unix::ffi::OsStrExt;
        let wiki = self.wiki.display().to_string();
        let mut snap = brain_dirty::Snapshot { wiki, ..Default::default() };
        if !self.wiki.as_os_str().is_empty() {
            let is_repo = match spira_config::git_refs::git_dirs(&self.wiki).filter(|(gitdir, _)| gitdir.join("HEAD").is_file()) {
                Some(_) => true,
                None => git(&self.wiki, &["rev-parse", "--git-dir"])?.0 == Some(0),
            };
            if is_repo {
                let read = || -> Result<Vec<u8>, String> {
                    let (code, raw) = git(&self.wiki, &["status", "--porcelain", "-z", "--untracked-files=all"])?;
                    if code != Some(0) {
                        return Err(format!("git status exited {code:?}"));
                    }
                    Ok(raw)
                };
                let raw = match spira_config::git_refs::worktree_digest(&self.wiki) {
                    Some(digest) => spira_config::git_refs::held(&self.wiki, "status", &digest.to_string(), read)?,
                    None => read()?,
                };
                for path in brain_dirty::porcelain_paths(&raw) {
                    let mtime = fs::metadata(self.wiki.join(std::ffi::OsStr::from_bytes(&path))).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                    if let Some(d) = mtime {
                        snap.dirty.push((String::from_utf8_lossy(&path).into_owned(), d.as_secs()));
                    }
                }
            } else {
                snap.not_repo = true;
            }
        }
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    fn state_of(&self, id: &str) -> String {
        self.lc(&["state", id]).map(|s| s.trim().to_string()).unwrap_or_default()
    }

    /// Every bead under a manual hold, read unless `input` says its version was already read as no request.
    pub fn supersede_snapshot(&self, input: &str) -> Result<String, String> {
        let known: BTreeMap<String, String> = serde_json::from_str(input).unwrap_or_default();
        let listed = crate::reconcile_host::lc_read(&self.lc_socket, &["list", "--live", "--hold", "manual"])?;
        let rows: Vec<Value> = serde_json::from_str(&listed).map_err(|e| format!("held list unparsed: {e}"))?;
        let text = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
        let mut held = Vec::new();
        for r in rows.iter().filter(|r| !r["bead_id"].is_null()) {
            let (id, version) = (text(&r["bead_id"]), text(&r["version"]));
            let reading = if known.get(&id) == Some(&version) {
                Reading::Skipped
            } else {
                let raise = manual_hold_of(r).ok_or_else(|| format!("{id}: held manual but its record has no manual_hold; the store has not applied the manual_hold migration"))?;
                match supersede_duty::request_of(raise) {
                    None => Reading::NotARequest,
                    Some(succ) => Reading::Request { succ, own: r["state"].as_str().unwrap_or("").to_string(), succ_state: String::new() },
                }
            };
            held.push(Held { id, version, reading });
        }
        let successors: BTreeSet<String> = held
            .iter()
            .filter_map(|h| match &h.reading {
                Reading::Request { succ, own, .. } if !own.is_empty() && !supersede_duty::terminal(own) => Some(succ.clone()),
                _ => None,
            })
            .collect();
        if !successors.is_empty() {
            let ids: Vec<String> = successors.into_iter().collect();
            let client = spira_store::lc_call::LcClient::socket(&self.lc_socket);
            let states: BTreeMap<String, String> = store_view::rows_by_ids(&client, &ids)?.into_iter().map(|r| (r.bead_id, r.state)).collect();
            for h in &mut held {
                if let Reading::Request { succ, own, succ_state } = &mut h.reading {
                    if !own.is_empty() && !supersede_duty::terminal(own) {
                        *succ_state = states.get(succ.as_str()).cloned().unwrap_or_default();
                    }
                }
            }
        }
        serde_json::to_string(&supersede_duty::Snapshot { held }).map_err(|e| e.to_string())
    }

    /// Every live item with a builder claim: its claim count and kept session ends, as the lifecycle row holds them.
    pub fn claim_count_snapshot(&self) -> Result<String, String> {
        let listed = crate::reconcile_host::lc_read(&self.lc_socket, &["list", "--live"])?; // BDQ_READ_CONSIDERED="the spira-lc lifecycle door, not a bd read"
        let rows: Vec<Value> = serde_json::from_str(&listed).map_err(|e| format!("live list unparsed: {e}"))?;
        let mut snap = claim_count::Snapshot::default();
        for r in &rows {
            let id = r["bead_id"].as_str().ok_or_else(|| format!("live row without a bead_id: {r}"))?;
            let n: u32 = match &r["builder_claims"] {
                Value::String(s) => s.trim().parse().map_err(|e| format!("{id}: builder_claims {s:?} unparsed: {e}"))?,
                Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| format!("{id}: builder_claims {n} unparsed"))?,
                other => return Err(format!("{id}: builder_claims is {other}, not a number")),
            };
            let ends = r["session_ends"].as_array().ok_or_else(|| format!("{id}: session_ends is {}, not a list", r["session_ends"]))?.clone();
            if n >= 1 {
                snap.rows.push(claim_count::Claims { id: id.to_string(), n, ends });
            }
        }
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    /// One adjudication write through the lifecycle, then the evidence note on the bead.
    pub fn supersede_act(&self, input: &str) -> Result<String, String> {
        let act: Act = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let done = match act.verb {
            Verb::Unhold => lc_call(&self.lc_socket, &["unhold", &act.id, "manual", supersede_duty::ACTOR]),
            Verb::Supersede => lc_call(&self.lc_socket, &["supersede", &act.id, &act.succ, supersede_duty::ACTOR]),
        };
        if let Err(e) = done {
            return if act.verb == Verb::Supersede && supersede_duty::terminal(&self.state_of(&act.id)) { Ok(supersede_duty::SETTLED.into()) } else { Err(e.to_string()) };
        }
        let mut flags = vec![];
        if act.verb == Verb::Supersede && self.lc(&["bd-supersede", &act.id, &act.succ]).is_none() {
            flags.push(supersede_duty::BD_FAILED);
        }
        if self.lc(&["bd-note", &act.id, &act.note]).is_none() {
            flags.push(supersede_duty::NOTE_FAILED);
        }
        Ok(if flags.is_empty() { "done".into() } else { flags.join(" ") })
    }

    fn lc_json(&self, argv: &[&str]) -> Result<Value, String> {
        let held = match self.observe_bound {
            Some((began, total)) if store_view::shared().is_some() => {
                let owned: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
                let left = total.saturating_sub(began.elapsed());
                run_within(left, move || store_view::held_read(&owned.iter().map(String::as_str).collect::<Vec<_>>()))
                    .ok_or_else(|| format!("store unavailable: the view's answer to spira-lc {} was not ready in the observe's bound of {} s", argv.join(" "), total.as_secs()))?
            }
            _ => store_view::held_read(argv),
        };
        let text = match held {
            Some(held) => held.map_err(|e| e.to_string())?,
            None if self.observe_bound.is_some() => self.lc_named(argv)?,
            None => lc_call(&self.lc_socket, argv).map_err(|e| e.to_string())?,
        };
        serde_json::from_str(&text).map_err(|e| format!("{} unparsed: {e}", argv[0]))
    }

    /// The beads still in the backlog: every work bead carrying the scope label whose lifecycle
    /// row is not terminal, the set `bd list --exclude-type epic,event --label <scope>` selected.
    /// The label is not the repository: a bead filed for another repository carries the scope
    /// label too, and a live epic has a lifecycle row, so the content store decides both.
    fn flow_backlog(&self) -> Result<u64, String> {
        let rows = spira_store::lc_state::parse_rows(&self.lc_listed(&["list", "--live"])?).map_err(|e| format!("lifecycle state unreadable: {e}"))?;
        let live: Vec<&str> = rows.iter().filter(|r| !r.terminal()).map(|r| r.bead_id.as_str()).collect();
        let mut ids = vec![];
        for chunk in live.chunks(FLOW_CONTENT_CHUNK) {
            let argv = ["plan", "items", "--ids", &chunk.join(",")];
            let out = self.lc_named(&argv)?;
            let found = plan_items(&out).ok_or_else(|| format!("spira-lc plan items printed no item list: {:?}", out.chars().take(200).collect::<String>()))?;
            ids.extend(found.iter().filter(|r| self.in_flow_backlog(r)).filter_map(|r| r["id"].as_str().map(str::to_string)));
        }
        Ok(::reconciler_flow::io::backlog_of(&ids, &spira_store::lc_state::index(rows)))
    }

    fn in_flow_backlog(&self, content: &Value) -> bool {
        let work = !matches!(content["issue_type"].as_str(), Some("epic" | "event"));
        let scoped = self.scope_label.is_empty() || content["labels"].as_array().is_some_and(|l| l.iter().any(|x| x.as_str() == Some(self.scope_label.as_str())));
        work && scoped
    }

    /// Brings `bead-stage` up to the lifecycle log. A pass that runs the exporter takes its verdict:
    /// exit 0 means it drained the log, and the store may not yet show those rows. Only a pass with
    /// nothing to export asks the store whether the family trails the log.
    fn flow_export(&self) -> Result<(), String> {
        let logged = self.lc_json(&["event-seq", "--head"])?["seq"].as_u64().ok_or("event-seq: no seq")?;
        let checkpoint = self.run_dir.join("tsd").join(".bead-stage-checkpoint-lifecycle.json");
        let exported = fs::read_to_string(checkpoint).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["last_seq"].as_u64()).unwrap_or(0);
        let bin = self.export_bin.as_str();
        if bin.is_empty() {
            return if logged <= exported { Ok(()) } else { Err("bead-stage is behind the lifecycle log and no exporter is configured".into()) };
        }
        if logged > exported {
            return exporter_run(bin, "lifecycle");
        }
        let mut lc = spira_store::lc_call::LcClient::socket(&self.lc_socket);
        let mut metrics = spira_otel::query::Store::new(self.metrics_endpoint.as_deref()).map_err(|e| e.to_string())?;
        if let Some((began, total)) = self.observe_bound {
            let left = total.saturating_sub(began.elapsed());
            if left.is_zero() {
                return Err(format!("store unavailable: tried for {} s, the observe's bound, before the bead-stage check", total.as_secs()));
            }
            lc = lc.within(left);
            metrics = metrics.with_deadline(left);
        }
        tsd_lifecycle_export::check(&lc, &metrics)
    }

    pub fn flow_snapshot(&self, input: &str) -> Result<String, String> {
        use ::reconciler_flow::io as flow;
        let p: Params = serde_json::from_str(input).map_err(|e| format!("reconciler-flow params unparsed: {e}"))?;
        if self.run_dir.join("world.halted").exists() {
            return serde_json::to_string(&FlowSnapshot::halted(p.now)).map_err(|e| e.to_string());
        }
        let export = self.flow_export();
        let (velocity_floor, dwell_limit) = flow::flow_floors(&self.desired_dir);
        let baseline = flow::backlog_baseline_in_process(&self.run_dir, p.now, p.baseline_hours);
        let backlog = self.flow_backlog();
        if let Ok(count) = &backlog {
            let _ = flow::append_backlog_sample_in_process(&self.run_dir, p.now, *count);
        }
        let endpoint = self.metrics_endpoint.as_deref();
        let stage = flow::stage_rows(endpoint).map(|rows| Stage {
            waiting: flow::waiting_to_land(&rows),
            velocity: flow::velocity_metrics(&rows, p.now, p.window_hours, p.baseline_hours),
            dwell: flow::dwell_metrics(&rows, p.now, p.window_hours, p.baseline_hours),
            round_health: flow::round_health_metrics(&rows, p.now, p.window_hours, p.baseline_hours),
            rework: flow::rework_metrics(&rows, p.now, p.rework_window_hours),
            regression: flow::dwell_regression_metrics(&rows, p.now, p.window_hours, p.baseline_hours),
        });
        let slots = flow::slots_samples_in_process(&self.run_dir).map(|(prev, cur)| (Slots::from(prev), Slots::from(cur)));
        let sentinel = flow::sentinel_pass_wall_seconds(endpoint, p.now, (p.sentinel_timer_secs * 3).max(3600));
        serde_json::to_string(&FlowSnapshot { now: p.now, halted: false, export, velocity_floor, dwell_limit, backlog, baseline, stage, slots, sentinel }).map_err(|e| e.to_string())
    }

    /// Records each verdict as a `reconciler-status` row; a row the store refuses is dropped, as the pass always did.
    pub fn flow_status(&self, input: &str) -> Result<String, String> {
        let s: Statuses = serde_json::from_str(input).map_err(|e| format!("reconciler-flow statuses unparsed: {e}"))?;
        let store = tsd::store::Pusher::new(self.metrics_endpoint.clone());
        for row in &s.rows {
            reconciler_engine::io::push(&store, reconciler_engine::io::STATUS_FAMILY, &s.ts, row);
        }
        Ok(format!("{} rows", s.rows.len()))
    }

    pub fn sweep_snapshot(&self) -> Result<String, String> {
        let list = |states: &str| self.lc_json(&["list", "--state", states]);
        let rows = list("READY,REWORK,SUBMITTED,CERTIFIED,WORKING,OPEN,IN_DELIVERY")?;
        let [flag, filter] = status_args(Kind::Ask, Which::Live);
        let content = self.lc_json(&["content", "list", &flag, &filter, "--json", "--limit", "0"])?;
        let beads: Vec<Bead> = content.as_array().ok_or("content list is not an array")?.iter().filter_map(sweep_bead).collect();
        let labels_of: BTreeMap<&str, &Vec<String>> = beads.iter().map(|b| (b.id.as_str(), &b.labels)).collect();
        let mut rows_out: Vec<Row> = Vec::new();
        for raw in rows.as_array().ok_or("list is not an array")? {
            let Some(mut r) = sweep_row(raw) else { continue };
            let ours = r.holds.iter().any(|h| h == "manual") && !r.holds.iter().any(|h| h == "poison" || h == "ask") && !labels_of.get(r.id.as_str()).is_some_and(|l| l.contains(&self.ask_label));
            if ours && bead_sweep::review(&r.reason).is_none() {
                let raise = manual_hold_of(raw).ok_or_else(|| format!("{}: held manual but its record has no manual_hold; the store has not applied the manual_hold migration", r.id))?;
                if let Some(at) = raise["at"].as_f64() {
                    r.held_at = Some(at);
                    r.hold_review = raise["detail"].as_str().filter(|d| bead_sweep::review(d).is_some()).map(str::to_string);
                }
            }
            rows_out.push(r);
        }
        let rows = rows_out;
        let terminal = self.terminal_among(beads.iter().map(|b| b.id.as_str()).chain(rows.iter().flat_map(|r| r.blocked_by.iter().map(String::as_str))), &rows)?;
        let (stops, reserved) = self.scoped_stops();
        let pr_repos = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).unwrap_or_default().into_iter().filter(|r| r.land == "pr").map(|r| r.name).collect();
        let snap = bead_sweep::Snapshot { rows, content: beads, terminal, stops, reserved, carried: self.integrations(), pr_repos };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    /// The terminal rows among the beads the sweep names that hold no live row. The projection holds
    /// live rows only, so these ask the store, by id, once.
    fn terminal_among<'a>(&self, named: impl Iterator<Item = &'a str>, live: &[Row]) -> Result<Vec<(String, String)>, String> {
        let live: BTreeSet<&str> = live.iter().map(|r| r.id.as_str()).collect();
        let ids: BTreeSet<&str> = named.filter(|i| !live.contains(i)).collect();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<&str> = ids.into_iter().collect();
        let mut terminal = Vec::new();
        for chunk in ids.chunks(100) {
            let out = lc_call(&self.lc_socket, &["list", "--ids", &chunk.join(",")]).map_err(|e| e.to_string())?; // BDQ_READ_CONSIDERED="LcClient is the spira-lc door; no bdq"
            let rows: Value = serde_json::from_str(&out).map_err(|e| format!("list unparsed: {e}"))?;
            terminal.extend(rows.as_array().ok_or("list is not an array")?.iter().filter_map(|r| Some((r["bead_id"].as_str()?.to_string(), r["state"].as_str()?.to_string()))).filter(|(_, st)| spira_store::lc_state::is_terminal(st)));
        }
        Ok(terminal)
    }

    fn scoped_stops(&self) -> (Vec<Stop>, Vec<Reservation>) {
        let dir = self.run_dir.join("scoped-stops");
        let mut files: Vec<PathBuf> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        let (mut stops, mut reserved): (Vec<Stop>, Vec<Reservation>) = (vec![], vec![]);
        for f in files {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(v) = fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
            let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
            let since = v["since"].as_f64().unwrap_or(0.0);
            if name.starts_with("repo-") {
                let lanes = v["lanes"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()).unwrap_or_default();
                let s = Stop { repo: text("repo"), lanes, owner: text("owner"), until: text("until"), since };
                match stops.iter().position(|o| o.repo == s.repo) {
                    Some(i) => stops[i] = s,
                    None => stops.push(s),
                }
            } else if name.starts_with("reserve-") {
                let r = Reservation { bead: text("bead"), owner: text("owner"), reason: text("reason"), until: text("until"), since };
                match reserved.iter().position(|o| o.bead == r.bead) {
                    Some(i) => reserved[i] = r,
                    None => reserved.push(r),
                }
            }
        }
        (stops, reserved)
    }

    /// Each repo's open integrations: a record line is the integration's id then its members.
    fn integrations(&self) -> Vec<(String, Vec<String>)> {
        let mut dirs: Vec<PathBuf> = fs::read_dir(&self.queue_dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        dirs.sort();
        let mut out: Vec<(String, Vec<String>)> = vec![];
        for d in dirs {
            for line in fs::read_to_string(d.join("integrations")).unwrap_or_default().lines() {
                let mut f = line.split_whitespace().map(str::to_string);
                let Some(id) = f.next() else { continue };
                let members: Vec<String> = f.collect();
                match out.iter().position(|(o, _)| *o == id) {
                    Some(i) => out[i].1 = members,
                    None => out.push((id, members)),
                }
            }
        }
        out
    }

    /// Mirrors the `blocks` edges the content bead carries into the lifecycle store; the edges, or why not all of them were.
    fn mirror_blocks(&self, id: &str) -> Result<Vec<String>, String> {
        let listed = lc_call(&self.lc_socket, &["content", "dep", "list", id, "--type", "blocks", "--json"]).map_err(|e| format!("edges unreadable: {e}"))?;
        let targets = bead::parse_blocks_targets(&listed);
        for t in &targets {
            let argv = bead::lc::dep_argv("dep-add", id, t, Some("blocks"));
            lc_call(&self.lc_socket, &argv.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|e| format!("edge {id} -> {t} not mirrored: {e}"))?;
        }
        Ok(targets)
    }

    /// Gives each rowless bead the row it lacks, OPEN, then its edges, then promotes it; `create-bead` ignores a bead that already has one.
    /// Nothing is created while `skew-check` refuses: after a restore bd can be hours behind the
    /// control plane, and its open beads may be ones the control plane already closed.
    pub fn sweep_repair(&self, input: &str) -> Result<String, String> {
        let Repair { ids: asked, claims } = serde_json::from_str(input).map_err(|e| e.to_string())?;
        if let Err(e) = lc_call(&self.lc_socket, &["skew-check"]) {
            eprintln!("bead-sweep: the rowless repair is refused: {e}");
            return serde_json::to_string(&Repaired::default()).map_err(|e| e.to_string());
        }
        let snap: bead_sweep::Snapshot = serde_json::from_str(&self.sweep_snapshot()?).map_err(|e| e.to_string())?;
        let todo: Vec<_> = bead_sweep::repairable(&self.ask_label, &snap).into_iter().filter(|r| asked.contains(&r.id)).collect();
        let has_row: BTreeSet<&str> = snap.rows.iter().map(|r| r.id.as_str()).collect();
        let mut edges = BTreeMap::new();
        let mut ids = vec![];
        for r in &todo {
            if !has_row.contains(r.id.as_str()) {
                let mut argv = vec!["create-bead", &r.id];
                if let Some(repo) = &r.repo {
                    argv.extend(["--repo", repo]);
                }
                if self.lc(&argv).is_none() {
                    continue;
                }
            }
            match self.mirror_blocks(&r.id) {
                Ok(mirrored) => {
                    if self.lc(&["promote-filed", &r.id]).is_some() {
                        edges.insert(r.id.clone(), mirrored);
                        ids.push(r.id.clone());
                    }
                }
                Err(e) => eprintln!("bead-sweep: {} keeps its OPEN row, its edges are retried next pass: {e}", r.id),
            }
        }
        let todo_claims: Vec<String> = bead_sweep::claim_unset(&snap).into_iter().filter(|id| claims.contains(id)).collect();
        let claimed = todo_claims
            .into_iter()
            .filter(|id| match lc_call(&self.lc_socket, &["sync-claim-inputs", "--only-if-unset", id]) {
                Ok(out) => !out.contains(&format!("kept {id}:")),
                Err(e) => {
                    eprintln!("bead-sweep: claim inputs of {id} not written: {e}");
                    false
                }
            })
            .collect();
        serde_json::to_string(&Repaired { ids, edges, claimed }).map_err(|e| e.to_string())
    }

    /// Per queue.local repo, how far the forge trails the landing ref, as `spira-lc unpublished` measures it.
    pub fn publish_backlog_snapshot(&self) -> Result<String, String> {
        self.publish_backlog_through(&forge_refresh(&SysProc, crate::tasks::a::queue_step::edge::registry().ok()), &crate::unpublished::measure)
    }

    fn publish_backlog_through(&self, refresh: &(dyn Fn(&Path, &MapRow) + Sync), measure: &dyn Fn(&Path, &str, u64) -> Result<crate::unpublished::Measurement, String>) -> Result<String, String> {
        let rows = match fs::read_to_string(&self.repo_map) {
            Ok(t) => map_rows(&t),
            Err(e) => return serde_json::to_string(&publish_backlog::Snapshot { repos: vec![(publish_backlog::REPO_MAP_KEY.into(), Read::Blind(format!("the repo map is unreadable: {e}")))] }).map_err(|e| e.to_string()),
        };
        let git = self.git_bin.as_deref().unwrap_or(Path::new("git"));
        publish_backlog_snapshot_from(&rows, &|r| refresh(git, r), &|name| self.unpublished(measure, git, name))
    }

    /// Measures the repo here and records the measurement for lc-serve; a record that cannot be
    /// written is logged and the snapshot still carries what was read. `Ok(None)` is a repo declared to publish nowhere.
    fn unpublished(&self, measure: &dyn Fn(&Path, &str, u64) -> Result<crate::unpublished::Measurement, String>, git: &Path, name: &str) -> Result<Option<Read>, String> {
        let now = spira_config::vtime::now_epoch() as u64;
        let measurement = measure(git, name, now)?;
        if let Err(e) = lc_call(&self.lc_socket, &["git-fact", "put", "unpublished", "--json", &measurement.record(name)]) {
            eprintln!("publish-backlog: the measurement of {name} was not recorded: {}", e.0);
        }
        Ok(match measurement {
            crate::unpublished::Measurement::Declared => None,
            crate::unpublished::Measurement::Measured(m) => {
                let age = now.saturating_sub(m.read_at);
                Some(Read::Measured { count: m.count, arrival: m.oldest_arrival, forge_tip_age_secs: (age >= spira_store::lc_call::UNPUBLISHED_STALE_AFTER_SECS).then_some(age) })
            }
        })
    }

    pub fn view_snapshot(&self, input: &str) -> Result<String, String> {
        let (live, error) = match hunk::live_ids(&self.hunk_dir) {
            Ok(live) => (Some(live), None),
            Err(e) => (None, Some(e)),
        };
        let lines = |name: &str| fs::read_to_string(self.cockpit_state.join(name)).unwrap_or_default();
        let ids = lines("dismissed").lines().filter(|l| !l.is_empty()).map(str::to_string).collect();
        let absent = lines("dismissed.absent").lines().filter_map(|l| l.split_once(' ')).filter_map(|(id, since)| Some((id.to_string(), since.parse().ok()?))).fold(BTreeMap::new(), |mut m, (id, since)| {
            m.entry(id).or_insert(since);
            m
        });
        serde_json::to_string(&view::Snapshot { seed: input == view::SEED, live, error, dismissed: view::Dismissed { ids, absent } }).map_err(|e| e.to_string())
    }

    fn tmux(&self, args: &[&str]) -> Result<String, String> {
        tmux(&self.tmux, args, TMUX_DEADLINE)
    }

    /// Shows the cockpit window linked to `<to>:0`: the cockpit-local index whose window id is that window's.
    pub fn view_flip(&self, to: &str) -> Result<String, String> {
        if !matches!(to, view::HUNK | view::BRAIN) {
            return Err(format!("no such view {to:?}"));
        }
        let cockpit = format!("={}", self.cockpit_session);
        let id = self.tmux(&["display-message", "-p", "-t", &format!("={to}:0"), "#{window_id}"])?.trim().to_string();
        let windows = self.tmux(&["list-windows", "-t", &cockpit, "-F", "#{window_index} #{window_id}"])?;
        let idx = windows.lines().filter_map(|l| l.split_once(' ')).find(|(_, w)| !id.is_empty() && *w == id).map(|(i, _)| i.to_string()).ok_or_else(|| format!("no cockpit window shows {to}"))?;
        self.tmux(&["select-window", "-t", &format!("{cockpit}:{idx}")])?;
        Ok(format!("showing {to}"))
    }

    pub fn view_forget(&self, input: &str) -> Result<String, String> {
        let forget: view::Forget = serde_json::from_str(input).map_err(|e| e.to_string())?;
        fs::create_dir_all(&self.cockpit_state).map_err(|e| e.to_string())?;
        let path = |name: &str| self.cockpit_state.join(name);
        let marks: String = forget.dismissed.absent.iter().map(|(id, since)| format!("{id} {since}\n")).collect();
        if marks.is_empty() {
            let _ = fs::remove_file(path("dismissed.absent"));
        } else {
            fs::write(path("dismissed.absent"), marks).map_err(|e| e.to_string())?;
        }
        if forget.rewrite {
            let ids: String = forget.dismissed.ids.iter().map(|id| format!("{id}\n")).collect();
            fs::write(path("dismissed.tmp"), ids).and_then(|()| fs::rename(path("dismissed.tmp"), path("dismissed"))).map_err(|e| e.to_string())?;
        }
        Ok("forgot".into())
    }

    /// The rows waiting to land: SUBMITTED, CERTIFIED and IN_DELIVERY.
    pub fn waiting_rows(&self) -> Result<Value, String> {
        Ok(Value::Array(self.live_rows()?.into_iter().filter(|r| r["state"] != "WORKING").collect()))
    }

    /// The rows in a live state (`dependency::LIVE`): one read for the waiting beads and the blockers' states.
    fn live_rows(&self) -> Result<Vec<Value>, String> {
        let rows: Value = serde_json::from_str(&self.lc_listed(&["list", "--state", &crate::watchers::dependency::LIVE.join(",")])?).map_err(|e| e.to_string())?;
        Ok(rows.as_array().cloned().unwrap_or_default())
    }

    /// The day's landings and rounds for `watch.throughput`: per repo, the beads landed on its
    /// landing ref by hour and the beads waiting to land, and every repo's rounds from the batch rows.
    pub fn throughput_snapshot(&self, input: &str) -> Result<String, String> {
        let bounded = World { observe_bound: Some((std::time::Instant::now(), crate::tasks::e::watch_throughput::STORE_BOUND)), ..self.clone() };
        bounded.throughput_observed(input)
    }

    fn throughput_observed(&self, input: &str) -> Result<String, String> {
        let p: ThroughputParams = serde_json::from_str(input).map_err(|e| format!("throughput params unparsed: {e}"))?;
        let day = 24 * 3600;
        let halt = read_halt(&self.run_dir);
        let activated_at = crate::watchers::last_activation(&self.run_dir);
        let activation = crate::watchers::current_activation(&self.run_dir);
        let maintenance = spira_world::freeze::read_maintenance(&self.run_dir);
        let blind = |why: String| ThroughputSnapshot { readable: false, halt: halt.clone(), activated_at, activation: activation.clone(), maintenance: maintenance.clone(), why, ..Default::default() };
        let prefix = match spira_config::process::cfg("SPIRA_ID_PREFIX") {
            Ok(p) => p,
            Err(e) => return to_json(&blind(format!("SPIRA_ID_PREFIX: {e}"))),
        };
        let mut landings = match self.landed_on_refs(p.now, &prefix) {
            Ok(l) => l,
            Err(e) => return to_json(&blind(format!("landings: {e}"))),
        };
        let live = match self.live_rows() {
            Ok(w) => w,
            Err(e) => return to_json(&blind(format!("waiting rows: {e}"))),
        };
        let states: BTreeMap<String, String> = live.iter().filter_map(|r| Some((r["bead_id"].as_str()?.to_string(), r["state"].as_str()?.to_string()))).collect();
        let waiting: Vec<Value> = live.into_iter().filter(|r| r["state"] != "WORKING").collect();
        let batches = match self.lc_json(&["list", "--batches", "--window", &day.to_string()]) {
            Ok(b) => b,
            Err(e) => return to_json(&blind(format!("batches: {e}"))),
        };
        let Some(rows) = batches.as_array() else { return to_json(&blind("batches: not a list".into())) };
        stamp_landings(&mut landings, rows);
        stamp_waiting(&mut landings, &waiting);
        let batches = batches.as_array().map(Vec::as_slice).unwrap_or_default();
        let repos = round_counts(batches, p.now, p.abandon_window_s);
        let unlanded = unlanded_rounds(batches, p.now);
        let parts = self.round_record_partition();
        let records = Records { clear: parts.clear.iter().map(|f| f.display().to_string()).collect(), held: parts.held };
        to_json(&ThroughputSnapshot { readable: true, landings, repos, unlanded, records, halt, activated_at, states, ..Default::default() })
    }

    /// Every checkout of the repo map, as the registry resolves it. An unreadable map is one
    /// row carrying the error, so a caller's other work goes on and the failure is still reported.
    pub fn repos(&self) -> Vec<crate::tasks::c::sweep_reclaim::Repo> {
        use crate::tasks::c::sweep_reclaim::Repo;
        let map = match fs::read_to_string(&self.repo_map) {
            Ok(m) => m,
            Err(e) => return vec![Repo { name: "repo map".into(), error: format!("{}: {e}", self.repo_map.display()), ..Repo::default() }],
        };
        let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), self.home_repo.clone())]);
        let reg = spira_config::repos::Registry::new(Some(&map), &env, Path::new(""));
        reg.names().into_iter().map(|name| Repo { path: reg.root(&name).unwrap_or_default(), name, error: String::new() }).collect()
    }

    /// A git re-read is spent only while less than half the observe bound is gone, so the store
    /// reads after it keep the time they need to answer inside the deadline.
    fn has_time_to_reread(&self) -> bool {
        self.observe_bound.is_none_or(|(began, total)| began.elapsed() < total / 2)
    }

    /// The beads landed over the 24 h ending `now`, per registered repo, read from its landing ref
    /// as `spira_landref` resolves it (the map row's `base`, else the remote's default; never the
    /// lane name): by hour of the last commit naming each bead, the oldest hour first, and the
    /// time of the last such commit. A repo whose ref cannot be resolved or read is an error.
    pub fn landed_on_refs(&self, now: u64, prefix: &str) -> Result<Vec<RepoLanding>, String> {
        let day = 24 * 3600;
        let map = fs::read_to_string(&self.repo_map).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), self.home_repo.clone())]);
        let mut reg = spira_config::repos::Registry::new(Some(&map), &env, Path::new(""));
        if let Some(git) = &self.git_bin {
            reg = reg.with_git(git.clone());
        }
        let from = now.saturating_sub(day);
        let since = format!("--since=@{from}");
        let mut out = vec![];
        for row in map_rows(&map) {
            let land = spira_config::repos::landref_explained(&reg, &row.name)
                .or_else(|e| if self.has_time_to_reread() { spira_config::repos::landref_explained(&reg, &row.name) } else { Err(e) })
                .map_err(|why| format!("{}: no landing ref resolves: {why}", row.name))?;
            let git = self.git_bin.as_deref().unwrap_or(Path::new("git"));
            let log_args = ["log", land.as_str(), &since, "--format=%ct %s"];
            let read = || -> Result<Vec<(u64, String)>, String> {
                let log = git_ok_with(git, Path::new(&row.path), &log_args)
                    .or_else(|e| if self.has_time_to_reread() { git_ok_with(git, Path::new(&row.path), &log_args) } else { Err(e) })
                    .map_err(|e| format!("{}: git log {land} in {}: {e}", row.name, row.path))?;
                Ok(String::from_utf8_lossy(&log).lines().filter_map(|l| l.split_once(' ')).filter_map(|(at, subject)| Some((at.parse::<u64>().ok()?, subject.to_string()))).collect())
            };
            let rows = match spira_config::git_refs::sha_of(Path::new(&row.path), &land) {
                Some(tip) => spira_config::git_refs::held(Path::new(&row.path), &format!("landed {land}"), &tip, read)?,
                None => read()?,
            };
            let mut landed: BTreeMap<String, u64> = BTreeMap::new();
            for (at, subject) in rows.iter().filter(|(at, _)| *at >= from) {
                let at = *at;
                for id in bead_ids(subject, prefix) {
                    let t = landed.entry(id).or_insert(at);
                    *t = (*t).max(at);
                }
            }
            let mut hourly = vec![0u64; 24];
            for at in landed.values() {
                if let Some(age) = now.checked_sub(*at).filter(|a| *a < day) {
                    hourly[(23 - age / 3600) as usize] += 1;
                }
            }
            out.push(RepoLanding { repo: row.name, hourly, last_landing: landed.values().copied().max().unwrap_or(0), waiting: 0, oldest: None, undated: None, beads: vec![] });
        }
        Ok(out)
    }

    /// The waits at every lifecycle state for `watch.transitions`: `spira-lc dwell` over the
    /// control store, and the newest activation among the release machine records. Anything
    /// unreadable, or no release ever recorded, answers BLIND rather than an empty table.
    pub fn transitions_snapshot(&self, input: &str) -> Result<String, String> {
        let bounded = World { observe_bound: Some((std::time::Instant::now(), crate::tasks::e::watch_transitions::STORE_BOUND)), ..self.clone() };
        bounded.transitions_observed(input)
    }

    fn transitions_observed(&self, input: &str) -> Result<String, String> {
        self.transitions_phased(input, Arc::new(|_| {}))
    }

    /// Every phase of the observe runs on a worker the caller waits for only what is left of the
    /// bound, so a phase that hangs is a BLIND snapshot naming it. `enter` is called as each phase begins.
    fn transitions_phased(&self, input: &str, enter: Arc<dyn Fn(&str) + Send + Sync>) -> Result<String, String> {
        let progress: Arc<Mutex<Phases>> = Arc::default();
        let Some((began, total)) = self.observe_bound else {
            return self.transitions_phases(input, &progress, &*enter);
        };
        let (world, text, track) = (self.clone(), input.to_string(), Arc::clone(&progress));
        let answered = run_within(total.saturating_sub(began.elapsed()) + PHASE_GRACE, move || world.transitions_phases(&text, &track, &*enter));
        let phases = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.push_phase_durations(&phases);
        answered.unwrap_or_else(|| {
            let why = format!("store unavailable: the {} phase of the observe had not answered in its bound of {} s", phases.current, total.as_secs());
            let blind = crate::tasks::e::watch_transitions::Snapshot { activated_at: crate::watchers::last_activation(&self.run_dir), activation: crate::watchers::current_activation(&self.run_dir), maintenance: spira_world::freeze::read_maintenance(&self.run_dir), halt: read_halt(&self.run_dir), ..crate::tasks::e::watch_transitions::blind(&why) };
            serde_json::to_string(&blind).map_err(|e| e.to_string())
        })
    }

    fn push_phase_durations(&self, phases: &Phases) {
        let mut points: Vec<spira_otel::metrics::Point> = phases.done.iter().map(|(name, secs)| spira_otel::metrics::Point::Gauge { name, value: *secs }).collect();
        points.push(spira_otel::metrics::Point::Gauge { name: "spira_observe_phase_seconds_open", value: phases.since.elapsed().as_secs_f64() });
        let sink = spira_otel::metrics::MetricsSink::for_component(self.metrics_endpoint.as_deref(), "spira-runtime");
        sink.push(&points, &[("component", "spira-runtime"), ("operation", "watch.transitions"), ("open_phase", phases.current)]);
    }

    fn transitions_phases(&self, input: &str, progress: &Mutex<Phases>, enter: &dyn Fn(&str)) -> Result<String, String> {
        use crate::tasks::e::watch_transitions as wt;
        #[derive(serde::Deserialize)]
        struct P {
            window_s: u64,
            certify_par: u64,
            releases: PathBuf,
            release_repo: String,
        }
        let p: P =
            serde_json::from_str(input).map_err(|e| format!("transitions params unparsed: {e}"))?;
        let phase = |name: &'static str| {
            progress.lock().unwrap_or_else(|e| e.into_inner()).enter(name);
            enter(name);
        };
        let ancestor = wt::repo_ancestor(&self.repo_map, &p.release_repo);
        phase("dwell");
        let mut snap = wt::observe(
            &self.run_dir,
            &p.releases,
            &p.release_repo,
            &|argv| self.lc_json(argv),
            &ancestor,
            p.window_s,
        );
        phase("open_rounds");
        snap.open_rounds = self.open_rounds();
        snap.halt = read_halt(&self.run_dir);
        phase("mark_landed");
        wt::mark_landed(&mut snap, &|repo, tip| {
            let root = wt::repo_root(&self.repo_map, repo).or_else(|_| wt::repo_root(&self.repo_map, &p.release_repo))?;
            wt::ancestor_in(&root, tip, &crate::tasks::e::watch_transitions::landing_ref(&root))
        });
        phase("mark_gated");
        let pool = spira_config::admission::gate_occupancy(&self.run_dir, p.certify_par);
        wt::mark_gated(
            &mut snap,
            &wt::gates_of(&landing_pass::gateq::GateQueue::new(&self.run_dir).pending(), &pool.holders),
            &landing_pass::records::gate_backoffs(&self.run_dir),
        );
        snap.gate_slots_free = p.certify_par.saturating_sub(pool.used());
        if let Ok(map) = fs::read_to_string(&self.repo_map) {
            let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), self.home_repo.clone())]);
            let reg = spira_config::repos::Registry::new(Some(&map), &env, Path::new(""));
            wt::mark_prs(&mut snap, &|repo| reg.land(repo) == "pr");
        }
        snap.landing_quiet_s = spira_config::ticked::quiet_secs(&self.run_dir, "landing");
        progress.lock().unwrap_or_else(|e| e.into_inner()).enter("done");
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    pub fn round_record_partition(&self) -> install::round_records::Partition {
        if !self.batches_on_store {
            return install::round_records::Partition { clear: vec![], held: vec![] };
        }
        let (named, bare): (Vec<_>, Vec<_>) = install::round_records::round_files(&self.queue_dir).into_iter().partition(|f| !f.batch.is_empty() || !f.pr.is_empty());
        let mut parts = install::round_records::partition(&named, |batch| match self.lc_json(&["show-batch", batch]) {
            Ok(v) => Ok(v["state"].as_str().map(str::to_string)),
            Err(e) if e.trim() == "{}" => Ok(None),
            Err(e) => Err(e),
        });
        for f in &bare {
            if f.is_forge_state() {
                if let Some(why) = self.forge_state_refusal(&f.path) {
                    parts.held.push(why);
                }
            } else {
                parts.held.push(format!("{} names no round, so it cannot be told from a live record", f.path.display()));
            }
        }
        parts
    }

    /// Why a bare `forge-state` file is refused: `None` while the store holds an open batch for its repo, the file's live record.
    fn forge_state_refusal(&self, path: &Path) -> Option<String> {
        let repo = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let lc = queue::real::RealLc { bin: None, client: Some(spira_store::lc_call::LcClient::socket(&self.lc_socket)) };
        match queue::batches::open_batches(&lc, &repo) {
            Ok(open) if !open.is_empty() => None,
            Ok(_) => Some(format!("{} names no round and the store holds no open batch for {repo}", path.display())),
            Err(e) => Some(format!("{} names no round and the store cannot tell whether {repo} has an open batch: {e}", path.display())),
        }
    }

    /// Removes the round record files in `input` (a JSON list of paths) that the store still says
    /// nothing live names, re-asked now: a file whose round opened since the observation stays.
    pub fn clear_round_records(&self, input: &str) -> Result<String, String> {
        let asked: Vec<PathBuf> = serde_json::from_str::<Vec<String>>(input).map_err(|e| format!("round record paths unparsed: {e}"))?.into_iter().map(PathBuf::from).collect();
        let parts = self.round_record_partition();
        let mut cleared = 0;
        for path in parts.clear.iter().filter(|p| asked.contains(p)) {
            fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
            cleared += 1;
        }
        Ok(format!("cleared {cleared} of {}; {} refused", asked.len(), parts.held.len()))
    }

    /// Records each non-empty line of `input` in the Concierge inbox through the inbox's one append-and-push leg (`inbox_rules::record`),
    /// so a line matching the push pattern wakes the session as an `inbox-triage append` does.
    pub fn inbox_append(&self, input: &str) -> Result<String, String> {
        let flusher = || match self.inbox_flusher.as_os_str().is_empty() {
            true => spira_config::release_env::tool("inbox-triage").map(PathBuf::from),
            false => Ok(self.inbox_flusher.clone()),
        };
        let lines: Vec<&str> = input.lines().filter(|l| !l.is_empty()).collect();
        let cursor = watchd::paths::cursorfile(&self.run_dir.to_string_lossy(), INBOX_WATCHER);
        let publisher = || self.inbox_bus.publisher("spira-runtime");
        let pushed = inbox_rules::record_lines(&self.inbox, &lines, &self.push_wake, &self.push_pattern, flusher, &cursor, publisher)?;
        Ok(if pushed { "appended, pushed" } else { "appended" }.into())
    }
}

#[derive(serde::Deserialize)]
struct ThroughputParams {
    now: u64,
    abandon_window_s: u64,
}

/// The phase an observe is in and how long each finished one took, seconds, as gauge names.
#[derive(Clone)]
struct Phases {
    current: &'static str,
    since: std::time::Instant,
    done: Vec<(&'static str, f64)>,
}

impl Default for Phases {
    fn default() -> Self {
        Phases { current: "start", since: std::time::Instant::now(), done: vec![] }
    }
}

impl Phases {
    fn enter(&mut self, next: &'static str) {
        let name = match self.current {
            "dwell" => "spira_observe_phase_seconds_dwell",
            "open_rounds" => "spira_observe_phase_seconds_open_rounds",
            "mark_landed" => "spira_observe_phase_seconds_mark_landed",
            "mark_gated" => "spira_observe_phase_seconds_mark_gated",
            _ => "",
        };
        if !name.is_empty() {
            self.done.push((name, self.since.elapsed().as_secs_f64()));
        }
        self.current = next;
        self.since = std::time::Instant::now();
    }
}

/// How long the observe's worker is awaited past the bound, so a store read that gives up on the bound names itself before the worker is abandoned.
const PHASE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// `f` on a worker, abandoned when it has not answered within `left`: `None`.
fn run_within<T: Send + 'static>(left: std::time::Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let task = spira_store::lc_call::current_task();
    std::thread::spawn(move || {
        let _ = tx.send(spira_store::lc_call::as_task_of(task.as_deref(), f));
    });
    rx.recv_timeout(left).ok()
}

/// One store call held to what is left of its observe's bound; an absent store, a silent one, or no time left,
/// answers `store unavailable`, which an observe's snapshot carries as its `why`.
fn lc_bounded(client: &spira_store::lc_call::LcClient, total: std::time::Duration, spent: std::time::Duration, argv: &[&str]) -> Result<String, String> {
    use spira_store::lc_call::LcError;
    let left = total.saturating_sub(spent);
    if left.is_zero() {
        return Err(format!("store unavailable: tried for {} s, the observe's bound, before spira-lc {}", total.as_secs(), argv.join(" ")));
    }
    client.within(left).answer(argv).map_err(|e| match e {
        LcError::NotRunning(m) | LcError::Fault(m) => format!("store unavailable: {m}"),
        LcError::ReadEnded(m) => format!("read ended at the store's read limit: {m}"),
        e => e.to_string(),
    })
}

use crate::tasks::e::watch_throughput::{RepoLanding, RepoRounds, Records, Snapshot as ThroughputSnapshot, Unlanded, Waiting};

fn to_json(s: &ThroughputSnapshot) -> Result<String, String> {
    serde_json::to_string(s).map_err(|e| e.to_string())
}

/// The distinct bead ids (`<prefix>-<word>`) named in commit subjects.
pub fn bead_ids(subjects: &str, prefix: &str) -> BTreeSet<String> {
    let lead = format!("{prefix}-");
    subjects
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .filter(|w| w.starts_with(&lead) && w.len() > lead.len() && w[lead.len()..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
        .map(|w| w.trim_end_matches('.').to_string())
        .collect()
}

fn round_unended(state: &str) -> bool {
    !matches!(state, "LANDED" | "SETTLED" | "ABANDONED" | "DISCARDED")
}

/// Per repo, batches short of a terminal state in any phase; a row naming no repo is skipped.
pub fn open_rounds_by_repo(batches: &[Value]) -> BTreeMap<String, u64> {
    let mut by = BTreeMap::new();
    for b in batches {
        let (Some(repo), Some(state)) = (b["repo"].as_str(), b["state"].as_str()) else { continue };
        if round_unended(state) {
            *by.entry(repo.to_string()).or_default() += 1;
        }
    }
    by
}

/// Per repo: rounds cut over the batch rows' day, how each ended, and how many ended inside
/// `recent_s` (by the batch's last applied event, not by when it was cut).
pub fn round_counts(batches: &[Value], now: u64, recent_s: u64) -> Vec<RepoRounds> {
    let mut by: BTreeMap<String, RepoRounds> = BTreeMap::new();
    for b in batches {
        let Some(repo) = b["repo"].as_str() else { continue };
        let r = by.entry(repo.to_string()).or_insert_with(|| RepoRounds { repo: repo.to_string(), ..Default::default() });
        let recent = b["last_at"].as_i64().is_some_and(|t| now.saturating_sub(t.max(0) as u64) <= recent_s);
        r.cut += 1;
        r.green_runs += b["passes"].as_array().map_or(0, |p| p.iter().filter(|p| p["verdict"] == "green").count() as u64);
        match b["state"].as_str() {
            Some("ABANDONED") => {
                r.abandoned += 1;
                r.abandoned_recent += u64::from(recent);
            }
            Some("LANDED" | "SETTLED") => {
                r.landed += 1;
                r.landed_recent += u64::from(recent);
            }
            _ => {}
        }
    }
    by.into_values().collect()
}

/// A round's merge commits carry its cut time, so the lifecycle's LANDED time is when the ref
/// moved. A repo with no LANDED round in the window keeps the ref's commit time.
pub fn stamp_landings(landings: &mut [RepoLanding], batches: &[Value]) {
    for l in landings {
        let landed = batches.iter().filter(|b| b["repo"].as_str() == Some(l.repo.as_str()) && b["state"] == "LANDED").filter_map(|b| b["last_at"].as_i64()).map(|t| t.max(0) as u64).max();
        if let Some(t) = landed {
            l.last_landing = t;
        }
    }
}

/// Each repo's waiting count, its oldest dated waiting bead, and the first with a null `since`.
pub fn stamp_waiting(landings: &mut [RepoLanding], rows: &[Value]) {
    for l in landings {
        let mine: Vec<Waiting> = rows.iter().filter(|r| r["repo"].as_str() == Some(l.repo.as_str())).map(|r| Waiting { bead: r["bead_id"].as_str().unwrap_or_default().to_string(), since: as_u64(&r["since"]) }).collect();
        l.waiting = mine.len() as u64;
        l.beads = mine.iter().map(|w| w.bead.clone()).collect();
        l.undated = mine.iter().find(|w| w.since.is_none()).map(|w| w.bead.clone());
        l.oldest = mine.into_iter().filter(|w| w.since.is_some()).min_by_key(|w| w.since);
    }
}

/// Rounds that passed CI and sit in a state short of landed, with the seconds since their last event.
pub fn unlanded_rounds(batches: &[Value], now: u64) -> Vec<Unlanded> {
    batches
        .iter()
        .filter_map(|b| {
            let state = b["state"].as_str().filter(|s| matches!(*s, "GREEN" | "LANDING"))?;
            let last = b["last_at"].as_i64()?.max(0) as u64;
            Some(Unlanded { repo: b["repo"].as_str()?.to_string(), batch: b["batch_id"].as_str()?.to_string(), state: state.to_string(), age_s: now.saturating_sub(last) })
        })
        .collect()
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// A JSON number, or a string of digits, as a float; anything else is zero.
fn as_num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0.0)
}

fn strings(v: &Value) -> Vec<String> {
    let parsed = v.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok());
    parsed.as_ref().unwrap_or(v).as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// The raise of the manual hold a listed row stands under; `None` when its record has none.
fn manual_hold_of(r: &Value) -> Option<&Value> {
    Some(&r["manual_hold"]).filter(|m| m.is_object())
}

fn sweep_row(r: &Value) -> Option<Row> {
    let since = [&r["since"], &r["updated_at"]].into_iter().map(as_num).find(|n| *n != 0.0);
    Some(Row {
        id: r["bead_id"].as_str()?.to_string(),
        state: r["state"].as_str().unwrap_or("").to_string(),
        holds: strings(&r["holds"]),
        since,
        blocked_by: strings(&r["blocked_by"]),
        reason: r["reason"].as_str().unwrap_or("").to_string(),
        lease_until: as_num(&r["lease_until"]),
        holder: match &r["holder"] {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        },
        hold_review: None,
        held_at: None,
        claim: r["claim_view"]["verdict"].as_str().map(str::to_string),
        claim_unset: r["claim"].is_null(),
        claim_why: claim_why(&r["claim_view"]),
        express: spira_store::lc_state::express(r.get("express")).unwrap_or(false),
    })
}

fn claim_why(view: &Value) -> Option<String> {
    let missing = strings(&view["missing"]);
    if missing.is_empty() {
        return view["error"].as_str().map(str::to_string);
    }
    Some(missing.iter().map(|m| format!("no {m}")).collect::<Vec<_>>().join(", "))
}

fn sweep_bead(b: &Value) -> Option<Bead> {
    Some(Bead {
        id: b["id"].as_str()?.to_string(),
        labels: strings(&b["labels"]),
        issue_type: b["issue_type"].as_str().map(str::to_string),
        status: b["status"].as_str().unwrap_or("").to_string(),
        priority: b["priority"].as_i64().unwrap_or(2),
        title: b["title"].as_str().unwrap_or("").to_string(),
    })
}

#[cfg(test)]
mod pr_snapshot_tests {
    use super::*;
    use crate::forge::tests::{answer, forge_at, stub};
    use crate::watchers::pr_notify::{decide, Carry, Deploy, DeployRun, PrNotify, RepoCarry, Said, Snapshot, Tracked};

    fn target(name: &str, land: &str) -> PrTarget {
        PrTarget { name: name.into(), slug: Some("o/r".into()), land: land.into(), ..PrTarget::default() }
    }

    fn snapshot_of(records: &RecordedPrs, targets: &[PrTarget], forge: &Forge, carry: &Carry) -> Result<String, String> {
        pr_snapshot_from(records, targets, forge, carry, &|_, _| None)
    }

    const OLD: &str = "bdab763500000000";
    const NEW: &str = "70bf904e00000000";
    const GITEA: &str = "git@gitea.example:acme/pipeline.git";

    fn checkout(root: &Path, name: &str, origin: &str) -> MapRow {
        let dir = root.join(name);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/config"), format!("[remote \"origin\"]\n\turl = {origin}\n")).unwrap();
        MapRow { name: name.into(), path: dir.display().to_string(), land: "queue.local".into(), base: "local/main".into() }
    }

    fn at_tip(name: &str, tip: &str) -> Carry {
        Carry { repos: BTreeMap::from([(name.to_string(), RepoCarry { tip: tip.into(), ..Default::default() })]) }
    }

    fn judge(carry: &Carry, out: &str, now: u64) -> (Carry, Said) {
        decide(&PrNotify { interval_ms: 1, deploy_giveup_secs: 100 }, carry, &snapshot(out), now)
    }

    #[test_id(tulzkqf)]
    #[test]
    fn a_repo_whose_origin_is_not_github_is_not_live_and_quotes_its_declared_deploy() {
        let tmp = testkit::TempDir::new("pr-gitea");
        let targets = pr_targets(vec![checkout(tmp.path(), "pipeline", GITEA)], "spira", &|_| Some(NEW.into()));
        let (api, seen) = stub(vec![]);
        let carry = at_tip("pipeline", OLD);
        let declared = "on the host, install.sh --pipeline builds it; ## probe $(x)";
        let out = pr_snapshot_from(&RecordedPrs::new(), &targets, &forge_at(api), &carry, &|_, spec| (spec == format!("{NEW}:.spira/deploy.toml")).then(|| format!("after_landing = \"{declared}\"\n"))).unwrap();
        assert!(seen.join().unwrap().is_empty(), "no forge is asked about a repository that is not on GitHub");
        let (after, said) = judge(&carry, &out, 1000);
        assert_eq!(said.notes.len(), 1);
        assert_eq!(said.notes[0].subject, "NOT LIVE [pipeline] 70bf904e0");
        assert_eq!(said.notes[0].body, format!("NOT LIVE [pipeline] 70bf904e0: origin {GITEA} is not a GitHub repository; no deploy run is observable\nDeclared deploy: {declared}\n"));
        assert_eq!(said.lines, [format!("NOT LIVE [pipeline] 70bf904e0: origin {GITEA} is not a GitHub repository; no deploy run is observable")]);
        assert_eq!(judge(&after, &out, 1090).1, Said::default());
    }

    #[test_id(tv7qebm)]
    #[test]
    fn a_login_in_an_origin_url_is_not_repeated_to_the_operator() {
        let tmp = testkit::TempDir::new("pr-login");
        let url = format!("https://{}gitea.example/acme/pipeline.git", concat!("someone:hunter2", "@"));
        let targets = pr_targets(vec![checkout(tmp.path(), "pipeline", &url)], "spira", &|_| None);
        assert_eq!(targets[0].origin, "https://gitea.example/acme/pipeline.git");
    }

    #[test_id(tq8jc60)]
    #[test]
    fn a_queue_local_repo_on_github_is_asked_by_its_slug() {
        let tmp = testkit::TempDir::new("pr-github");
        let rows = vec![checkout(tmp.path(), "cubby", "git@github.com:acme/cubby.git")];
        let targets = pr_targets(rows, "spira", &|_| Some(NEW.into()));
        assert_eq!((targets[0].slug.as_deref(), targets[0].tip.as_deref()), (Some("acme/cubby"), Some(NEW)));
    }

    #[test_id(tolpjwq)]
    #[test]
    fn the_home_repo_gets_no_deploy_verdict() {
        let tmp = testkit::TempDir::new("pr-home");
        let rows = vec![checkout(tmp.path(), "spira", "git@github.com:acme/spira.git")];
        let targets = pr_targets(rows, "spira", &|_| Some(NEW.into()));
        let (api, seen) = stub(vec![]);
        let mut carry = at_tip("spira", OLD);
        carry.repos.get_mut("spira").unwrap().deploys.push(Deploy { sha: OLD.into(), n: 7, at: 1000, ..Default::default() });
        let out = snapshot_of(&RecordedPrs::new(), &targets, &forge_at(api), &carry).unwrap();
        assert!(seen.join().unwrap().is_empty(), "no deploy run is asked for");
        let (waiting, said) = judge(&carry, &out, 1050);
        assert_eq!((said, waiting.repos["spira"].deploys.len()), (Said::default(), 1), "the PR's merged topic is carried as before");
        let (after, said) = judge(&waiting, &out, 5000);
        assert_eq!((said, after.repos["spira"].deploys.len()), (Said::default(), 0));
        let gitea = pr_targets(vec![checkout(tmp.path(), "spira", GITEA)], "spira", &|_| Some(NEW.into()));
        assert!(gitea.is_empty());
    }

    #[test_id(t9hc2m7)]
    #[test]
    fn an_unanswered_deploy_listing_is_not_a_verdict() {
        let mut carry = Carry::default();
        carry.repos.entry("r".into()).or_default().deploys.push(Deploy { sha: NEW.into(), n: 7, at: 1000, ..Default::default() });
        let (api, seen) = stub(vec![answer("500 Internal Server Error", "", "{}")]);
        let out = snapshot_of(&RecordedPrs::new(), &[target("r", "queue.local")], &forge_at(api), &carry).unwrap();
        assert_eq!(seen.join().unwrap().len(), 1);
        let (after, said) = judge(&carry, &out, 1_000_000);
        assert_eq!((said, after.repos["r"].deploys.len()), (Said::default(), 1));
    }

    #[test_id(to5c871)]
    #[test]
    fn a_404_on_the_deploy_listing_is_no_workflow_and_an_empty_listing_is_not_yet() {
        let (api, seen) = stub(vec![answer("404 Not Found", "", "{}"), answer("200 OK", "", r#"{"workflow_runs":[{"status":"in_progress"}]}"#), answer("200 OK", "", r#"{"message":"odd"}"#)]);
        let forge = forge_at(api);
        assert_eq!(forge.deploy_run("o/r", "a"), Ok(DeployRun::NoWorkflow));
        assert_eq!(forge.deploy_run("o/r", "b"), Ok(DeployRun::NotYet));
        assert!(forge.deploy_run("o/r", "c").is_err());
        assert!(seen.join().unwrap()[0].starts_with("get /repos/o/r/actions/workflows/deploy.yml/runs?head_sha=a"));
    }

    fn snapshot(out: &str) -> Snapshot {
        serde_json::from_str(out).unwrap()
    }

    fn tracking(n: u64) -> Carry {
        let rc = RepoCarry { prs: BTreeMap::from([(n, Tracked { status: "green".into(), title: "t".into(), ..Default::default() })]), ..Default::default() };
        Carry { repos: BTreeMap::from([("r".to_string(), rc)]) }
    }

    #[test_id(th8nekc)]
    #[test]
    fn nothing_recorded_nothing_tracked_and_no_deploy_makes_no_forge_call() {
        let (api, seen) = stub(vec![]);
        let out = snapshot_of(&RecordedPrs::new(), &[target("a", "queue"), target("r", "pr")], &forge_at(api), &Carry::default()).unwrap();
        assert!(seen.join().unwrap().is_empty());
        assert!(snapshot(&out).repos.iter().all(|(_, o)| o.open == Some(vec![])));
    }

    #[test_id(t8r1wx7)]
    #[test]
    fn a_recorded_pr_is_asked_by_number_and_its_checks_read() {
        let pr = r#"{"number":12,"state":"open","title":"batch","head":{"sha":"abc","ref":"spira/b"}}"#;
        let checks = r#"{"check_runs":[{"name":"gate","status":"completed","conclusion":"success","check_suite":{"id":1}}]}"#;
        let runs = r#"{"workflow_runs":[]}"#;
        let (api, seen) = stub(vec![answer("200 OK", "", pr), answer("200 OK", "", checks), answer("200 OK", "", runs)]);
        let records = RecordedPrs::from([("r".to_string(), Ok(vec![12]))]);
        let out = snapshot_of(&records, &[target("r", "queue")], &forge_at(api), &Carry::default()).unwrap();
        let seen = seen.join().unwrap();
        assert!(seen[0].starts_with("get /repos/o/r/pulls/12 "), "{}", seen[0]);
        assert!(seen[1].contains("/commits/abc/check-runs") && seen[2].contains("head_sha=abc")); // not a path: request URL path
        assert!(seen.iter().all(|r| !r.contains("state=open")));
        let open = snapshot(&out).repos[0].1.open.clone().unwrap();
        assert_eq!((open[0].number, open[0].status.as_str()), (12, "green"));
    }

    #[test_id(tfwsbcc)]
    #[test]
    fn a_tracked_pr_no_longer_recorded_that_closed_gets_its_final_state_and_no_check_read() {
        let closed = r#"{"number":12,"state":"closed","merged":false,"head":{"sha":"abc","ref":"b"}}"#;
        let (api, seen) = stub(vec![answer("200 OK", "", closed), answer("200 OK", "", closed)]);
        let out = snapshot_of(&RecordedPrs::new(), &[target("r", "queue")], &forge_at(api), &tracking(12)).unwrap();
        let seen = seen.join().unwrap();
        assert!(seen.iter().all(|r| r.starts_with("get /repos/o/r/pulls/12 ")), "{seen:?}");
        let obs = snapshot(&out).repos[0].1.clone();
        assert_eq!(obs.finals[&12].state, "CLOSED");
        assert_eq!(obs.open, Some(vec![]));
    }

    #[test_id(tu6c3vs)]
    #[test]
    fn the_publish_record_names_its_pr_and_an_unreadable_one_is_an_error_not_no_pr() {
        let tmp = testkit::TempDir::new("pr-snap");
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("r")).unwrap();
        fs::create_dir_all(dir.join("bad").join("publish")).unwrap();
        fs::write(dir.join("r").join("publish"), "pr=41\nhead=x\n").unwrap();
        assert_eq!(publish_pr(&dir, "r"), Ok(Some(41)));
        assert_eq!(publish_pr(&dir, "none"), Ok(None));
        assert!(publish_pr(&dir, "bad").is_err());
    }

    #[test_id(tqgb9vp)]
    #[test]
    fn strays_are_listed_only_on_the_tick_a_tracked_pr_leaves_the_open_set() {
        let (api, seen) = stub(vec![]);
        snapshot_of(&RecordedPrs::new(), &[target("r", "pr")], &forge_at(api), &Carry::default()).unwrap();
        assert!(seen.join().unwrap().is_empty(), "a quiet tick lists no branches");

        let closed = r#"{"number":12,"state":"closed","merged":false,"head":{"sha":"abc","ref":"b"}}"#;
        let (api, seen) = stub(vec![answer("200 OK", "", closed), answer("200 OK", "", closed), answer("200 OK", "", "[]")]);
        let out = snapshot_of(&RecordedPrs::new(), &[target("r", "pr")], &forge_at(api), &tracking(12)).unwrap();
        let seen = seen.join().unwrap();
        assert!(seen[2].starts_with("get /repos/o/r/branches"), "{}", seen[2]);
        assert_eq!(snapshot(&out).repos[0].1.strays, Some(vec![]));
    }

    // covers: UC-operator-channel-38
    #[test_id(t9ikldf)]
    #[test]
    fn a_push_or_hold_repo_is_never_searched_for_strays_when_a_tracked_pr_leaves() {
        for land in ["push", "hold", "queue"] {
            let closed = r#"{"number":12,"state":"closed","merged":false,"head":{"sha":"abc","ref":"b"}}"#;
            let (api, seen) = stub(vec![answer("200 OK", "", closed), answer("200 OK", "", closed), answer("200 OK", "", "[]")]);
            let addr = api.trim_start_matches("http://").to_string();
            let out = snapshot_of(&RecordedPrs::new(), &[target("r", land)], &forge_at(api), &tracking(12)).unwrap();
            // The stub waits for a third request: a branch listing would be it, so a probe stands in
            // for it and is told apart by carrying no path.
            drop(std::net::TcpStream::connect(addr));
            let seen = seen.join().unwrap();
            assert!(seen.iter().all(|r| !r.contains("/branches")), "{land}: {seen:?}");
            assert_eq!(snapshot(&out).repos[0].1.strays, None, "{land}");
        }
    }

    #[test_id(tpv3fnx)]
    #[test]
    fn a_repo_whose_records_cannot_be_read_is_unobserved_and_makes_no_call() {
        let (api, seen) = stub(vec![]);
        let records = RecordedPrs::from([("r".to_string(), Err("store down".to_string()))]);
        let out = snapshot_of(&records, &[target("r", "pr")], &forge_at(api), &tracking(12)).unwrap();
        assert!(seen.join().unwrap().is_empty());
        assert_eq!(snapshot(&out).repos[0].1.open, None);
    }
}

#[cfg(test)]
mod sweep_row_tests {
    use super::*;

    #[test_id(thx6bje)]
    #[test]
    fn a_list_row_keeps_the_claim_views_verdict() {
        let r = sweep_row(&serde_json::json!({"bead_id": "sp-r", "state": "REWORK", "claim_view": {"verdict": "claimable", "blocker": null, "runes": ["a"]}})).unwrap();
        assert_eq!(r.claim.as_deref(), Some("claimable"));
        let bare = sweep_row(&serde_json::json!({"bead_id": "sp-r"})).unwrap();
        assert_eq!(bare.claim, None);
        assert!(sweep_row(&serde_json::json!({"bead_id": "sp-r", "claim": null})).unwrap().claim_unset);
        assert!(!sweep_row(&serde_json::json!({"bead_id": "sp-r", "claim": {"blocked_by": []}})).unwrap().claim_unset);
        let why = |view: serde_json::Value| sweep_row(&serde_json::json!({"bead_id": "sp-r", "claim": {}, "claim_view": view})).unwrap().claim_why;
        assert_eq!(why(serde_json::json!({"verdict": "cannot-tell", "missing": ["repo"]})).as_deref(), Some("no repo"));
        assert_eq!(why(serde_json::json!({"verdict": "cannot-tell", "missing": ["claim", "repo"]})).as_deref(), Some("no claim, no repo"));
        assert_eq!(why(serde_json::json!({"verdict": "cannot-tell", "missing": [], "error": "store down"})).as_deref(), Some("store down"));
        assert_eq!(why(serde_json::json!({"verdict": "claimable", "missing": []})), None);
    }

    #[test_id(t10vtx3)]
    #[test]
    fn a_list_rows_express_column_reads_as_a_flag() {
        let express = |v: serde_json::Value| sweep_row(&serde_json::json!({"bead_id": "sp-r", "express": v})).unwrap().express;
        assert!(express(serde_json::json!("1")) && express(serde_json::json!(true)) && express(serde_json::json!("true")));
        assert!(!express(serde_json::json!("0")) && !express(serde_json::json!(false)) && !express(serde_json::Value::Null));
        assert!(!sweep_row(&serde_json::json!({"bead_id": "sp-r"})).unwrap().express);
    }
}

#[cfg(test)]
mod last_hold_tests {
    use super::*;

    #[test_id(tefw59o)]
    #[test]
    fn the_newest_hold_gives_its_seq_and_the_detail_of_its_object_evidence() {
        let h = r#"[
          {"seq":915000,"event":"Hold","evidence":{"Hold":{"kind":"Wait","cause":"c","detail":"older"}}},
          {"seq":916913,"event":"Hold","evidence":{"Hold":{"kind":"Ask","cause":"operator-question","detail":"4 claims ended without progress inside 180m; last: x"}}}
        ]"#;
        assert_eq!(last_hold(h), Some(("916913".into(), "4 claims ended without progress inside 180m; last: x".into())));
    }

    #[test_id(tq7vzhm)]
    #[test]
    fn a_history_with_no_hold_has_no_last_hold() {
        assert_eq!(last_hold(r#"[{"seq":1,"event":"Claim","evidence":{}}]"#), None);
    }
}

#[cfg(test)]
mod publish_backlog_tests {
    use super::*;
    use runtime::ports::{ProcOutput, ProcSpec};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recording(Mutex<Vec<ProcSpec>>);

    impl Proc for Recording {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            self.0.lock().unwrap().push(spec.clone());
            Ok(ProcOutput { code: Some(0), stdout: vec![], stderr: vec![] })
        }
    }

    fn row(dir: &Path, name: &str, land: &str) -> MapRow {
        let path = dir.join(name);
        fs::create_dir_all(path.join(".git")).unwrap();
        MapRow { name: name.into(), path: path.to_string_lossy().into(), land: land.into(), base: String::new() }
    }

    fn measured(count: u64) -> Read {
        Read::Measured { count, arrival: None, forge_tip_age_secs: None }
    }

    #[test_id(tqz6yv3)]
    #[test]
    fn the_backlog_snapshot_refreshes_every_queue_local_repo_before_measuring_it() {
        let tmp = testkit::TempDir::new("backlog");
        let rows = vec![row(tmp.path(), "a", "queue.local"), row(tmp.path(), "b", "queue.local"), row(tmp.path(), "q", "queue")];
        let calls = Mutex::new(vec![]);
        publish_backlog_snapshot_from(
            &rows,
            &|r| calls.lock().unwrap().push(format!("refresh {}", r.name)),
            &|n| {
                calls.lock().unwrap().push(format!("measure {n}"));
                Ok(Some(measured(1)))
            },
        )
        .unwrap();
        let mut calls = calls.into_inner().unwrap();
        let measures = calls.split_off(2);
        calls.sort();
        assert_eq!(calls, ["refresh a", "refresh b"]);
        assert_eq!(measures, ["measure a", "measure b"]);
    }

    #[test_id(t0c73oq)]
    #[test]
    fn a_refresh_that_hangs_or_fails_still_yields_a_measurement() {
        let tmp = testkit::TempDir::new("backlog");
        let rows = vec![row(tmp.path(), "a", "queue.local"), row(tmp.path(), "b", "queue.local")];
        let out = publish_backlog_snapshot_from(&rows, &|r| assert_ne!(r.name, "a", "refresh failed"), &|_| Ok(Some(measured(3)))).unwrap();
        let snap: publish_backlog::Snapshot = serde_json::from_str(&out).unwrap();
        assert_eq!(snap.repos, vec![("a".to_string(), measured(3)), ("b".to_string(), measured(3))]);
    }

    #[test_id(t8j0xi6)]
    #[test]
    fn the_production_refresh_fetches_the_forge_branch_with_the_git_it_was_handed() {
        let tmp = testkit::TempDir::new("backlog");
        let alpha = row(tmp.path(), "alpha", "queue.local");
        let refs = Path::new(&alpha.path).join(".git/refs/heads/local");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), format!("{}\n", "a".repeat(40))).unwrap();
        let map = format!("alpha|{}|queue.local|local/main||\n", alpha.path);
        let reg = spira_config::repos::Registry::new(Some(&map), &BTreeMap::new(), tmp.path());
        let rec = Recording::default();
        let handed = tmp.path().join("handed-git");

        forge_refresh(&rec, Some(reg))(&handed, &alpha);

        let specs = rec.0.into_inner().unwrap();
        assert_eq!(specs.len(), 1, "one fetch through the port: {specs:?}");
        assert_eq!(specs[0].prog, handed.display().to_string(), "the fetch ran the handed git");
        assert_eq!(specs[0].args, ["-C", alpha.path.as_str(), "fetch", "--quiet", "origin", "main"]);
    }

    #[test_id(tpr5txo)]
    #[test]
    fn the_snapshot_records_the_unpublished_range_it_measured_and_runs_no_verb_that_measures() {
        let dir = testkit::TempDir::new("world-publish-backlog");
        let alpha = row(dir.path(), "alpha", "queue.local");
        let map = dir.path().join("repos");
        fs::write(&map, format!("alpha|{}|queue.local|local/main||\n", alpha.path)).unwrap();

        let sock = dir.path().join("lc.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let asked: std::sync::Arc<Mutex<Vec<Vec<String>>>> = Default::default();
        let seen = asked.clone();
        std::thread::spawn(move || {
            runtime::socket::answer(&listener, &|argv| {
                seen.lock().unwrap().push(argv.to_vec());
                match argv.first().map(String::as_str) {
                    Some("git-fact") => Ok("{}".into()),
                    _ => Err(runtime::RuntimeError::new("the double has no such verb")),
                }
            })
        });

        let handed = dir.path().join("handed-git");
        let world = World { lc_socket: sock, repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), git_bin: Some(handed.clone()), ..World::default() };
        let refreshed = Mutex::new(vec![]);
        let measured_with = Mutex::new(vec![]);
        let snapshot = world
            .publish_backlog_through(&|git: &Path, _: &MapRow| refreshed.lock().unwrap().push(git.to_path_buf()), &|git, name, now| {
                measured_with.lock().unwrap().push((git.to_path_buf(), name.to_string()));
                Ok(crate::unpublished::Measurement::Measured(crate::unpublished::Measured { landing_tip: "landing".into(), forge_tip: "forge".into(), count: 2, oldest_arrival: Some(1_700_000_000), read_at: now }))
            })
            .unwrap();

        assert!(snapshot.contains("\"count\":2"), "{snapshot}");
        assert_eq!(*refreshed.lock().unwrap(), std::slice::from_ref(&handed), "the forge refresh was handed the world's git");
        assert_eq!(*measured_with.lock().unwrap(), [(handed, "alpha".to_string())], "the measurement was handed the world's git");
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "one call, the record: {asked:?}");
        assert_eq!(asked[0][..4], ["git-fact", "put", "unpublished", "--json"]);
        let body: serde_json::Value = serde_json::from_str(&asked[0][4]).unwrap();
        assert_eq!((body["repo"].as_str(), body["count"].as_u64()), (Some("alpha"), Some(2)));
        assert_eq!((body["forge_tip"].as_str(), body["landing_tip"].as_str(), body["oldest_arrival"].as_u64()), (Some("forge"), Some("landing"), Some(1_700_000_000)));
        assert!(body["read_at"].is_u64(), "{body}");
    }
}

#[cfg(test)]
mod bounded_store_tests {
    use super::lc_bounded;
    use spira_store::lc_call::{LcClient, LcError, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Absent(Mutex<Vec<Duration>>);

    impl Transport for Absent {
        fn exchange(&self, _: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            unreachable!("a socket client asks exchange_kind")
        }
        fn exchange_kind(&self, _: &[String], _: Option<&str>, _: Duration, wait: Duration) -> Result<Reply, LcError> {
            self.0.lock().unwrap().push(wait);
            Err(LcError::NotRunning(format!("store not running: nothing was sent; waited {} s", wait.as_secs())))
        }
    }

    #[test_id(tzv6gll)]
    #[test]
    fn a_store_absent_past_the_bound_is_unavailable_within_what_was_left_of_the_observe() {
        let double = Arc::new(Absent(Mutex::default()));
        let client = LcClient::over(double.clone(), true);
        let why = lc_bounded(&client, Duration::from_secs(25), Duration::from_secs(10), &["dwell"]).unwrap_err();
        assert!(why.starts_with("store unavailable:") && why.contains("waited 15 s"), "{why}");
        assert_eq!(*double.0.lock().unwrap(), [Duration::from_secs(15)], "the client was bounded by the time left");
    }

    struct Ended;

    impl Transport for Ended {
        fn exchange(&self, _: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            unreachable!("a socket client asks exchange_kind")
        }
        fn exchange_kind(&self, _: &[String], _: Option<&str>, _: Duration, _: Duration) -> Result<Reply, LcError> {
            Ok(Reply { code: 2, out: "cannot tell: dwell".into(), err: String::new(), read_ended: true })
        }
    }

    #[test_id(tgtm0pn)]
    #[test]
    fn a_read_the_store_ended_at_its_limit_is_reported_as_that_and_not_as_an_unavailable_store() {
        let client = LcClient::over(Arc::new(Ended), true);
        let why = lc_bounded(&client, Duration::from_secs(25), Duration::ZERO, &["dwell"]).unwrap_err();
        assert!(why.starts_with("read ended at the store's read limit"), "{why}");
        assert!(!why.contains("store unavailable"), "{why}");
    }

    #[test_id(txhwgjg)]
    #[test]
    fn a_store_that_accepts_and_never_replies_is_unavailable_inside_the_bound() {
        let t = testkit::TempDir::new("observe-silent");
        let listener = std::os::unix::net::UnixListener::bind(t.path().join("sock")).unwrap();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().flatten().collect();
            drop(held);
        });
        let client = LcClient::socket(t.path().join("sock"));
        let (done, answered) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(lc_bounded(&client, Duration::from_secs(25), Duration::from_secs(24), &["list"])).unwrap()); // BDQ_READ_CONSIDERED="LcClient is the spira-lc door; no bdq"
        let why = answered.recv_timeout(Duration::from_secs(3)).expect("the call returns inside what was left of the bound").unwrap_err(); // clock-safe: caps the wait for the call to return
        assert!(why.starts_with("store unavailable"), "{why}");
    }

    #[test_id(t094c4i)]
    #[test]
    fn an_observe_with_no_time_left_is_unavailable_without_asking_the_store() {
        let double = Arc::new(Absent(Mutex::default()));
        let client = LcClient::over(double.clone(), true);
        let why = lc_bounded(&client, Duration::from_secs(25), Duration::from_secs(26), &["dwell"]).unwrap_err();
        assert!(why.contains("store unavailable") && why.contains("25 s"), "{why}");
        assert!(double.0.lock().unwrap().is_empty());
    }

    #[test_id(t4brhmz)]
    #[test]
    fn an_observe_with_the_store_absent_past_its_bound_answers_unreadable_with_the_store_unavailable() {
        let t = testkit::TempDir::new("observe-bound");
        let rel = t.path().join("releases");
        std::fs::create_dir_all(&rel).unwrap();
        release::machine::enter(&t.path().join("release"), &"a".repeat(40), release::machine::ReleaseState::Activated, "current switched").unwrap();
        let world = super::super::World { run_dir: t.path().into(), repo_map: t.path().join("map"), lc_socket: t.path().join("absent.sock"), observe_bound: Some((std::time::Instant::now(), Duration::from_millis(200))), ..Default::default() };
        let input = serde_json::json!({ "window_s": 3600, "certify_par": 2, "releases": rel, "release_repo": "spira" }).to_string();
        let snap: crate::tasks::e::watch_transitions::Snapshot = serde_json::from_str(&world.transitions_observed(&input).unwrap()).unwrap();
        assert!(!snap.readable && snap.why.contains("store unavailable"), "{}", snap.why);
    }

    fn blocked_in(phase: &'static str, release: Arc<(Mutex<bool>, std::sync::Condvar)>) -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(move |entered| {
            if entered == phase {
                let (m, c) = &*release;
                let _held = c.wait_while(m.lock().unwrap(), |open| !*open).unwrap();
            }
        })
    }

    fn slow_phase_is_blind(phase: &'static str) {
        let t = testkit::TempDir::new("observe-phase");
        let rel = t.path().join("releases");
        std::fs::create_dir_all(&rel).unwrap();
        release::machine::enter(&t.path().join("release"), &"a".repeat(40), release::machine::ReleaseState::Activated, "current switched").unwrap();
        let bound = Duration::from_millis(1500);
        let listener = std::os::unix::net::UnixListener::bind(t.path().join("hangup.sock")).unwrap();
        std::thread::spawn(move || listener.incoming().flatten().for_each(drop));
        let world = super::World { run_dir: t.path().into(), repo_map: t.path().join("map"), lc_socket: t.path().join("hangup.sock"), observe_bound: Some((std::time::Instant::now(), bound)), ..Default::default() };
        let input = serde_json::json!({ "window_s": 3600, "certify_par": 2, "releases": rel, "release_repo": "spira" }).to_string();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let hook = blocked_in(phase, release.clone());
        let snap: crate::tasks::e::watch_transitions::Snapshot = serde_json::from_str(&world.transitions_phased(&input, hook).unwrap()).unwrap();
        assert!(!snap.readable && snap.why.contains("store unavailable") && snap.why.contains(phase), "{}", snap.why);
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    #[test_id(t75yddq)]
    #[test]
    fn a_held_read_that_blocks_past_the_bound_is_blind_naming_the_dwell_phase() {
        slow_phase_is_blind("dwell");
    }

    #[test_id(tnrq5te)]
    #[test]
    fn a_slow_open_rounds_read_is_blind_naming_its_phase() {
        slow_phase_is_blind("open_rounds");
    }

    #[test_id(t5su0ho)]
    #[test]
    fn a_slow_landed_ancestry_is_blind_naming_mark_landed() {
        slow_phase_is_blind("mark_landed");
    }

    #[test_id(t0c9uxm)]
    #[test]
    fn a_held_read_not_ready_within_what_is_left_gives_up() {
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let r2 = release.clone();
        let got = super::super::run_within(Duration::from_millis(200), move || {
            let (m, c) = &*r2;
            drop(c.wait_while(m.lock().unwrap(), |open| !*open).unwrap());
        });
        assert!(got.is_none(), "the worker is still blocked, so the call returned on its bound");
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    #[test_id(tjv9adv)]
    #[test]
    fn a_worker_run_within_a_task_carries_that_task() {
        let got = spira_store::lc_call::as_task("t", || super::super::run_within(Duration::from_secs(5), spira_store::lc_call::current_task));
        assert_eq!(got, Some(Some("t".to_string())));
    }
}

/// The epoch of `hour`:00 in zone `tz`, `days_after` local days after the local day of epoch `at`.
/// The zone conversion is the `date` program's, as in `idea-prompt`. The answer is held for the
/// local day `at` falls in, bounded by the epochs `date` gave for that day's start and the next's.
pub fn cr_open_epoch(tz: &str, hour: u32, at: u64, days_after: u32) -> Result<u64, String> {
    static HELD: std::sync::OnceLock<HeldEpochs> = std::sync::OnceLock::new();
    open_epoch_held(HELD.get_or_init(Default::default), tz, hour, at, days_after, &|args| {
        let out = spira_config::bounded::bounded("date").env("TZ", tz).args(args).output().map_err(|e| format!("date: {e}"))?;
        if !out.status.success() {
            return Err(format!("date {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
}

type HeldEpochs = std::sync::Mutex<std::collections::HashMap<(String, u32, u32), (i64, i64, u64)>>;

fn open_epoch_held(held: &HeldEpochs, tz: &str, hour: u32, at: u64, days_after: u32, date: &dyn Fn(&[&str]) -> Result<String, String>) -> Result<u64, String> {
    let key = (tz.to_string(), hour, days_after);
    if let Some(&(_, _, epoch)) = held.lock().unwrap_or_else(|e| e.into_inner()).get(&key).filter(|(from, to, _)| (*from..*to).contains(&(at as i64))) {
        return Ok(epoch);
    }
    let epoch_of = |when: String| -> Result<i64, String> { date(&["-d", &when, "+%s"])?.parse().map_err(|_| format!("date gave no epoch for TZ={tz}")) };
    let day = date(&["-d", &format!("@{at}"), "+%F"])?;
    let (from, to) = (epoch_of(format!("{day} 00:00"))?, epoch_of(format!("{day} 00:00 1 day"))?);
    let epoch = u64::try_from(epoch_of(format!("{day} {hour:02}:00 {days_after} day"))?).map_err(|_| format!("date gave a negative epoch for TZ={tz}"))?;
    held.lock().unwrap_or_else(|e| e.into_inner()).insert(key, (from, to, epoch));
    Ok(epoch)
}

#[cfg(test)]
mod cr_cadence_tests {
    use super::*;
    use cr::record::{self, Kind, Review, State};

    const LA: &str = "America/Los_Angeles";
    const LATE: u64 = 1_795_159_800; // 2026-11-19 23:30 PST, which is already the 20th in UTC
    const SEVEN_NEXT_MORNING: u64 = 1_795_186_800; // 2026-11-20 07:00 PST
    const SMALL_HOURS: u64 = 1_795_086_000; // 2026-11-19 03:00 PST

    fn world(run: &Path, tz: &str, hour: &str) -> World {
        World { run_dir: run.into(), cr_tz: tz.into(), cr_open_hour: hour.into(), ..World::default() }
    }

    fn scheduled(run: &Path, state: State, closed_at: Option<u64>) {
        let unit = cr::units::Unit { id: "u".into(), kind: cr::units::Kind::Module, files: vec![], title: "t".into() };
        let n = record::allocate(run).unwrap();
        let r = Review { number: n, kind: Kind::Scheduled, state, source_repo: "s".into(), source_sha: "c".into(), collection: None, url: None, unit, sealed_sha256: None, staged_at: 0, opened_at: None, published_at: None, closed_at };
        record::save(run, &r).unwrap();
    }

    fn counting_date(asked: &std::cell::Cell<u32>) -> impl Fn(&[&str]) -> Result<String, String> + '_ {
        move |args| {
            asked.set(asked.get() + 1);
            let when = args[1];
            Ok(match args[2] {
                "+%F" => if when.trim_start_matches('@').parse::<u64>().unwrap() < 86_400 { "1970-01-01" } else { "1970-01-02" }.to_string(),
                _ => {
                    let day = if when.starts_with("1970-01-01") { 0 } else { 86_400 };
                    let hour: u64 = when.get(11..13).unwrap().parse().unwrap();
                    let next = if when.ends_with("1 day") { 86_400 } else { 0 };
                    (day + hour * 3600 + next).to_string()
                }
            })
        }
    }

    #[test_id(tkr5kyh)]
    #[test]
    fn an_open_epoch_is_asked_of_date_once_per_local_day_and_again_after_midnight() {
        let (held, asked) = (HeldEpochs::default(), std::cell::Cell::new(0));
        let date = counting_date(&asked);
        let open = |at: u64| open_epoch_held(&held, "Z", 7, at, 1, &date).unwrap();
        assert_eq!(open(100), 86_400 + 7 * 3600);
        let first = asked.get();
        assert!(first > 0);
        assert_eq!(open(80_000), 86_400 + 7 * 3600);
        assert_eq!(asked.get(), first, "the same local day asks date nothing");
        assert_eq!(open(86_400 + 5), 2 * 86_400 + 7 * 3600);
        assert_eq!(asked.get(), 2 * first, "a crossed midnight asks again, once");
    }

    #[test_id(t753hk6)]
    #[test]
    fn a_failing_date_is_not_held_and_is_asked_again() {
        let held = HeldEpochs::default();
        let asked = std::cell::Cell::new(0);
        let failing = |_: &[&str]| -> Result<String, String> { asked.set(asked.get() + 1); Err("no date".into()) };
        assert!(open_epoch_held(&held, "Z", 7, 100, 0, &failing).is_err());
        assert!(open_epoch_held(&held, "Z", 7, 100, 0, &failing).is_err());
        assert_eq!(asked.get(), 2);
    }

    fn due(w: &World, input: &str) -> u64 {
        serde_json::from_str::<cr_cadence::Snapshot>(&w.cr_cadence_snapshot(input).unwrap()).unwrap().due_at
    }

    #[test_id(tftq9pf)]
    #[test]
    fn the_world_snapshot_names_the_staged_scheduled_review_as_the_blocker_and_none_once_it_closes() {
        let t = testkit::TempDir::new("cr-cadence-blocker");
        scheduled(t.path(), State::Staged, None);
        let snap = |w: &World| serde_json::from_str::<cr_cadence::Snapshot>(&w.cr_cadence_snapshot("").unwrap()).unwrap();
        assert_eq!(snap(&world(t.path(), LA, "7")).blocker, Some((1, State::Staged)));
        record::set_state(t.path(), 1, State::Closed, 5).unwrap();
        assert_eq!(snap(&world(t.path(), LA, "7")).blocker, None);
    }

    #[test_id(tilri32)]
    #[test]
    fn a_review_closed_at_half_past_eleven_local_is_due_the_next_days_open_hour() {
        let t = testkit::TempDir::new("cr-cadence-late");
        scheduled(t.path(), State::Closed, Some(LATE * 1000));
        assert_eq!(due(&world(t.path(), LA, "7"), ""), SEVEN_NEXT_MORNING);
    }

    #[test_id(thbagur)]
    #[test]
    fn with_no_scheduled_review_ever_the_first_is_due_the_next_open_hour_after_the_anchor() {
        let t = testkit::TempDir::new("cr-cadence-first");
        let carry = |anchor| serde_json::to_string(&cr_cadence::Carry { anchor: Some(anchor), ..Default::default() }).unwrap();
        let w = world(t.path(), LA, "7");
        assert_eq!((due(&w, &carry(LATE)), due(&w, &carry(SMALL_HOURS))), (SEVEN_NEXT_MORNING, SEVEN_NEXT_MORNING - 86_400));
    }

    #[test_id(tax06qy)]
    #[test]
    fn an_unset_zone_or_a_bad_hour_fails_naming_its_key() {
        let t = testkit::TempDir::new("cr-cadence-unset");
        assert!(world(t.path(), "", "7").cr_cadence_snapshot("").unwrap_err().contains("SPIRA_CR_TZ"));
        for hour in ["", "24", "seven"] {
            assert!(world(t.path(), LA, hour).cr_cadence_snapshot("").unwrap_err().contains("SPIRA_CR_OPEN_HOUR"), "{hour}");
        }
    }
}

#[cfg(test)]
mod ask_mail_tests {
    use super::*;
    use std::cell::Cell;

    fn mail(bead: &str, subject: &str, default: &str, id: &str) -> String {
        format!("Subject: {subject}\nX-Spira-Default: {default}\nMessage-ID: {id}\nX-Spira-Work-Bead: {bead}\n\nbody\n")
    }

    fn seed(root: &Path) {
        for b in ["a", "b", "c"] {
            for sub in ["new", "cur"] {
                fs::create_dir_all(root.join(b).join(sub)).unwrap();
            }
        }
        let filler = |b: &str, sub: &str, n: usize| for i in 0..n {
            fs::write(root.join(b).join(sub).join(format!("{i:03}")), "Subject: noise\n\nbody\n").unwrap();
        };
        filler("a", "new", 48);
        filler("a", "cur", 50);
        filler("b", "new", 50);
        filler("b", "cur", 50);
        filler("c", "new", 51);
        filler("c", "cur", 47);
        fs::write(root.join("a/cur/zz1"), mail("sp-one", "first", "d1", "<m1>")).unwrap();
        fs::write(root.join("b/new/zz2"), mail("sp-one", "second", "d2", "<m2>")).unwrap();
        fs::write(root.join("c/new/zz3"), mail("sp-two", "other", "d3", "<m3>")).unwrap();
        fs::write(root.join("c/cur/zz4"), mail("sp-three", "third", "d4", "<m4>")).unwrap();
    }

    fn world(root: &Path) -> World {
        World { mail: root.into(), lc_socket: root.join("absent.sock"), ..World::default() }
    }

    const PINNED: [&str; 4] = [
        "NEW ASK HOLD sp-one: first | default: d1 | message-id: <m1>",
        "NEW ASK HOLD sp-two: other | default: d3 | message-id: <m3>",
        "NEW ASK HOLD sp-nomail: <no text recorded> | NO MAIL for the question; answer with spira-lc reply sp-nomail seq:?",
        "NEW ASK HOLD sp-three: third | default: d4 | message-id: <m4>",
    ];

    #[test_id(tre2cna)]
    #[test]
    fn hold_lines_name_the_last_mail_and_a_held_ask_without_mail_gets_the_no_mail_line() {
        let t = testkit::TempDir::new("ask-mail-lines");
        seed(t.path());
        let w = world(t.path());
        let ids = ["sp-one", "sp-two", "sp-nomail", "sp-three"].map(String::from).to_vec();
        let got: Vec<String> = w.hold_lines(ids, &World::read_mail).into_iter().map(|(_, l)| l).collect();
        assert_eq!(got, PINNED);
    }

    #[test_id(tjewpgl)]
    #[test]
    fn the_asks_text_joins_each_held_asks_mails_in_held_order_then_mailbox_order() {
        let t = testkit::TempDir::new("ask-mail-text");
        seed(t.path());
        let w = world(t.path());
        let got = w.ask_text_from("[]".into(), ["sp-three", "sp-one"].map(String::from).to_vec(), &World::read_mail);
        let want = ["[]".to_string(), mail("sp-three", "third", "d4", "<m4>"), mail("sp-one", "second", "d2", "<m2>"), mail("sp-one", "first", "d1", "<m1>")];
        assert_eq!(got, want.join("\n"));
    }

    #[test_id(ttyywvh)]
    #[test]
    fn mails_by_bead_reads_each_file_once_however_many_beads_are_held() {
        let t = testkit::TempDir::new("ask-mail-reads");
        seed(t.path());
        let w = world(t.path());
        let reads = Cell::new(0usize);
        let counting = |p: &Path| { reads.set(reads.get() + 1); fs::read_to_string(p).ok() };
        let ids: BTreeSet<String> = ["sp-one", "sp-two", "sp-three"].map(String::from).into();
        let got = w.mails_by_bead(&ids, &counting);
        assert_eq!(reads.get(), 300);
        assert_eq!(got["sp-one"], [mail("sp-one", "second", "d2", "<m2>"), mail("sp-one", "first", "d1", "<m1>")]);
        assert_eq!(got["sp-two"], [mail("sp-two", "other", "d3", "<m3>")]);
        assert_eq!(got["sp-three"], [mail("sp-three", "third", "d4", "<m4>")]);

        reads.set(0);
        for id in &ids {
            w.mails_by_bead(&[id.clone()].into(), &counting);
        }
        assert_eq!(reads.get(), 900, "one call per bead, as the pre-fix mails_for was, rereads every file");
    }
}

#[cfg(test)]
mod margin_deliver_tests {
    use super::*;
    use crate::sources::smtp::Delivery;
    use crate::watchers::margin_comments::Deliver;
    use cr::record::{self, Kind, Review, State};
    use margin_client::{Anchor, Doc, FakeMargin, Margin};

    const BASE: &str = "http://reader.test";

    struct Fixture {
        dir: testkit::TempDir,
        fake: FakeMargin,
        input: String,
    }

    fn fixture(review: bool) -> Fixture {
        let dir = testkit::TempDir::new("margin-deliver");
        let fake = FakeMargin::default();
        let text: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let col = fake.post_collection("Review of a", "cover", &[Doc { path: "src/a.rs".into(), kind: "code".into(), text }], None).unwrap();
        let anchor = Anchor { path: "src/a.rs".into(), first: 6, last: 7 };
        let thread = fake.his_thread(&col.id, anchor, "why is this pair swapped?", 1_795_000_000_000);
        if review {
            let unit = cr::units::Unit { id: "u".into(), kind: cr::units::Kind::Module, files: vec![], title: "t".into() };
            let n = record::allocate(dir.path()).unwrap();
            let r = Review { number: n, kind: Kind::Scheduled, state: State::Open, source_repo: "s".into(), source_sha: "c".into(), collection: Some(col.id.clone()), url: None, unit, sealed_sha256: None, staged_at: 0, opened_at: None, published_at: None, closed_at: None };
            record::save(dir.path(), &r).unwrap();
        }
        let input = serde_json::to_string(&Deliver { collection: col.id, comment: format!("{thread}-0"), thread, seq: 4, batch: vec![4], through: 4 }).unwrap();
        Fixture { dir, fake, input }
    }

    fn deliver(f: &Fixture) -> Result<String, String> {
        let mail_root = f.dir.path().join("mail");
        let delivery = Delivery { run_dir: f.dir.path().into(), home: f.dir.path().into(), mail_root, mute: false };
        World { run_dir: f.dir.path().into(), ..World::default() }.margin_deliver_with(&f.fake, BASE, &delivery, &f.input)
    }

    fn mailbox(f: &Fixture) -> Vec<String> {
        let new = f.dir.path().join("mail/concierge/new");
        let mut files: Vec<_> = fs::read_dir(new).into_iter().flatten().flatten().map(|e| fs::read_to_string(e.path()).unwrap()).collect();
        files.sort();
        files
    }

    #[test_id(t5j29kn)]
    #[test]
    fn a_comment_on_a_review_lands_once_in_the_concierge_mailbox_with_its_source_and_the_cr_reply_command() {
        let f = fixture(true);
        assert_eq!(deliver(&f).unwrap(), format!("comment {}-0 delivered", "col1-his0"));
        let mail = mailbox(&f);
        assert_eq!(mail.len(), 1);
        let m = &mail[0];
        assert!(m.contains("Subject: margin: Review of a — src/a.rs:6-7\n"), "{m}");
        assert!(m.contains("Message-ID: <margin-comment-col1-his0-0@spira>\n") && m.contains("From: Operator <operator@spira>\n"), "{m}");
        assert!(m.contains("why is this pair swapped?"), "{m}");
        assert!(m.contains("     1  line 1\n") && m.contains(">    6  line 6\n") && m.contains(">    7  line 7\n") && m.contains("    12  line 12\n"), "{m}");
        assert!(!m.contains(">    8  line 8"), "{m}");
        assert!(m.contains("cr reply --review 001 --thread col1-his0 < reply.txt"), "{m}");
        assert_eq!(deliver(&f).unwrap(), "comment col1-his0-0 already delivered");
        assert_eq!(mailbox(&f).len(), 1, "a second run delivers nothing new");
        assert_eq!(crate::sources::margin_changed::resume_point(f.dir.path()), 4);
    }

    #[test_id(t2j5z84)]
    #[test]
    fn the_thread_so_far_and_the_reader_url_stand_in_when_no_review_holds_the_collection() {
        let f = fixture(false);
        f.fake.reply_as("col1-his0", &FakeMargin::agent_author(), "it is deliberate", 1_795_000_100_000);
        f.fake.reply_as("col1-his0", "ryan", "then say so in a comment", 1_795_000_200_000);
        let d: Deliver = serde_json::from_str(&f.input).unwrap();
        let input = serde_json::to_string(&Deliver { comment: "col1-his0-r2".into(), ..d }).unwrap();
        let f = Fixture { input, ..f };
        deliver(&f).unwrap();
        let m = &mailbox(&f)[0];
        assert!(m.contains("then say so in a comment"), "{m}");
        assert!(m.contains("Thread so far:") && m.contains("concierge at 2026-") && m.contains("it is deliberate"), "{m}");
        assert!(m.contains(&format!("{BASE}/c/col1#col1-his0-r2")) && !m.contains("cr reply"), "{m}");
    }

    #[test_id(tkozu7k)]
    #[test]
    fn a_publish_replayed_for_a_review_that_is_no_longer_open_runs_no_cr() {
        let f = fixture(true);
        record::set_state(f.dir.path(), 1, State::Closed, 5).unwrap();
        let input = serde_json::to_string(&crate::watchers::margin_comments::Publish { collection: "col1".into(), seq: 2, batch: vec![2], through: 2 }).unwrap();
        let w = World { run_dir: f.dir.path().into(), ..World::default() };
        assert_eq!(w.margin_publish(&input).unwrap(), "review 001 is closed; nothing to unseal");
    }

    #[test_id(t9q2xyo)]
    #[test]
    fn a_published_collection_no_review_holds_is_not_a_code_review() {
        let t = testkit::TempDir::new("margin-publish");
        let input = serde_json::to_string(&crate::watchers::margin_comments::Publish { collection: "other".into(), seq: 2, batch: vec![2], through: 2 }).unwrap();
        let w = World { run_dir: t.path().into(), ..World::default() };
        assert_eq!(w.margin_publish(&input).unwrap(), "not a code review");
        assert_eq!(crate::sources::margin_changed::resume_point(t.path()), 2);
    }

    fn design_fixture() -> Fixture {
        let f = fixture(false);
        let rec = design_review::Record { page: "wiki/projects/p/designs/d.md".into(), collection: "col1".into(), document: "col1-d1".into(), version: 1, url: format!("{BASE}/c/col1"), sha256: "x".into(), state: "open".into(), posted_at_ms: 1_795_000_000_000 };
        let path = f.dir.path().join("design-review");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("p.json"), serde_json::to_string(&rec).unwrap()).unwrap();
        f
    }

    #[test_id(tdm0001)]
    #[test]
    fn a_comment_on_a_design_page_names_the_design_review_reply_line_and_the_design_subject() {
        let f = design_fixture();
        deliver(&f).unwrap();
        let m = &mailbox(&f)[0];
        assert!(m.contains("Subject: margin: design wiki/projects/p/designs/d.md — src/a.rs:6-7\n"), "{m}");
        assert!(m.contains("design-review reply --page wiki/projects/p/designs/d.md --thread col1-his0 < reply.txt"), "{m}");
    }

    #[test_id(tdm0002)]
    #[test]
    fn a_design_publish_mails_the_concierge_once_with_the_count_of_his_threads() {
        let f = design_fixture();
        let w = World { run_dir: f.dir.path().into(), ..World::default() };
        let anchor = margin_client::Anchor { path: "src/a.rs".into(), first: 1, last: 1 };
        f.fake.thread_by("col1", anchor, &FakeMargin::agent_author(), "mine", 2);
        let ports = || Ok((Box::new(f.fake.clone()) as Box<dyn Margin>, Delivery { run_dir: f.dir.path().into(), home: f.dir.path().into(), mail_root: f.dir.path().join("mail"), mute: false }));
        let input = serde_json::to_string(&crate::watchers::margin_comments::Publish { collection: "col1".into(), seq: 9, batch: vec![9], through: 9 }).unwrap();
        assert_eq!(w.margin_publish_with(&ports, &input).unwrap(), "design wiki/projects/p/designs/d.md publish mailed");
        assert_eq!(w.margin_publish_with(&ports, &input).unwrap(), "design wiki/projects/p/designs/d.md publish already mailed");
        assert_eq!(crate::sources::margin_changed::resume_point(f.dir.path()), 9);
        let mail = mailbox(&f);
        assert_eq!(mail.len(), 1);
        let m = &mail[0];
        assert!(m.contains("Message-ID: <margin-publish-col1-1@spira>\n") && m.contains("Subject: margin: design wiki/projects/p/designs/d.md — his pass is published\n"), "{m}");
        assert!(m.contains("His threads on version 1: 1\n") && m.contains("http://reader.test/c/col1"), "{m}");
    }
}
