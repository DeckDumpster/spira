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
/// bead forward to the landing pass — bookkeeping, not rework, so the CERTIFIED lifecycle
/// row and the submitted label it arrived with must survive); `eject` is deliberate for census
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
    /// The `$SPIRA_RUN/ejected/<id>` sidecar the gate reads unconditionally — best-effort
    /// (bash: `printf ... && mv -f ... || true`).
    fn write_ejected(&mut self, id: &str, suites: &str);
    /// The checks a red ejection names, kept where `work submit` reads them back
    /// (`spira_config::proof`). Best-effort like the sidecar.
    fn record_failing(&mut self, id: &str, suites: &str);
    /// The machine's return-to-rework (`spira-lc reopen`): the lifecycle row records the
    /// event its state implies and the bead is claimable again. A bead with no row has no
    /// state to diverge and is `Ok`; a terminal row is refused.
    fn lc_reopen(&mut self, id: &str, cause: &str) -> Result<(), String>;
    /// Stop the live aeon holding `id` and wait for it to exit; `Err` when it is still
    /// running, in which case the bead must not leave WORKING.
    fn stop_holder(&mut self, id: &str, cause: &str) -> Result<(), String>;
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
/// (`local id="$1" cause="${2:-unrecorded}" note="${3:-}" suites="${4:-}"`).
#[derive(Debug, Clone)]
pub struct Opts {
    pub id: String,
    pub cause: String,
    pub note: String,
    pub suites: String,
}

/// `bead_reopen`'s own stderr line, printed by the caller (main.rs) when [`run`] returns
/// non-zero: `printf 'bead_reopen: %s — bd refused the reopen, the release or the note\n'`.
pub const FAILURE_REASON: &str = "the lifecycle machine refused the reopen, or bd the note";

/// `bead_reopen`. Returns the exit code (`rc` in bash: 0, or 1 if `bdq reopen`,
/// `release_claim` or the note each separately failed).
pub fn run(o: &Opts, w: &mut dyn World) -> i32 {
    let mut rc = 0;
    let exempt = admission_exempt(&o.cause);

    if !o.suites.is_empty() {
        w.write_ejected(&o.id, &o.suites);
        if o.cause == RED_CAUSE {
            w.record_failing(&o.id, &o.suites);
        }
    }

    // A deliberate conversion is the holder's own hand-off; every other reopen is the
    // harness acting on a bead whose aeon may still be editing its worktree.
    if !exempt && w.stop_holder(&o.id, &o.cause).is_err() {
        return 1;
    }

    // A deliberate conversion carries the bead's row forward untouched.
    if !exempt && w.lc_reopen(&o.id, &o.cause).is_err() {
        rc = 1;
    }

    w.write_reopen_event(&o.id, &o.cause);

    if !o.note.is_empty() && w.note(&o.id, &o.note).is_err() {
        rc = 1;
    }

    rc
}

/// The reopen cause of a red ejection; only it names failing checks that a resubmit must prove.
pub const RED_CAUSE: &str = "eject-red";

/// How long a harness release waits for the holder to exit after asking it to stop.
pub const STOP_WAIT_S: u64 = 120;

/// Ask the aeon whose lease is running on `id` to stop (the `aeon stop` request file its
/// heartbeat reads) and wait until that lease is gone. `Ok` when no aeon holds the bead, or
/// when the caller is itself an aeon (`SPIRA_AEON` set): an aeon's own hand-back never waits
/// on itself.
pub fn stop_holder_in(
    run: &std::path::Path,
    id: &str,
    why: &str,
    own_aeon: bool,
    wait_s: u64,
    now: &dyn Fn() -> i64,
    sleep: &dyn Fn(),
) -> Result<(), String> {
    if own_aeon {
        return Ok(());
    }
    let suffix = format!("-{id}.pid");
    let live = || -> Option<std::path::PathBuf> {
        std::fs::read_dir(run).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("aeon-") && n.ends_with(&suffix))
                && sending::reap::lease_live(p, now())
        })
    };
    if live().is_none() {
        return Ok(());
    }
    let req = run.join("aeon").join(format!("{id}.stop"));
    if let Some(d) = req.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&req, format!("released by the harness: {why}\n")).map_err(|e| format!("cannot record the stop: {e}"))?;
    let deadline = now() + wait_s as i64;
    while live().is_some() {
        if now() >= deadline {
            return Err(format!("the aeon on {id} did not exit within {wait_s}s of the stop request"));
        }
        sleep();
    }
    Ok(())
}

#[cfg(test)]
#[path = "reopen_tests.rs"]
mod tests;
