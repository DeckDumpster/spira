//! # watch.statute_audit — contract
//!
//! This task replaces the `spira-statute-audit-prod` unit, which runs `rule audit --file --for builder --max 5` and appends its stdout and stderr to `run/statute-audit.log` (TimeoutStartSec=300). Pattern P4. Part of [[scheduler-consolidation-2026-11-03]].
//!
//! Catalogue differs: the catalogue lists the children as "bead.sh, bd, git". The `rule audit` code forks `bd` twice (`rule/src/main.rs:96-97`, called at `:111` and `:657`) and `bead.sh file` once per filing (`:728-732`). It forks no `git`. Any `git` would come from inside `bead.sh` (unverified).
//!
//! ## Tick
//!
//! - Calendar `weekly`, which is Monday 00:00 in local time, through the S8 `Calendar::next_after`.
//! - `Persistent` is not set on the timer (unverified), so a missed run is not caught up.
//! - Single-flight.
//!
//! ## Observe
//!
//! `watch.statute_audit.observe` returns:
//!
//! ```text
//! Snapshot {
//!   memories: BTreeMap<slug, text>,
//!   release: BTreeMap<tool, content>,
//!   open_titles: Vec<String>,
//!   persona: "builder",
//!   repo,
//! }
//! ```
//!
//! Each field comes from one source:
//!
//! - **`memories`** is the statute book. Today it is read with `bd -C $SPIRA_DB memories --json` (`main.rs:110-120`). The target is the store's `content memories` verb through `LcClient` (S7a). If the book cannot be read, the probe fails; it does not return an empty book (today that case exits 1 at `main.rs:698-704`).
//! - **`release`** is the content of each tool named by `extract_refs` (in `rule/src/audit.rs`). The lookup reads `SPIRA_HOME`, then its parent, then `bin/`, then `model-bin/` of the activated release (`main.rs:640-654`). This is a host read of the release tree. No store holds this information.
//! - **`open_titles`** is the list of open bead titles. Today it comes from `bd list --json --limit 0` (`main.rs:656-666`); whether that list is limited to open beads is unverified. The target is the store's `content list` verb filtered by lifecycle state, through `LcClient`.
//! - **`repo`** comes from `--repo`, or failing that from the basename of `SPIRA_HOME_REPO` (`main.rs:698-702`).
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<Act>` is pure and keeps no carry. It reuses existing code:
//!
//! - `rule::audit::audit(memories, lookup)` (`rule/src/audit.rs:111-125`), with `lookup` served from `snap.release`.
//! - `finding_title` (`audit.rs:127-129`).
//! - The filter and bound from `cmd_audit` (`main.rs:714-727`): skip a finding whose slug appears in any open title, and file at most `max = 5` per run. The rest wait for the next week.
//!
//! The bead body is exactly `"{title}\n\nThe statute names the above and the activated release has no such tool or verb. Build it, or amend the statute to name what exists (rule.sh enact with --tool).\n"` (`main.rs:724-727`).
//!
//! Fixtures are the three tests in `rule/src/audit.rs` (from `:156`). This task adds one fixture for the open-title skip and one for the cap of 5.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.statute_audit.observe` (probe) | store + host (release read) | 60 s | none | — |
//! | `watch.statute_audit.file_bead` | store (today `bead.sh file <title> --for builder --repo <repo> --body-file -`, `main.rs:728-743`) | 60 s | `watch.statute_audit:<slug>:<fnv(missing list)>` | a slug already in an open title; the sixth finding of a run; a missing persona or repo (`main.rs:698-701`) |
//!
//! `bead.sh` is an operator tool that stays (§2). How the runner files a bead without forking it is open: it needs the store's create path in-process (unverified that a library exists).
//!
//! Today's log lines go only to `run/statute-audit.log`:
//!
//! - `{slug}: {missing, …}`
//! - `{n} of {m} statutes name something the release does not have`
//! - `rule: audit filed 5, the batch bound; the rest wait for the next run.` (`main.rs:706-721`)
//!
//! No reader of that log exists: the only hit for it in the release is the unit file. The counts become wide-event fields (`ctx.event().set`), not a log.
//!
//! ## Silence
//!
//! A week with no findings, or with only findings that already have open beads, emits the probe only. There is no carry, so there is never an Emit.
//!
//! ## Replaces
//!
//! - `spira-statute-audit-prod.service` and `spira-statute-audit-prod.timer`. There is no watchd row.
//! - The children `bd` (×2) and `bead.sh` (one per filing).
//! - The config keys pinned are `SPIRA_DB` (`main.rs:57-65`), `SPIRA_HOME` (the release root that `release_lookup` reads) and `SPIRA_HOME_REPO`. Persona `builder` and the cap of 5 become task settings.
//!
//! ## Reverse
//!
//! - Release rollback restores the weekly unit.
//! - Observe mode shadows `file_bead`. The shadowed specs list what would have been filed.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;

pub const NAME: &str = "watch.statute_audit";
pub const STREAM: &str = "watch.statute_audit";
pub const TICK: EventKindTag = "watch.statute_audit.tick";
/// Never emitted: the task carries no state.
pub const STATE: EventKindTag = "watch.statute_audit.state";
pub const OBSERVE: &str = "watch.statute_audit.observe";
pub const FILE_BEAD: &str = "watch.statute_audit.file_bead";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_BEAD];
/// `SPIRA_HOME` is not a config key: the release root is located by the config door itself.
pub const KEYS: &[&str] = &["SPIRA_DB", "SPIRA_HOME_REPO"];
/// `OnCalendar=weekly` (S8: Monday 00:00 UTC).
pub const CALENDAR: &str = "weekly";
const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// The unit argv (`rule audit --file --for builder --max 5`).
pub const PERSONA: &str = "builder";
pub const MAX: usize = 5;

/// When the tick after one at `at_ms` is due.
pub fn next_tick(at_ms: u64) -> u64 {
    pattern::calendar(CALENDAR).next_after(at_ms)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub db: String,
    pub home_repo: String,
    pub persona: String,
    pub max: usize,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { db: cfg("SPIRA_DB")?, home_repo: cfg("SPIRA_HOME_REPO")?, persona: PERSONA.into(), max: MAX })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The statute book: slug → text.
    pub memories: BTreeMap<String, String>,
    /// tool → its content in the activated release.
    pub release: BTreeMap<String, String>,
    pub open_titles: Vec<String>,
    pub persona: String,
    pub repo: String,
}

/// What `file_bead` takes: `bead.sh file <title> --for <persona> --repo <repo> --body-file -`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub title: String,
    pub body: String,
    pub persona: String,
    pub repo: String,
}

const FILE_DEADLINE_MS: u64 = 60_000;

/// `rule::audit::audit` over the book, then `cmd_audit`'s open-title skip and bound of `max`.
/// A missing persona or repo files nothing, as `audit --file` refused.
pub fn decide(snap: &Snapshot, cfg: &Config) -> Vec<EffectSpec> {
    let persona = if snap.persona.is_empty() { cfg.persona.clone() } else { snap.persona.clone() };
    if persona.is_empty() || snap.repo.is_empty() {
        return vec![];
    }
    let book: BTreeMap<String, serde_json::Value> = snap.memories.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    let findings = rule::audit::audit(&book, &|tool: &str| snap.release.get(tool).cloned());
    let (batch, _) = rule::audit::to_file(&findings, &snap.open_titles, cfg.max);
    batch
        .into_iter()
        .map(|f| {
            let title = rule::audit::finding_title(f);
            let filing = Filing { body: rule::audit::bead_body(&title), title, persona: persona.clone(), repo: snap.repo.clone() };
            EffectSpec { name: FILE_BEAD.into(), input: serde_json::to_string(&filing).unwrap_or_default(), deadline_ms: FILE_DEADLINE_MS }
        })
        .collect()
}

/// The persona a scheduler filing is made under on the store's `work - file` verb: one the
/// broker's allow table lets file work for another persona.
pub const FILER: &str = "ops";

/// A work bead filed through the store's brokered `work - file` verb (it runs `bead.sh file` and
/// creates the lifecycle row), over the socket with the effect's own deadline: `priority` and
/// `extra` (`--json`, say) join the argv as `bead.sh file` takes them. Answers the verb's stdout.
pub fn file_work(lc_socket: &std::path::Path, f: &Filing, priority: Option<&str>, deadline_ms: u64) -> Result<String, String> {
    let mut argv: Vec<String> = ["work", "-", "file", &f.title, "--for", &f.persona, "--repo", &f.repo].iter().map(|s| s.to_string()).collect();
    if let Some(p) = priority {
        argv.extend(["--priority".to_string(), p.to_string()]);
    }
    argv.extend(["--body-file", "-", "--stdin", &f.body, "--actor", FILER].iter().map(|s| s.to_string()));
    let stream = std::os::unix::net::UnixStream::connect(lc_socket).map_err(|e| format!("cannot tell: the lifecycle service is not answering on {}: {e}", lc_socket.display()))?;
    let wait = Some(std::time::Duration::from_millis(deadline_ms.max(1)));
    stream.set_read_timeout(wait).and_then(|_| stream.set_write_timeout(wait)).map_err(|e| e.to_string())?;
    let reply = spira_config::lc_call::exchange(stream, &argv, None)?;
    match reply.code {
        0 => Ok(reply.out),
        _ => Err(format!("work file {:?}: {}", f.title, reply.why())),
    }
}

/// The probe: the book over `content memories --json` (unreadable is a failed probe, never an
/// empty book), each tool it names read from the activated release, the open titles over
/// `content list`, and the repo from `SPIRA_HOME_REPO`'s basename.
pub fn observe(e: &crate::Effects, _key: &str, _spec: &EffectSpec) -> Result<String, String> {
    let lc = spira_config::lc_call::LcClient::socket(&e.world.lc_socket);
    let raw = lc.answer(&["content", "memories", "--json"]).map_err(|e| format!("the statute book could not be read: {e}"))?;
    let book: BTreeMap<String, serde_json::Value> = serde_json::from_str(&raw).map_err(|e| format!("the statute book did not parse: {e}"))?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let lookup = rule::audit::release_lookup(&home.to_string_lossy());
    let release = rule::audit::named_tools(&book).into_iter().filter_map(|t| lookup(&t).map(|c| (t, c))).collect();
    let listed = lc.answer(&["content", "list", "--json", "--limit", "0"]).map_err(|e| format!("the open beads could not be listed: {e}"))?;
    let open_titles = serde_json::from_str::<Vec<serde_json::Value>>(&listed).unwrap_or_default().iter().filter_map(|b| b["title"].as_str().map(str::to_string)).collect();
    let repo = cfg("SPIRA_HOME_REPO").ok().and_then(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let memories = book.into_iter().filter_map(|(k, v)| v.as_str().map(|t| (k, t.to_string()))).collect();
    serde_json::to_string(&Snapshot { memories, release, open_titles, persona: PERSONA.into(), repo }).map_err(|e| e.to_string())
}

pub fn file_bead(e: &crate::Effects, _key: &str, spec: &EffectSpec) -> Result<String, String> {
    let f: Filing = serde_json::from_str(&spec.input).map_err(|e| format!("{FILE_BEAD}: bad input: {e}"))?;
    file_work(&e.world.lc_socket, &f, None, spec.deadline_ms)
}

pub struct WatchStatuteAudit {
    pub config: Result<Config, String>,
}

impl WatchStatuteAudit {
    pub fn new(config: Config) -> Self {
        WatchStatuteAudit { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchStatuteAudit { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.statute_audit/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchStatuteAudit {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, next_tick(ev.at));
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, |_: &(), snap: &Snapshot, _, config: &Config| ((), decide(snap, config)))
    }
}
