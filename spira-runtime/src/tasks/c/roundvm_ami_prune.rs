//! # roundvm.ami_prune — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/roundvm-ami-prune.md`;
//! its Acceptance section is the test file `tests/task_c_roundvm_ami_prune.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=*-*-* 06:30:00`, `RandomizedDelaySec=10min`, `Persistent=true`
//!   (unit file). Jitter drawn from `Ctx::rand_u64` (`runtime/src/event.rs:335`), deterministic by seed.
//! - **Catch-up:** an overdue tick fires **once**. `last_fired` carried in `roundvm.ami_prune.state`
//!   (timers are memory-only, `runtime/src/event.rs:290-300`).
//! - **Wakes (proposed, not added):** a new round image (`round-vm template` publishing one) is
//!   when an old one becomes prunable; daily is enough.
//! - **Single-flight:** yes. **Guard:** `ExecStartPre=world guard` today → probe reports
//!   `world.halted` (`spira-world/src/guard.rs:11-20`); nothing acts while halted.
//!
//! ## Observe
//!
//! `roundvm.ami_prune.observe` (probe, class host, deadline 120 s) returns:
//!
//! ```text
//! Snapshot { world_halted: bool, stores: Vec<ImageStore>, in_use: Vec<image_id>, current_recipe: String }
//! ```
//!
//! - `stores` = `Ec2Api::image_stores` (`round-vm/src/ec2.rs:121`, impl `:708`); `in_use` =
//!   `Ec2Api::images_in_use` (`:123`, impl `:717`) — every non-terminated instance's image in the
//!   account. AWS is the component that decides both; read through the `aws` CLI child
//!   (`ec2.rs:600-603`), which stays per the plan.
//! - `current_recipe` = `crate::recipe::sha()` (in-process; `cli.rs` `recipe-sha` verb).
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<ImageStore>` is `ec2::prune_plan(stores, in_use, current_recipe)`
//! (`round-vm/src/ec2.rs:101-111`), pure already: images named `spira-round-*` (`:85`), newest
//! first, skip the newest `KEEP_IMAGES = 2` (`:84`), never the newest image of the current recipe,
//! never one in use. No carry beyond `last_fired`.
//! **Fixtures:** `ec2.rs:1357` (`prune_images_deregisters_then_deletes_snapshots_and_reports_the_gib`)
//! and the `prune_plan` unit tests.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `roundvm.ami_prune.deregister` | host (aws) | 60 s | `ami/<image_id>` | an image the snapshot did not plan; re-checks `in_use` is not stale by refusing when the plan is older than 10 min (proposed; unverified need) |
//! | `roundvm.ami_prune.delete_snapshot` | host (aws) | 60 s | `snap/<snapshot_id>` | a snapshot whose image is not yet deregistered (ordering: `ec2.rs:318-321` deregisters first) |
//!
//! Runners: `Ec2Api::deregister_image` (`ec2.rs:726-728`, `aws ec2 deregister-image`) and
//! `delete_snapshot` (`:730-732`). Today one failure aborts the rest of the pass and reports what
//! was done (`ec2.rs:318-320`); as effects, each image is independent and a failed one is retried
//! on the next day's plan. Repeating a deregister on an id AWS already removed returns an error
//! (unverified that it is distinguishable as "already gone"); the runner should treat
//! `InvalidAMIID.Unavailable`/`NotFound` as success.
//!
//! ## Silence
//!
//! The common day: `prune_plan` empty → nothing beyond the probe. Today the unit prints
//! `round-vm ami-prune: <report>` to the journal every run (`cli.rs:317-320`); that goes.
//!
//! ## Replaces
//!
//! - Unit `spira-round-vm-ami-prune-prod.service` / `.timer`; output to the journal.
//! - Children: `world guard` (ExecStartPre); `aws` (stays, at the edge).
//! - Config keys pinned: `SPIRA_ROUND_VM_STATE_DIR` (`config.rs:232`, for `state_dir/ec2`,
//!   `cli.rs:308`), the EC2 keys `Ec2Config::from_lookup` reads (`cli.rs:307`; key list in
//!   `round-vm/src/config.rs`, unverified exhaustively). The unit's `SPIRA_HOME` is pinned to
//!   release `0ef4f0f93…`, not `current`.
//!
//! ## Reverse
//!
//! Release rollback restores unit and timer. `Observe` mode shadows both effects; images
//! accumulate (cost: EBS snapshot storage, ~100 GiB per image per the fixture at `ec2.rs:1366`).
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "roundvm.ami_prune";
pub const STREAM: &str = "task.roundvm.ami_prune";
pub const TICK: EventKindTag = "roundvm.ami_prune.tick";
pub const STATE: EventKindTag = "roundvm.ami_prune.state";

pub const OBSERVE: &str = "roundvm.ami_prune.observe";
pub const DEREGISTER: &str = "roundvm.ami_prune.deregister";
pub const DELETE_SNAPSHOT: &str = "roundvm.ami_prune.delete_snapshot";
pub const EFFECTS: &[&str] = &[OBSERVE, DEREGISTER, DELETE_SNAPSHOT];

/// `KEEP_IMAGES`.
pub const KEEP_IMAGES: usize = 2;

/// `OnCalendar=*-*-* 06:30:00`.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ROUNDVM_AMI_PRUNE_CALENDAR";
/// `RandomizedDelaySec=10min`.
pub const JITTER_KEY: &str = "SPIRA_RUNTIME_ROUNDVM_AMI_PRUNE_JITTER_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, "SPIRA_ROUND_VM_STATE_DIR", "SPIRA_ROUND_VM_EC2_REGION", "SPIRA_ROUND_VM_EC2_PROFILE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// `ec2::ImageStore`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageStore {
    pub image_id: String,
    pub name: String,
    pub created: String,
    pub recipe: String,
    pub snapshots: Vec<String>,
    pub gib: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub world_halted: bool,
    pub stores: Vec<ImageStore>,
    pub in_use: Vec<String>,
    pub current_recipe: String,
}

/// Per image (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Planned,
    Deregistered,
    SnapshotsDeleted,
}

/// `state × event → state` per image. `delete_snapshot` before `deregistered` is refused
/// naming `planned`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Planned, "deregister:ok", Phase::Deregistered), (Phase::Deregistered, "delete_snapshot:ok", Phase::SnapshotsDeleted)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub last_fired: Option<u64>,
    pub images: BTreeMap<String, Phase>,
}

pub struct RoundvmAmiPrune {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `ec2::prune_plan(stores, in_use, current_recipe)`.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("roundvm.ami_prune: not ported yet")
}

crate::tasks::b::hosted!(RoundvmAmiPrune);
