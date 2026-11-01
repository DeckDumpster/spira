//! Supersede on a broken base: every live round whose `tested_base` is neither the landing ref
//! nor the head of a live round below it can no longer land, so it becomes SUPERSEDED, keeping
//! its verdicts, and its members go back to the pool for the cut later in the same pass.

use crate::outcome::Outcome;
use crate::ports::{Ports, Verb};
use crate::refusal::{self, Refusal};
use crate::stack::{self, supersedable, Stack};

/// The step name a supersede's standing refusal is kept under.
pub const STEP: &str = "supersede";

/// Supersedes every round of `repo` that can no longer land and answers their batch ids. A repo
/// with no lane record is not the controller's yet and is left alone; so is a round cut before
/// stacking, which carries no `tested_base`, and one in a state the verb does not move.
pub fn check(p: &Ports, repo: &str) -> Result<Vec<String>, Refusal> {
    let Some(lane) = stack::lane(p, repo)? else {
        return Ok(Vec::new());
    };
    let rounds = stack::live(p, repo)?;
    let grounded = Stack::of(&lane, &rounds);
    let mut done = Vec::new();
    for r in &rounds {
        if r.round.tested_base.is_none()
            || grounded.grounded(&r.batch)
            || !supersedable(r.round.row.state)
        {
            continue;
        }
        let got = p
            .verbs
            .supersede(repo, &r.batch, r.round.last_lease_token.as_deref());
        if let Err(refusal) = got {
            return Err(Refusal::Verb {
                batch: Some(r.batch.clone()),
                refusal,
            });
        }
        p.log.write(&Outcome::of::<()>(
            p.clock.now_ms(),
            repo,
            Some(&r.batch),
            Verb::Supersede,
            &Ok(()),
        ));
        done.push(r.batch.clone());
    }
    Ok(done)
}

/// `pass`'s supersede step: [`check`], its refusal told once.
pub fn step(p: &Ports, repo: &str) {
    let r = check(p, repo);
    refusal::note(p, repo, STEP, r.as_ref().err());
}
