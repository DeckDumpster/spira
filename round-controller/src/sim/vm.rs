//! A fake round VM: a scripted verdict per pass identity, taking virtual time scaled by the
//! profile, with progress marks and a teardown on TERM. A pass with no script, with no free
//! runner slot or over its cap is a fault nobody scripted, which the scenario's invariants refuse.

use std::sync::{Arc, Mutex};

use runtime::ports::{Clock, VirtualClock};
use serde::Deserialize;

use crate::ports::{fields, CertifyEnd, Child, Mark, Signal, Verb, VerbRefusal};
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
    /// certify's `fault=` reason, for a fault.
    #[serde(default)]
    pub fault: Option<String>,
    /// round-vm's phases, in order; the last is the one the pass is in until it ends.
    #[serde(default = "phases")]
    pub phases: Vec<String>,
    /// round-vm marks progress this often from the start; every poll is fresh when unset…
    #[serde(default)]
    pub marks_every_secs: Option<u64>,
    /// …until this many seconds after the start: a hung child.
    #[serde(default)]
    pub marks_stop_secs: Option<u64>,
    /// How long the VM takes to tear down after TERM.
    #[serde(default)]
    pub teardown_secs: u64,
    /// round-vm ignores TERM, so only KILL ends it.
    #[serde(default)]
    pub ignores_term: bool,
    /// The VM round-vm names once the pass is past its boot.
    #[serde(default)]
    pub vm: Option<String>,
}

impl VmScript {
    /// A pass that answers `verdict` after `secs`, naming `test-a.sh` when red.
    pub fn of(identity: &str, verdict: VmVerdict, secs: u64) -> VmScript {
        VmScript {
            identity: identity.into(),
            verdict,
            suites: vec!["test-a.sh".into()],
            duration_secs: secs,
            fault: None,
            phases: phases(),
            marks_every_secs: None,
            marks_stop_secs: None,
            teardown_secs: 0,
            ignores_term: false,
            vm: None,
        }
    }
}

fn phases() -> Vec<String> {
    ["vm", "build", "suites"].map(String::from).to_vec()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmRun {
    pub identity: String,
    pub started: u64,
    pub ends: u64,
    pub result: Result<CertifyEnd, VerbRefusal>,
    /// Its verdict was read by a poll at or after `ends`.
    pub collected: bool,
    /// When a signal tore it down before it ended.
    pub torn_down_at: Option<u64>,
    pub signals: Vec<(u64, Signal)>,
}

impl VmRun {
    /// When the VM was gone: torn down, or ended.
    pub fn gone_at(&self) -> u64 {
        self.torn_down_at.map_or(self.ends, |t| t.min(self.ends))
    }

    /// It was torn down before it could answer.
    pub fn cut_short(&self) -> bool {
        self.torn_down_at.is_some_and(|t| t < self.ends)
    }
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

fn answer(s: &VmScript) -> CertifyEnd {
    let (exit, out) = match s.verdict {
        VmVerdict::Green => (0, String::new()),
        VmVerdict::Red => (1, format!("red={}\n", s.suites.join(","))),
        VmVerdict::Fault => (
            crate::judge::FAULT,
            s.fault
                .as_ref()
                .map(|f| format!("fault={f}\n"))
                .unwrap_or_default(),
        ),
    };
    CertifyEnd {
        exit,
        fields: fields(&out),
        output: String::new(),
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

    pub fn start(self: &Arc<Self>, identity: &str) -> Box<dyn Child> {
        let now = self.clock.now_ms();
        let mut runs = crate::lock(&self.runs);
        let mut faults = crate::lock(&self.faults);
        let live = runs.iter().filter(|r| r.gone_at() > now).count();
        let script = self.scripts.iter().find(|s| s.identity == identity);
        let (ms, result) = match script {
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
                if ms > cap {
                    faults.push(format!("pass {identity} over its cap: {ms} ms > {cap} ms"));
                    (
                        cap,
                        Err(refusal(format!("pass {identity} over its {cap} ms cap"))),
                    )
                } else {
                    (ms, Ok(answer(s)))
                }
            }
        };
        runs.push(VmRun {
            identity: identity.into(),
            started: now,
            ends: now + ms,
            result,
            collected: false,
            torn_down_at: None,
            signals: vec![],
        });
        Box::new(Handle {
            vm: self.clone(),
            i: runs.len() - 1,
            script: script.cloned(),
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
    script: Option<VmScript>,
}

impl Child for Handle {
    fn poll(&self) -> Option<Result<CertifyEnd, VerbRefusal>> {
        let now = self.vm.clock.now_ms();
        let mut runs = crate::lock(&self.vm.runs);
        let r = &mut runs[self.i];
        if r.cut_short() && r.gone_at() <= now {
            return Some(Err(refusal("killed by a signal before it exited".into())));
        }
        (now >= r.ends).then(|| {
            r.collected = true;
            r.result.clone()
        })
    }

    fn mark(&self) -> Option<Mark> {
        let s = self.script.as_ref()?;
        let now = self.vm.clock.now_ms();
        let r = crate::lock(&self.vm.runs)[self.i].clone();
        let at = |secs: u64| r.started + secs * 1000;
        let until = s.marks_stop_secs.map_or(now, |t| now.min(at(t)));
        let at_ms = match s.marks_every_secs {
            Some(e) if e > 0 => {
                r.started + until.saturating_sub(r.started) / (e * 1000) * (e * 1000)
            }
            _ => until,
        };
        let mut phases = s.phases.clone();
        if now >= r.ends {
            phases.push("done".into());
        }
        Some(Mark {
            round: r.identity.clone(),
            phase: phases.last().cloned().unwrap_or_default(),
            vm: s.vm.clone().filter(|_| phases.len() > 1),
            phases,
            at_ms,
        })
    }

    fn signal(&self, sig: Signal) -> Result<(), String> {
        let now = self.vm.clock.now_ms();
        let mut runs = crate::lock(&self.vm.runs);
        let r = &mut runs[self.i];
        r.signals.push((now, sig));
        let (teardown, ignores) = self
            .script
            .as_ref()
            .map_or((0, false), |s| (s.teardown_secs * 1000, s.ignores_term));
        let at = match sig {
            Signal::Term if ignores => return Ok(()),
            Signal::Term => now + teardown,
            Signal::Kill => now,
        };
        r.torn_down_at = Some(r.torn_down_at.map_or(at, |t| t.min(at)));
        Ok(())
    }

    fn gone(&self) -> bool {
        crate::lock(&self.vm.runs)[self.i].gone_at() <= self.vm.clock.now_ms()
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
        VmScript::of(identity, verdict, secs)
    }

    fn green() -> CertifyEnd {
        CertifyEnd::default()
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
        assert!(!h.gone());
        assert!(!vm.runs()[0].collected);
        clock.advance(1);
        assert_eq!(h.poll(), Some(Ok(green())));
        assert!(h.gone());
        assert_eq!(
            vm.runs()[0],
            VmRun {
                identity: "h1".into(),
                started: 1000,
                ends: 21_000,
                result: Ok(green()),
                collected: true,
                torn_down_at: None,
                signals: vec![],
            }
        );
        assert!(vm.faults().is_empty());
    }

    #[test]
    fn red_names_its_suites_and_a_fault_answers_certify_s_fault_exit_with_its_reason() {
        let clock = VirtualClock::new(0);
        let mut f = script("f", VmVerdict::Fault, 0);
        f.fault = Some("fences-cap".into());
        let vm = FakeVm::new(
            vec![script("r", VmVerdict::Red, 0), f],
            profile(2, 1.0, 60),
            clock,
        );
        let r = vm.start("r").poll().unwrap().unwrap();
        assert_eq!((r.exit, r.fields["red"].as_str()), (1, "test-a.sh"));
        let f = vm.start("f").poll().unwrap().unwrap();
        assert_eq!((f.exit, f.fields["fault"].as_str()), (4, "fences-cap"));
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

    #[test]
    fn marks_step_every_interval_until_they_stop_and_name_the_vm_past_boot() {
        let clock = VirtualClock::new(0);
        let mut s = script("h", VmVerdict::Green, 100);
        s.marks_every_secs = Some(10);
        s.marks_stop_secs = Some(35);
        s.vm = Some("vm-7".into());
        let vm = FakeVm::new(vec![s], profile(1, 1.0, 600), clock.clone());
        let h = vm.start("h");
        clock.advance(25_000);
        let m = h.mark().unwrap();
        assert_eq!(
            (m.at_ms, m.phase.as_str(), m.vm.as_deref()),
            (20_000, "suites", Some("vm-7"))
        );
        clock.advance(50_000);
        assert_eq!(
            h.mark().unwrap().at_ms,
            30_000,
            "a hung child marks nothing more"
        );
        clock.advance(25_000);
        assert_eq!(
            h.mark().unwrap().phases.last().map(String::as_str),
            Some("done")
        );
    }

    #[test]
    fn term_tears_the_vm_down_after_its_teardown_kill_at_once_and_a_cut_short_pass_answers_no_verdict(
    ) {
        let clock = VirtualClock::new(0);
        let mut slow = script("slow", VmVerdict::Green, 100);
        slow.teardown_secs = 5;
        let mut deaf = script("deaf", VmVerdict::Green, 100);
        deaf.ignores_term = true;
        let vm = FakeVm::new(vec![slow, deaf], profile(2, 1.0, 600), clock.clone());
        let (a, b) = (vm.start("slow"), vm.start("deaf"));
        clock.advance(10_000);
        a.signal(Signal::Term).unwrap();
        b.signal(Signal::Term).unwrap();
        clock.advance(4_999);
        assert!(!a.gone() && a.poll().is_none());
        clock.advance(1);
        assert!(a.gone());
        assert_eq!(a.poll().unwrap().unwrap_err().code, None);
        assert!(!b.gone(), "TERM is ignored");
        b.signal(Signal::Kill).unwrap();
        assert!(b.gone());
        let runs = vm.runs();
        assert_eq!(runs[0].gone_at(), 15_000);
        assert!(runs[0].cut_short() && !runs[0].collected);
        assert_eq!(
            runs[1].signals,
            [(10_000, Signal::Term), (15_000, Signal::Kill)]
        );
    }
}
