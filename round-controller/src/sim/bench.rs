//! Fakes for the liveness ports, and a bench that owns every port on a virtual clock for a test
//! to hand to the library through [`Bench::ports`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use runtime::ports::{Proc, ProcOutput, ProcSpec, SeededRng, VirtualClock};
use state_store::{
    Config, Contract, Fence, Item, MemBackend, Object, Request, Rev, Selector, Txn, TxnResult,
};

use crate::death::{Holder, UnitState, Units, VmFact, Vms};
use crate::liveness::LeaseSettings;
use crate::outcome::MemLog;
use crate::ports::{Launch, Launched, Ports, Store};
use crate::real::Unserved;
use crate::sim::judge::FakeLedger;
use crate::sim::verbs::FakeVerbs;
use crate::sim::vm::FakeVm;
use crate::slots::{SlotGrant, SlotSettings};

/// Unit states by name; a unit never set is one systemd does not know, and a state set `until` a
/// time reads as unknown from then on.
pub struct FakeUnits {
    states: Mutex<BTreeMap<String, (UnitState, Option<u64>)>>,
    clock: VirtualClock,
}

impl FakeUnits {
    pub fn new(clock: VirtualClock) -> FakeUnits {
        FakeUnits {
            states: Mutex::default(),
            clock,
        }
    }

    pub fn set(&self, unit: &str, s: UnitState) {
        self.set_until(unit, s, None);
    }

    pub fn set_until(&self, unit: &str, s: UnitState, until_ms: Option<u64>) {
        crate::lock(&self.states).insert(unit.into(), (s, until_ms));
    }
}

impl Units for FakeUnits {
    fn state(&self, unit: &str) -> UnitState {
        use runtime::ports::Clock;
        match crate::lock(&self.states).get(unit) {
            Some((s, until)) if until.is_none_or(|u| self.clock.now_ms() < u) => s.clone(),
            _ => UnitState::Stopped("unknown".into()),
        }
    }
}

/// VM facts by the holder's unit name; a unit never set had no VM, and a fact set `until` a time
/// reads as no VM from then on.
pub struct FakeVms {
    facts: Mutex<BTreeMap<String, (VmFact, Option<u64>)>>,
    clock: VirtualClock,
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl FakeVms {
    pub fn new(clock: VirtualClock) -> FakeVms {
        FakeVms {
            facts: Mutex::default(),
            clock,
            asked: Mutex::default(),
        }
    }

    pub fn set(&self, unit: &str, f: VmFact) {
        self.set_until(unit, f, None);
    }

    pub fn set_until(&self, unit: &str, f: VmFact, until_ms: Option<u64>) {
        crate::lock(&self.facts).insert(unit.into(), (f, until_ms));
    }

    /// Every slot lookup: the unit and the VM id it was asked by.
    pub fn asked(&self) -> Vec<(String, Option<String>)> {
        crate::lock(&self.asked).clone()
    }

    fn fact(&self, unit: &str) -> VmFact {
        use runtime::ports::Clock;
        match crate::lock(&self.facts).get(unit) {
            Some((f, until)) if until.is_none_or(|u| self.clock.now_ms() < u) => f.clone(),
            _ => VmFact::NoVm,
        }
    }
}

impl Vms for FakeVms {
    fn vm_of(&self, old: &Holder) -> VmFact {
        match old.unit() {
            Ok(u) => self.fact(&u),
            Err(why) => VmFact::Unreadable { why },
        }
    }
    fn vm_of_slot(&self, unit: &str, vm: Option<&str>) -> VmFact {
        crate::lock(&self.asked).push((unit.into(), vm.map(str::to_string)));
        self.fact(unit)
    }
}

/// One launch tried: its unit, and the slot and slot token it was handed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tried {
    pub unit: String,
    pub slot: String,
    pub slot_token: Rev,
}

/// Launches answered in script order: `Ok` starts, `Err` fails with its text. An unscripted
/// launch fails and is kept as a fault.
#[derive(Default)]
pub struct FakeLaunch {
    script: Mutex<VecDeque<Result<(), String>>>,
    tried: Mutex<Vec<Tried>>,
    faults: Mutex<Vec<String>>,
}

impl FakeLaunch {
    pub fn script(&self, r: Result<(), String>) {
        crate::lock(&self.script).push_back(r);
    }

    /// The unit of every launch tried, started or not.
    pub fn launched(&self) -> Vec<String> {
        self.tried().into_iter().map(|t| t.unit).collect()
    }

    pub fn tried(&self) -> Vec<Tried> {
        crate::lock(&self.tried).clone()
    }

    pub fn faults(&self) -> Vec<String> {
        crate::lock(&self.faults).clone()
    }
}

impl Launch for FakeLaunch {
    fn start(
        &self,
        _: &Ports,
        repo: &str,
        batch: &str,
        lease_token: Rev,
        slot: &SlotGrant,
    ) -> Launched {
        let unit = match crate::death::unit_name(batch, lease_token) {
            Ok(u) => u,
            Err(text) => return Launched::Failed { text },
        };
        crate::lock(&self.tried).push(Tried {
            unit: unit.clone(),
            slot: slot.slot().to_string(),
            slot_token: slot.token(),
        });
        match crate::lock(&self.script).pop_front() {
            Some(Ok(())) => Launched::Started { unit },
            Some(Err(text)) => Launched::Failed { text },
            None => {
                let text = format!("unscripted launch of {unit} for {repo}");
                crate::lock(&self.faults).push(text.clone());
                Launched::Failed { text }
            }
        }
    }
}

/// A test spawns no process: every effect has a port of its own.
#[derive(Default)]
pub struct NoProc(pub Mutex<Vec<String>>);

impl Proc for NoProc {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        crate::lock(&self.0).push(format!("spawned {}", spec.prog));
        Err(format!("a scenario spawns no process: {}", spec.prog))
    }
}

/// The settings a bench runs under: not the shipped defaults, so nothing passes by matching them.
pub fn bench_settings() -> LeaseSettings {
    LeaseSettings {
        ttl_ms: 60_000,
        deadlines_ms: [30_000, 40_000, 50_000, 60_000, 20_000],
        pass_wall_max_ms: 600_000,
        redrive_retries: 3,
        ec2_max_lifetime_ms: 3_600_000,
    }
}

/// Spill on with three EC2 slots: not the shipped default, so nothing passes by matching it.
pub fn bench_slot_settings() -> SlotSettings {
    SlotSettings {
        ec2_slots: 3,
        spill: true,
    }
}

enum Front {
    Store,
    /// Every txn after the first `skip` first rewrites this key, so a CAS on it never holds.
    Racing {
        key: String,
        skip: usize,
    },
    Unserved,
}

pub struct Bench {
    pub clock: VirtualClock,
    /// Shared with the fake queue, which writes the rounds it opens.
    pub store: Arc<Contract<MemBackend>>,
    pub rng: Mutex<SeededRng>,
    pub log: MemLog,
    pub proc_: NoProc,
    pub verbs: FakeVerbs,
    pub units: FakeUnits,
    pub vms: FakeVms,
    pub launch: FakeLaunch,
    pub settings: LeaseSettings,
    pub slot_settings: SlotSettings,
    pub ledger: FakeLedger,
    /// When each runner slot was given back, on the virtual clock.
    pub slot_releases: Mutex<Vec<u64>>,
    front: Mutex<Front>,
}

pub const EPOCH_MS: u64 = 1_800_000_000_000;

impl Bench {
    pub fn new() -> Bench {
        Bench::build(bench_settings(), EPOCH_MS, 1, Front::Store)
    }

    pub fn with_ttl_ms(ms: u64) -> Bench {
        Bench::build(
            LeaseSettings {
                ttl_ms: ms,
                ..bench_settings()
            },
            EPOCH_MS,
            1,
            Front::Store,
        )
    }

    /// A bench whose store refuses every call, as the binary's does until spira-lc serves it.
    pub fn unserved() -> Bench {
        Bench::build(bench_settings(), EPOCH_MS, 1, Front::Unserved)
    }

    pub fn at(settings: LeaseSettings, epoch_ms: u64, seed: u64) -> Bench {
        Bench::build(settings, epoch_ms, seed, Front::Store)
    }

    fn build(settings: LeaseSettings, epoch_ms: u64, seed: u64, front: Front) -> Bench {
        let clock = VirtualClock::new(epoch_ms);
        let store = Arc::new(
            Contract::open(
                MemBackend::new(),
                state_store::Ports::virtual_clock(&clock),
                Config {
                    min_lock_ttl_ms: 1000,
                },
            )
            .expect("a fresh in-memory store opens"),
        );
        let vm = FakeVm::new(
            vec![],
            crate::sim::profiles()
                .expect("the shipped profiles parse")
                .remove(0),
            clock.clone(),
        );
        let verbs = FakeVerbs::new(vec![], vm, clock.clone()).expect("an empty script loads");
        Bench {
            units: FakeUnits::new(clock.clone()),
            vms: FakeVms::new(clock.clone()),
            clock,
            store,
            rng: Mutex::new(SeededRng::new(seed)),
            log: MemLog::default(),
            proc_: NoProc::default(),
            verbs,
            launch: FakeLaunch::default(),
            settings,
            slot_settings: bench_slot_settings(),
            ledger: FakeLedger::default(),
            slot_releases: Mutex::default(),
            front: Mutex::new(front),
        }
    }

    pub fn ports(&self) -> Ports<'_> {
        Ports {
            clock: &self.clock,
            proc_: &self.proc_,
            rng: &self.rng,
            store: self,
            verbs: &self.verbs,
            ledger: &self.ledger,
            log: &self.log,
            units: &self.units,
            vms: &self.vms,
            launcher: &self.launch,
            leases: &self.settings,
            slots: &self.slot_settings,
        }
    }

    pub fn seed(&self, key: &str, data: serde_json::Value) {
        let req = Request::new(&format!("seed-{key}"), "bench", "seed");
        self.store
            .create(key, Object::new(data), &req)
            .unwrap_or_else(|e| panic!("seed {key}: {e}"));
    }

    pub fn racing_writes(&self, key: &str) {
        self.racing_writes_after(key, 0);
    }

    pub fn racing_writes_after(&self, key: &str, skip: usize) {
        *crate::lock(&self.front) = Front::Racing {
            key: key.into(),
            skip,
        };
    }

    fn race(&self) {
        let mut front = crate::lock(&self.front);
        let Front::Racing { key, skip } = &mut *front else {
            return;
        };
        if *skip > 0 {
            *skip -= 1;
            return;
        }
        let n = self.store.head().expect("the in-memory store has a head");
        let t = Txn::new().put(key, Object::new(serde_json::json!({ "racer": n })));
        self.store
            .txn(&t, &Request::new(&format!("race-{n}"), "bench", "race"))
            .expect("the racing write commits");
    }

    fn served(&self) -> bool {
        !matches!(*crate::lock(&self.front), Front::Unserved)
    }
}

impl Default for Bench {
    fn default() -> Self {
        Bench::new()
    }
}

impl Store for Bench {
    fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
        if !self.served() {
            return Unserved.get(key);
        }
        self.store.get(key)
    }
    fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
        if !self.served() {
            return Unserved.list(prefixes, sel);
        }
        self.store.list(prefixes, sel)
    }
    fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<TxnResult> {
        if !self.served() {
            return Unserved.txn(t, req);
        }
        self.race();
        self.store.txn(t, req)
    }
    fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> state_store::Result<Fence> {
        if !self.served() {
            return Unserved.lock(resource, ttl_ms, req);
        }
        self.store.lock(resource, ttl_ms, req)
    }
    fn renew(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        if !self.served() {
            return Unserved.renew(f, req);
        }
        self.store.renew(f, req)
    }
    fn release(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        if !self.served() {
            return Unserved.release(f, req);
        }
        self.store.release(f, req)?;
        if f.resource.starts_with("runner-slot/") {
            use runtime::ports::Clock;
            crate::lock(&self.slot_releases).push(self.clock.now_ms());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unscripted_launch_is_refused_and_kept_as_a_fault() {
        let b = Bench::default();
        let p = b.ports();
        let slot = SlotGrant::handed(
            &p,
            crate::slots::Slot {
                pool: crate::slots::Pool::Proxmox,
                n: 0,
            },
            Rev::new(5, 0),
        );
        let l = FakeLaunch::default();
        l.script(Ok(()));
        l.script(Err("systemd-run: exit 1".into()));
        assert!(matches!(
            l.start(&p, "r", "b", Rev::new(1, 0), &slot),
            Launched::Started { .. }
        ));
        assert_eq!(
            l.start(&p, "r", "b", Rev::new(2, 0), &slot),
            Launched::Failed {
                text: "systemd-run: exit 1".into()
            }
        );
        assert!(matches!(
            l.start(&p, "r", "b", Rev::new(3, 0), &slot),
            Launched::Failed { .. }
        ));
        assert!(matches!(
            l.start(&p, "r", "-b", Rev::new(4, 0), &slot),
            Launched::Failed { .. }
        ));
        assert_eq!(
            l.faults(),
            ["unscripted launch of round-controller-run-b-3-0 for r"]
        );
        assert_eq!(l.launched().len(), 3);
        assert_eq!(
            l.tried()[0],
            Tried {
                unit: "round-controller-run-b-1-0".into(),
                slot: "proxmox/0".into(),
                slot_token: Rev::new(5, 0)
            }
        );
        assert!(Bench::default()
            .ports()
            .proc_
            .run(&ProcSpec {
                prog: "git".into(),
                args: vec![],
                stdin: vec![],
                deadline: std::time::Duration::from_secs(1),
            })
            .is_err());
    }

    #[test]
    fn a_unit_set_until_a_time_is_unknown_from_then_on() {
        let b = Bench::new();
        let now = runtime::ports::Clock::now_ms(&b.clock);
        b.units.set_until(
            "u",
            UnitState::Running("activating".into()),
            Some(now + 10),
        );
        assert_eq!(b.units.state("u"), UnitState::Running("activating".into()));
        b.clock.advance(10);
        assert_eq!(b.units.state("u"), UnitState::Stopped("unknown".into()));
    }

    #[test]
    fn a_racing_bench_moves_the_key_before_every_txn() {
        let b = Bench::new();
        b.seed("batches/r/b", serde_json::json!({}));
        let rev = b.store.get("batches/r/b").unwrap().unwrap().mod_rev;
        b.racing_writes("batches/r/b");
        let t = Txn::new()
            .when(state_store::Compare::ModRev(
                "batches/r/b".into(),
                Some(rev),
            ))
            .put("batches/r/b", Object::new(serde_json::json!(1)));
        assert!(
            !Store::txn(&b, &t, &Request::new("x", "t", "k"))
                .unwrap()
                .succeeded
        );
    }
}
