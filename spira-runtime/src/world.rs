//! Where the watchers' snapshots come from: files under the run directory and the lifecycle
//! store through spira-lc's socket. No process is launched here; the wiki's git is `wiki_git`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use runtime::socket::{lc_call, lc_call_within};
use spira_config::nonwork::{status_args, Kind, Which};
use serde_json::Value;

use crate::watchers::inbox_keeper;
use crate::forge::{slug, Forge};
use crate::watchers::pr_notify::{self, RepoObs};
use crate::watchers::asks;
use crate::watchers::design_asks::{self, Page};
use crate::wiki_git::git;
use crate::watchers::brain_dirty;
use crate::watchers::supersede_duty::{self, Act, Held, Reading, Verb};
use crate::watchers::bead_sweep::{self, Bead, Repair, Repaired, Reservation, Row, Stop};
use crate::watchers::publish_backlog::{self, Read};
use crate::watchers::round_duty::{RoundState, Snapshot};

const STORE_ATTEMPTS: u32 = 3;
const UNPUBLISHED_WAIT: std::time::Duration = std::time::Duration::from_secs(9);
const STORE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    pub run_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub repo_map: PathBuf,
    pub home_repo: String,
    pub inbox: PathBuf,
    pub lc_socket: PathBuf,
    pub proc_dir: PathBuf,
    pub mail: PathBuf,
    pub ask_label: String,
    pub wiki: PathBuf,
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

fn lane_stopped(run_dir: &Path, repo: &str, lane: &str) -> bool {
    let Ok(text) = fs::read_to_string(run_dir.join("scoped-stops").join(format!("repo-{repo}.json"))) else { return false };
    serde_json::from_str::<Value>(&text).ok().and_then(|v| v["lanes"].as_array().map(|a| a.iter().any(|l| l == lane))).unwrap_or(false)
}

fn kv_get(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')).map(str::to_string))
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

impl World {
    fn lc(&self, argv: &[&str]) -> Option<String> {
        lc_call(&self.lc_socket, argv).ok()
    }

    /// The home repo's checkout, from the repo map.
    pub fn home_path(&self) -> Result<PathBuf, String> {
        let text = fs::read_to_string(&self.repo_map).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        map_rows(&text).into_iter().find(|r| r.name == self.home_repo).map(|r| PathBuf::from(r.path)).ok_or_else(|| format!("repo map has no row for {}", self.home_repo))
    }

    /// Unheld certified beads of one repo; `None` when the lifecycle did not answer.
    fn certified(&self, repo: &str) -> Option<u64> {
        let rows: Value = serde_json::from_str(&self.lc(&["list", "--state", "CERTIFIED", "--repo", repo])?).ok()?;
        let held = |r: &Value| r["holds"].as_array().is_some_and(|h| !h.is_empty());
        Some(rows.as_array()?.iter().filter(|r| r.get("bead_id").is_some() && !held(r)).count() as u64)
    }

    /// The rounds the lifecycle still holds open for the home repo, from the queue's records.
    fn open_rounds(&self) -> Option<u64> {
        let rows = fs::read_to_string(&self.repo_map).ok().map(|t| map_rows(&t))?;
        let home = rows.iter().find(|r| r.name == self.home_repo)?;
        let dir = self.queue_dir.join(&home.name);
        let mut live = 0;
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(0),
            Err(_) => return None,
        };
        let mut records: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "round" || n.starts_with("round."))).collect();
        records.sort();
        for p in records {
            let Ok(text) = fs::read_to_string(&p) else { continue };
            let Some(batch) = kv_get(&text, "batch_id").filter(|b| !b.is_empty()) else { continue };
            let dead = self.lc(&["show-batch", &batch]).and_then(|b| serde_json::from_str::<Value>(&b).ok()).is_some_and(|v| matches!(v["state"].as_str(), Some("ABANDONED" | "DISCARDED")));
            if !dead {
                live += 1;
            }
        }
        Some(live)
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
        let (mut certified, mut by_repo, mut stopped) = (0, vec![], vec![]);
        for r in rows.iter().filter(|r| r.land == "queue.local") {
            if lane_stopped(&self.run_dir, &r.name, "rounds") {
                stopped.push(r.name.clone());
                continue;
            }
            let n = self.certified(&r.name).ok_or_else(|| format!("lifecycle did not answer for {}", r.name))?;
            if n > 0 {
                certified += n;
                by_repo.push(format!("{}={n}", r.name));
            }
        }
        let open = self.open_rounds();
        let (lines, fresh) = self.results(&known);
        let snap = Snapshot {
            certified,
            by_repo: by_repo.join(" "),
            stopped: stopped.join(","),
            state: match open {
                None => RoundState::Unknown,
                Some(0) => RoundState::None,
                Some(_) => RoundState::Open,
            },
            open: open.unwrap_or(0),
            lines,
            fresh,
        };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    /// Whether any process's command line names inbox-triage, and when the inbox last changed.
    pub fn keeper_snapshot(&self) -> Result<String, String> {
        let alive = fs::read_dir(&self.proc_dir).map_err(|e| format!("{}: {e}", self.proc_dir.display()))?.flatten().any(|e| {
            e.file_name().to_str().is_some_and(|n| n.bytes().next().is_some_and(|b| b.is_ascii_digit()))
                && fs::read(e.path().join("cmdline")).is_ok_and(|raw| String::from_utf8_lossy(&raw).replace('\0', " ").contains("inbox-triage"))
        });
        let inbox_mtime = fs::metadata(&self.inbox).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        serde_json::to_string(&inbox_keeper::Snapshot { triage_alive: alive, inbox_mtime }).map_err(|e| e.to_string())
    }

    /// Every repository registered with a checkout, asked of its forge. A tracked PR that left
    /// the open list and a merge awaiting its deploy are asked about by name, from `input`.
    pub fn pr_snapshot(&self, forge: &Forge, input: &str) -> Result<String, String> {
        let carry: pr_notify::Carry = serde_json::from_str(input).unwrap_or_default();
        let rows = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        let mut repos = vec![];
        for r in rows.iter().filter(|r| Path::new(&r.path).join(".git").is_dir()) {
            let Some(slug) = slug(Path::new(&r.path)) else { continue };
            let prev = carry.repos.get(&r.name).cloned().unwrap_or_default();
            let mut obs = RepoObs::default();
            if let Some(open) = forge.open_prs(&slug) {
                for n in prev.prs.keys().filter(|n| !open.iter().any(|(p, _)| p.number == **n)) {
                    obs.finals.extend(forge.final_state(&slug, *n).map(|f| (*n, f)));
                }
                if r.land == "pr" {
                    let heads: Vec<String> = open.iter().map(|(_, h)| h.clone()).collect();
                    let base = r.base.strip_prefix("origin/").unwrap_or(&r.base);
                    obs.strays = forge.stray_branches(&slug, if base.is_empty() { "main" } else { base }, &heads);
                }
                obs.open = Some(open.into_iter().map(|(p, _)| p).collect());
            }
            for d in &prev.deploys {
                obs.runs.extend(forge.deploy_run(&slug, &d.sha).map(|run| (d.sha.clone(), run)));
            }
            repos.push((r.name.clone(), obs));
        }
        serde_json::to_string(&pr_notify::Snapshot { repos }).map_err(|e| e.to_string())
    }

    /// Mail files naming `id` as their work bead, in mailbox order: every `new/` then every `cur/`.
    fn mails_for(&self, id: &str) -> Vec<String> {
        let mut boxes: Vec<PathBuf> = fs::read_dir(&self.mail).into_iter().flatten().flatten().map(|e| e.path()).collect();
        boxes.sort();
        let want = format!("X-Spira-Work-Bead: {id}");
        let mut hits = vec![];
        for sub in ["new", "cur"] {
            for b in &boxes {
                let mut files: Vec<PathBuf> = fs::read_dir(b.join(sub)).into_iter().flatten().flatten().map(|e| e.path()).collect();
                files.sort();
                hits.extend(files.into_iter().filter_map(|f| fs::read_to_string(f).ok()).filter(|text| text.lines().any(|l| l == want)));
            }
        }
        hits
    }

    fn hold_line(&self, id: &str) -> String {
        if let Some(text) = self.mails_for(id).pop() {
            let header = |k: &str| text.lines().take_while(|l| !l.is_empty()).find_map(|l| l.strip_prefix(k)).unwrap_or("").to_string();
            return format!("NEW ASK HOLD {id}: {} | default: {} | message-id: {}", header("Subject: "), header("X-Spira-Default: "), header("Message-ID: "));
        }
        let last = self
            .lc(&["history", id, "--machine", "bead"])
            .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
            .and_then(|rows| {
                rows.iter().rfind(|r| r["event"] == "Hold").map(|r| {
                    let seq = r["seq"].as_str().map(str::to_string).unwrap_or_else(|| r["seq"].to_string());
                    let hold: Value = r["evidence"].as_str().and_then(|e| serde_json::from_str(e).ok()).unwrap_or_default();
                    let detail = hold["Hold"]["detail"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
                    (seq, detail.chars().take(400).collect::<String>())
                })
            });
        let (seq, q) = last.unwrap_or(("?".into(), String::new()));
        let q = if q.is_empty() { "<no text recorded>".to_string() } else { q };
        format!("NEW ASK HOLD {id}: {q} | NO MAIL for the question; answer with spira-lc reply {id} seq:{seq}")
    }

    /// One spira-lc call, retried so a store restart inside the tick is not an outage; an error
    /// names the command and what the store said.
    fn lc_named(&self, argv: &[&str]) -> Result<String, String> {
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

    /// The beads the lifecycle holds on an ask, from the socket's `list` (the `list-held` caller verb is the CLI's composition of it).
    fn held_asks(&self) -> Result<Vec<String>, String> {
        let out = self.lc_named(&["list", "--live", "--hold", "ask"])?;
        let rows: Vec<Value> = serde_json::from_str(&out).map_err(|e| format!("held asks unparsed: {e}"))?;
        Ok(rows.iter().filter_map(|r| r["bead_id"].as_str().map(str::to_string)).collect())
    }

    /// The open asks as a JSON list; lines before the list (a notice the store printed) are skipped.
    fn open_asks_json(&self) -> Result<String, String> {
        let [flag, filter] = status_args(Kind::Ask, Which::Open);
        let argv = ["content-rows", "--label", &self.ask_label, &flag, &filter, "--full"];
        let out = self.lc_named(&argv)?;
        let list = out.char_indices().filter(|&(i, c)| c == '[' && (i == 0 || out.as_bytes()[i - 1] == b'\n')).map(|(i, _)| &out[i..]).find(|rest| serde_json::from_str::<Vec<Value>>(rest).is_ok());
        match list {
            Some(rest) => Ok(rest.to_string()),
            None => Err(format!("spira-lc {} exited 0 but printed no JSON list: {:?}", argv.join(" "), out.chars().take(200).collect::<String>())),
        }
    }

    pub fn asks_snapshot(&self, input: &str) -> Result<String, String> {
        let seen: BTreeSet<String> = serde_json::from_str(input).unwrap_or_default();
        let listed = self.open_asks_json()?;
        let rows: Vec<Value> = serde_json::from_str(&listed).map_err(|e| format!("ask list unparsed: {e}"))?;
        let open = rows.iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), r["title"].as_str()?.split_whitespace().collect::<Vec<_>>().join(" ")))).collect();
        let held = self.held_asks().unwrap_or_default().into_iter().filter(|id| !seen.contains(id)).map(|id| { let line = self.hold_line(&id); (id, line) }).collect();
        serde_json::to_string(&asks::Snapshot { open, held }).map_err(|e| e.to_string())
    }

    /// Every open ask's JSON, then the mail of every held ask.
    fn asks_text(&self) -> Result<String, String> {
        let listed = self.open_asks_json()?;
        let mails = self.held_asks()?.into_iter().flat_map(|id| self.mails_for(&id));
        Ok(std::iter::once(listed).chain(mails).collect::<Vec<_>>().join("\n"))
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

    /// The wiki checkout's changed paths and their mtimes. A path that no longer exists has none
    /// and is left out; an unset wiki observes nothing.
    pub fn brain_dirty_snapshot(&self) -> Result<String, String> {
        use std::os::unix::ffi::OsStrExt;
        let wiki = self.wiki.display().to_string();
        let mut snap = brain_dirty::Snapshot { wiki, ..Default::default() };
        if !self.wiki.as_os_str().is_empty() {
            let (code, _) = git(&self.wiki, &["rev-parse", "--git-dir"])?;
            if code == Some(0) {
                let (code, raw) = git(&self.wiki, &["status", "--porcelain", "-z", "--untracked-files=all"])?;
                if code != Some(0) {
                    return Err(format!("git status exited {code:?}"));
                }
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

    fn reading(&self, id: &str) -> Reading {
        let Some(history) = self.lc(&["history", id, "--machine", "bead"]) else { return Reading::Unreadable };
        match supersede_duty::request_in(&history) {
            Err(_) => Reading::Unreadable,
            Ok(None) => Reading::NotARequest,
            Ok(Some(succ)) => {
                let own = self.state_of(id);
                let succ_state = if own.is_empty() || supersede_duty::terminal(&own) { String::new() } else { self.state_of(&succ) };
                Reading::Request { succ, own, succ_state }
            }
        }
    }

    /// Every bead under a manual hold, read unless `input` says its version was already read as no request.
    pub fn supersede_snapshot(&self, input: &str) -> Result<String, String> {
        let known: BTreeMap<String, String> = serde_json::from_str(input).unwrap_or_default();
        let listed = lc_call(&self.lc_socket, &["list", "--live", "--hold", "manual", "--snapshot"]).map_err(|e| e.to_string())?;
        let rows: Vec<Value> = serde_json::from_str(&listed).map_err(|e| format!("held list unparsed: {e}"))?;
        let text = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
        let held = rows
            .iter()
            .filter(|r| !r["bead_id"].is_null())
            .map(|r| {
                let (id, version) = (text(&r["bead_id"]), text(&r["version"]));
                let reading = if known.get(&id) == Some(&version) { Reading::Skipped } else { self.reading(&id) };
                Held { id, version, reading }
            })
            .collect();
        serde_json::to_string(&supersede_duty::Snapshot { held }).map_err(|e| e.to_string())
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
        let text = lc_call(&self.lc_socket, argv).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("{} unparsed: {e}", argv[0]))
    }

    pub fn sweep_snapshot(&self) -> Result<String, String> {
        let list = |states: &str| self.lc_json(&["list", "--state", states]);
        let rows = list("READY,REWORK,SUBMITTED,CERTIFIED,WORKING,OPEN,IN_DELIVERY")?;
        let [flag, filter] = status_args(Kind::Ask, Which::Live);
        let content = self.lc_json(&["content", "list", &flag, &filter, "--json", "--limit", "0"])?;
        let term = list("LANDED,DONE,DROPPED,SUPERSEDED")?;
        let beads: Vec<Bead> = content.as_array().ok_or("content list is not an array")?.iter().filter_map(sweep_bead).collect();
        let labels_of = |id: &str| beads.iter().find(|b| b.id == id).map(|b| b.labels.clone()).unwrap_or_default();
        let rows: Vec<Row> = rows.as_array().ok_or("list is not an array")?.iter().filter_map(sweep_row).map(|mut r| {
            let ours = r.holds.iter().any(|h| h == "manual") && !r.holds.iter().any(|h| h == "poison" || h == "ask") && !labels_of(&r.id).contains(&self.ask_label);
            if ours && bead_sweep::review(&r.reason).is_none() {
                r.hold_review = self.lc_json(&["history", &r.id, "--machine", "bead"]).ok().and_then(|h| bead_sweep::review_in_history(&h));
            }
            r
        }).collect();
        let terminal = term.as_array().ok_or("list is not an array")?.iter().filter_map(|r| Some((r["bead_id"].as_str()?.to_string(), r["state"].as_str()?.to_string()))).collect();
        let (stops, reserved) = self.scoped_stops();
        let pr_repos = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).unwrap_or_default().into_iter().filter(|r| r.land == "pr").map(|r| r.name).collect();
        let solo_skipped = fs::read_dir(self.run_dir.join("solo-skips")).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        let snap = bead_sweep::Snapshot { rows, content: beads, terminal, stops, reserved, carried: self.integrations(), solo_skipped, pr_repos };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
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

    /// Gives each rowless bead the row it lacks; `create-bead` ignores a bead that already has one.
    pub fn sweep_repair(&self, input: &str) -> Result<String, String> {
        let Repair { snapshot, todo } = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let ids = todo
            .iter()
            .filter(|r| {
                let priority = r.priority.to_string();
                let mut argv = vec!["create-bead", &r.id, "--priority", &priority, "--title", &r.title];
                if let Some(repo) = &r.repo {
                    argv.extend(["--repo", repo]);
                }
                self.lc(&argv).is_some()
            })
            .map(|r| r.id.clone())
            .collect();
        serde_json::to_string(&Repaired { snapshot, ids }).map_err(|e| e.to_string())
    }

    /// Per queue.local repo, how far the forge trails the landing ref, as `spira-lc unpublished` measures it.
    pub fn publish_backlog_snapshot(&self) -> Result<String, String> {
        let rows = match fs::read_to_string(&self.repo_map) {
            Ok(t) => map_rows(&t),
            Err(e) => return serde_json::to_string(&publish_backlog::Snapshot { repos: vec![(publish_backlog::REPO_MAP_KEY.into(), Read::Blind(format!("the repo map is unreadable: {e}")))] }).map_err(|e| e.to_string()),
        };
        let mut repos = vec![];
        for r in &rows {
            if r.path.is_empty() {
                repos.push((r.name.clone(), Read::Blind("the repo map has no checkout for it".into())));
                continue;
            }
            let git = Path::new(&r.path).join(".git");
            if !git.is_dir() || r.land != "queue.local" {
                continue;
            }
            let read = self.unpublished(&r.name).unwrap_or_else(|| Read::Blind("the unpublished range could not be computed".into()));
            repos.push((r.name.clone(), read));
        }
        serde_json::to_string(&publish_backlog::Snapshot { repos }).map_err(|e| e.to_string())
    }

    /// `None` when the verb cannot measure the range.
    fn unpublished(&self, name: &str) -> Option<Read> {
        let out = lc_call_within(&self.lc_socket, &["unpublished", name], UNPUBLISHED_WAIT).ok()?;
        let v: Value = serde_json::from_str(&out).ok()?;
        let count = v["count"].as_u64()?;
        let arrival = match (count, &v["oldest_arrival"]) {
            (0, _) => None,
            (_, a) => Some(a.as_u64()?),
        };
        Some(Read::Measured { count, arrival })
    }

    pub fn inbox_append(&self, line: &str) -> Result<String, String> {
        if let Some(dir) = self.inbox.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.inbox).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
        Ok("appended".into())
    }
}

/// A JSON number, or a string of digits, as a float; anything else is zero.
fn as_num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0.0)
}

fn strings(v: &Value) -> Vec<String> {
    let parsed = v.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok());
    parsed.as_ref().unwrap_or(v).as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
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
    })
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
