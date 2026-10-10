//! The VM pool as a state machine: each VM moves provisioning → ready → leased → released /
//! doomed → destroyed, and every move is an event with a reason. The event log is the record;
//! `status --json` reads it back and nothing else.

use serde::{Deserialize, Serialize};

use crate::schema::{now, PoolState};

/// The events kept: older ones of a destroyed VM are dropped first.
pub const MAX_EVENTS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Provisioning,
    Ready,
    Leased,
    Released,
    Doomed,
    Destroyed,
}

impl VmState {
    pub fn as_str(self) -> &'static str {
        match self {
            VmState::Provisioning => "provisioning",
            VmState::Ready => "ready",
            VmState::Leased => "leased",
            VmState::Released => "released",
            VmState::Doomed => "doomed",
            VmState::Destroyed => "destroyed",
        }
    }

    pub fn can_go(self, to: VmState) -> bool {
        use VmState::*;
        matches!(
            (self, to),
            (Provisioning, Ready | Leased | Doomed | Destroyed)
                | (Ready, Leased | Released | Doomed)
                | (Leased, Released | Doomed)
                | (Released, Doomed | Destroyed)
                | (Doomed, Destroyed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolEvent {
    pub vm: String,
    pub to: VmState,
    pub reason: String,
    pub at: u64,
}

impl PoolState {
    pub fn state_of(&self, vm: &str) -> Option<VmState> {
        self.events.iter().rev().find(|e| e.vm == vm).map(|e| e.to)
    }

    /// Applies a transition, or refuses naming the state the VM is in.
    pub fn record(&mut self, vm: &str, to: VmState, reason: &str) -> Result<(), String> {
        let ok = match self.state_of(vm) {
            None => to == VmState::Provisioning,
            // Proxmox's /cluster/nextid reuses ids, so a new allocation of a destroyed VM's id is a new
            // VM (law-the-allocator-assigns-the-identifier). Refusing it wedged every round once id
            // 109 came round again (r-auto-110, 2026-10-10).
            Some(VmState::Destroyed) => to == VmState::Provisioning,
            Some(from) => from.can_go(to),
        };
        if !ok {
            let from = self.state_of(vm).map_or("unknown", VmState::as_str);
            return Err(format!("round-vm: VM {vm} is {from}; it cannot become {} ({reason})", to.as_str()));
        }
        self.events.push(PoolEvent { vm: vm.to_string(), to, reason: reason.to_string(), at: now() });
        self.trim();
        Ok(())
    }

    /// `record` for the pool's own call sites: a refusal is named on stderr and nothing is written.
    pub fn note(&mut self, vm: &str, to: VmState, reason: &str) {
        if let Err(e) = self.record(vm, to, reason) {
            eprintln!("{e}");
        }
    }

    /// Records `Destroyed` for a VM that is released or doomed; any other state is left alone.
    pub fn settle(&mut self, vm: &str, reason: &str) {
        if matches!(self.state_of(vm), Some(VmState::Released | VmState::Doomed)) {
            self.note(vm, VmState::Destroyed, reason);
        }
    }

    /// Gives a VM the pool already holds (state from before events existed) a history.
    pub fn adopt_untracked(&mut self) {
        let mut held: Vec<(String, VmState)> = Vec::new();
        held.extend(self.ready.iter().map(|v| (v.handle.clone(), VmState::Ready)));
        held.extend(self.leases.iter().map(|l| (l.vm.handle.clone(), VmState::Leased)));
        held.extend(self.provisioning.iter().filter_map(|p| p.vmid.clone()).map(|h| (h, VmState::Provisioning)));
        held.extend(self.doomed.iter().map(|h| (h.clone(), VmState::Doomed)));
        for (vm, to) in held {
            if self.state_of(&vm).is_none() {
                self.events.push(PoolEvent { vm, to, reason: "adopted".into(), at: now() });
            }
        }
    }

    fn trim(&mut self) {
        while self.events.len() > MAX_EVENTS {
            let Some(i) = (0..self.events.len()).find(|&i| {
                let vm = &self.events[i].vm;
                self.state_of(vm) == Some(VmState::Destroyed)
            }) else {
                return;
            };
            self.events.remove(i);
        }
    }

    /// Every VM not yet destroyed: its state, since when and why.
    pub fn vms(&self) -> Vec<(String, VmState, u64, String)> {
        let mut seen: Vec<String> = Vec::new();
        for e in &self.events {
            if !seen.contains(&e.vm) {
                seen.push(e.vm.clone());
            }
        }
        seen.into_iter()
            .filter_map(|vm| {
                let e = self.events.iter().rev().find(|e| e.vm == vm)?;
                (e.to != VmState::Destroyed).then(|| (vm, e.to, e.at, e.reason.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_destroyed_vm_id_provisions_again_as_a_new_vm() {
        let mut p = PoolState::default();
        p.record("109", VmState::Provisioning, "adopted").unwrap();
        p.record("109", VmState::Doomed, "its provisioner died").unwrap();
        p.record("109", VmState::Destroyed, "destroy verified").unwrap();
        p.record("109", VmState::Provisioning, "provision started").expect("a reused id starts over");
        let mut q = PoolState::default();
        q.record("7", VmState::Provisioning, "x").unwrap();
        q.record("7", VmState::Doomed, "x").unwrap();
        q.record("7", VmState::Destroyed, "x").unwrap();
        assert!(q.record("7", VmState::Ready, "x").is_err(), "only provisioning follows destroyed");
    }

    use super::*;
    use VmState::*;

    const ALL: [VmState; 6] = [Provisioning, Ready, Leased, Released, Doomed, Destroyed];
    const LEGAL: [(VmState, VmState); 10] = [
        (Provisioning, Ready),
        (Provisioning, Leased),
        (Provisioning, Doomed),
        (Provisioning, Destroyed),
        (Ready, Leased),
        (Ready, Released),
        (Ready, Doomed),
        (Leased, Released),
        (Leased, Doomed),
        (Released, Destroyed),
    ];

    fn at(state: VmState) -> PoolState {
        let mut s = PoolState::default();
        s.events.push(PoolEvent { vm: "7".into(), to: state, reason: "fixture".into(), at: 0 });
        s
    }

    #[test]
    fn every_legal_transition_applies_and_is_recorded() {
        let mut legal: Vec<(VmState, VmState)> = LEGAL.to_vec();
        legal.extend([(Released, Doomed), (Doomed, Destroyed)]);
        for (from, to) in legal {
            let mut s = at(from);
            s.record("7", to, "because").unwrap_or_else(|e| panic!("{from:?}->{to:?}: {e}"));
            let last = s.events.last().unwrap();
            assert_eq!((last.vm.as_str(), last.to, last.reason.as_str()), ("7", to, "because"));
            assert_eq!(s.state_of("7"), Some(to));
        }
    }

    #[test]
    fn every_illegal_transition_is_refused_naming_the_state() {
        let legal = |f, t| LEGAL.contains(&(f, t)) || (f, t) == (Released, Doomed) || (f, t) == (Doomed, Destroyed) || (f, t) == (Destroyed, Provisioning);
        for from in ALL {
            for to in ALL {
                if legal(from, to) {
                    continue;
                }
                let mut s = at(from);
                let e = s.record("7", to, "because").unwrap_err();
                assert!(e.contains(from.as_str()) && e.contains(to.as_str()), "{e}");
                assert_eq!(s.events.len(), 1, "a refusal writes nothing");
            }
        }
    }

    #[test]
    fn an_unknown_vm_can_only_begin_provisioning() {
        let mut s = PoolState::default();
        assert!(s.record("9", Leased, "x").unwrap_err().contains("unknown"));
        s.record("9", Provisioning, "x").unwrap();
    }

    #[test]
    fn vms_reads_exactly_the_recorded_state() {
        let mut s = PoolState::default();
        s.record("1", Provisioning, "p").unwrap();
        s.record("1", Ready, "r").unwrap();
        s.record("2", Provisioning, "p").unwrap();
        s.record("2", Destroyed, "gone").unwrap();
        let got = s.vms();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0.as_str(), got[0].1, got[0].3.as_str()), ("1", Ready, "r"));
    }

    #[test]
    fn the_log_is_bounded_by_dropping_destroyed_history_first() {
        let mut s = PoolState::default();
        s.record("keep", Provisioning, "p").unwrap();
        for i in 0..MAX_EVENTS {
            let vm = format!("v{i}");
            s.record(&vm, Provisioning, "p").unwrap();
            s.record(&vm, Destroyed, "d").unwrap();
        }
        assert!(s.events.len() <= MAX_EVENTS);
        assert_eq!(s.state_of("keep"), Some(Provisioning));
    }

    #[test]
    fn untracked_holdings_are_adopted_once() {
        let mut s = PoolState::default();
        s.ready = Some(crate::schema::Vm { handle: "5".into(), addr: "a".into() });
        s.doomed.push("6".into());
        s.adopt_untracked();
        s.adopt_untracked();
        assert_eq!((s.state_of("5"), s.state_of("6"), s.events.len()), (Some(Ready), Some(Doomed), 2));
    }
}
