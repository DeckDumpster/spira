//! `intake.kit_bump`: a new release of the kit crate becomes a bump bead per site. Replaces
//! `spira-kit-bump-prod` (`bin/kit-bump pass --config run/kit-bump.conf --state
//! run/kit-bump.state`). Pattern P1. Contract:
//! `wiki/projects/spira/designs/tasks/intake-kit-bump.md`. Not ported yet: [`decide`] and
//! [`step`] are `unimplemented!`, and the row waits in `tasks::f::STAGED`.
//!
//! # Tick
//!
//! Every 10 min (`SPIRA_RUNTIME_INTAKE_KIT_BUMP_SECS`), single-flight, no `world guard`. When
//! `SPIRA_KIT_REGISTRY` is empty the task is unconfigured and a tick does nothing.
//!
//! # Observe
//!
//! `intake.kit_bump.observe` returns a [`Snapshot`]: `release` (`newest_release` over the sparse
//! index, in-process over HTTP as the heartbeat does; a 404 or 410 means nothing is published),
//! `sites` (`git show <ref>:<file>` and the sibling `Cargo.lock`, parsed by `parse_requirement`
//! and `parse_locked`; git stays a class B child) and `open` (the carried bump joined with the
//! `LcClient` show). Sites come from `SPIRA_KIT_SITES` resolved against the repo map; the
//! rendered conf file retires.
//!
//! # Decide
//!
//! The pure core is `kit_bump::pass` and `site_pass` with `title`, `body` and `semver::admits`.
//! Per site, against the newest release: locked at or past it is current; an open bump at that
//! target is current; an open bump at an older target is amended; otherwise file, breaking when
//! the requirement does not admit the release. Carried state is `intake.kit_bump.state { bumps }`.
//! Fixtures port `kit-bump/src/tests.rs` (6 cases) and the two `main.rs` config tests.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.kit_bump.observe` (probe) | host (http) | 30 s | tick id | a registry version that is not semver |
//! | `intake.kit_bump.file` | store | 120 s | `<site>/<release>` | a site whose carry already holds an open bump |
//! | `intake.kit_bump.amend` | store | 120 s | `<site>/<release>` | a bump whose target already equals the release |
//!
//! file: `bead.sh file "<title>" --for <persona> --repo <site>` then the label `kit-bump`; the
//! title is `update <crate> lockfile to <v> in <site>` or `upgrade <crate> to <v> (breaking) in
//! <site>`. amend: `bead.sh amend <id> --replace --note "kit-bump: target moved to <v>"`. Each
//! success is followed by an Emit of the carry. Per-site failures are a wide-event field
//! (`kit_bump.failed`), not an inbox line.
//!
//! # Silence
//!
//! An idle tick (every site current, or no release published) emits the probe only, and no Emit
//! when the bumps map is unchanged.
//!
//! # Replaces
//!
//! `spira-kit-bump-prod.service` and `.timer` and the rendered `run/kit-bump.conf`; children
//! `curl`, `git show` ×2 per site, `spira-lc show`, `bead.sh file/amend/label`. Pinned config:
//! [`KEYS`] (`SPIRA_KIT_PERSONA` stands in for the `--persona` flag's `builder` default).
//!
//! # Reverse
//!
//! Release rollback restores the unit; the carry renders back to `kit-bump.state`
//! (`site\tid\ttarget`). Observe mode shadows `file` and `amend`.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{commands, Cycle, Transition};
use crate::watchers::carried;

pub const NAME: &str = "intake.kit_bump";
pub const STREAM: &str = "task.intake.kit_bump";
pub const TICK: EventKindTag = "intake.kit_bump.tick";
pub const STATE: EventKindTag = "intake.kit_bump.state";

pub const OBSERVE: &str = "intake.kit_bump.observe";
pub const FILE: &str = "intake.kit_bump.file";
pub const AMEND: &str = "intake.kit_bump.amend";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE, AMEND];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_KIT_BUMP_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_KIT_REGISTRY", "SPIRA_KIT_CRATE", "SPIRA_KIT_SITES", "SPIRA_KIT_PERSONA", "SPIRA_METRICS_ENDPOINT"];

const DEADLINE_MS: u64 = 30_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    pub locked: String,
    pub requirement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Open {
    pub id: String,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub release: Result<Option<String>, String>,
    pub sites: BTreeMap<String, Result<Site, String>>,
    pub open: BTreeMap<String, Result<Option<Open>, String>>,
}

/// `intake.kit_bump.state`: per site, the open bump's bead id and target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub bumps: BTreeMap<String, (String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteState {
    Current,
    BumpOpen,
}

pub const TRANSITIONS: &[Transition<SiteState>] = &[
    Transition { from: SiteState::Current, on: "file", to: SiteState::BumpOpen, effects: &[FILE] },
    Transition { from: SiteState::BumpOpen, on: "amend", to: SiteState::BumpOpen, effects: &[AMEND] },
    Transition { from: SiteState::BumpOpen, on: "landed", to: SiteState::Current, effects: &[] },
];

/// The site's next state on `on`, or a refusal naming the state it is in.
pub fn step(_from: SiteState, _on: &str) -> Result<SiteState, String> {
    unimplemented!("intake.kit_bump: not ported yet")
}

/// The pure decision, with `config` the pinned [`KEYS`]: the next carry and the acts.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _config: &BTreeMap<String, String>) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("intake.kit_bump: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeKitBump {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeKitBump {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(IntakeKitBump { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.kit_bump/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IntakeKitBump {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let (next, acts) = decide(&carry, &snap, &self.config);
            commands(&carry, &next, STATE, acts)
        })
    }
}
