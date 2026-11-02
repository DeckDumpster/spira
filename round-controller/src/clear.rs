//! `round-controller lease clear`: an operator's way past a stuck round lease. The lease is
//! released only once its holder is proven dead, by the unit its holder runs under and the VM of
//! its pass; anything less is refused, naming what could not be shown.

use crate::death::{self, prove_runner_dead, Holder, Proof};
use crate::lease::{holder_of, token_word, Lease, LeaseError, Resource};
use crate::orphans::HOLDER;
use crate::ports::Ports;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleared {
    /// No live lease held the round.
    Unheld,
    Released {
        holder: String,
        token: String,
        evidence: String,
    },
}

pub fn clear(p: &Ports, repo: &str, batch: &str) -> Result<Cleared, String> {
    let res = Resource::round(repo, batch);
    let Some(held) = holder_of(p, &res)? else {
        return Ok(Cleared::Unheld);
    };
    let token = token_word(held.token);
    let who = held.holder.clone().unwrap_or_default();
    let named = format!(
        "round {batch} is leased by {} under token {token}",
        if who.is_empty() {
            "a holder no record names"
        } else {
            &who
        }
    );
    let old = Holder {
        repo: repo.into(),
        batch: batch.into(),
        token: held.token,
    };
    let proof = match who.as_str() {
        HOLDER => match old.unit() {
            Ok(unit) => prove_runner_dead(p, &unit, || p.vms.vm_of(&old)),
            Err(why) => Proof::Unknown { why },
        },
        death::HAND_HOLDER => match death::hand_unit_name(batch, held.token) {
            Ok(unit) => prove_runner_dead(p, &unit, || p.vms.vm_of_slot(&unit, None)),
            Err(why) => Proof::Unknown { why },
        },
        _ => Proof::Unknown {
            why: "no unit is known for that holder, so it cannot be shown stopped".into(),
        },
    };
    let evidence = match proof {
        Proof::Dead { evidence } => evidence,
        Proof::Unknown { why } => {
            return Err(format!("refused: {named}, {}s left, and its holder is not proven dead: {why} — nothing changed; wait for the lease to lapse", held.ttl_left_ms / 1000));
        }
    };
    match Lease::handed(res, held.token).release(p) {
        Ok(()) => Ok(Cleared::Released {
            holder: who,
            token,
            evidence,
        }),
        Err(LeaseError::Lost) => Ok(Cleared::Unheld),
        Err(e) => Err(format!(
            "{named}, proven dead ({evidence}), and its release failed: {e:?}"
        )),
    }
}
