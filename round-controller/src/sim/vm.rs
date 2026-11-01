//! A fake round VM: a scripted verdict per pass identity, taking virtual time scaled by the
//! profile. A pass with no script, with no free runner slot or over its cap is a fault nobody
//! scripted, which the scenario's invariants refuse.

use std::sync::{Arc, Mutex};

use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::ports::{Certified, Pending, Verb, VerbRefusal};
use crate::sim::scenario::Profile;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum VmVerdict {
    Green,
    Red,
    Fault,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmScript {
    pub identity: String,
    pub verdict: VmVerdict,
    #[serde(default)]
    pub suites: Vec<String>,
    pub duration_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmRun {
    pub identity: String,
    pub started: u64,
    pub ends: u64,
    pub result: Result<Certified, VerbRefusal>,
    /// Its verdict was read by a poll at or after `ends`.
    pub collected: bool,
}

pub struct FakeVm {
    scripts: Vec<VmScript>,
    profile: Profile,
    clock: VirtualClock,
    runs: Mutex<Vec<VmRun>>,
    faults: Mutex<Vec<String>>,
}

fn refusal(text: String) -> VerbRefusal {
    VerbRefusal {
        verb: Verb::Certify,
        code: None,
        text,
    }
}

impl FakeVm {
    pub fn new(scripts: Vec<VmScript>, profile: Profile, clock: VirtualClock) -> Arc<FakeVm> {
        Arc::new(FakeVm {
            scripts,
            profile,
            clock,
            runs: Mutex::default(),
            faults: Mutex::default(),
        })
    }

    pub fn start(self: &Arc<Self>, identity: &str) -> Box<dyn Pending<Certified>> {
        let now = self.clock.now_ms();
        let mut runs = crate::lock(&self.runs);
        let mut faults = crate::lock(&self.faults);
        let live = runs.iter().filter(|r| r.ends > now).count();
        let (ms, result) = match self.scripts.iter().find(|s| s.identity == identity) {
            None => {
                faults.push(format!("unscripted pass {identity}"));
                (0, Err(refusal(format!("no script for pass {identity}"))))
            }
            Some(s) => {
                let ms =
                    (s.duration_secs as f64 * self.profile.duration_factor * 1000.0).round() as u64;
                let cap = self.profile.pass_cap_secs * 1000;
                if live >= self.profile.runner_slots {
                    faults.push(format!(
                        "pass {identity} started with {live} of {} runner slot(s) in use",
                        self.profile.runner_slots
                    ));
                }
                match s.verdict {
                    _ if ms > cap => {
                        faults.push(format!("pass {identity} over its cap: {ms} ms > {cap} ms"));
                        (
                            cap,
                            Err(refusal(format!("pass {identity} over its {cap} ms cap"))),
                        )
                    }
                    VmVerdict::Green => (ms, Ok(Certified::Green)),
                    VmVerdict::Red => (
                        ms,
                        Ok(Certified::Red {
                            suites: s.suites.clone(),
                        }),
                    ),
                    VmVerdict::Fault => (
                        ms,
                        Err(VerbRefusal {
                            verb: Verb::Certify,
                            code: Some(2),
                            text: format!("scripted fault in pass {identity}"),
                        }),
                    ),
                }
            }
        };
        runs.push(VmRun {
            identity: identity.into(),
            started: now,
            ends: now + ms,
            result,
            collected: false,
        });
        Box::new(Handle {
            vm: self.clone(),
            i: runs.len() - 1,
        })
    }

    pub fn runs(&self) -> Vec<VmRun> {
        crate::lock(&self.runs).clone()
    }

    pub fn faults(&self) -> Vec<String> {
        crate::lock(&self.faults).clone()
    }
}

struct Handle {
    vm: Arc<FakeVm>,
    i: usize,
}

impl Pending<Certified> for Handle {
    fn poll(&self) -> Option<Result<Certified, VerbRefusal>> {
        let mut runs = crate::lock(&self.vm.runs);
        let r = &mut runs[self.i];
        (self.vm.clock.now_ms() >= r.ends).then(|| {
            r.collected = true;
            r.result.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(slots: usize, factor: f64, cap: u64) -> Profile {
        Profile {
            name: "p".into(),
            runner_slots: slots,
            vm_vcpus: 2,
            vm_mem_mib: 4096,
            duration_factor: factor,
            pass_cap_secs: cap,
            note: String::new(),
        }
    }

    fn script(identity: &str, verdict: VmVerdict, secs: u64) -> VmScript {
        VmScript {
            identity: identity.into(),
            verdict,
            suites: vec!["test-a.sh".into()],
            duration_secs: secs,
        }
    }

    #[test]
    fn a_scripted_pass_answers_once_its_scaled_virtual_duration_has_passed() {
        let clock = VirtualClock::new(1000);
        let vm = FakeVm::new(
            vec![script("h1", VmVerdict::Green, 10)],
            profile(1, 2.0, 60),
            clock.clone(),
        );
        let h = vm.start("h1");
        clock.advance(19_999);
        assert_eq!(h.poll(), None);
        assert!(!vm.runs()[0].collected);
        clock.advance(1);
        assert_eq!(h.poll(), Some(Ok(Certified::Green)));
        assert_eq!(
            vm.runs()[0],
            VmRun {
                identity: "h1".into(),
                started: 1000,
                ends: 21_000,
                result: Ok(Certified::Green),
                collected: true
            }
        );
        assert!(vm.faults().is_empty());
    }

    #[test]
    fn red_names_its_suites_and_a_scripted_fault_is_a_refusal_but_no_unscripted_fault() {
        let clock = VirtualClock::new(0);
        let vm = FakeVm::new(
            vec![
                script("r", VmVerdict::Red, 0),
                script("f", VmVerdict::Fault, 0),
            ],
            profile(2, 1.0, 60),
            clock,
        );
        assert_eq!(
            vm.start("r").poll(),
            Some(Ok(Certified::Red {
                suites: vec!["test-a.sh".into()]
            }))
        );
        assert_eq!(vm.start("f").poll().unwrap().unwrap_err().code, Some(2));
        assert!(vm.faults().is_empty());
    }

    #[test]
    fn an_unscripted_pass_a_pass_with_no_free_slot_and_an_over_cap_pass_are_unscripted_faults() {
        let clock = VirtualClock::new(0);
        let vm = FakeVm::new(
            vec![
                script("long", VmVerdict::Green, 100),
                script("h2", VmVerdict::Green, 1),
            ],
            profile(1, 1.0, 60),
            clock.clone(),
        );
        assert!(vm.start("ghost").poll().unwrap().is_err());
        let long = vm.start("long");
        vm.start("h2");
        let f = vm.faults();
        assert_eq!(f.len(), 3, "{f:?}");
        assert!(
            f[0].contains("unscripted pass ghost")
                && f[1].contains("over its cap")
                && f[2].contains("1 of 1 runner slot"),
            "{f:?}"
        );
        clock.advance(60_000);
        assert!(long
            .poll()
            .unwrap()
            .unwrap_err()
            .text
            .contains("over its 60000 ms cap"));
    }
}
