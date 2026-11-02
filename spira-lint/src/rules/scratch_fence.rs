//! `scratch-fence` — refuse dwarf scratch files tracked at the harness root.
//! Contract: DESIGN.md. Ported from `spira/scratch-fence.sh` (deleted).

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ScratchFence;

const NAME: &str = "scratch-fence";
/// Set for the one commit that removes existing offenders; named in the commit message.
const OVERRIDE_VAR: &str = "SCRATCH_FENCE_OK";

/// A tracked, root-level `sp-*` dwarf working note or a `*.fixed` hand-patched artefact.
pub fn is_offender(path: &str) -> bool {
    if path.contains('/') {
        return false;
    }
    path.starts_with("sp-") || path.ends_with(".fixed")
}

fn overridden() -> bool {
    std::env::var(OVERRIDE_VAR).as_deref() == Ok("1")
}

/// [`ScratchFence::check`], with the override's state passed in rather than read from the
/// real process environment — the seam a test uses so no test needs a real
/// `env::set_var`/`remove_var`, which every thread in the test binary shares and races on.
fn check_with_override(rule: &ScratchFence, tree: &Tree, override_on: bool) -> Result<Vec<Finding>, LintError> {
    let tracked = tree.entries.iter().filter(|e| e.tracked).count();
    if tracked < 1 {
        return Err(LintError::EmptyScope);
    }
    if override_on {
        return Ok(Vec::new());
    }
    Ok(tree
        .entries
        .iter()
        .filter(|e| rule.applies_to(e))
        .map(|e| Finding {
            rule: NAME,
            path: e.path.clone(),
            line: None,
            message: "dwarf scratch file at the harness root — delete it, or set SCRATCH_FENCE_OK=1 only for the commit that removes it".to_string(),
        })
        .collect())
}

impl Rule for ScratchFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked && is_offender(&e.path)
    }

    /// Deliberately not `crate::scope`: an empty *offender* set is the normal, clean state,
    /// not a refusal. The refusal is an empty index — nothing tracked at all, which is
    /// indistinguishable from a clean tree unless checked directly
    /// (law-absence-needs-a-positive-control).
    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        check_with_override(self, tree, overridden())
    }

    fn hint(&self) -> &'static str {
        "Dwarf working notes committed to the harness root ship to every consumer and widen the \
suite-coverage fallback on every branch that follows. Delete them; SCRATCH_FENCE_OK=1 is the \
named override, valid only for the commit that removes existing offenders."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ScratchFence),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn is_offender_matches_the_bash_regex() {
        assert!(is_offender("sp-xxxx-notes.md"));
        assert!(is_offender("some-script.sh.fixed"));
        assert!(!is_offender("spira/sp-abc.md"));
        assert!(!is_offender("helper.sh"));
    }
}
