//! The VM port's production implementation. Until the runner-slot records name a VM, it cannot
//! show any VM gone, so every orphan is held rather than re-driven on a guess.

use crate::death::{Holder, VmFact, Vms};

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

#[cfg(test)]
mod tests {
    use super::*;
    use state_store::Rev;

    #[test]
    fn with_no_vm_records_nothing_is_proven() {
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
    }
}
