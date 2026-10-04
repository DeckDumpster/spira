//! `spira-claim reopen` and `spira-claim release` — lib.sh family I's `bead_reopen` and
//! `release_claim` (wave 4.19, wave4-decomposition.md row 19, sp-3wfcb), plus the one
//! declared list `bead_reopen` reads to decide admission exemption —
//! `_census_deliberate_reopen_causes`/`_census_reopen_admission_exempt` — which row M's
//! census SQL producers (still bash; a later bead) also read, through the lib.sh shim.
//! Safety note (c7): this is the most-called mutation in the harness (6 crates, groomer.sh,
//! auron), and it carries the census admission exemption and the claim release. Any
//! divergence between this port and the bash original reopens a bead with the wrong cause,
//! charges the wrong attempt counter, or leaves a dead aeon's name on a bead nothing will
//! claim again.
//!
//! The flow is written against [`World`], the same shape `unpoison` uses: every bd,
//! landing-pass and filesystem touch is a trait method, so the decision logic — what gets
//! withdrawn, what gets a note, when the exit code goes non-zero — is unit-tested with no
//! subprocess at all. [`crate::unpoison::Live`] implements this trait too (at the bottom of
//! unpoison.rs, where its private `bd`/`bd_ok` helpers are in scope), so the live bd/
//! landing-pass plumbing is written once, not duplicated.

// =========================================================================================
// (c) The declared deliberate-reopen-cause list. lib.sh's own comment: kept adjacent to
// the SQL that reads it (row M's _census_class_fold_map) the same way that fold is kept
// adjacent to the SQL using it — this is the one declared list both bead_reopen (here) and
// census's SQL producers (_census_deliberate_causes_sql_list, still bash) may read.
// =========================================================================================

/// `_census_deliberate_reopen_causes`'s declared list, in bash's own print order: `work-
/// close-converted` is exempt (aeon.sh's teardown carrying a session's own finished work
/// bead forward to the landing pass — bookkeeping, not rework, so the CERTIFIED landstate
/// and the submitted label it arrived with must survive); `eject` is deliberate for census
/// (withdrawing a CERTIFIED-but-unbatched bead is the system working) but NOT exempt — it
/// still needs WITHDRAWN written and the label stripped, or the bead stays admissible to
/// the next batch cut (sp-eiatd).
pub const DELIBERATE_CAUSES: &[(&str, bool)] = &[("work-close-converted", true), ("eject", false)];

/// lib.sh `_census_deliberate_reopen_causes` — "<cause> <0|1>\n" lines, one per cause, in
/// [`DELIBERATE_CAUSES`]'s order.
pub fn deliberate_causes_text() -> String {
    let mut out = String::new();
    for (c, exempt) in DELIBERATE_CAUSES {
        out.push_str(c);
        out.push(' ');
        out.push_str(if *exempt { "1" } else { "0" });
        out.push('\n');
    }
    out
}

/// lib.sh `_census_reopen_admission_exempt <cause>` — true only for a cause
/// [`DELIBERATE_CAUSES`] marks exempt (today, only `work-close-converted`).
pub fn admission_exempt(cause: &str) -> bool {
    DELIBERATE_CAUSES.iter().any(|(c, exempt)| *c == cause && *exempt)
}

// =========================================================================================
// bead_reopen
// =========================================================================================

/// Every store, file and bd touch `bead_reopen` makes, as a trait method —
/// [`run`] below is pure decision logic over this interface, and [`crate::unpoison::Live`]
/// is the one real implementation.
pub trait World {
    /// The `$LANDSTATE/<id>.ejected` sidecar gate.sh reads unconditionally — best-effort
    /// (bash: `printf ... && mv -f ... || true`).
    fn write_ejected(&mut self, id: &str, suites: &str);
    /// `bdq reopen "$id"` — the first of the two steps whose failure sets the exit code.
    fn bd_reopen(&mut self, id: &str) -> Result<(), String>;
    /// `bdq label remove "$id" "$submitted_label"` — best-effort (bash: `|| true`). A
    /// reopen means rework, so a bead still wearing the submitted label (added by the one
    /// reopen this never touches: the work-close-converted conversion itself) would be
    /// unclaimable (`fayth_exclude`) and never land.
    fn remove_submitted_label(&mut self, id: &str, label: &str);
    /// `release_claim "$id"` — the second step whose failure sets the exit code. Clearing
    /// the assignee is what makes a reopen a reopen (see lib.sh's own comment on
    /// `bead_reopen`): without it the bead goes back to the board wearing a dead aeon's
    /// name, visible and ready, but unclaimable.
    fn release_claim(&mut self, id: &str) -> Result<(), String>;
    /// `_bump_write_event "$id" reopen "$cause"` — ALWAYS treated as succeeded. The bash
    /// wrapper it shims (`_bump_write_event`, lib.sh) swallows its child's own output and
    /// exit code and unconditionally `return 0`s, so `bead_reopen`'s trailing `|| rc=1`
    /// after it can never fire. No `Result` here IS the parity, not a gap.
    fn write_reopen_event(&mut self, id: &str, cause: &str);
    /// `bdq note "$id" "$note"` — only called when `note` is non-empty; its failure sets
    /// the exit code.
    fn note(&mut self, id: &str, text: &str) -> Result<(), String>;
}

/// `bead_reopen <id> <cause> [note] [suites]`'s own argument defaults
/// (`local id="$1" cause="${2:-unrecorded}" note="${3:-}" suites="${4:-}"`), plus the
/// resolved submitted-label this call needs (`${SPIRA_SUBMITTED_LABEL:-spira-submitted}`).
#[derive(Debug, Clone)]
pub struct Opts {
    pub id: String,
    pub cause: String,
    pub note: String,
    pub suites: String,
    pub submitted_label: String,
}

/// `bead_reopen`'s own stderr line, printed by the caller (main.rs) when [`run`] returns
/// non-zero: `printf 'bead_reopen: %s — bd refused the reopen, the release or the note\n'`.
pub const FAILURE_REASON: &str = "bd refused the reopen, the release or the note";

/// `bead_reopen`. Returns the exit code (`rc` in bash: 0, or 1 if `bdq reopen`,
/// `release_claim` or the note each separately failed).
pub fn run(o: &Opts, w: &mut dyn World) -> i32 {
    let mut rc = 0;
    let exempt = admission_exempt(&o.cause);

    if !o.suites.is_empty() {
        w.write_ejected(&o.id, &o.suites);
    }

    if w.bd_reopen(&o.id).is_err() {
        rc = 1;
    }

    // A REOPEN MEANS REWORK, so a submitted bead stops being submitted — except the one
    // reopen that ADDS the label (the conversion itself), left alone.
    if !exempt {
        w.remove_submitted_label(&o.id, &o.submitted_label);
    }

    if w.release_claim(&o.id).is_err() {
        rc = 1;
    }

    w.write_reopen_event(&o.id, &o.cause);

    if !o.note.is_empty() && w.note(&o.id, &o.note).is_err() {
        rc = 1;
    }

    rc
}

#[cfg(test)]
#[path = "reopen_tests.rs"]
mod tests;
