//! # watch.runtime — contract
//!
//! Replaces `spira-runtime-watch-prod` (`runtime-watch pass --state run/runtime-watch.state --max-files 10`), pattern P1. It files a bump bead when a repo pins a Rust, Node or Python line that upstream no longer supports.
//!
//! ## Tick
//!
//! - Every 6 h (`OnUnitActiveSec=6h`, `OnBootSec=15min`); single-flight.
//!
//! ## Observe
//!
//! `watch.runtime.observe` returns:
//!
//! ```text
//! { today, repos: [{name, path, ref, files: {path → text|absent},
//!   naming: {needle → [file]}}], rust: stable|err, node: eol|err,
//!   python: eol|err, open_bumps: {(repo, runtime) → bead_id} }
//! ```
//!
//! Each part is read from the component that owns it:
//!
//! - **Repos:** the registry (`spira_config::repos`), in-process (`runtime-watch/src/real.rs:169-179`).
//! - **Pin files:** `git ls-tree` / `git show <ref>:<file>` and `git grep -l -F` at the base ref (`real.rs:117-121,181-196`). The git class stays a child.
//! - **Upstream support:** `curl` of four URLs (`real.rs:88-91,110-115,198-208`), parsed by `parse_rust_stable` / `parse_node` / `parse_python` (`lib.rs:104-160`). These are external and stay children (W1 allow-lists curl).
//! - **Open bumps:** store verbs `show <id>` for the ledgered id, else `list --live --repo <name>` filtered by `names_bump` (`real.rs:144-158,210-226`, `lib.rs:77`). Both run over the in-process `LcClient` (S7a). The lifecycle state (`TERMINAL`, `real.rs:92`) is the store's answer.
//!
//! The probe needs each pin's needle before it can grep, so it runs in two phases: pins (`rust_pins` / `node_pins` / `python_pins`, `lib.rs:177-250`), then `naming` for the stale ones. The observe stays one probe; the probe body calls the pin parsers (pure) between the reads.
//!
//! ## Decide
//!
//! - `decide(carry, snap, max_files) -> (Carry, Vec<Bump>)` is today's `pass` (`lib.rs:290-356`) with the `World` reads served from the snapshot. That covers `rust_stale` / `eol_stale`, already-open skip, the `max_files` bound, and the title `bump <runtime> <old pin> to <target>` (`lib.rs:348`).
//! - The body is `body()` (`lib.rs:275-288`).
//! - Carry is today's `--state` ledger, `"<repo>\t<runtime>" → bead id` (`real.rs:123-142`), emitted as `watch.runtime.state` on each successful filing.
//! - Fixtures: `runtime-watch/src/tests.rs`:
//!   - two-behind files once and a second pass files nothing (`:69`)
//!   - stable or grace files nothing (`:86`)
//!   - unreachable is cannot-check (`:97`)
//!   - node EOL (`:105`)
//!   - python floor (`:120`)
//!   - batch bounded (`:127`)
//!   - unlisted line is cannot-check (`:136`)
//!   - parsers (`:144`)
//!   - bump filed outside the ledger is amended, not duplicated (`:168`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.runtime.observe` (probe) | host (curl + git) | 120 s (4 × curl 30 s, `real.rs:112`) | — | — |
//! | `watch.runtime.file_bump` | store | 120 s (`real.rs:162`) | `watch.runtime:<repo>:<runtime>:<target>` | a (repo, runtime) whose carried or listed bump is live |
//! | `watch.runtime.metrics` | host | 5 s | `watch.runtime:metrics:<tick>` | — |
//!
//! - **`watch.runtime.file_bump`** files the bead with `--for builder --repo <repo>`, then adds label `LABEL` (`real.rs:228-237`).
//!   - Today this forks `bead.sh file … --body-file <tmp>` and `bead.sh label add` (`real.rs:160-165,231,235`).
//!   - In the task it is the store's file and label verbs. §2 keeps `bead.sh` as an operator tool, so the task does not fork it.
//! - **`watch.runtime.metrics`** pushes `spira_runtime_watch_cannot_check` (gauge) and `spira_runtime_bumps_filed` (count) (`main.rs:59-61`).
//!
//! No inbox line. The summary `runtime-watch: <f> filed, <o> already open, <c> supported, <x> failed, <n> cannot check` and the `cannot check — …` lines (`main.rs:62-75`) go to `run/runtime-watch.log` (unit stdout). That is the log row `spira/watchers:178`, which `watchd notify` reads.
//!
//! The task keeps the `cannot check` lines only, as a `watch.runtime.log` host append to that file with key `watch.runtime:cc:<tag>:<day>`. "Cannot check" must not go silent, and the per-pass summary is dropped.
//!
//! ## Silence
//!
//! An idle tick (every pin supported or already open) emits the probe and one metrics push. There is no state Emit and no log line.
//!
//! ## Replaces
//!
//! - `spira-runtime-watch-prod.service` and `spira-runtime-watch-prod.timer` (`CPUQuota=25%`, `TimeoutStartSec=600`).
//! - Log row `runtime-watch|log|@SPIRA_RUN@/runtime-watch.log` (`spira/watchers:178`) stays as the read contract.
//! - Children:
//!   - `curl` (`real.rs:112`)
//!   - `git` (`real.rs:118`)
//!   - `spira-lc show` / `list` (`real.rs:146,214`)
//!   - `bead.sh file` / `label add` (`real.rs:162`)
//! - Keys pinned: `SPIRA_METRICS_ENDPOINT` (`main.rs:56`), `--persona` (default `builder`, `main.rs:21`), `--max-files` (10).
//!
//! The catalogue row matches the code: curl, git, spira-lc, bead.sh.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Observe mode shadows `file_bump`; the ledger is unchanged, so the bump files after the flip.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;
use crate::watchers::{answered_spec, carried};

pub const NAME: &str = "watch.runtime";
pub const STREAM: &str = "watch.runtime";
pub const TICK: EventKindTag = "watch.runtime.tick";
pub const STATE: EventKindTag = "watch.runtime.state";
pub const OBSERVE: &str = "watch.runtime.observe";
pub const FILE_BUMP: &str = "watch.runtime.file_bump";
pub const METRICS: &str = "watch.runtime.metrics";
pub const LOG: &str = "watch.runtime.log";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_BUMP, METRICS, LOG];
pub const KEYS: &[&str] = &["SPIRA_METRICS_ENDPOINT", "SPIRA_RUN"];
/// The unit argv (`main.rs:21`); no config key carries them today.
pub const PERSONA: &str = "builder";
pub const MAX_FILES: usize = 10;
/// `OnUnitActiveSec=6h`.
pub const CADENCE_MS: u64 = 6 * 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 120_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub metrics_endpoint: String,
    pub run: String,
    pub persona: String,
    pub max_files: usize,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { metrics_endpoint: cfg("SPIRA_METRICS_ENDPOINT")?, run: cfg("SPIRA_RUN")?, persona: PERSONA.into(), max_files: MAX_FILES })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    /// path → text, `None` when the file is absent at the ref.
    pub files: BTreeMap<String, Option<String>>,
    /// needle → the files naming it.
    pub naming: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub today: String,
    pub repos: Vec<Repo>,
    /// The upstream answers, each the fetched text or why it could not be read.
    pub rust: Result<String, String>,
    pub node: Result<String, String>,
    pub python: Result<String, String>,
    /// `"<repo>\t<runtime>"` → the live bump bead.
    pub open_bumps: BTreeMap<String, String>,
}

/// The `--state` ledger: `"<repo>\t<runtime>"` → bead id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub ledger: BTreeMap<String, String>,
}

const FILE_DEADLINE_MS: u64 = 120_000;
const ACT_DEADLINE_MS: u64 = 5_000;
/// The component name today's pass pushed its metrics and wrote its log under.
pub const COMPONENT: &str = "runtime-watch";

/// What `watch.runtime.file_bump` is handed; `key` is the ledger's `"<repo>\t<runtime>"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bump {
    pub key: String,
    pub repo: String,
    pub persona: String,
    pub title: String,
    pub body: String,
}

/// What `watch.runtime.metrics` pushes (`runtime-watch/src/main.rs:59-61`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    pub cannot_check: u64,
    pub filed: u64,
}

/// `runtime_watch::World` served from the snapshot: every read is an answer the probe already
/// made, and `file_bump` records the filing to request instead of filing it.
struct SnapWorld<'a> {
    snap: &'a Snapshot,
    persona: &'a str,
    bumps: std::cell::RefCell<Vec<Bump>>,
}

fn key(repo: &runtime_watch::Repo, rt: runtime_watch::Runtime) -> String {
    format!("{}\t{}", repo.name, rt.name())
}

impl SnapWorld<'_> {
    fn repo(&self, r: &runtime_watch::Repo) -> Option<&Repo> {
        self.snap.repos.iter().find(|x| x.name == r.name)
    }
}

impl runtime_watch::World for SnapWorld<'_> {
    fn repos(&self) -> Vec<runtime_watch::Repo> {
        self.snap.repos.iter().map(|r| runtime_watch::Repo { name: r.name.clone(), path: r.path.clone(), git_ref: r.ref_.clone() }).collect()
    }

    fn read(&self, repo: &runtime_watch::Repo, file: &str) -> Result<Option<String>, String> {
        Ok(self.repo(repo).and_then(|r| r.files.get(file).cloned().flatten()))
    }

    fn naming(&self, repo: &runtime_watch::Repo, needle: &str) -> Vec<String> {
        self.repo(repo).and_then(|r| r.naming.get(needle).cloned()).unwrap_or_default()
    }

    fn rust(&self) -> Result<runtime_watch::RustSupport, String> {
        self.snap.rust.clone().and_then(|t| runtime_watch::parse_rust_stable(&t)).map(|stable| runtime_watch::RustSupport { stable })
    }

    fn node(&self) -> Result<runtime_watch::EolSupport, String> {
        let pair: NodeTexts = serde_json::from_str(self.snap.node.as_ref().map_err(String::clone)?).map_err(|e| format!("node answer: {e}"))?;
        runtime_watch::parse_node(&pair.schedule, &pair.index, &self.snap.today)
    }

    fn python(&self) -> Result<runtime_watch::EolSupport, String> {
        self.snap.python.clone().and_then(|t| runtime_watch::parse_python(&t, &self.snap.today))
    }

    fn open_bump(&self, repo: &runtime_watch::Repo, rt: runtime_watch::Runtime) -> Result<Option<String>, String> {
        Ok(self.snap.open_bumps.get(&key(repo, rt)).cloned())
    }

    fn file_bump(&self, repo: &runtime_watch::Repo, rt: runtime_watch::Runtime, title: &str, body: &str) -> Result<String, String> {
        self.bumps.borrow_mut().push(Bump { key: key(repo, rt), repo: repo.name.clone(), persona: self.persona.into(), title: title.into(), body: body.into() });
        Ok(String::new())
    }

    fn today(&self) -> String {
        self.snap.today.clone()
    }
}

/// Node's answer is two documents: the release schedule and the dist index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTexts {
    pub schedule: String,
    pub index: String,
}

/// Today's `pass` over the snapshot: the bumps to file, the metrics push, the `cannot check` lines.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let w = SnapWorld { snap, persona: &cfg.persona, bumps: Default::default() };
    let o = runtime_watch::pass(&w, cfg.max_files);
    let bumps = w.bumps.into_inner();
    let mut acts: Vec<EffectSpec> = bumps.iter().map(|b| EffectSpec::keyed(FILE_BUMP, format!("{NAME}:{}", b.key.replace('\t', ":")), serde_json::to_string(b).unwrap_or_default(), FILE_DEADLINE_MS)).collect();
    let m = Metrics { cannot_check: o.cannot_check.len() as u64, filed: bumps.len() as u64 };
    acts.push(EffectSpec::keyed(METRICS, format!("{NAME}:metrics:{now_ms}"), serde_json::to_string(&m).unwrap_or_default(), ACT_DEADLINE_MS));
    acts.extend(o.cannot_check.iter().enumerate().map(|(i, c)| EffectSpec::keyed(LOG, format!("{NAME}:log:{now_ms}:{i}"), format!("{COMPONENT}: cannot check — {c}"), ACT_DEADLINE_MS)));
    (carry.clone(), acts)
}

/// The ledger once the filing `spec` succeeded with bead id `outcome`.
pub fn filed(carry: &Carry, spec: &EffectSpec, outcome: &str) -> Carry {
    let mut next = carry.clone();
    if let (Ok(b), id) = (serde_json::from_str::<Bump>(&spec.input), outcome.trim()) {
        if !id.is_empty() {
            next.ledger.insert(b.key, id.to_string());
        }
    }
    next
}

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];
// batch-job: an upstream download, not a store read (runtime-watch's own fetch carried the same mark)
const CURL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
/// A local git read: the 5 s call cap runtime-watch's `bounded("git")` held it to.
const GIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// One child: the git and curl class stay processes (W1), run through the `Proc` port.
fn child(prog: &str, args: &[&str], deadline: std::time::Duration) -> Result<String, String> {
    use runtime::ports::Proc;
    let spec = runtime::ports::ProcSpec { prog: prog.into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: vec![], deadline };
    let out = runtime::ports::SysProc.run(&spec)?;
    if out.code == Some(0) {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{prog} exited {:?}: {}", out.code, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn fetch(url: &str) -> Result<String, String> {
    child("curl", &["--fail", "--silent", "--show-error", "--location", "--max-time", "25", url], CURL_DEADLINE).map_err(|e| format!("cannot fetch {url}: {e}"))
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    child("git", &[&["-C", path], args].concat(), GIT_DEADLINE)
}

/// A live bump for `(repo, rt)`: the ledgered id when the store shows it live, else the first
/// live bead of the repo whose title names a bump of `rt` (`runtime-watch/src/real.rs:144-226`).
fn open_bump(lc: &spira_config::lc_call::LcClient, ledgered: Option<&String>, live: &[serde_json::Value], rt: runtime_watch::Runtime) -> Result<Option<String>, String> {
    if let Some(id) = ledgered {
        let out = lc.call(&["show", id.as_str()]).map_err(|e| format!("cannot tell whether {id} is open: {e}"))?;
        match out.code {
            1 => {}
            0 => {
                let doc: serde_json::Value = serde_json::from_str(&out.out).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                if !TERMINAL.contains(&state) {
                    return Ok(Some(id.clone()));
                }
            }
            _ => return Err(format!("cannot tell whether {id} is open: {}", out.out.trim())),
        }
    }
    Ok(live.iter().filter(|b| b["state"].as_str().is_some_and(|s| !TERMINAL.contains(&s))).find(|b| b["title"].as_str().is_some_and(|t| runtime_watch::names_bump(t, rt))).and_then(|b| b["bead_id"].as_str().map(str::to_string)))
}

/// The probe (`input` is the carried ledger): the registry's repos, each pin file at the base
/// ref, the files naming each pin, the four upstream documents, and each live bump.
pub fn observe(input: &str) -> Result<String, String> {
    use spira_config::repos::{Column, Registry};
    let ledger: BTreeMap<String, String> = serde_json::from_str(input).unwrap_or_default();
    let home = spira_config::resolve::locate_home_for_process()?;
    let registry = Registry::from_env(std::env::vars().collect(), &home);
    let lc = spira_config::lc_call::LcClient::configured()?;
    let mut repos = vec![];
    let mut open_bumps = BTreeMap::new();
    for name in registry.all() {
        let (Some(path), Some(ref_)) = (registry.root(&name).filter(|p| !p.is_empty()), registry.field(&name, Column::Base).filter(|r| !r.is_empty())) else { continue };
        let mut files = BTreeMap::new();
        for f in runtime_watch::PIN_FILES {
            let present = !git(&path, &["ls-tree", "--name-only", &ref_, "--", f])?.trim().is_empty();
            files.insert(f.to_string(), if present { Some(git(&path, &["show", &format!("{ref_}:{f}")])?) } else { None });
        }
        let mut repo = Repo { name: name.clone(), path: path.clone(), ref_: ref_.clone(), files, naming: BTreeMap::new() };
        let probe = Snapshot { today: String::new(), repos: vec![repo.clone()], rust: Err(String::new()), node: Err(String::new()), python: Err(String::new()), open_bumps: BTreeMap::new() };
        let w = SnapWorld { snap: &probe, persona: "", bumps: Default::default() };
        let r = runtime_watch::World::repos(&w).remove(0);
        let pins = [runtime_watch::rust_pins(&w, &r), runtime_watch::node_pins(&w, &r), runtime_watch::python_pins(&w, &r)].into_iter().flat_map(Result::unwrap_or_default);
        let prefix = format!("{ref_}:");
        for p in pins {
            let hits = git(&path, &["grep", "-l", "-F", "-e", &p.needle, &ref_]).unwrap_or_default().lines().map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string()).collect();
            repo.naming.insert(p.needle, hits);
        }
        let live: Vec<serde_json::Value> = match lc.call(&["list", "--live", "--repo", &name]) {
            Ok(r) if r.code == 0 => serde_json::from_str(&r.out).unwrap_or_default(),
            _ => vec![],
        };
        for rt in runtime_watch::Runtime::ALL {
            let k = key(&r, rt);
            if let Some(id) = open_bump(&lc, ledger.get(&k), &live, rt)? {
                open_bumps.insert(k, id);
            }
        }
        repos.push(repo);
    }
    let node = fetch(runtime_watch::real::NODE_SCHEDULE_URL).and_then(|schedule| fetch(runtime_watch::real::NODE_INDEX_URL).map(|index| serde_json::to_string(&NodeTexts { schedule, index }).unwrap_or_default()));
    let today = runtime_watch::civil_date(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    let snap = Snapshot { today, repos, rust: fetch(runtime_watch::real::RUST_URL), node, python: fetch(runtime_watch::real::PYTHON_URL), open_bumps };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The filing: the store's `work - file` (the body as the request body), then the label; the
/// answer is the bead id, which the ledger records.
pub fn file_bump(input: &str) -> Result<String, String> {
    let b: Bump = serde_json::from_str(input).map_err(|e| format!("{FILE_BUMP}: {e}"))?;
    let lc = spira_config::lc_call::LcClient::configured()?.within(std::time::Duration::from_millis(FILE_DEADLINE_MS));
    let out = lc.call_stdin(&["work", "-", "file", &b.title, "--for", &b.persona, "--repo", &b.repo, "--body-file", "-", "--actor", COMPONENT], &b.body).map_err(|e| format!("work file {:?}: {e}", b.title))?;
    if out.code != 0 {
        return Err(format!("work file {:?}: {}", b.title, out.why()));
    }
    let id = out.out.lines().rev().map(str::trim).find(|l| !l.is_empty()).ok_or("work file printed no id")?.to_string();
    let _ = lc.call(&["content", "update", &id, "--add-label", runtime_watch::LABEL]);
    Ok(id)
}

/// `spira_runtime_watch_cannot_check` (gauge) and `spira_runtime_bumps_filed` (count).
pub fn metrics(cfg: &Config, input: &str) -> Result<String, String> {
    use spira_otel::metrics::{MetricsSink, Point};
    let m: Metrics = serde_json::from_str(input).map_err(|e| format!("{METRICS}: {e}"))?;
    let endpoint = Some(cfg.metrics_endpoint.as_str()).filter(|e| !e.trim().is_empty());
    let sink = MetricsSink::for_component(endpoint, COMPONENT);
    sink.push(&[Point::Gauge { name: "spira_runtime_watch_cannot_check", value: m.cannot_check as f64 }], &[]);
    sink.push(&[Point::Count { name: "spira_runtime_bumps_filed", value: m.filed }], &[]);
    Ok(format!("{} cannot check, {} filed", m.cannot_check, m.filed))
}

/// A `cannot check` line appended to `run/runtime-watch.log`, the `runtime-watch|log` row.
pub fn log(cfg: &Config, line: &str) -> Result<String, String> {
    use std::io::Write;
    let path = std::path::Path::new(&cfg.run).join("runtime-watch.log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

pub struct WatchRuntime {
    pub config: Result<Config, String>,
}

impl WatchRuntime {
    pub fn new(config: Config) -> Self {
        WatchRuntime { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchRuntime { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.runtime/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

/// The rows a restart keeps; what a Carry field is left out of is rebuilt by the first tick.
const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "ledger", drop: &[] }];

impl Handler for WatchRuntime {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let ledger = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE).ledger).unwrap_or_default();
            return pattern::tick(ev, OBSERVE, ledger, OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        if let Some(spec) = answered_spec(view, ev).filter(|s| s.name == FILE_BUMP && ev.kind.tag == EFFECT_SUCCEEDED) {
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            return pattern::commands(STATE, &carry, &filed(&carry, &spec, &ev.kind.body), vec![]);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
