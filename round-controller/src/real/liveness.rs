//! The liveness ports' production implementations. Until the runner-slot records and the
//! launcher exist, the VM port cannot show any VM gone and the launcher starts nothing, so every
//! orphan is held rather than re-driven on a guess.

use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use state_store::Rev;

use crate::death::{Holder, UnitState, Units, VmFact, Vms};
use crate::orphans::{Launch, Launched};

const IS_ACTIVE_DEADLINE: Duration = Duration::from_secs(20); // batch-job: one systemctl query

/// `systemctl --user is-active <unit>`.
pub struct SystemdUnits<'a> {
    pub proc_: &'a dyn Proc,
}

impl Units for SystemdUnits<'_> {
    fn state(&self, unit: &str) -> UnitState {
        let spec = ProcSpec {
            prog: "systemctl".into(),
            args: vec!["--user".into(), "is-active".into(), unit.into()],
            stdin: vec![],
            deadline: IS_ACTIVE_DEADLINE,
        };
        match self.proc_.run(&spec) {
            Ok(o) => UnitState::from_is_active(&String::from_utf8_lossy(&o.stdout)),
            Err(e) => UnitState::Unreadable(e),
        }
    }
}

pub struct NoVmRecords;

impl Vms for NoVmRecords {
    fn vm_of(&self, _: &Holder) -> VmFact {
        VmFact::Unreadable {
            why: "no runner-slot record names a VM yet".into(),
        }
    }
    fn vm_of_slot(&self, _: &str, _: Option<&str>) -> VmFact {
        VmFact::Unreadable {
            why: "round-vm is not asked about a slot's VM yet".into(),
        }
    }
}

pub struct NoLauncher;

impl Launch for NoLauncher {
    fn launch(&self, _: &str, _: &str, _: Rev) -> Result<Launched, String> {
        Err("round-controller has no launcher yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::ProcOutput;
    use std::sync::Mutex;

    struct Answer(Mutex<Vec<String>>, Result<ProcOutput, String>);

    impl Proc for Answer {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.0).push(format!("{} {}", spec.prog, spec.args.join(" ")));
            self.1.clone()
        }
    }

    #[test]
    fn a_unit_is_read_through_systemctl_user_is_active() {
        let out = |s: &str, code| {
            Ok(ProcOutput {
                code: Some(code),
                stdout: s.into(),
                stderr: vec![],
            })
        };
        let p = Answer(Mutex::default(), out("inactive\n", 3));
        assert_eq!(
            SystemdUnits { proc_: &p }.state("u-1"),
            UnitState::Stopped("inactive".into())
        );
        assert_eq!(*crate::lock(&p.0), ["systemctl --user is-active u-1"]);
        let p = Answer(Mutex::default(), out("active\n", 0));
        assert_eq!(
            SystemdUnits { proc_: &p }.state("u"),
            UnitState::Running("active".into())
        );
        let p = Answer(Mutex::default(), Err("no systemctl".into()));
        assert_eq!(
            SystemdUnits { proc_: &p }.state("u"),
            UnitState::Unreadable("no systemctl".into())
        );
    }

    #[test]
    fn with_no_vm_records_and_no_launcher_nothing_is_proven_or_started() {
        let h = Holder {
            repo: "r".into(),
            batch: "b".into(),
            token: Rev::new(1, 0),
        };
        assert!(matches!(NoVmRecords.vm_of(&h), VmFact::Unreadable { .. }));
        assert!(matches!(
            NoVmRecords.vm_of_slot("u", Some("vm-7")),
            VmFact::Unreadable { .. }
        ));
        assert!(NoLauncher.launch("r", "b", Rev::new(1, 0)).is_err());
    }
}
