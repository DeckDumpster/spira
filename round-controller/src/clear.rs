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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::death::{UnitState, VmFact};
    use crate::lease::acquire;
    use crate::sim::bench::Bench;
    use serde_json::json;

    fn leased(b: &Bench, holder: &str) -> Lease {
        b.seed("batches/r/b1", json!({ "state": "CI_RUNNING" }));
        acquire(&b.ports(), &Resource::round("r", "b1"), holder).unwrap()
    }

    fn still_held(b: &Bench) -> bool {
        holder_of(&b.ports(), &Resource::round("r", "b1"))
            .unwrap()
            .is_some()
    }

    #[test]
    fn a_hand_drivers_lease_is_cleared_only_once_its_unit_is_stopped_and_its_vm_gone() {
        let b = Bench::new();
        let l = leased(&b, death::HAND_HOLDER);
        let unit = death::hand_unit_name("b1", l.token()).unwrap();
        assert!(unit.starts_with("round-recert-b1-"), "{unit}");
        b.units.set(&unit, UnitState::Running("active".into()));
        let e = clear(&b.ports(), "r", "b1").unwrap_err();
        assert!(
            e.starts_with("refused: round b1 is leased by round-recert under token")
                && e.contains(&format!("unit {unit} is active"))
                && e.contains("nothing changed"),
            "{e}"
        );
        assert!(still_held(&b));
        b.units.set(&unit, UnitState::Stopped("inactive".into()));
        b.vms.set(
            &unit,
            VmFact::Running {
                vm: "vm-103".into(),
            },
        );
        assert!(clear(&b.ports(), "r", "b1")
            .unwrap_err()
            .contains("VM vm-103"));
        assert!(still_held(&b));
        b.vms.set(&unit, VmFact::NoVm);
        assert_eq!(
            clear(&b.ports(), "r", "b1").unwrap(),
            Cleared::Released {
                holder: death::HAND_HOLDER.into(),
                token: token_word(l.token()),
                evidence: format!("unit {unit} inactive; no VM"),
            }
        );
        assert!(!still_held(&b));
        assert_eq!(clear(&b.ports(), "r", "b1").unwrap(), Cleared::Unheld);
    }

    #[test]
    fn a_runners_lease_is_proven_by_its_runner_unit_and_an_unknown_holder_is_refused() {
        let b = Bench::new();
        let l = leased(&b, HOLDER);
        let unit = death::unit_name("b1", l.token()).unwrap();
        b.vms.set(
            &unit,
            VmFact::Unreadable {
                why: "pve down".into(),
            },
        );
        assert!(clear(&b.ports(), "r", "b1")
            .unwrap_err()
            .contains("unreadable: pve down"));
        b.vms.set(&unit, VmFact::NoVm);
        assert!(matches!(
            clear(&b.ports(), "r", "b1").unwrap(),
            Cleared::Released { .. }
        ));

        let b = Bench::new();
        leased(&b, "someone-else");
        let e = clear(&b.ports(), "r", "b1").unwrap_err();
        assert!(e.contains("no unit is known for that holder"), "{e}");
        assert!(still_held(&b));
    }
}
