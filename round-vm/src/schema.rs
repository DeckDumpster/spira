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

/// A VM handed out and not yet released. `owner: None` is a handle given to an external
/// caller by `round-vm acquire`, who must `release` it; `Some` is a `run` in progress,
/// destroyed by the next acquire if that process is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub vm: Vm,
    pub owner: Option<ProcId>,
    pub since: u64,
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
        };
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        for k in [
            "tree_sha", "tree_sha_found", "commit_sha", "vm", "acquire", "vcpus", "maxpar",
            "batch_wall_secs", "build_wall_secs", "suite_wall_secs_sum",
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
