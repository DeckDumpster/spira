//! The launcher: a runner is the transient user unit `death::unit_name` names, started by
//! `systemd-run` and read back by `systemctl --user is-active`. The unit is handed only
//! `SPIRA_TOML` and `SPIRA_RELEASE`, and the runner resolves the rest itself
//! (law-a-binary-resolves-the-config-it-reads). Nothing sets its CPU share or priority
//! (law-leave-scheduling-to-the-os).

use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use state_store::Rev;

use crate::death::{unit_name, UnitState, Units};
use crate::lease::token_word;
use crate::ports::{Launch, Launched, Ports};
use crate::slots::SlotGrant;

const START_DEADLINE: Duration = Duration::from_secs(30); // batch-job: systemd-run without --wait returns once the unit is queued
const IS_ACTIVE_DEADLINE: Duration = Duration::from_secs(20); // batch-job: one systemctl query

/// The seam standing where `systemd-run` stands: a world's `sim-launch`.
pub const LAUNCH_SEAM: &str = "SPIRA_LAUNCH";
/// The seam standing where `systemctl` stands for the units [`LAUNCH_SEAM`] started.
pub const LAUNCH_CTL_SEAM: &str = "SPIRA_LAUNCH_CTL";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Programs {
    pub run: String,
    pub ctl: String,
}

impl Programs {
    /// Each seam's program, or the real one when the seam is unset or empty.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Programs {
        let or = |k: &str, real: &str| {
            get(k)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| real.to_string())
        };
        Programs {
            run: or(LAUNCH_SEAM, "systemd-run"),
            ctl: or(LAUNCH_CTL_SEAM, "systemctl"),
        }
    }
}

/// `systemctl --user is-active <unit>` through `ctl`.
pub fn is_active(proc_: &dyn Proc, ctl: &str, unit: &str) -> UnitState {
    // FORK-EDGE: needs Units start/facts carrying systemd-run properties; this seam is shared with the sim harness (LAUNCH_SEAM)
    let spec = ProcSpec {
        prog: ctl.into(),
        args: vec!["--user".into(), "is-active".into(), unit.into()],
        stdin: vec![],
        deadline: IS_ACTIVE_DEADLINE,
    };
    match proc_.run(&spec) {
        Ok(o) => UnitState::from_is_active(&String::from_utf8_lossy(&o.stdout)),
        Err(e) => UnitState::Unreadable(e),
    }
}

/// The Units port proof of death reads.
pub struct SystemdUnits<'a> {
    pub proc_: &'a dyn Proc,
    pub ctl: String,
}

impl Units for SystemdUnits<'_> {
    fn state(&self, unit: &str) -> UnitState {
        is_active(self.proc_, &self.ctl, unit)
    }
}

/// What a unit runs and is handed.
pub struct Systemd {
    pub programs: Programs,
    /// The `round-controller` binary the unit runs.
    pub exe: String,
    /// The `SPIRA_TOML` spec, handed on as it is.
    pub toml: String,
    pub release: Option<String>,
}

impl Systemd {
    /// `systemd-run`'s arguments: the unit options, then `round-controller run` with both tokens.
    pub fn argv(
        &self,
        unit: &str,
        repo: &str,
        batch: &str,
        lease_token: Rev,
        slot: &SlotGrant,
    ) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "--user".into(),
            format!("--unit={unit}"),
            "--collect".into(),
            "--quiet".into(),
            format!("--setenv=SPIRA_TOML={}", self.toml),
        ];
        if let Some(r) = &self.release {
            a.push(format!("--setenv={}={r}", spira_config::RELEASE_ENV));
        }
        a.extend([
            self.exe.clone(),
            "run".into(),
            "--repo".into(),
            repo.into(),
            "--batch".into(),
            batch.into(),
            "--lease-token".into(),
            token_word(lease_token),
            "--slot".into(),
            slot.slot().to_string(),
            "--slot-token".into(),
            token_word(slot.token()),
        ]);
        a
    }
}

/// Starts the runner of `batch` under `lease_token` and `slot`. A non-zero `systemd-run` is a
/// failure, with its text whole, unless the unit is running under the name anyway.
pub fn start(
    p: &Ports,
    l: &Systemd,
    repo: &str,
    batch: &str,
    lease_token: Rev,
    slot: &SlotGrant,
) -> Launched {
    let unit = match unit_name(batch, lease_token) {
        Ok(u) => u,
        Err(text) => return Launched::Failed { text },
    };
    // FORK-EDGE: needs Units start/facts carrying systemd-run properties; this seam is shared with the sim harness (LAUNCH_SEAM)
    let spec = ProcSpec {
        prog: l.programs.run.clone(),
        args: l.argv(&unit, repo, batch, lease_token, slot),
        stdin: vec![],
        deadline: START_DEADLINE,
    };
    let text = match p.proc_.run(&spec) {
        Ok(o) if o.code == Some(0) => return Launched::Started { unit },
        Ok(o) => match (o.stderr.is_empty(), o.stdout.is_empty()) {
            (false, _) => String::from_utf8_lossy(&o.stderr).into_owned(),
            (true, false) => String::from_utf8_lossy(&o.stdout).into_owned(),
            (true, true) => format!("{} exited {:?} and said nothing", spec.prog, o.code),
        },
        Err(e) => e,
    };
    match is_active(p.proc_, &l.programs.ctl, &unit) {
        UnitState::Running(_) => Launched::Running { unit },
        _ => Launched::Failed { text },
    }
}

impl Launch for Systemd {
    fn start(
        &self,
        p: &Ports,
        repo: &str,
        batch: &str,
        lease_token: Rev,
        slot: &SlotGrant,
    ) -> Launched {
        start(p, self, repo, batch, lease_token, slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::bench::Bench;
    use crate::slots::{Pool, Slot};
    use runtime::ports::ProcOutput;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Answers each call in turn and keeps every call's program and arguments.
    #[derive(Default)]
    struct Script {
        calls: Mutex<Vec<Vec<String>>>,
        answers: Mutex<VecDeque<Result<ProcOutput, String>>>,
    }

    impl Script {
        fn new(answers: Vec<Result<ProcOutput, String>>) -> Script {
            Script {
                calls: Mutex::default(),
                answers: Mutex::new(answers.into()),
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            crate::lock(&self.calls).clone()
        }
    }

    impl Proc for Script {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            let mut c = vec![spec.prog.clone()];
            c.extend(spec.args.iter().cloned());
            crate::lock(&self.calls).push(c);
            crate::lock(&self.answers)
                .pop_front()
                .unwrap_or_else(|| Err("unscripted call".into()))
        }
    }

    fn out(code: i32, stdout: &str, stderr: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn launcher() -> Systemd {
        Systemd {
            programs: Programs {
                run: "/seam/launch".into(),
                ctl: "/seam/ctl".into(),
            },
            exe: "/rel/bin/round-controller".into(),
            toml: "/cfg/base.toml:/cfg/over.toml".into(),
            release: Some("/rel".into()),
        }
    }

    fn slot() -> SlotGrant {
        SlotGrant::handed(
            &Bench::new().ports(),
            Slot {
                pool: Pool::Ec2,
                n: 2,
            },
            Rev::new(11, 3),
        )
    }

    fn started(proc_: &Script, l: &Systemd) -> Launched {
        let b = Bench::new();
        let p = Ports {
            proc_,
            ..b.ports()
        };
        start(&p, l, "r", "b1", Rev::new(9, 0), &slot())
    }

    #[test]
    fn a_runner_starts_as_its_token_named_unit_with_the_exact_argv() {
        let s = Script::new(vec![out(0, "", "")]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Started {
                unit: "round-controller-run-b1-9-0".into()
            }
        );
        assert_eq!(
            s.calls(),
            [[
                "/seam/launch",
                "--user",
                "--unit=round-controller-run-b1-9-0",
                "--collect",
                "--quiet",
                "--setenv=SPIRA_TOML=/cfg/base.toml:/cfg/over.toml",
                "--setenv=SPIRA_RELEASE=/rel",
                "/rel/bin/round-controller",
                "run",
                "--repo",
                "r",
                "--batch",
                "b1",
                "--lease-token",
                "9-0",
                "--slot",
                "ec2/2",
                "--slot-token",
                "11-3",
            ]
            .map(String::from)]
        );
        let s = Script::new(vec![out(0, "", "")]);
        let bare = Systemd {
            release: None,
            ..launcher()
        };
        started(&s, &bare);
        assert!(!s.calls()[0].iter().any(|a| a.contains("SPIRA_RELEASE")));
    }

    #[test]
    fn a_non_zero_systemd_run_is_failed_with_its_text_whole_unless_the_unit_runs() {
        let text = "Failed to start transient service unit: Unit round-controller-run-b1-9-0.service already exists.\n  detail\n";
        let s = Script::new(vec![out(1, "", text), out(3, "inactive\n", "")]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Failed { text: text.into() }
        );
        assert_eq!(
            s.calls()[1],
            [
                "/seam/ctl",
                "--user",
                "is-active",
                "round-controller-run-b1-9-0"
            ]
        );
        let s = Script::new(vec![out(1, "said on stdout", ""), out(3, "failed\n", "")]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Failed {
                text: "said on stdout".into()
            }
        );
        let s = Script::new(vec![out(1, "", ""), out(3, "unknown\n", "")]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Failed {
                text: "/seam/launch exited Some(1) and said nothing".into()
            }
        );
        let s = Script::new(vec![Err("no such program".into()), Err("no ctl".into())]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Failed {
                text: "no such program".into()
            }
        );
        let s = Script::new(vec![out(1, "", text), out(0, "activating\n", "")]);
        assert_eq!(
            started(&s, &launcher()),
            Launched::Running {
                unit: "round-controller-run-b1-9-0".into()
            }
        );
    }

    #[test]
    fn a_batch_that_cannot_name_a_unit_launches_nothing() {
        let s = Script::default();
        let b = Bench::new();
        let p = Ports {
            proc_: &s,
            ..b.ports()
        };
        let got = start(&p, &launcher(), "r", "-b", Rev::new(9, 0), &slot());
        assert!(
            matches!(&got, Launched::Failed { text } if text.contains("cannot name a unit")),
            "{got:?}"
        );
        assert!(s.calls().is_empty());
    }

    #[test]
    fn is_active_maps_each_systemctl_answer_and_an_unspawnable_ctl_is_unreadable() {
        let cases = [
            ("active\n", 0, UnitState::Running("active".into())),
            ("activating\n", 0, UnitState::Running("activating".into())),
            ("reloading\n", 0, UnitState::Running("reloading".into())),
            (
                "deactivating\n",
                0,
                UnitState::Running("deactivating".into()),
            ),
            ("inactive\n", 3, UnitState::Stopped("inactive".into())),
            ("failed\n", 3, UnitState::Stopped("failed".into())),
            ("unknown\n", 4, UnitState::Stopped("unknown".into())),
            (
                "",
                1,
                UnitState::Unreadable("is-active printed no state".into()),
            ),
        ];
        for (said, code, want) in cases {
            let s = Script::new(vec![out(code, said, "")]);
            assert_eq!(is_active(&s, "/seam/ctl", "u-1"), want, "{said:?}");
            assert_eq!(s.calls(), [["/seam/ctl", "--user", "is-active", "u-1"]]);
        }
        let s = Script::new(vec![Err("no systemctl".into())]);
        let units = SystemdUnits {
            proc_: &s,
            ctl: "/seam/ctl".into(),
        };
        assert_eq!(
            units.state("u"),
            UnitState::Unreadable("no systemctl".into())
        );
    }

    #[test]
    fn each_seam_names_its_program_and_unset_or_empty_is_the_real_one() {
        let set = |k: &str| match k {
            LAUNCH_SEAM => Some("/w/bin/sim-launch".to_string()),
            LAUNCH_CTL_SEAM => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            Programs::from_env(&set),
            Programs {
                run: "/w/bin/sim-launch".into(),
                ctl: "systemctl".into()
            }
        );
        assert_eq!(
            Programs::from_env(&|_| None),
            Programs {
                run: "systemd-run".into(),
                ctl: "systemctl".into()
            }
        );
    }

    #[test]
    fn the_port_starts_through_the_same_launcher() {
        let s = Script::new(vec![out(0, "", "")]);
        let b = Bench::new();
        let p = Ports {
            proc_: &s,
            ..b.ports()
        };
        let l: &dyn Launch = &launcher();
        assert_eq!(
            l.start(&p, "r", "b1", Rev::new(9, 0), &slot()),
            Launched::Started {
                unit: "round-controller-run-b1-9-0".into()
            }
        );
    }
}
