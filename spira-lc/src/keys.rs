//! The control plane's key layout. Later work adds keys only under prefixes of its own.

pub const LIFECYCLE: &str = "lifecycle/";
pub const READINESS: &str = "readiness/";
pub const DELIVERIES: &str = "deliveries/";
pub const ASKS: &str = "asks/";
pub const MENDINGS: &str = "mendings/";
/// Each member branch a mend moved, by bead: what a second mender must build on.
pub const MENDS: &str = "mends/";
pub const BATCHES: &str = "batches/";
/// Each round's id, naming the repo it was cut in: a round id is unique across repos.
pub const BATCH_IDS: &str = "batch-ids/";
pub const RUNTIME: &str = "runtime/";
pub const HISTORY: &str = "history/";
/// Knobs a section owner moves by applying an intent from one value to another.
pub const KNOBS: &str = "knobs/";
/// Each record's blocker-facing projection, under a prefix of its own so a `lifecycle/` list or
/// watch sees records and nothing else.
pub const GATES: &str = "gates/";
/// The planning watermark: `{seq}`, advanced by `plan stamp` after every planning write.
pub const PLANNING_WATERMARK: &str = "planning/watermark";
/// The finalizer that keeps a lifecycle record until the planning plane has closed its bead.
pub const PLANNING_CLOSED: &str = "planning-closed";
/// Each watcher's conditions, under its own name: `conditions/<owner>/<key>`.
pub const CONDITIONS: &str = "conditions/";
/// What the cutover import left: its baseline revision, the archive it read and its report.
pub const IMPORT: &str = "import/";
/// The import's baseline: every record's stored state at revision `rev`, one past the last
/// dolt event seq. Records the report lists were imported as stored, not replayed.
pub const IMPORT_BASELINE: &str = "import/baseline";

pub fn lifecycle(id: &str) -> String {
    format!("{LIFECYCLE}{id}")
}

/// The record's blocker-facing projection, written in the txn of any transition that moves it.
pub fn gate(id: &str) -> String {
    format!("{GATES}{id}")
}

pub fn readiness(id: &str) -> String {
    format!("{READINESS}{id}")
}

pub fn delivery(id: &str) -> String {
    format!("{DELIVERIES}{id}")
}

pub fn ask(id: &str) -> String {
    format!("{ASKS}{id}")
}

pub fn conditions(owner: &str) -> String {
    format!("{CONDITIONS}{owner}/")
}

pub fn condition(owner: &str, key: &str) -> String {
    format!("{}{key}", conditions(owner))
}

pub fn mending(id: &str) -> String {
    format!("{MENDINGS}{id}")
}

/// Every mend recorded on `bead`'s branch, oldest first.
pub fn mends(bead: &str) -> String {
    format!("{MENDS}{bead}/")
}

/// The `n`th member a mend planned at `planned_at` moved: its keys sort in the order mends land.
pub fn mend(bead: &str, planned_at: u64, n: usize) -> String {
    format!("{}{planned_at:020}/{n:04}", mends(bead))
}

pub fn batch(repo: &str, id: &str) -> String {
    format!("{BATCHES}{repo}/{id}")
}

pub fn batch_index(id: &str) -> String {
    format!("{BATCH_IDS}{id}")
}

pub fn batch_progress(repo: &str, id: &str) -> String {
    format!("{}/progress", batch(repo, id))
}

/// The lock resource a claim holds: the lifecycle record it guards, so the contract keeps it
/// at `locks/lifecycle/<id>`.
pub fn claim_resource(id: &str) -> String {
    lifecycle(id)
}

/// Where the contract keeps every claim's lock, so a listing reads them beside the records.
pub fn claim_locks() -> String {
    format!("{}{LIFECYCLE}", state_store::LOCKS_PREFIX)
}

/// The key of a claim's lock; its `create_rev` is the holder's fencing token.
pub fn claim_lock(id: &str) -> String {
    format!("{}{id}", claim_locks())
}

/// Every history entry of `id`: one per txn whose audit names it.
pub fn history(id: &str) -> String {
    format!("{HISTORY}{id}/")
}

/// One txn's entry in `id`'s history; its `mod_rev` is the txn's revision. A request id
/// commits once while its request key lives, and the head the txn was planned at has moved
/// past that commit by the time the id could be reused, so no entry is ever overwritten.
pub fn history_entry(id: &str, planned_at: u64, request: &str) -> String {
    format!("{}{planned_at:020}/{request}", history(id))
}

/// The id a key under `prefix` names, or `None` for a key that is not directly under it.
pub fn id_of<'a>(prefix: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix(prefix)
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_of_reads_only_a_direct_child() {
        assert_eq!(id_of(LIFECYCLE, "lifecycle/sp-a"), Some("sp-a"));
        assert_eq!(id_of(BATCHES, "batches/harness/b-1"), None);
        assert_eq!(id_of(LIFECYCLE, "readiness/sp-a"), None);
        assert_eq!(id_of(LIFECYCLE, "lifecycle/"), None);
    }
}
