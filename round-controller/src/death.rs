//! Proof that a lease's old runner is dead, before anything re-drives its round or reuses its
//! slot: its unit is not running, and round-vm reports no VM for its pass. A fact that cannot be
//! shown is not a proof, so the round is held.

use state_store::Rev;

use crate::ports::Ports;

/// The runner of `batch` under lease `token`. A re-drive takes a new token, so two re-drives of
/// one batch never share a unit name.
pub fn unit_name(batch: &str, token: Rev) -> String {
    format!("round-controller-run-{batch}-{}-{}", token.main, token.sub)
}

/// A lease's past holder: the runner launched for `batch` under `token`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub repo: String,
    pub batch: String,
    pub token: Rev,
}

impl Holder {
    pub fn unit(&self) -> String {
        unit_name(&self.batch, self.token)
    }
}

/// `systemctl --user is-active`'s answer, or why there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitState {
    /// A state in which the unit has stopped: `inactive`, `failed`, or one systemd does not know.
    Stopped(String),
    /// Any other state: active, activating, deactivating, reloading, or one not listed here.
    Running(String),
    Unreadable(String),
}

impl UnitState {
    pub fn from_is_active(stdout: &str) -> UnitState {
        match stdout.lines().next().map(str::trim) {
            Some(s @ ("inactive" | "failed" | "unknown")) => UnitState::Stopped(s.into()),
            Some(s) if !s.is_empty() => UnitState::Running(s.into()),
            _ => UnitState::Unreadable("is-active printed no state".into()),
        }
    }
}

pub trait Units {
    fn state(&self, unit: &str) -> UnitState;
}

/// What round-vm says about the VM of a holder's pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmFact {
    /// No VM for the pass: none was recorded, or the recorded one is gone or terminated.
    NoVm,
    Running {
        vm: String,
    },
    Unreadable {
        why: String,
    },
    /// An EC2 instance whose state cannot be read. Every spill VM shuts itself down by its
    /// maximum lifetime, so one launched longer ago than that is dead.
    Ec2Unreadable {
        instance: String,
        launched_at_ms: u64,
    },
}

pub trait Vms {
    fn vm_of(&self, old: &Holder) -> VmFact;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    Dead {
        evidence: String,
    },
    /// Not shown to be dead, which holds the round.
    Unknown {
        why: String,
    },
}

pub fn prove_dead(p: &Ports, old: &Holder) -> Proof {
    let unit = old.unit();
    let unit_said = match p.units.state(&unit) {
        UnitState::Stopped(s) => s,
        UnitState::Running(s) => {
            return Proof::Unknown {
                why: format!("unit {unit} is {s}"),
            }
        }
        UnitState::Unreadable(why) => {
            return Proof::Unknown {
                why: format!("unit {unit} unreadable: {why}"),
            }
        }
    };
    let vm_said = match p.vms.vm_of(old) {
        VmFact::NoVm => "no VM".to_string(),
        VmFact::Running { vm } => {
            return Proof::Unknown {
                why: format!("VM {vm} of {unit} is running"),
            }
        }
        VmFact::Unreadable { why } => {
            return Proof::Unknown {
                why: format!("VM of {unit} unreadable: {why}"),
            }
        }
        VmFact::Ec2Unreadable {
            instance,
            launched_at_ms,
        } => {
            let deadline = launched_at_ms.saturating_add(p.leases.ec2_max_lifetime_ms);
            if p.clock.now_ms() < deadline {
                return Proof::Unknown {
                    why: format!("EC2 {instance} of {unit} unreadable and inside its lifetime"),
                };
            }
            format!("EC2 {instance} past its self-termination deadline")
        }
    };
    Proof::Dead {
        evidence: format!("unit {unit} {unit_said}; {vm_said}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::bench::Bench;

    fn old() -> Holder {
        Holder {
            repo: "r".into(),
            batch: "b1".into(),
            token: Rev::new(7, 0),
        }
    }

    #[test]
    fn unit_names_carry_the_token_so_two_redrives_of_one_batch_never_share_one() {
        assert_eq!(old().unit(), "round-controller-run-b1-7-0");
        let names: std::collections::BTreeSet<String> =
            [Rev::new(7, 0), Rev::new(9, 0), Rev::new(9, 1)]
                .into_iter()
                .map(|t| unit_name("b1", t))
                .collect();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn is_active_output_is_stopped_running_or_unreadable() {
        for s in ["inactive", "failed\n", "unknown"] {
            assert!(
                matches!(UnitState::from_is_active(s), UnitState::Stopped(_)),
                "{s}"
            );
        }
        for s in [
            "active",
            "activating",
            "deactivating",
            "reloading",
            "maintenance",
        ] {
            assert!(
                matches!(UnitState::from_is_active(s), UnitState::Running(_)),
                "{s}"
            );
        }
        assert!(matches!(
            UnitState::from_is_active(""),
            UnitState::Unreadable(_)
        ));
    }

    #[test]
    fn a_stopped_unit_and_no_vm_is_proof() {
        let b = Bench::new();
        b.units
            .set(&old().unit(), UnitState::Stopped("inactive".into()));
        assert_eq!(
            prove_dead(&b.ports(), &old()),
            Proof::Dead {
                evidence: "unit round-controller-run-b1-7-0 inactive; no VM".into()
            }
        );
    }

    #[test]
    fn a_running_or_unreadable_unit_or_vm_holds() {
        let b = Bench::new();
        let u = old().unit();
        let why = |b: &Bench| match prove_dead(&b.ports(), &old()) {
            Proof::Unknown { why } => why,
            d => panic!("not held: {d:?}"),
        };
        b.units.set(&u, UnitState::Running("active".into()));
        assert_eq!(why(&b), "unit round-controller-run-b1-7-0 is active");
        b.units
            .set(&u, UnitState::Unreadable("no systemctl".into()));
        assert!(why(&b).contains("unreadable: no systemctl"));
        b.units.set(&u, UnitState::Stopped("failed".into()));
        b.vms.set(&u, VmFact::Running { vm: "vm-1".into() });
        assert!(why(&b).contains("VM vm-1 of round-controller-run-b1-7-0 is running"));
        b.vms.set(
            &u,
            VmFact::Unreadable {
                why: "pve down".into(),
            },
        );
        assert!(why(&b).contains("unreadable: pve down"));
    }

    #[test]
    fn an_unreadable_ec2_instance_is_dead_only_past_its_lifetime_deadline() {
        let b = Bench::new();
        let u = old().unit();
        b.units.set(&u, UnitState::Stopped("inactive".into()));
        let now = runtime::ports::Clock::now_ms(&b.clock);
        let life = b.ports().leases.ec2_max_lifetime_ms;
        b.vms.set(
            &u,
            VmFact::Ec2Unreadable {
                instance: "i-1".into(),
                launched_at_ms: now - life + 1,
            },
        );
        assert!(matches!(
            prove_dead(&b.ports(), &old()),
            Proof::Unknown { .. }
        ));
        b.clock.advance(1);
        assert_eq!(
            prove_dead(&b.ports(), &old()),
            Proof::Dead {
                evidence: "unit round-controller-run-b1-7-0 inactive; EC2 i-1 past its self-termination deadline".into()
            }
        );
    }
}
