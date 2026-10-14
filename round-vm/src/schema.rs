//! Every piece of data round-vm persists or exchanges (DESIGN.md §3.2, §3.4).

use serde::{Deserialize, Serialize};

/// A VM as the round path knows it: an opaque handle the provider issued and the address
/// the round reaches it by. Nothing past the provider names a VM any other way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vm {
    pub handle: String,
    pub addr: String,
}

/// A process identity that survives pid reuse: the pid plus its start time (clock ticks
/// since boot, field 22 of /proc/<pid>/stat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcId {
    pub pid: u32,
    pub start: u64,
}

impl std::fmt::Display for ProcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.pid, self.start)
    }
}

/// The one provision in flight: who is doing it, and the VMID once one was allocated, so a
/// provision whose owner died can be found and destroyed (G2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provisioning {
    pub owner: ProcId,
    pub vmid: Option<String>,
    pub since: u64,
}

/// A VM handed out and not yet released. `owner: Some` is a `run` in progress and `holder`
/// the process that called `round-vm acquire`; either gone makes the lease stale. A lease
/// naming neither is stale after `OWNERLESS_GRACE_SECS` unless `held` (set by `hold`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub vm: Vm,
    pub owner: Option<ProcId>,
    pub since: u64,
    /// The process that called `round-vm acquire`; the lease is stale once it is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<ProcId>,
    /// Set by `hold`: the lease deliberately belongs to no process.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// When `hold` kept the lease; the unheld grace counts from here. 0 if unrecorded.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub kept_at: u64,
    /// The process pulling this VM's results: no reaper may terminate the VM while it lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulling: Option<ProcId>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// How long a lease may name no owner and no holder before it counts as abandoned.
pub const OWNERLESS_GRACE_SECS: u64 = 60;
/// How long a tagged cloud instance may go unheld, or sit on a kept lease, before the reconciler terminates it.
pub const UNHELD_GRACE_SECS: u64 = 300;

impl Lease {
    pub fn pull_in_flight(&self) -> bool {
        self.pulling.is_some_and(|p| p.alive())
    }

    /// Whether the unheld reconciler must leave this lease alone at `t`.
    pub fn shields_from_reaper(&self, t: u64) -> bool {
        let kept_from = if self.kept_at > 0 { self.kept_at } else { self.since };
        self.pull_in_flight() || (!self.stale() && (!self.held || kept_from + UNHELD_GRACE_SECS >= t))
    }

    pub fn stale(&self) -> bool {
        self.owner.is_some_and(|o| !o.alive())
            || self.holder.is_some_and(|h| !h.alive())
            || (self.owner.is_none() && self.holder.is_none() && !self.held && now().saturating_sub(self.since) > OWNERLESS_GRACE_SECS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outage {
    pub reason: String,
    pub since: u64,
}

/// `<state>/pool.json`. G1 is the types: one `Option` for ready, one for provisioning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolState {
    #[serde(default)]
    pub ready: Option<Vm>,
    #[serde(default)]
    pub provisioning: Option<Provisioning>,
    /// Provisions on EC2 in flight: spilled passes are not the pool of one (G1), but each is
    /// still recorded before its instance exists (G2).
    #[serde(default)]
    pub spilled: Vec<Provisioning>,
    #[serde(default)]
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub doomed: Vec<String>,
    #[serde(default)]
    pub outage: Option<Outage>,
    #[serde(default)]
    pub refreshing: Option<ProcId>,
    #[serde(default)]
    pub events: Vec<crate::machine::PoolEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcquireMode {
    Warm,
    Cold,
}

impl AcquireMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AcquireMode::Warm => "warm",
            AcquireMode::Cold => "cold",
        }
    }
}

/// `<state>/manifests/<tree-sha>.json` — the round result's measurement record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub tree_sha: String,
    pub tree_sha_found: Option<String>,
    pub commit_sha: String,
    pub vm: String,
    pub acquire: AcquireMode,
    pub vcpus: u32,
    pub maxpar: u32,
    pub batch_wall_secs: u64,
    pub build_wall_secs: Option<u64>,
    pub suite_wall_secs_sum: u64,
    #[serde(default)]
    pub provider: String,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serialises_with_the_field_names_callers_read() {
        let m = Manifest {
            tree_sha: "t".into(),
            tree_sha_found: None,
            commit_sha: "c".into(),
            vm: "123".into(),
            acquire: AcquireMode::Cold,
            vcpus: 16,
            maxpar: 24,
            batch_wall_secs: 700,
            build_wall_secs: Some(90),
            suite_wall_secs_sum: 9000,
            provider: "proxmox".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        for k in [
            "tree_sha", "tree_sha_found", "commit_sha", "vm", "acquire", "vcpus", "maxpar",
            "batch_wall_secs", "build_wall_secs", "suite_wall_secs_sum", "provider",
        ] {
            assert!(v.get(k).is_some(), "manifest lacks {k}");
        }
        assert_eq!(v["acquire"], "cold");
        assert!(v["tree_sha_found"].is_null());
    }

    #[test]
    fn empty_pool_file_parses_as_empty_pool() {
        let s: PoolState = serde_json::from_str("{}").unwrap();
        assert_eq!(s, PoolState::default());
    }
}
