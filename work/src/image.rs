//! The submit-time proof that a branch changing the round image recipe still builds the image.
//! The Round AMI workflow is dispatched on a per-candidate branch of the tip and its verdict
//! read back; the workflow itself skips a recipe that already has an image. Every I/O is a port.

pub const RECIPE_PATHS: [&str; 11] = [
    "round-vm/ami-setup.sh",
    "round-vm/src/template.rs",
    "round-vm/src/template.sh",
    "round-vm/src/recipe.rs",
    "round-vm/ssm-commands.jq",
    ".github/workflows/round-ami-build.yml",
    "round-vm/wait-image.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
];

pub const POLL_SECS: u64 = 60;
pub const IDLE_POLLS: u32 = 3;
const LOG_TAIL: usize = 20;

pub fn touches_recipe(changed: &[String]) -> bool {
    changed.iter().any(|p| RECIPE_PATHS.contains(&p.as_str()))
}

/// `nonce` keeps a retry at the same tip from reading the previous attempt's run back.
pub fn candidate_branch(bead: &str, tip: &str, nonce: u64) -> String {
    format!("spira-round-candidate/{bead}-{}-{nonce}", &tip[..tip.len().min(12)])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Build {
    NotStarted,
    Running(String),
    Success(String),
    /// The run id and each `<job> / <step>` that failed.
    Failure(String, Vec<String>),
}

impl Build {
    /// `forge ami-result`'s answer; `None` for `?` or anything unrecognised.
    pub fn parse(text: &str) -> Option<Build> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let head = lines.next()?;
        let (word, id) = head.split_once(' ').unwrap_or((head, ""));
        match word {
            "none" => Some(Build::NotStarted),
            "running" => Some(Build::Running(id.into())),
            "success" => Some(Build::Success(id.into())),
            "failure" => Some(Build::Failure(id.into(), lines.filter_map(|l| l.strip_prefix("failed-step: ").map(String::from)).collect())),
            _ => None,
        }
    }
}

pub trait Ports {
    fn changed_paths(&self) -> Option<Vec<String>>;
    fn push(&self, branch: &str) -> Result<(), String>;
    fn delete(&self, branch: &str);
    fn dispatch(&self, branch: &str) -> Result<(), String>;
    /// `Err` names what could not be read: the route and the forge's own answer.
    fn result(&self, branch: &str) -> Result<Build, String>;
    fn log_tail(&self, run: &str, lines: usize) -> Vec<String>;
    fn now(&self) -> u64;
    fn sleep(&self, secs: u64);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Skipped,
    Built(String),
}

/// `Ok` when the diff leaves the recipe alone or its image build succeeded; `Err` is the
/// refusal text. An unreadable diff or forge and a build that never finishes all refuse.
pub fn gate(p: &dyn Ports, bead: &str, tip: &str, nonce: u64, wait_secs: u64) -> Result<Verdict, String> {
    let changed = p.changed_paths().ok_or("cannot read the branch's diff against the landing ref")?;
    if !touches_recipe(&changed) {
        return Ok(Verdict::Skipped);
    }
    let branch = candidate_branch(bead, tip, nonce);
    let began = p.now();
    let (mut pushed, mut dispatched, mut idle) = (false, false, 0u32);
    let out = loop {
        match p.result(&branch) {
            Err(e) => break Err(format!("the forge cannot be read for the round image build: {e}")),
            Ok(Build::Success(run)) => break Ok(Verdict::Built(run)),
            Ok(Build::Failure(run, steps)) => {
                let step = steps.first().map(String::as_str).unwrap_or("<unknown step>");
                let mut msg = format!("the round image build {run} failed at step {step}");
                for l in p.log_tail(&run, LOG_TAIL) {
                    msg.push_str("\n  ");
                    msg.push_str(&l);
                }
                break Err(msg);
            }
            Ok(Build::Running(_)) => idle = 0,
            Ok(Build::NotStarted) if !dispatched => {
                let started = p.push(&branch).map(|()| pushed = true).and_then(|()| p.dispatch(&branch));
                if let Err(e) = started {
                    break Err(format!("cannot start the round image build on {branch}: {e}"));
                }
                dispatched = true;
            }
            Ok(Build::NotStarted) => {
                idle += 1;
                if idle >= IDLE_POLLS {
                    break Err(format!("the round image build dispatched on {branch} never started"));
                }
            }
        }
        if p.now().saturating_sub(began) >= wait_secs {
            break Err(format!("the round image build on {branch} did not finish within {wait_secs}s"));
        }
        p.sleep(POLL_SECS);
    };
    if pushed {
        p.delete(&branch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Fake {
        changed: Option<Vec<String>>,
        results: RefCell<Vec<Option<Build>>>,
        clock: Cell<u64>,
        calls: RefCell<Vec<String>>,
    }

    fn fake(changed: &[&str], results: Vec<Option<Build>>) -> Fake {
        Fake {
            changed: Some(changed.iter().map(|s| s.to_string()).collect()),
            results: RefCell::new(results),
            clock: Cell::new(0),
            calls: RefCell::new(vec![]),
        }
    }

    impl Ports for Fake {
        fn changed_paths(&self) -> Option<Vec<String>> {
            self.changed.clone()
        }
        fn push(&self, b: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("push {b}"));
            Ok(())
        }
        fn delete(&self, b: &str) {
            self.calls.borrow_mut().push(format!("delete {b}"));
        }
        fn dispatch(&self, b: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("dispatch {b}"));
            Ok(())
        }
        fn result(&self, _: &str) -> Result<Build, String> {
            let mut r = self.results.borrow_mut();
            let next = if r.len() > 1 { r.remove(0) } else { r[0].clone() };
            next.ok_or_else(|| "route stub: the forge answered `?`".to_string())
        }
        fn log_tail(&self, _: &str, _: usize) -> Vec<String> {
            vec!["ami-setup: ROUND_USER must name the unprivileged user rounds run as".into()]
        }
        fn now(&self) -> u64 {
            self.clock.get()
        }
        fn sleep(&self, s: u64) {
            self.clock.set(self.clock.get() + s);
        }
    }

    #[test]
    fn a_branch_changing_ami_setup_dispatches_the_image_build_and_a_failure_refuses_naming_the_step() {
        let f = fake(
            &["round-vm/ami-setup.sh", "README.md"],
            vec![Some(Build::NotStarted), Some(Build::Running("7".into())), Some(Build::Failure("7".into(), vec!["build / Build, image and tag".into()]))],
        );
        let e = gate(&f, "sp-x", "0123456789abcdef", 7, 5400).unwrap_err();
        assert!(e.contains("build / Build, image and tag"), "{e}");
        assert!(e.contains("ROUND_USER must name the unprivileged user"), "{e}");
        let calls = f.calls.borrow().join(";");
        assert_eq!(
            calls,
            "push spira-round-candidate/sp-x-0123456789ab-7;dispatch spira-round-candidate/sp-x-0123456789ab-7;delete spira-round-candidate/sp-x-0123456789ab-7"
        );
    }

    #[test]
    fn a_successful_image_build_lets_the_submit_proceed() {
        let f = fake(&["round-vm/ssm-commands.jq"], vec![Some(Build::NotStarted), Some(Build::Success("8".into()))]);
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Built("8".into())));
    }

    #[test]
    fn an_unrelated_branch_neither_pushes_nor_dispatches() {
        let f = fake(&["work/src/main.rs", "round-vm/src/cli.rs"], vec![None]);
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Skipped));
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn an_unreadable_forge_refuses_rather_than_passes() {
        let f = fake(&["round-vm/ami-setup.sh"], vec![None]);
        assert!(gate(&f, "sp-x", "abc", 1, 5400).unwrap_err().contains("forge cannot be read for the round image build: route stub: the forge answered `?`"));
    }

    #[test]
    fn a_build_that_never_finishes_refuses_at_the_wait_bound() {
        let f = fake(&[".github/workflows/round-ami-build.yml"], vec![Some(Build::Running("9".into()))]);
        assert!(gate(&f, "sp-x", "abc", 1, 180).unwrap_err().contains("did not finish within 180s"));
    }

    #[test]
    fn an_unreadable_diff_refuses() {
        let mut f = fake(&[], vec![None]);
        f.changed = None;
        assert!(gate(&f, "sp-x", "abc", 1, 1).is_err());
    }

    #[test]
    fn every_path_the_workflow_builds_on_is_a_recipe_path() {
        let workflow = include_str!("../../.github/workflows/round-ami-build.yml");
        let listed: Vec<&str> = workflow.lines().skip_while(|l| l.trim() != "paths:").skip(1).map_while(|l| l.trim().strip_prefix("- ")).collect();
        assert!(!listed.is_empty(), "the matcher found no paths in the workflow");
        for p in listed {
            assert!(touches_recipe(&[p.to_string()]), "{p} triggers the image build but not this gate");
        }
        for p in ["round-vm/src/template.rs", "round-vm/ssm-commands.jq"] {
            assert!(touches_recipe(&[p.to_string()]), "{p}");
        }
    }

    #[test]
    fn a_dispatch_that_never_starts_refuses_and_removes_its_branch() {
        let f = fake(&["flake.lock"], vec![Some(Build::NotStarted)]);
        assert!(gate(&f, "sp-x", "abc", 1, 5400).unwrap_err().contains("never started"));
        assert_eq!(f.calls.borrow().last().map(String::as_str), Some("delete spira-round-candidate/sp-x-abc-1"));
    }

    #[test]
    fn a_timeout_removes_the_pushed_branch() {
        let f = fake(&["flake.lock"], vec![Some(Build::NotStarted), Some(Build::Running("9".into()))]);
        assert!(gate(&f, "sp-x", "abc", 1, 120).is_err());
        assert!(f.calls.borrow().iter().any(|c| c.starts_with("delete ")));
    }

    #[test]
    fn forge_answers_parse_and_an_unreadable_one_does_not() {
        assert_eq!(Build::parse("failure 9\nfailed-step: build / x\n"), Some(Build::Failure("9".into(), vec!["build / x".into()])));
        assert_eq!(Build::parse("?\n"), None);
    }
}
