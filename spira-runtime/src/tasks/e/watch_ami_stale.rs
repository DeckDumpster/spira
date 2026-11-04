//! # watch.ami_stale — contract
//!
//! watchd row `round-ami-stale` (`spira/watchers:158`), target `@SPIRA_HOME@/round-ami-stale.sh watch --interval 300`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! ## Tick
//!
//! - Cadence 300 s (`--interval 300`, `spira/watchers:158`; loop `spira/round-ami-stale.sh:313-330`). No waking events today; a release activation is the natural extra wake (the recipe hash changes) (unverified: no such event exists yet). Single-flight.
//!
//! ## Observe
//!
//! `watch.ami_stale.observe` returns:
//!
//! ```text
//! Snapshot { spill: String, verdict: Fresh | Stale { line } | Unreadable { rc, out } }
//! ```
//!
//! - `spill` from `SPIRA_ROUND_VM_SPILL`; anything but `ec2`, `ec2-always`, `ec2-only` → idle (`round-ami-stale.sh:290`).
//! - `verdict` from round-vm's own decision, not re-derived: today the child `timeout 60 round-vm ami-check` (`:292`) → `round-vm/src/cli.rs:269` → `Ec2::ami_check` (`round-vm/src/ec2.rs:328`, `ami_staleness(images, recipe_sha)`). In-process this becomes a library call into `round-vm`'s `ami_check`; the AWS image listing stays the external edge.
//! - Exit 0 → `Fresh` (`:294`); exit 1 with a last line → `Stale{line}`; any other exit or empty output → `Unreadable` with the line composed at `:296` ("round AMI check could not read the images: 'round-vm ami-check' exited <rc>: <out[..400]>").
//!
//! ## Decide
//!
//! - Carry `watch.ami_stale.state` = `{ said: Option<String> }` — today `run/watchd/round-ami-stale.said` (`:280`), which holds the last line said (`:300`), removed on fresh (`:294`).
//! - `Fresh` → `said = None`, nothing. `Stale|Unreadable` with `line == said` → nothing. Otherwise one inbox line, `said = Some(line)`.
//! - So a *different* stale line re-announces, and an unreadable listing is never read as fresh (law-absence-needs-a-positive-control).
//! - Fixtures: none in the release (unverified: no `test-round-ami-stale*.sh`). New: spill off; fresh; stale once; same stale twice; stale→different stale; unreadable; fresh clears then stale again.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.ami_stale.observe` | host (aws edge) | 60 s (today's `timeout 60`, `:292`) | (probe) | spill not ec2 → returns `spill` only, no AWS call |
//! | `inbox.append` (shared) | host | 10 s | `ami_stale/<fnv1a(line)>/<state event id>` | — |
//!
//! Inbox line, exactly as today (`spira/round-ami-stale.sh:285`, text `:299`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:round-ami-stale] <line>; EC2 rounds refuse the image until the Round AMI workflow builds one
//! ```
//!
//! Today the line goes both to stdout → `run/watchd/round-ami-stale.log` (`:283`) and directly to `SPIRA_CONCIERGE_INBOX` (`:284-285`). Readers of `run/watchd/round-ami-stale.log`: none by name in the release or `run/concierge-notes`; only watchd's generic walkers (`notify`, `drain`, `peek`, `tail`); no cursor file exists. The log copy is dropped.
//!
//! Health (`round-ami-stale.sh health`, `:303-311`) → S13 `scheduler status` for `watch.ami_stale` (last fired, lag, last outcome — the outcome now distinguishes `Unreadable`, which the health file never did).
//!
//! ## Silence
//!
//! Spill off, or `Fresh` with `said=None`, or the same line as `said`: probe only, no `Emit`, no effect.
//!
//! ## Replaces
//!
//! - `spira-watch-round-ami-stale-prod.service`; row `spira/watchers:158`.
//! - Children: `bash lib.sh` (`:277`), `timeout` + `round-vm ami-check` (`:292`), `grep`, `tail`, `tr`, `cut`.
//! - Files retired: `run/watchd/round-ami-stale.{said,health,log,restarts}`.
//! - Config pinned: `SPIRA_ROUND_VM_SPILL` (`spira/conf.d.index:3539`), plus whatever `round-vm`'s `Config::load` needs for `ami_check` (region, recipe sha) (unverified list).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use crate::tasks::d::pattern;
use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "watch.ami_stale";
pub const STREAM: &str = "watch.ami_stale";
pub const TICK: EventKindTag = "watch.ami_stale.tick";
pub const STATE: EventKindTag = "watch.ami_stale.state";
pub const OBSERVE: &str = "watch.ami_stale.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
/// `round-vm`'s own `Config::load` keys for `ami_check` are an unverified list in the contract.
pub const KEYS: &[&str] = &["SPIRA_ROUND_VM_SPILL", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag (`spira/round-ami-stale.sh:285`).
pub const INBOX_TAG: &str = "round-ami-stale";
/// Appended to every stale line (`round-ami-stale.sh:299`).
pub const SUFFIX: &str = "; EC2 rounds refuse the image until the Round AMI workflow builds one";
/// `--interval 300`.
pub const CADENCE_MS: u64 = 300_000;
const OBSERVE_DEADLINE_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub spill: String,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { spill: cfg("SPIRA_ROUND_VM_SPILL")?, inbox: cfg("SPIRA_CONCIERGE_INBOX")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[default]
    Fresh,
    Stale { line: String },
    Unreadable { rc: i32, out: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub spill: String,
    pub verdict: Verdict,
}

/// Today's `round-ami-stale.said`: the last line said, cleared on fresh.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: Option<String>,
}

/// `Fresh` clears; a stale or unreadable line other than `said` is said once.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.ami_stale: not ported yet")
}

pub struct WatchAmiStale {
    pub config: Result<Config, String>,
}

impl WatchAmiStale {
    pub fn new(config: Config) -> Self {
        WatchAmiStale { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchAmiStale { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.ami_stale/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchAmiStale {
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
