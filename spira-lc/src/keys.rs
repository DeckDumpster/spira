//! The control plane's key layout. Later work adds keys only under prefixes of its own.

pub const LIFECYCLE: &str = "lifecycle/";
pub const READINESS: &str = "readiness/";
pub const DELIVERIES: &str = "deliveries/";
pub const ASKS: &str = "asks/";
pub const MENDINGS: &str = "mendings/";
/// Each member branch a mend moved, by bead: what a second mender must build on.
pub const MENDS: &str = "mends/";
/// Each GitHub bead's pending issue closeout, `gh-closeout/<bead>`: written in the txn that
/// moves the bead to LANDED, SUPERSEDED or DROPPED, deleted once the issue is settled.
pub const GH_CLOSEOUTS: &str = "gh-closeout/";
/// Each incident raised and not yet filed, `incidents/<component of the bounded reference>`.
pub const INCIDENTS: &str = "incidents/";
pub const BATCHES: &str = "batches/";
/// Each round's id, naming the repo it was cut in: a round id is unique across repos.
pub const BATCH_IDS: &str = "batch-ids/";
/// Each bead's rounds, `batch-members/<bead>/<round id>`: a range read answers `batches-of`.
pub const BATCH_MEMBERS: &str = "batch-members/";
/// The runtime's journal: `runtime/state/`, `runtime/effect/`, `runtime/resolved/`,
/// `runtime/schedule/`, `runtime/fired/` and `runtime/poison/`, each keyed by its escaped natural key.
pub const RUNTIME: &str = "runtime/";
/// Each topic family ever published on the broker, `topics/<family>`; a family holds `/`.
/// Written by the topic tap alone.
pub const TOPICS: &str = "topics/";
/// The attempt and poison history, `facts/<bead>/<request id>`: facts that change no bead's state.
pub const FACTS: &str = "facts/";
pub const HISTORY: &str = "history/";
/// Each recorded `work` request, `work-requests/<id>`: a verb that runs a program, queued for the scheduler.
pub const WORK_REQUESTS: &str = "work-requests/";
/// The allocator behind `work-requests/<id>`: `{n}`, advanced in the txn that records a request.
pub const WORK_REQUEST_SEQ: &str = "work-requests-seq";
/// One empty row per request in a state a runner still owes it, `work-request-index/<state>/<id>`.
pub const WORK_REQUEST_INDEX: &str = "work-request-index/";
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
/// What the scheduler and the round programs read from git, `git-facts/<kind>/…`; see `control::git_facts`.
pub const GIT_FACTS: &str = "git-facts/";
/// What the cutover import left: its baseline revision, the archive it read and its report.
pub const IMPORT: &str = "import/";
/// The import's baseline: every record's stored state at revision `rev`, one past the last
/// dolt event seq. Records the report lists were imported as stored, not replayed.
pub const IMPORT_BASELINE: &str = "import/baseline";

/// Every fact of `bead`, in the order they committed.
pub fn facts_of(bead: &str) -> String {
    format!("{FACTS}{bead}/")
}

pub fn fact(bead: &str, request: &str) -> String {
    format!("{}{request}", facts_of(bead))
}

pub use spira_store::incident::component;

pub fn runtime(table: &str, natural: &[&str]) -> String {
    let parts: Vec<String> = natural.iter().map(|p| component(p)).collect();
    format!("{RUNTIME}{table}/{}", parts.join("/"))
}

pub fn runtime_table(table: &str) -> String {
    format!("{RUNTIME}{table}/")
}

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

pub fn topic(family: &str) -> String {
    format!("{TOPICS}{family}")
}

pub fn conditions(owner: &str) -> String {
    format!("{CONDITIONS}{owner}/")
}

pub fn condition(owner: &str, key: &str) -> String {
    format!("{}{key}", conditions(owner))
}

pub fn work_request(id: &str) -> String {
    format!("{WORK_REQUESTS}{id}")
}

pub fn work_request_index(state: &str) -> String {
    format!("{WORK_REQUEST_INDEX}{state}/")
}

pub fn gh_closeout(id: &str) -> String {
    format!("{GH_CLOSEOUTS}{id}")
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

/// Every membership entry of `bead`; a round id may hold a `/`, a bead id never does.
pub fn batch_members_of(bead: &str) -> String {
    format!("{BATCH_MEMBERS}{bead}/")
}

pub fn batch_member(bead: &str, id: &str) -> String {
    format!("{}{id}", batch_members_of(bead))
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

/// The tally of ended stays: `stays/<STATE>/<utc-day>` holds one state's histogram for one day, and
/// `stays/cursor` where `fold_stays` has read to. Written by `fold_stays` alone.
pub const STAYS: &str = "stays/";
pub const STAYS_CURSOR: &str = "stays/cursor";

pub fn stay_day(state: &str, day: i64) -> String {
    format!("{STAYS}{state}/{day:010}")
}

/// The tally of bead edges: `edges/<utc-minute>` holds one minute's counts and newest `at` per
/// edge, and `edges/cursor` where `fold_edges` has read to. Written by `fold_edges` alone.
pub const EDGES: &str = "edges/";
pub const EDGES_CURSOR: &str = "edges/cursor";

pub fn edge_minute(minute: i64) -> String {
    format!("{EDGES}{minute:010}")
}

/// The tally of refused bead events: `refusals/<utc-minute>` holds one minute's groups by who
/// asked, and `refusals/cursor` where `fold_refusals` has read to. Written by `fold_refusals` alone.
pub const REFUSALS: &str = "refusals/";
pub const REFUSALS_CURSOR: &str = "refusals/cursor";

pub fn refusal_minute(minute: i64) -> String {
    format!("{REFUSALS}{minute:010}")
}

/// The day's landings: `landed/at/<at>/<bead>` holds one applied move into LANDED, zero-padded so
/// key order is time order, and `landed/cursor` where `fold_landed` has read to. Written by
/// `fold_landed` alone.
pub const LANDED_AT: &str = "landed/at/";
pub const LANDED_CURSOR: &str = "landed/cursor";

pub fn landed_entry(at: i64, id: &str) -> String {
    format!("{LANDED_AT}{at:010}/{id}")
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

    #[test_id(tza86ut)]
    #[test]
    fn id_of_reads_only_a_direct_child() {
        assert_eq!(id_of(LIFECYCLE, "lifecycle/sp-a"), Some("sp-a"));
        assert_eq!(id_of(BATCHES, "batches/harness/b-1"), None);
        assert_eq!(id_of(LIFECYCLE, "readiness/sp-a"), None);
        assert_eq!(id_of(LIFECYCLE, "lifecycle/"), None);
    }
}
