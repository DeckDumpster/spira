//! # gate-check — contract
//!
//! Task id `gate.check`, pattern P1, family A of [[scheduler-consolidation-2026-11-03]]. Each tick
//! it does five things:
//!
//! - resolves `gh:run` gates whose CI escalated
//! - files flaky-suite beads
//! - files red-twice-suite beads
//! - ingests recent CI runs into TSD
//! - runs bd-gate discovery for pr-mode repos
//!
//! Paths are relative to `spira-releases/current`.
//!
//! **Code differs from the catalogue.** The catalogue lists its children as "lib.sh, forge". The
//! code forks `bdq` (`gate-check/src/real.rs:37-146`) and calls `gh` directly, not `forge`
//! (`real.rs:167-229`). It also forks `bead.sh` (`real.rs:150`), `tsd-ingest.sh` (`real.rs:252`),
//! `git` (`real.rs:247`) and `sh -c command -v gh` (`real.rs:163`), and runs the lib.sh seam
//! through `bash -c` (`real.rs:48-68`).
//!
//! ## Tick
//!
//! - Cadence 2 min (`spira-gate-check-prod.timer`: OnBootSec 90 s, OnUnitActiveSec 2 min).
//!   Single-flight.
//! - No event wake is needed. Optionally it could be woken by `pr.ci_completed` from the
//!   `repo_events` hosted handler (unverified that that event exists).
//! - Today the unit has **no `world guard` ExecStartPre**, unlike its siblings. The task keeps
//!   that: it runs while the world is halted.
//!
//! ## Observe
//!
//! The snapshot is
//! `{ gate_list, gate_check_out, lc_open, beads_by_title, ci: { recent_runs, failed_main_runs, last_green_sha, annotations_by_job, fail_lines } }`.
//!
//! - **bd gates.** `bd gate list --json` (`real.rs:88-92`) and `bd gate check --type=gh:run`
//!   (`real.rs:100-108`, stdout and stderr merged), plus `bd show <gate> --json` per ESCALATE line
//!   (`real.rs:110-114`). *No store verb exists for gates today (unverified that lc-serve exposes
//!   them). bd is banned for new code, so this needs a `gates` verb on the content store or the
//!   move of gates into the lifecycle. Until then this probe is the one `bd` reader left.* Also
//!   `gate check` may itself mutate gate state (unverified). If it does, it is an effect, not an
//!   observe.
//! - **Lifecycle rows.** `spira_config::lc_state::list()` (`real.rs:132-134`). This becomes
//!   LcClient `list` (S7a), with the 5 s deadline.
//! - **Bead titles for dedup.** `bd list --all --json` (`real.rs:126-130`), re-read inside the
//!   loop (`main.rs:138,164`). This becomes one content-store `list` read per tick, or the
//!   open-title projection.
//! - **CI facts.** `forge` class probes, one per gh call:
//!   - `gh run list` (`real.rs:167,187,198`)
//!   - `gh api …/jobs` (`real.rs:177`)
//!   - `gh api …/annotations` (`real.rs:182`)
//!   - `gh run view --log-failed` (`real.rs:229`)
//!   - `git log --oneline A..B` (`real.rs:247`), on the git class
//! - **Repo map** through `spira_config::repos::Registry` (`real.rs:33,72-86`). It is already
//!   in-process.
//! - **Config**: `SPIRA_FLAKY_GH_REPO` (`gate-check/src/main.rs:44`,
//!   `spira/conf.d/SPIRA_FLAKY_GH_REPO`, default empty). Empty skips steps 3–5 (`main.rs:78`).
//!
//! ## Decide
//!
//! Everything except the IO is already pure in `gate-check/src/engine.rs`:
//!
//! - `discover_branches` (`engine.rs:11`) and `stuck_count` (`engine.rs:41`)
//! - `escalate_lines` (`engine.rs:46`), `gate_id_in` (`engine.rs:52`) and `blocked_bead`
//!   (`engine.rs:85`)
//! - `flaky_suites` and `red_twice_suites` (`engine.rs:162,184`)
//! - `has_open_bead` and `find_open_bead` (`engine.rs:130,136`)
//! - the titles and bodies (`engine.rs:193-205`)
//!
//! `decide(carry, snap)` composes the walk of `main.rs:63-190` over the snapshot.
//!
//! - **Carried state.** `ingested: BTreeSet<run_id>`, so a run is ingested once rather than all
//!   20 every tick (`main.rs:186-190`). *This is a change: today every tick re-runs `tsd-ingest.sh`
//!   for the 20 latest runs.*
//! - **Fixtures.** `gate-check/src/tests.rs` (215 lines, a fake `World`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `gate.check.discover` (`bd gate discover --branch B` in repo, `real.rs:94-98`) | store | 10 s | `repo/branch` per tick | a non-pr repo (`main.rs:94`) |
//! | `gate.check.resolve` (`bd gate resolve`, `real.rs:116-120`) | store | 10 s | `gate_id` | a gate with no blocked bead (`main.rs:112`) |
//! | `gate.check.ci_failed` (`spira_event ci.failed` via lib.sh, `real.rs:122-124`; becomes an LcClient `event`) | store | 5 s | `ci.failed/<blocked>/<gate_id>` | none |
//! | `gate.check.file_bead` (`bead.sh file … --for builder`, `real.rs:148-159`) | store | 30 s | the title (`engine.rs:193,197`) | a title with an open bead (`main.rs:138,165`). It files nothing when the lc rows are unreadable (`main.rs:119-127`) |
//! | `gate.check.raise_priority` (`bd priority <id> 1`, `real.rs:136`) | store | 10 s | `id/1` | priority already ≤ 1 (`main.rs:166`) |
//! | `gate.check.note` (`bd note`, `real.rs:142`) | store | 10 s | `id/run_id` | none |
//! | `gate.check.tsd_ingest` (`tsd-ingest.sh repo run`, `real.rs:251-253`) | host | 60 s (unverified) | `repo/run_id` | a run already in `ingested` |
//!
//! ## Silence
//!
//! An idle tick has:
//!
//! - no gates listed, or no ESCALATE lines
//! - no new flaky or red-twice suite without an open bead
//! - no new run ids
//!
//! It emits the observe probe and nothing else. *Today an idle tick is not silent.* It forks
//! `tsd-ingest.sh` ×20 and `bd gate discover` per branch × pr repo every 2 min.
//!
//! ## Replaces
//!
//! - `spira-gate-check-prod.{service,timer}` and the shim `spira/gate-check.sh`, which sources
//!   `conf.sh` and execs `gate-check --home`.
//! - The children listed above.
//! - Pinned keys: `SPIRA_FLAKY_GH_REPO`, `SPIRA_HOME`. The lib.sh repository-map seam is already gone
//!   (`real.rs:72-86`).
//!
//! ## Reverse
//!
//! Release rollback restores the timer and the shim. Per task, `Observe` mode shadows every
//! `gate.check.*`. Its writes are deduped by title and gate id, so running old and new together
//! for one tick at worst double-ingests TSD.
//!

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "gate.check";
pub const STREAM: &str = "task.gate.check";
pub const TICK: EventKindTag = "gate.check.tick";
pub const STATE: EventKindTag = "gate.check.state";
pub const OBSERVE: &str = "gate.check.observe";
pub const DISCOVER: &str = "gate.check.discover";
pub const RESOLVE: &str = "gate.check.resolve";
pub const CI_FAILED: &str = "gate.check.ci_failed";
pub const FILE_BEAD: &str = "gate.check.file_bead";
pub const RAISE_PRIORITY: &str = "gate.check.raise_priority";
pub const NOTE: &str = "gate.check.note";
pub const TSD_INGEST: &str = "gate.check.tsd_ingest";

pub const CADENCE_MS: u64 = 120_000;
/// The contract gives the probe no deadline of its own; its slowest read is the 5 s LcClient `list`.
pub const OBSERVE_DEADLINE_MS: u64 = 30_000;

pub const ACTS: &[(&str, u64)] = &[(DISCOVER, 10_000), (RESOLVE, 10_000), (CI_FAILED, 5_000), (FILE_BEAD, 30_000), (RAISE_PRIORITY, 10_000), (NOTE, 10_000), (TSD_INGEST, 60_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, DISCOVER, RESOLVE, CI_FAILED, FILE_BEAD, RAISE_PRIORITY, NOTE, TSD_INGEST];
pub const KEYS: &[&str] = &["SPIRA_FLAKY_GH_REPO"];
/// `SPIRA_HOME` is the process's own home, not a `conf.d` key.
pub const UNREGISTERED: &[&str] = &["SPIRA_HOME"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub priority: u8,
    pub open: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub conclusion: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ci {
    pub recent_runs: Vec<Run>,
    pub failed_main_runs: Vec<Run>,
    pub last_green_sha: Option<String>,
    /// Annotations per job id, from which `flaky_suites` and `red_twice_suites` read.
    pub annotations_by_job: BTreeMap<String, Vec<String>>,
    pub fail_lines: BTreeMap<String, Vec<String>>,
}

/// A pr-mode repository and its branches, for bd-gate discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRepo {
    pub name: String,
    pub branches: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub gate_list: Vec<String>,
    /// `bd gate check --type=gh:run`, stdout and stderr merged.
    pub gate_check_out: String,
    /// The bead each ESCALATE line's gate blocks (`bd show <gate>`), `None` for none.
    pub gate_blocked: BTreeMap<String, Option<String>>,
    /// Open lifecycle rows; `None` when unreadable, and then nothing is filed.
    pub lc_open: Option<Vec<String>>,
    pub beads_by_title: BTreeMap<String, Bead>,
    pub ci: Ci,
    pub pr_repos: Vec<PrRepo>,
    /// `SPIRA_FLAKY_GH_REPO` as pinned; empty skips steps 3–5.
    pub flaky_gh_repo: String,
    /// The home repository a filed bead is filed against (`spira_home_repo`).
    #[serde(default)]
    pub home_repo: String,
    /// `git log --oneline <last green>..<sha>` per failed main run's sha, for the red-twice body.
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

/// Carried between ticks: every run id already ingested into TSD.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub ingested: BTreeSet<String>,
}

/// Record `write` over `recorded`, refusing a stale write: one that drops an id already recorded.
pub fn record_ingested(recorded: &Carry, write: &Carry) -> Result<Carry, String> {
    match recorded.ingested.difference(&write.ingested).next() {
        Some(dropped) => Err(format!("{NAME}: a write that drops ingested run {dropped} is stale — refused")),
        None => Ok(write.clone()),
    }
}

/// `ci.failed`'s input: the event `spira_event ci.failed <blocked> <title> <escalate line>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailed {
    pub blocked: String,
    pub gate: String,
    pub title: String,
    pub detail: String,
}

/// `file_bead`'s input: `bead.sh file <title> --for builder --repo <repo> -p <priority>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub title: String,
    pub repo: String,
    pub priority: u8,
    pub body: String,
}

/// The suites `pick` finds in the annotations of run `run_id`'s jobs. `annotations_by_job` is
/// keyed `<run id>/<job id>`; each value is one `check-runs/<job>/annotations` JSON answer.
fn suites_of(ci: &Ci, run_id: &str, pick: fn(&str) -> Vec<String>) -> Vec<String> {
    let prefix = format!("{run_id}/");
    ci.annotations_by_job.iter().filter(|(job, _)| job.starts_with(&prefix)).flat_map(|(_, pages)| pages.iter().flat_map(|p| pick(p))).collect()
}

fn open_bead<'a>(snap: &'a Snapshot, title: &str) -> Option<&'a Bead> {
    snap.beads_by_title.get(title).filter(|b| b.open)
}

/// The walk of `gate-check/src/main.rs:63-190` over the snapshot, with the pure core of
/// `gate_check::engine`.
///
/// TSD ingest is once per run (the contract's change): a run in `carry.ingested` is never
/// ingested again. With nothing recorded yet, the runs in view are the baseline — the old
/// timer re-ingested the latest twenty every tick, so each is in TSD already — and the
/// baseline is recorded with the first tick that journals anyway, so an idle first sight
/// journals nothing.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    use gate_check::engine;
    let mut acts = vec![];
    // STEP 1: discover, pr-mode repositories only.
    for repo in &snap.pr_repos {
        for branch in &repo.branches {
            acts.push(Act::new(DISCOVER, format!("{}/{branch}", repo.name)).with_input(serde_json::json!({"repo": repo.name, "branch": branch}).to_string()));
        }
    }
    // STEP 2b: each ESCALATE line's gate, when it blocks a bead.
    for esc in engine::escalate_lines(&snap.gate_check_out) {
        // The id as `gate_id_in` reads it, else a gate the snapshot knows named as a word in it.
        let named = || snap.gate_blocked.keys().find(|g| esc.split(|c: char| c.is_whitespace() || c == ':').any(|w| w == g.as_str())).cloned();
        let Some(gate) = engine::gate_id_in(esc).filter(|g| snap.gate_blocked.contains_key(g)).or_else(named) else { continue };
        let Some(Some(blocked)) = snap.gate_blocked.get(&gate) else { continue };
        acts.push(Act::new(RESOLVE, gate.clone()).with_input(gate.clone()));
        let event = CiFailed { blocked: blocked.clone(), gate: gate.clone(), title: format!("CI red — {blocked} returned to queue"), detail: esc.to_string() };
        acts.push(Act::new(CI_FAILED, format!("ci.failed/{blocked}/{gate}")).with_input(serde_json::to_string(&event).unwrap_or_default()));
    }
    let repo = snap.flaky_gh_repo.trim();
    let mut next = carry.clone();
    if !repo.is_empty() {
        // STEPS 3 and 4 file nothing when the lifecycle rows are unreadable (`main.rs:119-127`).
        if snap.lc_open.is_some() {
            let mut filed: BTreeSet<String> = BTreeSet::new();
            let mut file = |acts: &mut Vec<Act>, title: String, priority: u8, body: String| {
                if filed.insert(title.clone()) {
                    let f = Filing { title: title.clone(), repo: snap.home_repo.clone(), priority, body };
                    acts.push(Act::new(FILE_BEAD, title).with_input(serde_json::to_string(&f).unwrap_or_default()));
                }
            };
            for run in &snap.ci.recent_runs {
                for suite in suites_of(&snap.ci, &run.id, engine::flaky_suites) {
                    let title = engine::flaky_title(&suite);
                    if open_bead(snap, &title).is_none() {
                        file(&mut acts, title, 2, engine::flaky_body(&run.id, &suite));
                    }
                }
            }
            let last_green = snap.ci.last_green_sha.clone().unwrap_or_default();
            let mut seen: Vec<String> = vec![];
            for run in &snap.ci.failed_main_runs {
                for suite in suites_of(&snap.ci, &run.id, engine::red_twice_suites) {
                    if seen.contains(&suite) {
                        continue;
                    }
                    seen.push(suite.clone());
                    let title = engine::red_twice_title(&suite);
                    if let Some(b) = open_bead(snap, &title) {
                        if b.priority > 1 {
                            acts.push(Act::new(RAISE_PRIORITY, format!("{}/1", b.id)).with_input(b.id.clone()));
                            let sha = if run.head_sha.is_empty() { "?" } else { run.head_sha.as_str() };
                            let text = format!("Raised to P1: {suite} still red on main (run {}, commit {sha})", run.id);
                            acts.push(Act::new(NOTE, format!("{}/{}", b.id, run.id)).with_input(serde_json::json!({"id": b.id, "text": text}).to_string()));
                        }
                        continue;
                    }
                    let fail_lines = snap.ci.fail_lines.get(&run.id).map(|l| l.join("\n")).unwrap_or_default();
                    let commits = snap.commits.get(&run.head_sha).cloned().unwrap_or_default();
                    file(&mut acts, title, 1, engine::red_twice_body(&suite, &run.id, &run.head_sha, &last_green, &fail_lines, &commits));
                }
            }
        }
        // STEP 5: TSD ingest, once per run.
        let seen: BTreeSet<String> = snap.ci.recent_runs.iter().map(|r| r.id.clone()).collect();
        if carry.ingested.is_empty() {
            if !acts.is_empty() {
                next.ingested = seen;
            }
        } else {
            for run in snap.ci.recent_runs.iter().filter(|r| !carry.ingested.contains(&r.id)) {
                acts.push(Act::new(TSD_INGEST, format!("{}/{}", run.repo, run.id)).with_input(serde_json::json!({"repo": run.repo, "run": run.id}).to_string()));
                next.ingested.insert(run.id.clone());
            }
        }
    }
    let next = record_ingested(carry, &next).unwrap_or_else(|_| carry.clone());
    (next, acts)
}

// ---------------------------------------------------------------------------------------------
// Runners. The store is reached over `LcClient` (its content door for gates, beads and notes);
// GitHub through `gh`, the one allowed child, on the `forge` class; the writes the store has no
// verb for yet run as transient units through the Units port, never as this process's children.

/// The store's 5 s door (S7a).
pub fn store(e: &crate::Effects) -> spira_config::lc_call::LcClient {
    spira_config::lc_call::LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_secs(5))
}

/// One `gh` call through the process port: stdout when it exited 0, `None` otherwise.
pub fn gh(e: &crate::Effects, args: &[&str], deadline_secs: u64) -> Option<String> {
    use runtime::ports::Proc;
    let prog = if e.forge.gh.is_empty() { "gh".to_string() } else { e.forge.gh.clone() };
    let spec = runtime::ports::ProcSpec { prog, args: args.iter().map(|a| a.to_string()).collect(), stdin: vec![], deadline: std::time::Duration::from_secs(deadline_secs) };
    let out = runtime::ports::SysProc.run(&spec).ok()?;
    (out.code == Some(0)).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One `git -C <repo>` call through the process port: stdout when it exited 0.
pub fn git(repo: &std::path::Path, args: &[&str], deadline_secs: u64) -> Option<String> {
    git_out(repo, args, deadline_secs).ok().filter(|(code, _, _)| *code == Some(0)).map(|(_, out, _)| out)
}

/// One `git -C <repo>` call: (exit code, stdout, stderr), or why it could not run.
pub fn git_out(repo: &std::path::Path, args: &[&str], deadline_secs: u64) -> Result<(Option<i32>, String, String), String> {
    use runtime::ports::Proc;
    let mut all = vec!["-C".to_string(), repo.display().to_string()];
    all.extend(args.iter().map(|a| a.to_string()));
    let out = runtime::ports::SysProc.run(&runtime::ports::ProcSpec { prog: "git".into(), args: all, stdin: vec![], deadline: std::time::Duration::from_secs(deadline_secs) })?;
    Ok((out.code, String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// A leg the store has no in-process verb for yet: the tool the old unit ran, started as a
/// transient unit named for `(task, key)` so a re-issued effect names the same unit.
pub fn unit_leg(e: &crate::Effects, task: &str, key: &str, argv: Vec<String>, cwd: Option<std::path::PathBuf>) -> Result<String, String> {
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    let spec = runtime::units::UnitSpec { task: task.into(), key: key.into(), argv, env: vec![], cwd };
    let h = units.start(&spec).map_err(|e| e.0)?;
    serde_json::to_string(&h).map_err(|e| e.to_string())
}

/// The snapshot: the gate list and each gate's blocked bead over the content door, the
/// lifecycle rows over `LcClient`, the repository map in-process, and the CI facts from `gh`.
/// `bd gate check` has no store verb, so no ESCALATE line is observed until one exists.
pub fn observe(e: &crate::Effects) -> Result<String, String> {
    use gate_check::engine;
    let lc = store(e);
    let flaky_gh_repo = spira_config::process::cfg("SPIRA_FLAKY_GH_REPO")?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    let gate_json = lc.answer(&["content", "gate", "list", "--json"]).unwrap_or_default();
    let branches = engine::discover_branches(&gate_json);
    let mut pr_repos = vec![];
    if !branches.is_empty() {
        for name in reg.names() {
            if reg.land(&name) == "pr" && reg.root(&name).is_some_and(|r| std::path::Path::new(&r).join(".git").exists()) {
                pr_repos.push(PrRepo { name, branches: branches.clone() });
            }
        }
    }
    let rows = spira_config::lc_state::list_on(&lc).ok();
    let lc_open = rows.as_ref().map(|r| r.iter().filter(|r| !r.terminal()).map(|r| r.bead_id.clone()).collect());
    let mut snap = Snapshot { gate_list: branches, lc_open, pr_repos, flaky_gh_repo: flaky_gh_repo.clone(), home_repo: reg.home_repo().to_string(), ..Snapshot::default() };
    let repo = flaky_gh_repo.trim();
    if repo.is_empty() {
        return serde_json::to_string(&snap).map_err(|e| e.to_string());
    }
    let runs = |args: &[&str]| gh(e, &[&["run", "list", "--repo", repo][..], args].concat(), 30).unwrap_or_default();
    let recent = runs(&["--status", "completed", "--limit", "20", "--json", "databaseId,headBranch,conclusion,headSha"]);
    let row = |v: &serde_json::Value| Run { id: v["databaseId"].as_u64().map(|n| n.to_string()).unwrap_or_default(), repo: repo.to_string(), branch: v["headBranch"].as_str().unwrap_or("").into(), conclusion: v["conclusion"].as_str().unwrap_or("").into(), head_sha: v["headSha"].as_str().unwrap_or("").into() };
    let parse = |json: &str| -> Vec<Run> { serde_json::from_str::<Vec<serde_json::Value>>(json).unwrap_or_default().iter().map(row).filter(|r| !r.id.is_empty()).collect() };
    snap.ci.recent_runs = parse(&recent);
    snap.ci.failed_main_runs = parse(&runs(&["--branch", "main", "--status", "failure", "--limit", "10", "--json", "databaseId,headSha"]));
    snap.ci.last_green_sha = engine::run_list_field(&runs(&["--branch", "main", "--status", "success", "--limit", "1", "--json", "headSha"]), "headSha").into_iter().next();
    let ids: BTreeSet<String> = snap.ci.recent_runs.iter().chain(&snap.ci.failed_main_runs).map(|r| r.id.clone()).collect();
    for run in &ids {
        let jobs = gh(e, &["api", &format!("repos/{repo}/actions/runs/{run}/jobs")], 30).unwrap_or_default();
        for job in engine::job_ids(&jobs) {
            if let Some(a) = gh(e, &["api", &format!("repos/{repo}/check-runs/{job}/annotations")], 30) {
                snap.ci.annotations_by_job.insert(format!("{run}/{job}"), vec![a]);
            }
        }
    }
    // Open beads only for the titles this snapshot can file, and the red-twice detail only for
    // the suites that will be filed.
    let list = lc.answer(&["content", "list", "--all", "--json"]).unwrap_or_default();
    let lc_rows = rows.map(spira_config::lc_state::index).unwrap_or_default();
    let titled = |pick: fn(&str) -> Vec<String>, title: fn(&str) -> String, runs: &[Run]| -> Vec<(String, Run)> { runs.iter().flat_map(|r| suites_of(&snap.ci, &r.id, pick).into_iter().map(|s| (title(&s), r.clone()))).collect() };
    let flaky = titled(engine::flaky_suites, engine::flaky_title, &snap.ci.recent_runs);
    let red = titled(engine::red_twice_suites, engine::red_twice_title, &snap.ci.failed_main_runs);
    for (title, _) in flaky.iter().chain(&red) {
        if let Some((id, priority)) = engine::find_open_bead(&list, title, &lc_rows) {
            snap.beads_by_title.insert(title.clone(), Bead { id, priority: priority.clamp(0, 9) as u8, open: true });
        }
    }
    let root = reg.root(&snap.home_repo);
    let green = snap.ci.last_green_sha.clone().unwrap_or_default();
    for (title, run) in red.iter().filter(|(t, _)| !snap.beads_by_title.contains_key(t)) {
        if !snap.ci.fail_lines.contains_key(&run.id) {
            let log = gh(e, &["run", "view", &run.id, "--repo", repo, "--log-failed"], 60).unwrap_or_default();
            snap.ci.fail_lines.insert(run.id.clone(), engine::fail_lines(&log));
        }
        let _ = title;
        if let (Some(root), false, false) = (&root, green.is_empty(), run.head_sha.is_empty() || run.head_sha == green) {
            snap.commits.entry(run.head_sha.clone()).or_insert_with(|| git(std::path::Path::new(root), &["log", "--oneline", &format!("{green}..{}", run.head_sha)], 30).unwrap_or_default().trim_end().to_string());
        }
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `ci.failed`: `spira_event`, now `bead::event::emit` in this process, at bead's own default
/// cooldown of an hour per (kind, target).
pub fn ci_failed(e: &crate::Effects, input: &str) -> Result<String, String> {
    let ev: CiFailed = serde_json::from_str(input).map_err(|e| format!("{CI_FAILED}: bad input: {e}"))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    bead::event::emit(&e.world.run_dir, 3_600, now, "ci.failed", &ev.blocked, &ev.title, &ev.detail).map_err(|_| format!("{CI_FAILED}: the event for {} could not be written", ev.blocked))?;
    Ok(format!("ci.failed {}", ev.blocked))
}

/// `note`: the store's `bd-note` verb.
pub fn note(e: &crate::Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{NOTE}: bad input: {e}"))?;
    let (id, text) = (v["id"].as_str().unwrap_or(""), v["text"].as_str().unwrap_or(""));
    store(e).answer(&["bd-note", id, text]).map_err(String::from)
}

/// `discover`: `bdq gate discover --branch B` in the repository, as a unit leg.
pub fn discover(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{DISCOVER}: bad input: {e}"))?;
    let (repo, branch) = (v["repo"].as_str().unwrap_or(""), v["branch"].as_str().unwrap_or(""));
    let home = spira_config::resolve::locate_home_for_process()?;
    let root = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home).root(repo).ok_or_else(|| format!("{DISCOVER}: {repo} has no checkout — refused"))?;
    unit_leg(e, NAME, key, ["bdq", "gate", "discover", "--branch", branch].map(String::from).to_vec(), Some(root.into()))
}

/// `resolve`: `bdq gate resolve <gate>`, as a unit leg.
pub fn resolve(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    unit_leg(e, NAME, key, ["bdq", "gate", "resolve", input].map(String::from).to_vec(), None)
}

/// `raise_priority`: `bdq priority <id> 1`, as a unit leg.
pub fn raise_priority(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    unit_leg(e, NAME, key, ["bdq", "priority", input, "1"].map(String::from).to_vec(), None)
}

/// `file_bead`: `bead.sh file … --body-file <path>`, as a unit leg; the body is written under
/// `$SPIRA_RUN/gate-check/` first, named by the title's hash so a re-issue rewrites one file.
pub fn file_bead(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    let f: Filing = serde_json::from_str(input).map_err(|e| format!("{FILE_BEAD}: bad input: {e}"))?;
    let dir = e.world.run_dir.join("gate-check");
    std::fs::create_dir_all(&dir).map_err(|err| format!("{FILE_BEAD}: {}: {err}", dir.display()))?;
    let body = dir.join(format!("body-{:016x}.md", runtime::event::fnv1a(&f.title)));
    std::fs::write(&body, format!("{}\n\n## Build\n\nFix the gate failure described above in this repository and land the fix.\n", f.body)).map_err(|err| format!("{FILE_BEAD}: {}: {err}", body.display()))?;
    let argv = vec!["bead.sh".into(), "file".into(), f.title, "--for".into(), "builder".into(), "--repo".into(), f.repo, "-p".into(), f.priority.to_string(), "--body-file".into(), body.display().to_string()];
    unit_leg(e, NAME, key, argv, None)
}

/// `tsd_ingest`: `tsd-ingest.sh <repo> <run>`, a minutes-long artifact download, as a unit leg.
pub fn tsd_ingest(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{TSD_INGEST}: bad input: {e}"))?;
    unit_leg(e, NAME, key, vec!["tsd-ingest.sh".into(), v["repo"].as_str().unwrap_or("").into(), v["run"].as_str().unwrap_or("").into()], None)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateCheck {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl GateCheck {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        GateCheck { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("gate.check/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for GateCheck {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
