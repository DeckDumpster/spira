//! Every effect the intake has outside its own arguments, as a trait (DESIGN.md §5):
//! `bd`, `mail`, the repository map and the clock. `real.rs` implements them against the
//! host; unit tests implement them as fakes so the orchestration in `run.rs` is testable
//! without a database.

use crate::decide::{BeadRow, Scope};

pub trait Bd {
    /// The incident beads in `scope` — bd's `list --all [--label <label>] [--closed-after
    /// <date>] --json` for the content, each bead's state from its lifecycle row (sp-jgjvh:
    /// incident beads are work beads; bd status is never read). Err when bd or the lifecycle
    /// machine could not be reached (distinct from an empty Ok(vec![]), a real "nothing").
    fn list(
        &self,
        db: &str,
        scope: Scope,
        label: Option<&str>,
        closed_after: Option<&str>,
    ) -> Result<Vec<BeadRow>, String>;

    /// `bd -C db create <title> --type T --priority P --labels L --external-ref R
    /// --body-file -` (payload on stdin) -> the new bead id.
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        db: &str,
        title: &str,
        kind: &str,
        priority: &str,
        labels: &str,
        external_ref: &str,
        body: &str,
        actor: Option<&str>,
    ) -> Result<String, String>;

    fn label_add(&self, db: &str, id: &str, label: &str) -> bool;
    fn label_remove(&self, db: &str, id: &str, label: &str) -> bool;
    fn label_list(&self, db: &str, id: &str) -> Vec<String>;
    fn note(&self, db: &str, id: &str, text: &str) -> bool;
    fn set_state(&self, db: &str, id: &str, kv: &str) -> bool;
    /// `spira-lc reopen <id> <cause>`: the machine's return-to-rework.
    fn reopen(&self, db: &str, id: &str, cause: &str) -> bool;
    /// `bd -C db dep relate <a> <b>`: a bidirectional see-also link (sp-nmlna: a fresh
    /// incident to the terminal one it recurs).
    fn relate(&self, db: &str, a: &str, b: &str) -> bool;
    /// `bd -C db duplicate <id> --of <survivor>`.
    fn duplicate(&self, db: &str, id: &str, survivor: &str) -> bool;
    /// The non-terminal bead a terminal `id` was superseded by (its `supersedes` dependency),
    /// None when it names none or the successor's row is terminal or absent.
    fn live_successor(&self, db: &str, id: &str) -> Option<String>;
    fn show_closed_at(&self, db: &str, id: &str) -> Option<String>;
    fn show_created_at(&self, db: &str, id: &str) -> Option<String>;
    /// A cheap reachability probe (`bd list --limit 1`) — used to distinguish "no open
    /// incident" from "the database could not be reached" when the dedup scan is empty.
    fn reachable(&self, db: &str) -> bool;
    /// One attempt-history fact in the lifecycle event log (`spira-lc fact`), true when written.
    fn fact(&self, id: &str, kind: &str, cause: &str) -> bool;
    /// How many facts of `kind` the log holds for `id`; None when it could not be read.
    fn fact_count(&self, id: &str, kind: &str) -> Option<usize>;
}

/// The recurrence counter (`bump_recur`/`recurs_of`): `recurred` facts in the lifecycle event log.
pub fn bump_recur(bd: &dyn Bd, id: &str, cause: &str) {
    let _ = bd.fact(id, "recurred", cause);
}

/// `None` means the log could not be read and the count is genuinely unknown (blind), never 0 —
/// law-absence-needs-a-positive-control (sp-39yd3).
pub fn recurs_of(bd: &dyn Bd, id: &str) -> Option<u32> {
    bd.fact_count(id, "recurred").map(|n| n as u32)
}

pub trait Mailer {
    /// `mail send operator --from "Incident <incident@spira>" --subject S --kind
    /// question --default D`, body on stdin. True on success.
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool;
}

pub trait Clock {
    fn now(&self) -> i64;
}
