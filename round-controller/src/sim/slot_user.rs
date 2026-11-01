//! A scripted slot user on the virtual clock: a round's runner or a hand driver. It takes a runner
//! slot through the real grant, records its unit and VM, holds the slot for its pass renewing it
//! every renewal interval, then tears its VM down and only then releases. A refused grant is kept
//! with whether its text named every slot's recorded holder, and is retried.

use serde::Deserialize;
use state_store::Selector;

use crate::death::{UnitState, VmFact};
use crate::ports::Ports;
use crate::sim::bench::Bench;
use crate::slots::{grant, grant_for_hand, Pool, Record, SlotGrant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotUserScript {
    /// Its holder name and its unit's name.
    pub name: String,
    /// A hand driver asks through `grant_for_hand`, whose pools are its own.
    #[serde(default)]
    pub hand: bool,
    #[serde(default)]
    pub prefer: Vec<String>,
    /// Seconds after the epoch it first asks for a slot.
    pub start_secs: u64,
    /// How long its VM runs once it holds a slot.
    pub hold_secs: u64,
    /// How often a refused user asks again; absent, every renewal interval.
    #[serde(default)]
    pub retry_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmSpan {
    pub pool: Pool,
    pub from: u64,
    /// `None` while the VM still runs.
    pub to: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub hand: bool,
    pub text: String,
    pub named_every_holder: bool,
}

pub struct SlotUser {
    script: SlotUserScript,
    prefer: Vec<Pool>,
    next_ms: u64,
    release_at: u64,
    grant: Option<SlotGrant>,
    done: bool,
    pub grants: Vec<(Pool, u64)>,
    pub refusals: Vec<Refusal>,
    pub spans: Vec<VmSpan>,
    pub faults: Vec<String>,
}

impl SlotUser {
    pub fn new(script: SlotUserScript, epoch_ms: u64) -> Result<SlotUser, String> {
        let prefer = if script.prefer.is_empty() {
            Pool::ALL.to_vec()
        } else {
            script
                .prefer
                .iter()
                .map(|w| {
                    Pool::from_word(w)
                        .ok_or_else(|| format!("slot_user {}: no pool {w:?}", script.name))
                })
                .collect::<Result<_, _>>()?
        };
        if script.hold_secs == 0 {
            return Err(format!(
                "slot_user {}: hold_secs must be above 0",
                script.name
            ));
        }
        Ok(SlotUser {
            next_ms: epoch_ms + script.start_secs * 1000,
            script,
            prefer,
            release_at: 0,
            grant: None,
            done: false,
            grants: Vec::new(),
            refusals: Vec::new(),
            spans: Vec::new(),
            faults: Vec::new(),
        })
    }

    /// When it next acts; `None` once it has released its slot.
    pub fn next_ms(&self) -> Option<u64> {
        (!self.done).then_some(self.next_ms)
    }

    fn vm(&self) -> String {
        format!("vm-{}", self.script.name)
    }

    /// One act at `next_ms`, which the caller has set the clock to.
    pub fn act(&mut self, b: &Bench, p: &Ports) {
        let now = self.next_ms;
        let every = p.leases.renew_every_ms();
        match self.grant.take() {
            None => self.ask(b, p, now, every),
            Some(g) if now >= self.release_at => {
                self.tear_down(b, now);
                if let Err(e) = g.release(p) {
                    self.fault(format!("could not release its slot: {e:?}"));
                }
            }
            Some(g) => match g.renew(p) {
                Ok(()) => {
                    self.grant = Some(g);
                    self.next_ms = (now + every).min(self.release_at);
                }
                Err(e) => {
                    self.tear_down(b, now);
                    self.fault(format!("lost its slot: {e:?}"));
                }
            },
        }
    }

    fn ask(&mut self, b: &Bench, p: &Ports, now: u64, every: u64) {
        let name = self.script.name.clone();
        let asked = if self.script.hand {
            grant_for_hand(p, &name)
        } else {
            grant(p, &self.prefer, &name)
        };
        match asked {
            Ok(mut g) => {
                if let Err(e) = g.record_holder(p, &name, Some(&self.vm())) {
                    self.fault(format!("could not record its unit: {e:?}"));
                    return;
                }
                b.units.set(&name, UnitState::Running("active".into()));
                b.vms.set(&name, VmFact::Running { vm: self.vm() });
                let pool = g.slot().pool;
                self.spans.push(VmSpan {
                    pool,
                    from: now,
                    to: None,
                });
                self.grants.push((pool, now));
                self.release_at = now + self.script.hold_secs * 1000;
                self.next_ms = (now + every).min(self.release_at);
                self.grant = Some(g);
            }
            Err(no) => {
                let text = no.to_string();
                self.refusals.push(Refusal {
                    hand: self.script.hand,
                    named_every_holder: unreleased_holders(b).iter().all(|h| {
                        text.contains(&format!("held by {h} "))
                            || text.contains(&format!("last holder {h} "))
                    }),
                    text,
                });
                self.next_ms = now + self.script.retry_secs.map_or(every, |s| s * 1000);
            }
        }
    }

    fn tear_down(&mut self, b: &Bench, now: u64) {
        b.units
            .set(&self.script.name, UnitState::Stopped("inactive".into()));
        b.vms.set(&self.script.name, VmFact::NoVm);
        if let Some(s) = self.spans.last_mut() {
            s.to = Some(now);
        }
        self.done = true;
    }

    fn fault(&mut self, what: String) {
        self.faults
            .push(format!("slot_user {} {what}", self.script.name));
        self.done = true;
    }
}

/// The holder of every slot record not given back.
fn unreleased_holders(b: &Bench) -> Vec<String> {
    let (items, _) = b
        .store
        .list(&["slots/"], &Selector::Everything)
        .expect("the in-memory store lists");
    items
        .iter()
        .filter_map(|i| Record::of(&i.value.data))
        .filter(|r| !r.released)
        .map(|r| r.holder)
        .collect()
}

/// The most VMs running at once across `spans`. A VM that stops at `t` is not beside one that
/// starts at `t`.
pub fn most_at_once<'a>(spans: impl Iterator<Item = &'a VmSpan>) -> u64 {
    let mut edges: Vec<(u64, i64)> = spans
        .flat_map(|s| std::iter::once((s.from, 1)).chain(s.to.map(|t| (t, -1))))
        .collect();
    edges.sort();
    let (mut live, mut most) = (0i64, 0i64);
    for (_, d) in edges {
        live += d;
        most = most.max(live);
    }
    most as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(pool: Pool, from: u64, to: Option<u64>) -> VmSpan {
        VmSpan { pool, from, to }
    }

    #[test]
    fn overlapping_spans_count_and_a_hand_off_at_one_instant_does_not() {
        let s = [
            span(Pool::Proxmox, 0, Some(10)),
            span(Pool::Proxmox, 10, Some(20)),
            span(Pool::Ec2, 5, None),
        ];
        assert_eq!(most_at_once(s.iter()), 2);
        assert_eq!(
            most_at_once(s.iter().filter(|s| s.pool == Pool::Proxmox)),
            1
        );
        assert_eq!(most_at_once(std::iter::empty()), 0);
    }

    #[test]
    fn a_user_naming_no_pool_or_holding_for_nothing_is_named() {
        let script = |prefer: &[&str], hold| SlotUserScript {
            name: "u".into(),
            hand: false,
            prefer: prefer.iter().map(|s| s.to_string()).collect(),
            start_secs: 0,
            hold_secs: hold,
            retry_secs: None,
        };
        assert_eq!(
            SlotUser::new(script(&["gcp"], 1), 0).err().unwrap(),
            "slot_user u: no pool \"gcp\""
        );
        assert!(SlotUser::new(script(&[], 0), 0)
            .err()
            .unwrap()
            .contains("hold_secs must be above 0"));
        assert_eq!(
            SlotUser::new(script(&[], 1), 0).unwrap().prefer,
            Pool::ALL.to_vec()
        );
    }
}
