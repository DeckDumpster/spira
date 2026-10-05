//! The bd claim record `lifecycle_enforce` OFF still runs on (DESIGN.md §8.7, "The off
//! claim record"). With the switch off, an aeon claims through bd's own ready query and
//! `bd update --claim` — sp-860zj kept that path unchanged — so bd's `in_progress` and
//! assignee ARE the claim: the lifecycle machine holds no WORKING row for an off-mode claim,
//! and off-mode unpoison/deadlocked never call spira-lc at all (§8.7, test-unpoison's off
//! case). These two functions are the only places spira-claim reads or writes a work bead's
//! bd status, each used only when the switch is off, and they are lifecycle-guard's named
//! `bd-status-read` exceptions by file and function (lifecycle-guard/DESIGN.md "The off
//! claim record"). They go when the off path does.
//!
//! With the switch on, the claim is the lifecycle row (its WORKING holder) and neither
//! function is called: unpoison and deadlocked read the holder from `spira-lc show`, and a
//! reopen leaves bd's status to the machine (bd status is inert, design §3.4).

use crate::unpoison::BeadRecord;

/// `lifecycle_enforce` off: the live holder of a bead — bd's own claim, `in_progress` with an
/// assignee — or `None` when nobody holds it.
pub fn off_holder(bead: &BeadRecord) -> Option<String> {
    let assignee = bead.assignee.as_deref().filter(|a| !a.trim().is_empty())?;
    (bead.status == "in_progress").then(|| format!("{assignee} (in_progress)"))
}

/// `lifecycle_enforce` off: the bd write that puts a reopened bead back in bd's ready set.
/// `bd reopen` only acts on a closed bead; a refused handoff reopens one still in_progress,
/// which `bd ready` (the off-mode ready set) excludes, so the status is set explicitly and
/// the assignee cleared (sp-k61z5).
pub fn off_release_args(id: &str) -> [&str; 6] {
    ["update", id, "--status", "open", "--assignee", ""]
}
