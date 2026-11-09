//! Runner slots: a declared budget in two pools, each slot the lock `locks/runner-slot/<pool>/<n>`
//! with its last holder recorded at `slots/<pool>/<n>`. A slot whose holder lapsed without
//! releasing is granted again only once that runner is proven dead, so a lost lease never puts two
//! VMs on a one-VM host. The controller and a hand driver take slots through the same grant.

use std::fmt;

use serde_json::{json, Value};
use state_store::{Compare, Error, Fence, Object, Request, Rev, Selector, Txn, LOCKS_PREFIX};

use crate::death::{prove_runner_dead, Proof};
use crate::lease::{holder_of, LeaseError, Resource};
use crate::ports::{Ports, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pool {
    Proxmox,
    Ec2,
}

impl Pool {
    pub const ALL: [Pool; 2] = [Pool::Proxmox, Pool::Ec2];

    pub fn word(self) -> &'static str {
        match self {
            Pool::Proxmox => "proxmox",
            Pool::Ec2 => "ec2",
        }
    }

    pub fn from_word(w: &str) -> Option<Pool> {
        Pool::ALL.into_iter().find(|p| p.word() == w)
    }
}

/// The host rule: the local hypervisor runs one VM round at a time, so the Proxmox pool is one
/// slot whatever else is configured. Side-by-side rounds run on EC2 spill.
pub const PROXMOX_SLOTS: u32 = 1;
pub const EC2_SLOTS_KEY: &str = "SPIRA_ROUND_RUNNER_SLOTS_EC2";
pub const SPILL_KEY: &str = "SPIRA_ROUND_VM_SPILL";
/// The spill words under which round-vm places a pass on EC2; any other value keeps it local, so
/// the EC2 pool offers nothing round-vm would not run.
pub const SPILL_WORDS: [&str; 3] = ["ec2", "ec2-always", "ec2-only"];

/// The pools a hand driver asks, in order: the same pools the controller draws from.
pub const HAND_PREFERENCE: [Pool; 2] = [Pool::Proxmox, Pool::Ec2];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSettings {
    pub ec2_slots: u32,
    /// `SPIRA_ROUND_VM_SPILL` lets round-vm place a pass on EC2.
    pub spill: bool,
}

impl SlotSettings {
    /// Every key through `get`, which answers a key's declared value or refuses naming it.
    pub fn from_cfg(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let v = get(EC2_SLOTS_KEY)?;
        let ec2_slots = v
            .trim()
            .parse::<u32>()
            .map_err(|e| format!("{EC2_SLOTS_KEY} = {v:?} does not parse: {e}"))?;
        let spill = SPILL_WORDS.contains(&get(SPILL_KEY)?.trim());
        Ok(SlotSettings { ec2_slots, spill })
    }

    pub fn budget(&self, pool: Pool) -> u32 {
        match pool {
            Pool::Proxmox => PROXMOX_SLOTS,
            Pool::Ec2 if self.spill => self.ec2_slots,
            Pool::Ec2 => 0,
        }
    }

    /// Why `pool` offers no slot, naming the key that opens it.
    fn closed(&self, pool: Pool) -> Option<String> {
        match pool {
            Pool::Ec2 if !self.spill => Some(format!("{SPILL_KEY} does not allow spill")),
            Pool::Ec2 if self.ec2_slots == 0 => Some(format!("{EC2_SLOTS_KEY} is 0")),
            _ => None,
        }
    }
}

pub fn budget(p: &Ports, pool: Pool) -> u32 {
    p.slots.budget(pool)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub pool: Pool,
    pub n: u32,
}

impl Slot {
    /// The lock's resource: its key is `locks/runner-slot/<pool>/<n>`.
    pub fn lock(&self) -> String {
        format!("runner-slot/{self}")
    }

    pub fn record_key(&self) -> String {
        format!("slots/{self}")
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.pool.word(), self.n)
    }
}

/// `slots/<pool>/<n>`: who took the slot, the runner unit and VM it recorded, and whether it
/// gave the slot back. Advisory: the lock decides who holds the slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub holder: String,
    pub unit: Option<String>,
    pub vm: Option<String>,
    pub released: bool,
}

impl Record {
    fn json(&self) -> Value {
        json!({
            "holder": self.holder,
            "unit": self.unit,
            "vm": self.vm,
            "released": self.released,
        })
    }

    pub(crate) fn of(v: &Value) -> Option<Record> {
        let opt = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Record {
            holder: v.get("holder")?.as_str()?.to_string(),
            unit: opt("unit"),
            vm: opt("vm"),
            released: v.get("released")?.as_bool()?,
        })
    }
}

/// Why one slot was not granted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Taken {
    Held {
        holder: Option<String>,
        unit: Option<String>,
        ttl_left_ms: u64,
    },
    /// Its last holder lapsed without releasing it and is not shown dead.
    Unproven {
        holder: String,
        why: String,
    },
    Unreadable {
        why: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotHolder {
    pub slot: Slot,
    pub taken: Taken,
}

/// No slot in the pools asked was free: each slot's holder, and each pool that offers none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoSlot {
    pub holders: Vec<SlotHolder>,
    pub closed: Vec<(Pool, String)>,
}

impl fmt::Display for NoSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self
            .holders
            .iter()
            .map(|h| match &h.taken {
                Taken::Held {
                    holder,
                    unit,
                    ttl_left_ms,
                } => format!(
                    "{} is held by {}{} for {}s more",
                    h.slot,
                    holder.as_deref().unwrap_or("an unrecorded holder"),
                    unit.as_deref()
                        .filter(|u| Some(*u) != holder.as_deref())
                        .map(|u| format!(" (unit {u})"))
                        .unwrap_or_default(),
                    ttl_left_ms.div_ceil(1000)
                ),
                Taken::Unproven { holder, why } => format!(
                    "{}'s last holder {holder} lapsed and is not proven dead: {why}",
                    h.slot
                ),
                Taken::Unreadable { why } => format!("{} cannot be read: {why}", h.slot),
            })
            .collect();
        parts.extend(
            self.closed
                .iter()
                .map(|(pool, why)| format!("{} offers no slot: {why}", pool.word())),
        );
        if parts.is_empty() {
            parts.push("no pool was asked".into());
        }
        write!(
            f,
            "no runner slot free: {}. A held slot frees when its holder releases it or its lease \
             lapses; a lapsed one is granted again once its unit has stopped and round-vm reports \
             its VM gone",
            parts.join("; ")
        )
    }
}

#[derive(Debug)]
pub struct SlotGrant {
    slot: Slot,
    fence: Fence,
    record: Record,
    evidence: Option<String>,
}

fn request(p: &Ports, op: &str) -> Request {
    Request::new(
        &format!("rc-slot-{op}-{:016x}", p.next_u64()),
        "round-controller",
        &format!("slot.{op}"),
    )
}

/// The first free slot in `prefer`'s pools, in order, taken for `holder` for one lease TTL.
pub fn grant(p: &Ports, prefer: &[Pool], holder: &str) -> Result<SlotGrant, NoSlot> {
    let mut no = NoSlot::default();
    for &pool in prefer {
        if let Some(why) = p.slots.closed(pool) {
            no.closed.push((pool, why));
            continue;
        }
        for n in 0..budget(p, pool) {
            let slot = Slot { pool, n };
            match take(p, slot, holder) {
                Ok(g) => return Ok(g),
                Err(taken) => no.holders.push(SlotHolder { slot, taken }),
            }
        }
    }
    Err(no)
}

/// A hand driver's slot: the same grant, pools and proof as the controller's, so a hand round
/// and a controller round never run two VMs on one slot.
pub fn grant_for_hand(p: &Ports, holder: &str) -> Result<SlotGrant, NoSlot> {
    grant(p, &HAND_PREFERENCE, holder)
}

fn prove_last(p: &Ports, r: &Record) -> Proof {
    match &r.unit {
        None => Proof::Dead {
            evidence: format!("{} recorded no unit, so launched none", r.holder),
        },
        Some(u) => prove_runner_dead(p, u, || p.vms.vm_of_slot(u, r.vm.as_deref())),
    }
}

/// A lapsed slot whose last holder is not shown dead, with the unit it recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnprovenSlot {
    pub slot: Slot,
    pub holder: String,
    pub unit: String,
    pub why: String,
}

/// Every slot on record that `grant` would refuse as unproven, whichever pool is open now.
pub fn unproven(p: &Ports) -> Result<Vec<UnprovenSlot>, String> {
    let (items, _) = p
        .store
        .list(&["slots/"], &Selector::Everything)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in items {
        let Some((pool, n)) = i.key.strip_prefix("slots/").and_then(|k| k.split_once('/')) else {
            continue;
        };
        let (Some(pool), Ok(n)) = (Pool::from_word(pool), n.parse::<u32>()) else {
            continue;
        };
        let slot = Slot { pool, n };
        let Some(r) = Record::of(&i.value.data).filter(|r| !r.released && r.unit.is_some()) else {
            continue;
        };
        let res = Resource {
            lock: slot.lock(),
            record: None,
        };
        if holder_of(p, &res)?.is_some() {
            continue;
        }
        if let Proof::Unknown { why } = prove_last(p, &r) {
            out.push(UnprovenSlot {
                slot,
                holder: r.holder,
                unit: r.unit.unwrap_or_default(),
                why,
            });
        }
    }
    Ok(out)
}

fn take(p: &Ports, slot: Slot, holder: &str) -> Result<SlotGrant, Taken> {
    let unreadable = |why: String| Taken::Unreadable { why };
    let res = Resource {
        lock: slot.lock(),
        record: None,
    };
    let key = slot.record_key();
    let item = p.store.get(&key).map_err(|e| unreadable(e.to_string()))?;
    let last = match &item {
        None => None,
        Some(i) => Some(
            Record::of(&i.value.data)
                .ok_or_else(|| unreadable(format!("{key} is not a slot record")))?,
        ),
    };
    if let Some(h) = holder_of(p, &res).map_err(unreadable)? {
        return Err(Taken::Held {
            holder: last.as_ref().map(|r| r.holder.clone()),
            unit: last.and_then(|r| r.unit),
            ttl_left_ms: h.ttl_left_ms,
        });
    }
    let evidence = match last.filter(|r| !r.released) {
        None => None,
        Some(r) => Some(match prove_last(p, &r) {
            Proof::Dead { evidence } => evidence,
            Proof::Unknown { why } => {
                return Err(Taken::Unproven {
                    holder: r.holder,
                    why,
                })
            }
        }),
    };
    let record = Record {
        holder: holder.into(),
        ..Record::default()
    };
    let t = Txn::new()
        .when(Compare::ModRev(key.clone(), item.map(|i| i.mod_rev)))
        .lock_acquire(&res.lock, p.leases.ttl_ms)
        .put(&key, Object::new(record.json()));
    let r = p
        .store
        .txn(&t, &request(p, "grant"))
        .map_err(|e| unreadable(e.to_string()))?;
    match (r.succeeded, r.fences.into_iter().next()) {
        (true, Some(fence)) => Ok(SlotGrant {
            slot,
            fence,
            record,
            evidence,
        }),
        _ => Err(match holder_of(p, &res) {
            Ok(Some(h)) => Taken::Held {
                holder: None,
                unit: None,
                ttl_left_ms: h.ttl_left_ms,
            },
            _ => unreadable(format!("{key} moved while it was being granted")),
        }),
    }
}

fn store_err(e: Error) -> LeaseError {
    match e {
        Error::LockLost { .. } => LeaseError::Lost,
        e => LeaseError::Store(e.to_string()),
    }
}

impl SlotGrant {
    /// The grant `pass` took and handed over by its token, with the holder it recorded; an
    /// unreadable record is renewed or refused on the lock, not here.
    pub fn handed(p: &Ports, slot: Slot, token: Rev) -> SlotGrant {
        let record = p
            .store
            .get(&slot.record_key())
            .ok()
            .flatten()
            .and_then(|i| Record::of(&i.value.data))
            .unwrap_or_default();
        SlotGrant {
            slot,
            fence: Fence {
                resource: slot.lock(),
                token,
            },
            record,
            evidence: None,
        }
    }

    pub fn slot(&self) -> Slot {
        self.slot
    }

    pub fn token(&self) -> Rev {
        self.fence.token
    }

    /// The proof that the slot's last holder was dead, when it lapsed without releasing.
    pub fn evidence(&self) -> Option<&str> {
        self.evidence.as_deref()
    }

    /// The slot lock's key, `locks/runner-slot/<pool>/<n>`.
    pub fn key(&self) -> String {
        self.fence.key()
    }

    /// The slot's lock is live and still held under this grant's token: the Planner cuts only on
    /// a slot it holds, so nothing is cut ahead of capacity.
    pub fn held(&self, store: &dyn Store) -> state_store::Result<bool> {
        Ok(store
            .get(&self.fence.key())?
            .is_some_and(|i| i.create_rev == self.fence.token && i.lock_expires_at_ms.is_some()))
    }

    /// The lock this grant holds, for a test that lets it go behind the grant's back.
    #[cfg(feature = "sim")]
    pub fn fence(&self) -> &Fence {
        &self.fence
    }

    /// A grant over a lock the scenario harness or a test took itself, with no holder record.
    #[cfg(feature = "sim")]
    pub fn of_fence(slot: Slot, fence: Fence) -> SlotGrant {
        SlotGrant {
            slot,
            fence,
            record: Record::default(),
            evidence: None,
        }
    }

    pub fn renew(&self, p: &Ports) -> Result<(), LeaseError> {
        p.store
            .renew(&self.fence, &request(p, "renew"))
            .map_err(store_err)
    }

    /// Records the runner under this slot. Record its unit before launching it, and its VM once
    /// round-vm names one: a lapsed holder that recorded no unit is taken to have launched none.
    pub fn record_holder(
        &mut self,
        p: &Ports,
        unit: &str,
        vm: Option<&str>,
    ) -> Result<(), LeaseError> {
        let record = Record {
            unit: Some(unit.into()),
            vm: vm.map(str::to_string),
            ..self.record.clone()
        };
        self.write(p, &record, "record")?;
        self.record = record;
        Ok(())
    }

    /// Gives the slot back. Only once the runner's VM is gone: a released slot is granted again
    /// without proof of death.
    pub fn release(self, p: &Ports) -> Result<(), LeaseError> {
        let record = Record {
            released: true,
            ..self.record.clone()
        };
        self.write(p, &record, "released")?;
        p.store
            .release(&self.fence, &request(p, "release"))
            .map_err(store_err)
    }

    fn write(&self, p: &Ports, record: &Record, op: &str) -> Result<(), LeaseError> {
        let t = Txn::new()
            .lock_check(&self.fence)
            .put(&self.slot.record_key(), Object::new(record.json()));
        match p.store.txn(&t, &request(p, op)).map_err(store_err)? {
            r if r.succeeded => Ok(()),
            _ => Err(LeaseError::Lost),
        }
    }
}

/// Slots held under a live lock, per pool, for the slots-in-use gauge.
pub fn in_use(p: &Ports) -> Result<Vec<(Pool, u32)>, String> {
    let now = p.clock.now_ms();
    Pool::ALL
        .into_iter()
        .map(|pool| {
            let prefix = format!("{LOCKS_PREFIX}runner-slot/{}/", pool.word());
            let (items, _) = p
                .store
                .list(&[&prefix], &state_store::Selector::Everything)
                .map_err(|e| e.to_string())?;
            let live = items
                .iter()
                .filter(|i| i.lock_expires_at_ms.is_some_and(|e| e > now))
                .count();
            Ok((pool, u32::try_from(live).unwrap_or(u32::MAX)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::death::{UnitState, VmFact};
    use crate::sim::bench::Bench;

    fn cfg(slots: &'static str, spill: &'static str) -> impl Fn(&str) -> Result<String, String> {
        move |k| match k {
            EC2_SLOTS_KEY => Ok(slots.into()),
            SPILL_KEY => Ok(spill.into()),
            _ => Err(format!("{k} is not registered")),
        }
    }

    fn spill_off(b: &mut Bench) {
        b.slot_settings.spill = false;
    }

    fn record(b: &Bench, s: Slot) -> Record {
        Record::of(&b.store.get(&s.record_key()).unwrap().unwrap().value.data).unwrap()
    }

    const PROXMOX: Slot = Slot {
        pool: Pool::Proxmox,
        n: 0,
    };

    #[test]
    fn proxmox_is_one_slot_and_ec2_is_its_key_only_while_spill_is_allowed() {
        for word in SPILL_WORDS {
            let s = SlotSettings::from_cfg(&cfg(" 4 ", word)).unwrap();
            assert_eq!(
                s,
                SlotSettings {
                    ec2_slots: 4,
                    spill: true
                }
            );
            assert_eq!((s.budget(Pool::Proxmox), s.budget(Pool::Ec2)), (1, 4));
        }
        for word in ["", "off", "EC2"] {
            let s = SlotSettings::from_cfg(&cfg("4", word)).unwrap();
            assert_eq!(
                (s.budget(Pool::Proxmox), s.budget(Pool::Ec2)),
                (1, 0),
                "{word:?}"
            );
        }
        let e = SlotSettings::from_cfg(&cfg("two", "ec2")).unwrap_err();
        assert!(
            e.starts_with("SPIRA_ROUND_RUNNER_SLOTS_EC2 = \"two\""),
            "{e}"
        );
        let e = SlotSettings::from_cfg(&|k: &str| match k {
            EC2_SLOTS_KEY => Ok("1".into()),
            _ => Err(format!("{k} is not registered")),
        })
        .unwrap_err();
        assert_eq!(e, "SPIRA_ROUND_VM_SPILL is not registered");
        assert_eq!(budget(&Bench::new().ports(), Pool::Ec2), 3);
    }

    #[test]
    fn every_pool_round_trips_its_word_and_an_unknown_word_is_none() {
        for p in Pool::ALL {
            assert_eq!(Pool::from_word(p.word()), Some(p));
        }
        assert_eq!(Pool::from_word("gcp"), None);
        assert_eq!(PROXMOX.lock(), "runner-slot/proxmox/0");
        assert_eq!(PROXMOX.record_key(), "slots/proxmox/0");
    }

    #[test]
    fn a_refusal_names_each_holder_its_unit_and_ttl_left_each_closed_pool_and_the_exit() {
        let no = NoSlot {
            holders: vec![
                SlotHolder {
                    slot: PROXMOX,
                    taken: Taken::Held {
                        holder: Some("round-recert".into()),
                        unit: Some("round-recert-b1-7-0".into()),
                        ttl_left_ms: 40_001,
                    },
                },
                SlotHolder {
                    slot: Slot {
                        pool: Pool::Ec2,
                        n: 0,
                    },
                    taken: Taken::Held {
                        holder: Some("u".into()),
                        unit: Some("u".into()),
                        ttl_left_ms: 1000,
                    },
                },
                SlotHolder {
                    slot: Slot {
                        pool: Pool::Ec2,
                        n: 1,
                    },
                    taken: Taken::Unproven {
                        holder: "old".into(),
                        why: "VM vm-7 of old-u is running".into(),
                    },
                },
                SlotHolder {
                    slot: Slot {
                        pool: Pool::Ec2,
                        n: 2,
                    },
                    taken: Taken::Unreadable {
                        why: "store down".into(),
                    },
                },
                SlotHolder {
                    slot: Slot {
                        pool: Pool::Ec2,
                        n: 3,
                    },
                    taken: Taken::Held {
                        holder: None,
                        unit: None,
                        ttl_left_ms: 0,
                    },
                },
            ],
            closed: vec![(
                Pool::Ec2,
                "SPIRA_ROUND_VM_SPILL does not allow spill".into(),
            )],
        };
        assert_eq!(
            no.to_string(),
            "no runner slot free: proxmox/0 is held by round-recert (unit round-recert-b1-7-0) for 41s more; \
             ec2/0 is held by u for 1s more; \
             ec2/1's last holder old lapsed and is not proven dead: VM vm-7 of old-u is running; \
             ec2/2 cannot be read: store down; \
             ec2/3 is held by an unrecorded holder for 0s more; \
             ec2 offers no slot: SPIRA_ROUND_VM_SPILL does not allow spill. \
             A held slot frees when its holder releases it or its lease lapses; a lapsed one is \
             granted again once its unit has stopped and round-vm reports its VM gone"
        );
        assert!(NoSlot::default().to_string().contains("no pool was asked"));
    }

    #[test]
    fn grants_fill_the_preferred_pools_in_order_and_the_next_is_refused_naming_every_holder() {
        let b = Bench::new();
        let p = b.ports();
        let order = [Pool::Proxmox, Pool::Ec2];
        let got: Vec<String> = (0..4)
            .map(|i| {
                grant(&p, &order, &format!("r{i}"))
                    .unwrap()
                    .slot()
                    .to_string()
            })
            .collect();
        assert_eq!(got, ["proxmox/0", "ec2/0", "ec2/1", "ec2/2"]);
        let no = grant(&p, &order, "r4").unwrap_err();
        assert_eq!(no.holders.len(), 4);
        for i in 0..4 {
            assert!(
                no.to_string()
                    .contains(&format!("held by r{i} for 60s more")),
                "{no}"
            );
        }
        let ec2_first = grant(&p, &[Pool::Ec2, Pool::Proxmox], "r5").unwrap_err();
        assert_eq!(
            ec2_first.holders[0].slot,
            Slot {
                pool: Pool::Ec2,
                n: 0
            }
        );
        assert_eq!(record(&b, PROXMOX).holder, "r0");
    }

    #[test]
    fn with_spill_off_the_hand_driver_waits_on_the_one_proxmox_slot_and_is_told_who_holds_it() {
        let mut b = Bench::new();
        spill_off(&mut b);
        let p = b.ports();
        let mut ctl = grant(&p, &[Pool::Proxmox], "round-controller-run").unwrap();
        ctl.record_holder(&p, "round-controller-run-b1-7-0", Some("vm-7"))
            .unwrap();
        b.clock.advance(20_000);
        let no = grant_for_hand(&p, "round-recert").unwrap_err();
        assert_eq!(
            no.closed,
            [(
                Pool::Ec2,
                "SPIRA_ROUND_VM_SPILL does not allow spill".to_string()
            )]
        );
        assert!(
            no.to_string().starts_with(
                "no runner slot free: proxmox/0 is held by round-controller-run \
                 (unit round-controller-run-b1-7-0) for 40s more; ec2 offers no slot"
            ),
            "{no}"
        );
        ctl.release(&p).unwrap();
        let hand = grant_for_hand(&p, "round-recert").unwrap();
        assert_eq!((hand.slot(), hand.evidence()), (PROXMOX, None));
        b.slot_settings.spill = true;
        b.slot_settings.ec2_slots = 0;
        let no = grant_for_hand(&b.ports(), "x").unwrap_err();
        assert_eq!(
            no.closed,
            [(Pool::Ec2, "SPIRA_ROUND_RUNNER_SLOTS_EC2 is 0".to_string())]
        );
    }

    #[test]
    fn a_released_slot_is_granted_again_without_asking_whether_its_runner_lives() {
        let b = Bench::new();
        let p = b.ports();
        let mut g = grant(&p, &[Pool::Proxmox], "a").unwrap();
        g.record_holder(&p, "unit-a", Some("vm-7")).unwrap();
        b.units.set("unit-a", UnitState::Running("active".into()));
        g.release(&p).unwrap();
        let r = record(&b, PROXMOX);
        assert_eq!(
            r,
            Record {
                holder: "a".into(),
                unit: Some("unit-a".into()),
                vm: Some("vm-7".into()),
                released: true
            }
        );
        let again = grant(&p, &[Pool::Proxmox], "b").unwrap();
        assert_eq!(again.evidence(), None);
        assert!(b.vms.asked().is_empty());
        assert_eq!(record(&b, PROXMOX).holder, "b");
    }

    #[test]
    fn a_lapsed_slot_is_held_until_its_unit_has_stopped_and_its_recorded_vm_is_gone() {
        let b = Bench::new();
        let p = b.ports();
        let mut g = grant(&p, &[Pool::Proxmox], "a").unwrap();
        g.record_holder(&p, "unit-a", Some("vm-7")).unwrap();
        b.clock.advance(p.leases.ttl_ms);
        let unproven = |b: &Bench| match &grant(&b.ports(), &[Pool::Proxmox], "b")
            .unwrap_err()
            .holders[0]
            .taken
        {
            Taken::Unproven { holder, why } => format!("{holder}: {why}"),
            t => panic!("not unproven: {t:?}"),
        };
        b.units.set("unit-a", UnitState::Running("active".into()));
        assert_eq!(unproven(&b), "a: unit unit-a is active");
        assert!(
            b.vms.asked().is_empty(),
            "a running unit is not asked about its VM"
        );
        b.units.set("unit-a", UnitState::Stopped("inactive".into()));
        b.vms.set("unit-a", VmFact::Running { vm: "vm-7".into() });
        assert_eq!(unproven(&b), "a: VM vm-7 of unit-a is running");
        assert_eq!(
            b.vms.asked(),
            [("unit-a".to_string(), Some("vm-7".to_string()))]
        );
        b.vms.set("unit-a", VmFact::NoVm);
        let again = grant(&p, &[Pool::Proxmox], "b").unwrap();
        assert_eq!(again.evidence(), Some("unit unit-a inactive; no VM"));
        assert_eq!(g.renew(&p), Err(LeaseError::Lost));
        assert_eq!(
            g.record_holder(&p, "unit-a", None),
            Err(LeaseError::Lost),
            "the lapsed holder's record write is fenced out"
        );
        assert_eq!(g.release(&p), Err(LeaseError::Lost));
        assert_eq!(
            record(&b, PROXMOX),
            Record {
                holder: "b".into(),
                ..Record::default()
            }
        );
    }

    #[test]
    fn a_lapsed_holder_that_recorded_no_unit_launched_none_and_its_slot_is_granted() {
        let b = Bench::new();
        let p = b.ports();
        let g = grant(&p, &[Pool::Proxmox], "a").unwrap();
        b.clock.advance(p.leases.ttl_ms);
        let again = grant(&p, &[Pool::Proxmox], "b").unwrap();
        assert_eq!(
            again.evidence(),
            Some("a recorded no unit, so launched none")
        );
        assert_ne!(again.token(), g.token());
    }

    #[test]
    fn a_renewed_slot_outlives_its_first_ttl() {
        let b = Bench::new();
        let p = b.ports();
        let g = grant(&p, &[Pool::Proxmox], "a").unwrap();
        for _ in 0..4 {
            b.clock.advance(p.leases.renew_every_ms());
            g.renew(&p).unwrap();
        }
        assert!(matches!(
            grant(&p, &[Pool::Proxmox], "b").unwrap_err().holders[0].taken,
            Taken::Held { .. }
        ));
    }

    #[test]
    fn a_malformed_record_a_raced_grant_and_an_unserved_store_are_never_granted() {
        let b = Bench::new();
        b.seed("slots/proxmox/0", json!({ "holder": 7 }));
        let p = b.ports();
        assert_eq!(
            grant(&p, &[Pool::Proxmox], "a").unwrap_err().holders[0].taken,
            Taken::Unreadable {
                why: "slots/proxmox/0 is not a slot record".into()
            }
        );
        let b = Bench::new();
        b.racing_writes("slots/proxmox/0");
        assert_eq!(
            grant(&b.ports(), &[Pool::Proxmox], "a")
                .unwrap_err()
                .holders[0]
                .taken,
            Taken::Unreadable {
                why: "slots/proxmox/0 moved while it was being granted".into()
            }
        );
        let down = Bench::unserved();
        let no = grant(&down.ports(), &[Pool::Proxmox, Pool::Ec2], "a").unwrap_err();
        assert_eq!(no.holders.len(), 4);
        assert!(no.holders.iter().all(
            |h| matches!(&h.taken, Taken::Unreadable { why } if why.contains("no spira-lc door"))
        ));
        assert!(in_use(&down.ports()).is_err());
    }

    #[test]
    fn a_slot_locked_with_no_record_is_held_by_an_unrecorded_holder() {
        let b = Bench::new();
        let p = b.ports();
        let slot = Slot {
            pool: Pool::Ec2,
            n: 0,
        };
        let _rival = b
            .store
            .lock(&slot.lock(), 60_000, &request(&p, "rival"))
            .unwrap();
        b.clock.advance(1);
        assert_eq!(
            take(&p, slot, "a").unwrap_err(),
            Taken::Held {
                holder: None,
                unit: None,
                ttl_left_ms: 59_999
            }
        );
    }

    #[test]
    fn slots_in_use_count_only_live_locks_per_pool() {
        let b = Bench::new();
        let p = b.ports();
        let _a = grant(&p, &[Pool::Proxmox], "a").unwrap();
        let _b = grant(&p, &[Pool::Ec2], "b").unwrap();
        let c = grant(&p, &[Pool::Ec2], "c").unwrap();
        c.release(&p).unwrap();
        assert_eq!(in_use(&p).unwrap(), [(Pool::Proxmox, 1), (Pool::Ec2, 1)]);
        b.clock.advance(p.leases.ttl_ms);
        assert_eq!(in_use(&p).unwrap(), [(Pool::Proxmox, 0), (Pool::Ec2, 0)]);
    }

    #[test]
    fn a_grant_is_held_while_its_lock_lives_under_its_token_and_not_after() {
        let b = Bench::new();
        let p = b.ports();
        let g = grant(&p, &[Pool::Proxmox], "a").unwrap();
        assert_eq!(g.key(), "locks/runner-slot/proxmox/0");
        assert_eq!(g.held(p.store), Ok(true));
        let slot = g.slot();
        let stale = SlotGrant::of_fence(slot, g.fence.clone());
        g.release(&p).unwrap();
        assert_eq!(stale.held(p.store), Ok(false), "released");
        let again = grant(&p, &[Pool::Proxmox], "b").unwrap();
        assert_ne!(again.token(), stale.token());
        assert_eq!(
            stale.held(p.store),
            Ok(false),
            "another holder's lock is not this grant"
        );
        assert_eq!(again.held(p.store), Ok(true));
        b.clock.advance(p.leases.ttl_ms);
        assert_eq!(again.held(p.store), Ok(false), "expired");
        assert!(again.held(&crate::real::Unserved).is_err());
    }
}
