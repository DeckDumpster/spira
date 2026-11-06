//! # roundvm.ami_prune — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/roundvm-ami-prune.md`;
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

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

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
    /// Each planned image's snapshots, from the snapshot that planned it: once the image is
    /// deregistered AWS no longer lists them by image.
    pub snapshots: BTreeMap<String, Vec<String>>,
}

/// The `delete_snapshot` effect's input: one image's snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Delete {
    pub image_id: String,
    pub snapshots: Vec<String>,
}

pub struct RoundvmAmiPrune {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Planned => "planned",
        Phase::Deregistered => "deregistered",
        Phase::SnapshotsDeleted => "snapshots_deleted",
    }
}

fn to_ec2(s: &ImageStore) -> round_vm::ec2::ImageStore {
    round_vm::ec2::ImageStore { image: round_vm::ec2::Image { id: s.image_id.clone(), recipe_sha: s.recipe.clone(), created: s.created.clone() }, name: s.name.clone(), snapshots: s.snapshots.clone(), gib: s.gib }
}

fn from_ec2(s: &round_vm::ec2::ImageStore) -> ImageStore {
    ImageStore { image_id: s.image.id.clone(), name: s.name.clone(), created: s.image.created.clone(), recipe: s.image.recipe_sha.clone(), snapshots: s.snapshots.clone(), gib: s.gib }
}

fn delete_act(image_id: &str, carry: &Carry) -> Result<Act, String> {
    let d = Delete { image_id: image_id.into(), snapshots: carry.snapshots.get(image_id).cloned().unwrap_or_default() };
    Ok(Act { name: DELETE_SNAPSHOT.into(), input: serde_json::to_string(&d).map_err(|e| e.to_string())?, deadline_ms: 60_000, probe: false })
}

/// The image an outcome body names: one whose snapshots include it, else the one whose id it
/// is or ends with (the runners answer with the image id).
fn named<'a>(carry: &'a Carry, body: &str) -> Option<&'a String> {
    let body = body.trim();
    carry.images.keys().find(|id| carry.snapshots.get(*id).is_some_and(|s| s.iter().any(|x| x == body))).or_else(|| carry.images.keys().find(|id| body == id.as_str() || body.ends_with(&format!("-{id}")) || body.ends_with(&format!(" {id}"))))
}

/// `ec2::prune_plan(stores, in_use, current_recipe)`.
///
/// Each planned image is deregistered, then its snapshots are deleted (never the reverse); a
/// finished image leaves the carry on the next day's observation, and one whose snapshot delete
/// failed is asked again then.
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(s) if s.world_halted => Ok((carry.clone(), vec![])),
        Input::Observed(s) => {
            let mut next = carry.clone();
            next.images.retain(|_, p| *p != Phase::SnapshotsDeleted);
            next.snapshots.retain(|id, _| next.images.contains_key(id));
            let mut acts = vec![];
            for (id, _) in next.images.iter().filter(|(_, p)| **p == Phase::Deregistered) {
                acts.push(delete_act(id, &next)?);
            }
            let stores: Vec<round_vm::ec2::ImageStore> = s.stores.iter().map(to_ec2).collect();
            for p in round_vm::ec2::prune_plan(&stores, &s.in_use, &s.current_recipe).iter().map(from_ec2) {
                if next.images.contains_key(&p.image_id) {
                    continue;
                }
                next.images.insert(p.image_id.clone(), Phase::Planned);
                next.snapshots.insert(p.image_id.clone(), p.snapshots.clone());
                acts.push(Act { name: DEREGISTER.into(), input: p.image_id.clone(), deadline_ms: 60_000, probe: false });
            }
            if !acts.is_empty() {
                next.last_fired = Some(now_ms);
            }
            Ok((next, acts))
        }
        Input::Outcome { effect, ok, body } if effect == DEREGISTER => {
            let id = named(carry, body).cloned().ok_or_else(|| format!("{NAME}: a deregister of {body:?}, which no image in the plan names"))?;
            match carry.images.get(&id).copied().unwrap_or_default() {
                Phase::Planned => {
                    let mut next = carry.clone();
                    if *ok {
                        next.images.insert(id.clone(), Phase::Deregistered);
                        let act = delete_act(&id, &next)?;
                        Ok((next, vec![act]))
                    } else {
                        // Not deregistered: tomorrow's plan names it again.
                        next.images.remove(&id);
                        next.snapshots.remove(&id);
                        Ok((next, vec![]))
                    }
                }
                p => Err(format!("{NAME}: {id} is {}, so a deregister outcome is not expected", phase_name(p))),
            }
        }
        Input::Outcome { effect, ok, body } if effect == DELETE_SNAPSHOT => {
            let id = named(carry, body).cloned().ok_or_else(|| format!("{NAME}: a snapshot delete of {body:?}, which no image in the plan names"))?;
            match carry.images.get(&id).copied().unwrap_or_default() {
                Phase::Deregistered if *ok => {
                    let mut next = carry.clone();
                    next.images.insert(id, Phase::SnapshotsDeleted);
                    Ok((next, vec![]))
                }
                // Left deregistered: the next observation asks again.
                Phase::Deregistered => Ok((carry.clone(), vec![])),
                p => Err(format!("{NAME}: {id} is {}, not deregistered, so its snapshots are not deleted", phase_name(p))),
            }
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: an outcome of {effect} is not expected")),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// The runner of [`OBSERVE`]: AWS's own lists through the `aws` CLI (the edge child), the
/// world's halt stamp, and this release's recipe.
pub fn observe(run: &std::path::Path) -> Result<String, String> {
    use round_vm::ec2::Ec2Api;
    let (api, _) = round_vm::cli::ec2_api()?;
    let snap = Snapshot { world_halted: run.join("world.halted").exists(), stores: api.image_stores()?.iter().map(from_ec2).collect(), in_use: api.images_in_use()?, current_recipe: round_vm::recipe::sha() };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// An AWS answer that the thing is already gone, which a repeated act treats as done.
fn already_gone(e: &str) -> bool {
    ["InvalidAMIID.Unavailable", "InvalidAMIID.NotFound", "InvalidSnapshot.NotFound"].iter().any(|c| e.contains(c))
}

/// The runner of [`DEREGISTER`]: `aws ec2 deregister-image`; answers with the image id.
pub fn deregister(image_id: &str) -> Result<String, String> {
    use round_vm::ec2::Ec2Api;
    let id = image_id.trim();
    if !id.starts_with("ami-") {
        return Err(format!("{id:?} is not an image id — refusing"));
    }
    let (api, _) = round_vm::cli::ec2_api()?;
    match api.deregister_image(id) {
        Ok(()) => Ok(id.to_string()),
        Err(e) if already_gone(&e) => Ok(id.to_string()),
        Err(e) => Err(e),
    }
}

/// The runner of [`DELETE_SNAPSHOT`]: `aws ec2 delete-snapshot` for each of one image's
/// snapshots; answers with the image id.
pub fn delete_snapshots(input: &str) -> Result<String, String> {
    use round_vm::ec2::Ec2Api;
    let d: Delete = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let (api, _) = round_vm::cli::ec2_api()?;
    for snap in &d.snapshots {
        match api.delete_snapshot(snap) {
            Ok(()) => {}
            Err(e) if already_gone(&e) => {}
            Err(e) => return Err(format!("{} deregistered but snapshot {snap}: {e}", d.image_id)),
        }
    }
    Ok(d.image_id)
}

crate::tasks::b::hosted!(RoundvmAmiPrune);
