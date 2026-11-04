//! # watch.ci_spill — contract
//!
//! watchd row `ci-spill-guard` (`spira/watchers:159`), target `@SPIRA_HOME@/ci-spill-guard.sh watch --interval 900`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the children are `lib.sh`, `git rev-parse`, `git archive | tar -x` and `awk` (`spira/ci-spill-guard.sh:89`, `:93`, `:19`), not `gh`. The guard reads each repo's landing ref from git, never the forge.
//!
//! ## Tick
//!
//! - Cadence 900 s (`spira/watchers:159`; loop `:120-137`). A landing on any registered repo is the natural extra wake (the landing ref moved) (unverified: no such event on the bus yet). Single-flight.
//!
//! ## Observe
//!
//! `watch.ci_spill.observe` (git class) takes the carried per-repo seen SHAs and returns:
//!
//! ```text
//! Snapshot { repos: Vec<Repo> }
//! Repo { name, ref_: Option<String>, sha: Option<String>, workflows: Option<Vec<(path, text)>> }  // workflows only when sha != seen
//! ```
//!
//! - Repo set and roots from the repo map: `repo_names` / `repo_root` (`spira/lib.sh:1728`, `:1737`) — in-process from `Settings` (S10).
//! - `ref_` from `spira_landref` (`spira/lib.sh:1930`), the landing component's own answer; `None` → the "no resolvable landing ref" finding (`ci-spill-guard.sh:88`).
//! - `sha` = `git rev-parse <ref>` (`:89`); `workflows` = the `.github/workflows/*.y{a,}ml` blobs at that ref — today `git archive <ref> .github/workflows | tar -x` into a tmpdir (`:92-93`); in-process a `git show`/tree read, no tmpdir.
//!
//! ## Decide
//!
//! - Pure scan: the awk in `_csg_file` (`:18-38`) becomes a Rust function `violations(path, text) -> Vec<String>` — a `uses: …/provision@…` step whose `spill:` is absent or not `ec2-only`. The same function serves `ci-spill-guard.sh --gate` (gate fence, `gate.steps`) once the gate moves; until then both exist (parity test pins them equal).
//! - Carry `watch.ci_spill.state` = `{ said: BTreeSet<String>, seen: BTreeMap<repo, sha> }` — today `run/watchd/ci-spill-guard.said` (append-only, `:75-80`, never cleared) and `run/watchd/ci-spill-guard.refs/<repo>` (`:90`, `:98`).
//! - For each repo with a new sha: every violation line not in `said` → one inbox line; then `seen[repo] = sha`. A repo with no ref → key `noref-<repo>` once.
//! - Note (from code): `said` is never pruned today, so a violation fixed and later reintroduced is not said again. Kept for parity; flagged for Ryan.
//! - Fixtures: none in the release (unverified: no `test-ci-spill-guard*.sh`). New: compliant workflow; missing `spill:`; `spill: ec2` (wrong); quoted value; nested step indentation; repo with no ref; unchanged sha skipped.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.ci_spill.observe` | git | 30 s | (probe) | a repo whose root does not exist → omitted, as `repo_root … || continue` (`:87`) |
//! | `inbox.append` (shared) | host | 10 s | `ci_spill/<said-key>` | — |
//!
//! Inbox line, exactly as today (`spira/ci-spill-guard.sh:79`, text `:96` / `:88`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:ci-spill-guard] CI SPILL: law-buy-capacity-rather-than-wait: <repo>:<file>:<line>: provision has no spill: input; override: spill: ec2-only (at <ref>)
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:ci-spill-guard] CI SPILL: <repo> has no resolvable landing ref; its workflows are unchecked
//! ```
//!
//! Today each line also goes to stdout → `run/watchd/ci-spill-guard.log` (`:77`). Readers of that log: none by name in the release or `run/concierge-notes`; watchd's generic walkers only; no cursor file exists. The log copy is dropped.
//!
//! Health (`ci-spill-guard.sh health`, `:109-118`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! No repo's sha moved → probe only (the snapshot carries no workflows), no `Emit`. A moved sha with no new violation emits the `seen` update only — one `state` event per landing, not per tick.
//!
//! ## Replaces
//!
//! - `spira-watch-ci-spill-guard-prod.service`; row `spira/watchers:159`.
//! - Children: `bash lib.sh` (`:70`), `git rev-parse` (`:89`), `git archive` + `tar` (`:93`), `mktemp`/`rm -rf` (`:92`, `:100`), `awk` (`:19`).
//! - Files retired: `run/watchd/ci-spill-guard.{said,health,log,restarts}`, `run/watchd/ci-spill-guard.refs/`.
//! - Config pinned: the repo map behind `repo_names`/`repo_root`/`spira_landref`; `SPIRA_CONCIERGE_INBOX`.
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`. The `scan`/`--gate` subcommands stay in the script until the gate fence moves, so nothing else regresses.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use crate::tasks::d::pattern;
use crate::watchers::{carried, INBOX_APPEND};

pub const NAME: &str = "watch.ci_spill";
pub const STREAM: &str = "watch.ci_spill";
pub const TICK: EventKindTag = "watch.ci_spill.tick";
pub const STATE: EventKindTag = "watch.ci_spill.state";
pub const OBSERVE: &str = "watch.ci_spill.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_REPO_MAP", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag in every line it wrote (`spira/ci-spill-guard.sh:79`).
pub const INBOX_TAG: &str = "ci-spill-guard";
/// `--interval 900`.
pub const CADENCE_MS: u64 = 900_000;
const OBSERVE_DEADLINE_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub repo_map: String,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { repo_map: cfg("SPIRA_REPO_MAP")?, inbox: cfg("SPIRA_CONCIERGE_INBOX")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub sha: Option<String>,
    /// `(path, text)` of each workflow; only when `sha` differs from the carried one.
    pub workflows: Option<Vec<(String, String)>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: Vec<Repo>,
}

/// `{ said, seen: repo → sha }`; `said` is never pruned, as today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: BTreeSet<String>,
    pub seen: BTreeMap<String, String>,
}

/// The awk of `_csg_file` as Rust: a `uses: …/provision@…` step whose `spill:` is absent or not
/// `ec2-only`, one finding per step.
pub fn violations(_path: &str, _text: &str) -> Vec<String> {
    unimplemented!("watch.ci_spill: not ported yet")
}

/// For each repo with a new sha, every violation not yet said as one inbox line; then `seen`.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.ci_spill: not ported yet")
}

pub struct WatchCiSpill {
    pub config: Result<Config, String>,
}

impl WatchCiSpill {
    pub fn new(config: Config) -> Self {
        WatchCiSpill { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchCiSpill { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.ci_spill/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchCiSpill {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let seen = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE).seen).unwrap_or_default();
            return pattern::tick(ev, OBSERVE, seen, OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
