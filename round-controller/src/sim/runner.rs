//! A scripted runner holding a round's lease on the virtual clock: it renews through the real
//! library, gated on its child's progress marks, every renewal interval. When the gate shuts or a
//! renewal is refused it tears down, so its unit stops and its VM is gone. A hung runner stops
//! acting at once and stays alive.

use serde::Deserialize;

use crate::death::{unit_name, UnitState, VmFact};
use crate::lease::{acquire, Lease, Resource};
use crate::liveness::Phase;
use crate::ports::Ports;
use crate::sim::bench::Bench;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerScript {
    pub repo: String,
    pub batch: String,
    pub phase: String,
    /// Seconds after the epoch the runner takes its lease and its pass starts.
    pub start_secs: u64,
    /// The child marks progress this often from the start…
    pub marks_every_secs: u64,
    /// …until this many seconds after the epoch: a hung child.
    #[serde(default)]
    pub marks_stop_secs: Option<u64>,
    /// The runner itself hangs this many seconds after the epoch, renewing nothing more.
    #[serde(default)]
    pub hangs_secs: Option<u64>,
}

pub struct Runner {
    script: RunnerScript,
    phase: Phase,
    epoch_ms: u64,
    next_ms: u64,
    lease: Option<Lease>,
    unit: Option<String>,
    done: bool,
    pub renewals: u64,
    pub faults: Vec<String>,
}

impl Runner {
    pub fn new(script: RunnerScript, epoch_ms: u64) -> Result<Runner, String> {
        let phase = Phase::from_word(&script.phase)
            .ok_or_else(|| format!("runner {}: no phase {:?}", script.batch, script.phase))?;
        if script.marks_every_secs == 0 {
            return Err(format!(
                "runner {}: marks_every_secs must be above 0",
                script.batch
            ));
        }
        Ok(Runner {
            next_ms: epoch_ms + script.start_secs * 1000,
            script,
            phase,
            epoch_ms,
            lease: None,
            unit: None,
            done: false,
            renewals: 0,
            faults: Vec::new(),
        })
    }

    /// When it next acts; `None` once it has torn down or hung.
    pub fn next_ms(&self) -> Option<u64> {
        (!self.done).then_some(self.next_ms)
    }

    fn at(&self, secs: u64) -> u64 {
        self.epoch_ms + secs * 1000
    }

    /// One act at `next_ms`, which the caller has set the clock to.
    pub fn act(&mut self, b: &Bench, p: &Ports) {
        let now = self.next_ms;
        self.next_ms += p.leases.renew_every_ms();
        if self.script.hangs_secs.is_some_and(|h| now >= self.at(h)) {
            self.done = true;
            return;
        }
        let Some(lease) = &self.lease else {
            let res = Resource::round(&self.script.repo, &self.script.batch);
            match acquire(p, &res, "round-controller-run") {
                Ok(l) => {
                    let unit = unit_name(&self.script.batch, l.token())
                        .expect("a scripted runner's batch names a unit");
                    b.units.set(&unit, UnitState::Running("active".into()));
                    b.vms.set(
                        &unit,
                        VmFact::Running {
                            vm: format!("vm-{unit}"),
                        },
                    );
                    self.unit = Some(unit);
                    self.lease = Some(l);
                }
                Err(e) => {
                    self.faults.push(format!(
                        "runner {} could not take its lease: {e:?}",
                        self.script.batch
                    ));
                    self.done = true;
                }
            }
            return;
        };
        let start = self.at(self.script.start_secs);
        let marks_until = self
            .script
            .marks_stop_secs
            .map_or(now, |s| now.min(self.at(s)));
        let every = self.script.marks_every_secs * 1000;
        let last_mark = start + marks_until.saturating_sub(start) / every * every;
        if p.leases.gate().may_renew(now, last_mark, self.phase, start) && lease.renew(p).is_ok() {
            self.renewals += 1;
            return;
        }
        let unit = self.unit.take().unwrap_or_default();
        b.units.set(&unit, UnitState::Stopped("inactive".into()));
        b.vms.set(&unit, VmFact::NoVm);
        self.done = true;
    }
}
