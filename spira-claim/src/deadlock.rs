//! `spira-claim deadlocked` — lift a poison from a candidate whose branch already carries
//! finished, landable work. Replaces `spira/attempts.sh deadlocked` (DESIGN.md §9).
//!
//! THE GIT STAYS OUT (same boundary as `select`, DESIGN.md §7 "`select` does no git"):
//! resolving a bead's repository and land ref, and asking whether `spira/<id>` names the
//! bead and merges cleanly, needs the repo map and `git`, which belong to lib.sh. The
//! caller (`groomer deadlocked`) does that legwork once per candidate and hands the
//! verdict in on `--merge-status`; this module owns only the decision every poisoned
//! candidate needs made from it, and the write.
//!
//! THE WRITE IS UNPOISON'S, NOT A SECOND COPY. Lifting the hold/label is the same two steps
//! (§8's 3 and 3b) `spira-claim unpoison` performs, through the same [`World`] — so a lift
//! here is provably the same write, not a parallel implementation that could drift from it.
//! Deliberately NOT shared as one function: `deadlocked` never floors the attempt count
//! (the rungs are left standing, "the record of how it got here" — attempts.sh's own
//! header), never resets ask history, and never closes an ask, so folding it into
//! `unpoison::one` would need a flag for every one of those to turn off — cheaper to repeat
//! the dozen lines than to grow that function a mode.

use crate::events;
use crate::unpoison::{LcApply, LcRow, World, POISON_LABEL};

/// One candidate's git verdict, from `groomer deadlocked`'s merge-tree check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candidate {
    pub id: String,
    /// The branch names the bead and merges cleanly into `base`.
    pub ok: bool,
    /// Why not, when `ok` is false — printed verbatim (KEEP's reason).
    pub why: String,
    pub branch: String,
    pub base: String,
}

pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    let v: serde_json::Value =
        serde_json::from_str(t).map_err(|e| format!("--merge-status: not JSON: {e}"))?;
    let arr = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Null => Vec::new(),
        other => vec![other],
    };
    arr.into_iter()
        .map(|r| {
            let s = |k: &str| {
                r.get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let id = s("id");
            if id.is_empty() {
                return Err(format!("--merge-status: row without id: {r}"));
            }
            Ok(Candidate {
                id,
                ok: r
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                why: s("why"),
                branch: s("branch"),
                base: s("base"),
            })
        })
        .collect()
}

pub struct Opts {
    pub apply: bool,
    pub actor: String,
    /// `lifecycle_enforce` (DESIGN.md §6a/§8.7): off reads the `spira-poison` bd label and
    /// never calls spira-lc; on reads and releases the lifecycle hold.
    pub enforce: bool,
}

enum Outcome {
    /// Not poisoned — attempts.sh's own rule: skip silently, exactly as `poisoned || continue`.
    NotOurs,
    Kept,
    Would,
    Restored,
    Failed,
}

fn one(o: &Opts, w: &mut dyn World, c: &Candidate, out: &mut String) -> Outcome {
    let bead = match w.bead(&c.id) {
        Ok(Some(b)) => b,
        Ok(None) => {
            out.push_str(&format!("FAIL {}: no such bead\n", c.id));
            return Outcome::Failed;
        }
        Err(e) => {
            out.push_str(&format!("FAIL {}: cannot tell (bd show: {e})\n", c.id));
            return Outcome::Failed;
        }
    };
    let lc: Option<LcRow> = if o.enforce {
        match w.lc_row(&c.id) {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!(
                    "FAIL {}: cannot tell (spira-lc show: {e}; lifecycle_enforce is on, so the machine must answer)\n",
                    c.id
                ));
                return Outcome::Failed;
            }
        }
    } else {
        None
    };
    let labelled = bead.labels.iter().any(|l| l == POISON_LABEL);
    let poisoned = if o.enforce {
        lc.as_ref().is_some_and(LcRow::poisoned)
    } else {
        labelled
    };
    if !poisoned {
        return Outcome::NotOurs;
    }

    if !c.ok {
        out.push_str(&format!("KEEP     {} {}\n", c.id, c.why));
        return Outcome::Kept;
    }

    // Live work is never touched (same guard `unpoison` uses, DESIGN.md §8.2 precondition 4):
    // a poisoned bead should never be claimed, but this is the one check standing between a
    // wrong merge-status verdict and a lift under a live aeon.
    let assignee = bead.assignee.as_deref().filter(|a| !a.trim().is_empty());
    let lc_holder = lc
        .as_ref()
        .filter(|r| r.state == lifecycle::bead::BeadState::Working)
        .and_then(|r| r.holder.as_deref())
        .filter(|h| !h.trim().is_empty());
    let held = match (bead.status.as_str(), assignee, lc_holder) {
        ("in_progress", Some(a), _) => Some(format!("{a} (in_progress)")),
        (_, _, Some(h)) => Some(format!("{h} (lifecycle WORKING)")),
        _ => None,
    };
    if let Some(h) = held {
        out.push_str(&format!(
            "FAIL {}: held by {h} — live work is never touched: let that aeon finish (or stop it: spira/slay.sh --bead {}), then re-run\n",
            c.id, c.id
        ));
        return Outcome::Failed;
    }

    if !o.apply {
        out.push_str(&format!(
            "WOULD    {} finished on {} and it merges into {} — would lift the poison\n",
            c.id, c.branch, c.base
        ));
        return Outcome::Would;
    }

    let mut warns: Vec<String> = Vec::new();
    if let Some(row) = lc.as_ref().filter(|r| r.poisoned()) {
        match w.lc_unhold_poison(&c.id, row, &o.actor) {
            LcApply::Applied => {}
            LcApply::Refused(_) => {
                // One retry from a fresh read, same as unpoison: a lost CAS race, not a
                // reason to leave the hold standing on finished work.
                if let Ok(Some(fresh)) = w.lc_row(&c.id) {
                    if fresh.poisoned() {
                        let _ = w.lc_unhold_poison(&c.id, &fresh, &o.actor);
                    }
                }
            }
            LcApply::CannotTell(e) => warns.push(format!("unhold: {e}")),
        }
    }
    if labelled {
        if let Err(e) = w.remove_label(&c.id, POISON_LABEL) {
            if !o.enforce {
                warns.push(format!("label remove: {e}"));
            }
        }
    }
    let note = format!(
        "Poison lifted by spira-claim deadlocked ({}): {} carries a commit naming {} and merges cleanly into {}, \
so this is finished, landable work. A poisoned bead stays open, an open bead carrying the label is claimed by \
nobody, and the landing pass lands only closed beads — so the label was holding completed work out of the queue \
permanently. The counters are left standing as the record of how it got here.",
        o.actor, c.branch, c.id, c.base
    );
    if let Err(e) = w.note(&c.id, &note) {
        warns.push(format!("note: {e}"));
    }
    // sp-wiyr2: the lift's own dedup. deadlocked never floors the count (the rungs stay
    // standing, on purpose — attempts.sh's own header comment), so without this CHECK 4's
    // very next pass reads the same n against a hold that is no longer there and poisons it
    // right back within minutes.
    let attempts_now = match w.events(&c.id) {
        Ok(rows) => events::fold(&c.id, &rows).attempts,
        Err(e) => {
            warns.push(format!("mark poison-lifted: could not re-read attempts ({e}); a same-count pass may re-poison"));
            0
        }
    };
    if let Err(e) = w.mark_poison_lifted(&c.id, attempts_now) {
        warns.push(format!("mark poison-lifted: {e}"));
    }

    let still_poisoned = if o.enforce {
        matches!(w.lc_row(&c.id), Ok(Some(r)) if r.poisoned())
    } else {
        matches!(w.bead(&c.id), Ok(Some(b)) if b.labels.iter().any(|l| l == POISON_LABEL))
    };
    if still_poisoned {
        out.push_str(&format!(
            "REFUSED  {}: the poison hold would not come off\n",
            c.id
        ));
        return Outcome::Failed;
    }
    for wm in &warns {
        out.push_str(&format!("warn {}: {wm}\n", c.id));
    }
    out.push_str(&format!(
        "RESTORED {} poison lifted; {} is finished and merges into {}\n",
        c.id, c.branch, c.base
    ));
    Outcome::Restored
}

/// Returns (exit code, stdout). Exit 3 mirrors unpoison's table: a candidate refused or a
/// lift that did not verify.
pub fn run(o: &Opts, candidates: &[Candidate], w: &mut dyn World) -> (i32, String) {
    let mut out = String::new();
    let (mut n_seen, mut n_hit, mut n_done) = (0u32, 0u32, 0u32);
    let mut failed = false;
    for c in candidates {
        match one(o, w, c, &mut out) {
            Outcome::NotOurs => {}
            Outcome::Kept => n_seen += 1,
            Outcome::Would => {
                n_seen += 1;
                n_hit += 1;
            }
            Outcome::Restored => {
                n_seen += 1;
                n_hit += 1;
                n_done += 1;
            }
            Outcome::Failed => failed = true,
        }
    }
    out.push_str(&format!("--- {n_seen} poisoned bead(s) examined, {n_hit} finished and landable, {n_done} restored\n"));
    if !o.apply {
        out.push_str("--- dry run; pass --apply to lift these\n");
    }
    (if failed { 3 } else { 0 }, out)
}

#[cfg(test)]
#[path = "deadlock_tests.rs"]
mod tests;
