//! # watch.skew — contract
//!
//! Replaces `spira-skew-prod` (`ExecStartPre=-skew refresh`, `ExecStart=skew check --escalate`), pattern P1. Both halves move: refresh and check are one tick.
//!
//! ## Tick
//!
//! - Hourly (`OnUnitActiveSec=1h`, `OnBootSec=10min`); single-flight.
//! - Order is kept: the refresh judgement first, then the check (unit `ExecStartPre`).
//!
//! ## Observe
//!
//! `watch.skew.observe` returns:
//!
//! ```text
//! { mode: local|release|checkout, activated, manifest_commit, base, tip,
//!   hotfix_line, verify: {ok, out}, queue_lock_held, tags[], tag_sidecar,
//!   decline_streak, stamps{key→condition} }
//! ```
//!
//! Each field comes from the component that decides it:
//!
//! - `current` symlink and `MANIFEST` `commit` (`skew/src/lib.rs:198-237`).
//! - Repo land mode from the registry `repo_land(home)`, in-process via `spira_config::repos` (`skew/src/real.rs:111-118`).
//! - `landref` and `rev_parse` / `is_ancestor` / `rev-list --count` through git (`real.rs:65-89,174-188`). The git class stays a child.
//! - `release verify <name> --no-pre-activate` (`real.rs:230-246`) and `release status`, whose `RUNNING UNLANDED` line names a hotfix (`real.rs:247-257`). Both are release-tool verbs; they become library calls into `release` (§2 sibling binaries).
//! - Queue lock: a non-blocking flock on `SPIRA_QUEUE_DIR/<repo>/lock` (`real.rs:401-417`).
//! - Release tags: the `.tags/<name>` sidecar, else `gh release list` (`real.rs:314-334`). Only artifact mode needs gh; under `queue.local` no gh is asked.
//! - Stamps today are files under `SPIRA_RUN`: `skew.escalated-<cksum>` and `skew.refresh-declined-<cksum>` (`lib.rs:575,828-850`, `real.rs:419-444`). They become carried state.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Acts)` composes today's pure core over the `World` seam (`skew/src/ports.rs`):
//!
//! - `refresh_check_only` gives `LOCAL-SKEW` (`lib.rs:903-982`).
//! - `check_local` gives `LOCAL-BEHIND` / `LOCAL-TAMPERED` / `CANNOT-VERIFY` (`lib.rs:479-569`).
//! - `check` gives `NOT-LATEST` / `MANIFEST-MISMATCH` (`lib.rs:183-334`).
//! - `escalate` sends once per condition key (`lib.rs:574-610`).
//! - The decline streak (`lib.rs:832-850`) alarms at 3 passes and every 180 after.
//!
//! Carry: `{ escalated: {cksum(key)→condition}, declined: {repo→n} }`, emitted as `watch.skew.state`.
//!
//! Fixtures: `skew/src/tests.rs` (50 tests), e.g.:
//!
//! - `check_local_tampered_and_behind_both_fire` (`tests.rs:365`)
//! - `check_local_hotfix_is_clean` (`:405`)
//! - `escalate_sends_once_per_condition_per_run` (`:568`)
//! - `refresh_queue_local_mismatch_escalates_local_skew` (`:640`)
//! - `…during_a_land_local_does_not_escalate` (`:658`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.skew.observe` (probe) | git | 60 s | — | an unsourceable config. Today that is exit 3, could-not-check (`main.rs:39-41`) |
//! | `watch.skew.escalate` | store | 30 s | `watch.skew:<cksum(condition_key)>` | a key already in carry |
//! | `watch.skew.log` | host | 5 s | `watch.skew:refresh-declined:<repo>:<n>` | a path outside `SPIRA_RUN` |
//! | `watch.skew.refresh` | git | 300 s | `watch.skew:refresh:<repo>:<base sha>` | `queue.local`, which is check-only and never resets (`lib.rs:656-658`) |
//!
//! - **`watch.skew.escalate`** is an operator question. The mail today is `mail send operator --from "Skew check <skew@spira>" --subject "The Spira copy in force is not the code that landed" --kind question --default <action>`, with the body from stdin (`real.rs:335-360`, `lib.rs:594-597`).
//! - **`watch.skew.log`** appends `skew: refresh of <repo> has declined <n> consecutive passes<lock note>; <what>` to `run/skew-refresh.log` (`lib.rs:845-848`, log row `spira/watchers:185`).
//! - **`watch.skew.refresh`** applies to checkout and release mode only: fetch, ff-merge, `release build/verify/activate`, `overrides.sh apply` (`lib.rs:676-700`, `real.rs:258-282`).
//!   - Release activation is a long, host-changing leg. If it is ever exercised it should be a `unit.start` (unverified that prod ever takes this path; prod's home repo is `queue.local`).
//!
//! No inbox line is written today. Verdict lines go to stdout, i.e. `run/skew.log` (unit `StandardOutput`), which no watchd row reads. The task drops them; the decisions are in the journal.
//!
//! ## Silence
//!
//! An idle tick, where running matches base or is a recorded hotfix and no stamp changes, emits the probe only. The verdict "in effect" is not journaled.
//!
//! ## Replaces
//!
//! - `spira-skew-prod.service` and `spira-skew-prod.timer` (`TimeoutStartSec=300`).
//! - Children:
//!   - `bash -c '. lib.sh'` init guard (`real.rs:28`)
//!   - `git` (`real.rs:65-89,216`)
//!   - `release verify/status/build/activate` (`real.rs:236,250,261`)
//!   - `overrides.sh apply` (`real.rs:281`)
//!   - `units-install --diff` (`real.rs:288`; `skew units`, not on the timer)
//!   - `gh release list` (`real.rs:321`)
//!   - `mail send` (`real.rs:339`)
//!   - `date -u` (`real.rs:363`)
//! - Keys pinned: `SPIRA_RELEASES`, `SPIRA_RELEASE_REPO`, `SPIRA_GH_INTAKE_REPO`, `SPIRA_GH`, `SPIRA_RUN`, `SPIRA_QUEUE_DIR` (`real.rs:40`), `SPIRA_REPO` (`lib.rs:200`).
//!
//! Catalogue differs: skew forks no `spira-lc`. Its children are git, release, mail, gh, bash `lib.sh`, overrides.sh and date. The ExecStartPre `skew refresh` is part of this unit and moves with it.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Observe mode shadows `escalate`, `log` and `refresh`.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;

pub const NAME: &str = "watch.skew";
pub const STREAM: &str = "watch.skew";
pub const TICK: EventKindTag = "watch.skew.tick";
pub const STATE: EventKindTag = "watch.skew.state";
pub const OBSERVE: &str = "watch.skew.observe";
pub const ESCALATE: &str = "watch.skew.escalate";
pub const LOG: &str = "watch.skew.log";
pub const REFRESH: &str = "watch.skew.refresh";
pub const EFFECTS: &[&str] = &[OBSERVE, ESCALATE, LOG, REFRESH];
pub const KEYS: &[&str] = &["SPIRA_RELEASES", "SPIRA_RELEASE_REPO", "SPIRA_GH_INTAKE_REPO", "SPIRA_GH", "SPIRA_RUN", "SPIRA_QUEUE_DIR"];
/// `OnUnitActiveSec=1h`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// The decline streak alarms at this many passes, then every `DECLINE_EVERY` after (`lib.rs:832-850`).
pub const DECLINE_ALARM: u32 = 3;
pub const DECLINE_EVERY: u32 = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub releases: String,
    pub release_repo: String,
    pub gh_intake_repo: String,
    pub gh: String,
    pub run: String,
    pub queue_dir: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { releases: cfg("SPIRA_RELEASES")?, release_repo: cfg("SPIRA_RELEASE_REPO")?, gh_intake_repo: cfg("SPIRA_GH_INTAKE_REPO")?, gh: cfg("SPIRA_GH")?, run: cfg("SPIRA_RUN")?, queue_dir: cfg("SPIRA_QUEUE_DIR")? })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Local,
    Release,
    Checkout,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verify {
    pub ok: bool,
    pub out: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The home repo's checkout, which the decline-streak log line names.
    pub repo: String,
    pub mode: Mode,
    pub activated: Option<String>,
    pub manifest_commit: Option<String>,
    pub base: Option<String>,
    pub tip: Option<String>,
    pub hotfix_line: Option<String>,
    pub verify: Verify,
    pub queue_lock_held: bool,
    pub tags: Vec<String>,
    pub tag_sidecar: Option<String>,
    pub decline_streak: u32,
    pub stamps: BTreeMap<String, String>,
}

/// `{ escalated: {cksum(key)→condition}, declined: {repo→n} }`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub escalated: BTreeMap<String, String>,
    pub declined: BTreeMap<String, u32>,
}

/// The refresh judgement, then the check, then `escalate` once per condition key.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.skew: not ported yet")
}

pub struct WatchSkew {
    pub config: Result<Config, String>,
}

impl WatchSkew {
    pub fn new(config: Config) -> Self {
        WatchSkew { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchSkew { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.skew/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchSkew {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
