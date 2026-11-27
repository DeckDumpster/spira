//! The submit-time proof that a branch changing the round image recipe still builds the image.
//! The Round AMI workflow is dispatched on a per-candidate branch of the tip and its verdict
//! read back; the workflow skips a recipe that already has an image unless forced. Every I/O is a port.

pub const RECIPE_PATHS: [&str; 12] = [
    "round-vm/ami-setup.sh",
    "round-vm/src/template.rs",
    "round-vm/src/template.sh",
    "round-vm/src/recipe.rs",
    "round-vm/ssm-commands.jq",
    ".github/workflows/round-ami-build.yml",
    "round-vm/wait-image.sh",
    "round-vm/wait-setup.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
];

pub const RECIPE_WORKFLOW: &str = ".github/workflows/round-ami-build.yml";

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

/// `Build::parse` of the forge's answer; a queue receipt that reached here says so, naming its request.
pub fn read_result(out: &str) -> Result<Build, String> {
    Build::parse(out).ok_or_else(|| match spira_store::lc_call::queued_id(out) {
        Some(id) => format!("`work forge ami-result` answered the queue receipt for request {id}, not its result; `spira-lc work-request show {id}` has its answer"),
        None => format!("`work forge ami-result` answered {:?}, which is not a build state", out.trim()),
    })
}

pub trait Ports {
    /// True when the tip tree carries [`RECIPE_WORKFLOW`]: only then is there an image to build.
    fn has_recipe(&self) -> bool;
    /// The repository's registered landing ref (`spira_landref`).
    fn landing_ref(&self) -> Option<String>;
    fn changed_paths(&self, base: &str) -> Option<Vec<String>>;
    /// `path` at the merge base of `base` and the tip, and at the tip; `None` for a side that cannot be read.
    fn texts(&self, base: &str, path: &str) -> (Option<String>, Option<String>);
    fn say(&self, line: &str);
    fn push(&self, branch: &str) -> Result<(), String>;
    fn delete(&self, branch: &str);
    /// `force` builds even when an image already carries the tip's recipe hash.
    fn dispatch(&self, branch: &str, force: bool) -> Result<(), String>;
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

/// `Ok` when the repository has no image recipe, the diff against its landing ref leaves the
/// recipe alone, or the image build succeeded; `Err` is the refusal text. An unresolvable landing
/// ref, an unreadable diff or forge and a build that never finishes all refuse.
pub fn gate(p: &dyn Ports, bead: &str, tip: &str, nonce: u64, wait_secs: u64) -> Result<Verdict, String> {
    if !p.has_recipe() {
        return Ok(Verdict::Skipped);
    }
    let base = p.landing_ref().ok_or("cannot resolve the repository's landing ref (spira_landref); give its registry row a base column")?;
    let changed = p.changed_paths(&base).ok_or("cannot read the branch's diff against the landing ref")?;
    let changed = spira_config::recipe::drop_same_image(changed, |path| p.texts(&base, path));
    if !touches_recipe(&changed) {
        return Ok(Verdict::Skipped);
    }
    // With no hash input changed, an image already carries this hash and an unforced run skips.
    let force = !spira_config::recipe::touches(&changed);
    let branch = candidate_branch(bead, tip, nonce);
    let began = p.now();
    let (mut pushed, mut dispatched, mut idle, mut announced) = (false, false, 0u32, false);
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
            Ok(Build::Running(run)) => {
                idle = 0;
                if !announced {
                    announced = true;
                    p.say(&format!("work submit: the round image build is running as run {run}"));
                }
            }
            Ok(Build::NotStarted) if !dispatched => {
                let started = p.push(&branch).map(|()| pushed = true).and_then(|()| p.dispatch(&branch, force));
                if let Err(e) = started {
                    break Err(format!("cannot start the round image build on {branch}: {e}"));
                }
                dispatched = true;
                p.say(&format!("work submit: {branch}: an image build of up to {} min has started", wait_secs / 60));
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

    #[test_id(tojo1g1)]
    #[test]
    fn a_queue_receipt_is_named_as_one_with_its_request_and_command() {
        let e = read_result("queued wr-14").unwrap_err();
        assert!(e.contains("queue receipt for request wr-14") && e.contains("spira-lc work-request show wr-14"), "{e}");
        assert!(read_result("garbage").unwrap_err().contains("not a build state"));
    }

    #[test_id(t1473lk)]
    #[test]
    fn read_result_agrees_with_parse_on_every_build_state() {
        for t in ["none", "running 9", "success 9", "failure 9\nfailed-step: build / ami"] {
            assert_eq!(read_result(t).ok(), Build::parse(t), "{t}");
        }
    }
    use std::cell::{Cell, RefCell};

    struct Fake {
        changed: Option<Vec<String>>,
        recipe: bool,
        landref: Option<String>,
        by_base: Vec<(String, Vec<String>)>,
        asked: RefCell<Vec<String>>,
        results: RefCell<Vec<Option<Build>>>,
        clock: Cell<u64>,
        calls: RefCell<Vec<String>>,
        events: RefCell<Vec<String>>,
        texts: Option<(Option<String>, Option<String>)>,
    }

    fn fake(changed: &[&str], results: Vec<Option<Build>>) -> Fake {
        Fake {
            changed: Some(changed.iter().map(|s| s.to_string()).collect()),
            recipe: true,
            landref: Some("local/main".into()),
            by_base: vec![],
            asked: RefCell::new(vec![]),
            results: RefCell::new(results),
            clock: Cell::new(0),
            calls: RefCell::new(vec![]),
            events: RefCell::new(vec![]),
            texts: None,
        }
    }

    impl Ports for Fake {
        fn has_recipe(&self) -> bool {
            self.recipe
        }
        fn landing_ref(&self) -> Option<String> {
            self.landref.clone()
        }
        fn changed_paths(&self, base: &str) -> Option<Vec<String>> {
            self.asked.borrow_mut().push(base.to_string());
            self.by_base.iter().find(|(b, _)| b == base).map(|(_, c)| c.clone()).or_else(|| self.changed.clone())
        }
        fn texts(&self, _: &str, _: &str) -> (Option<String>, Option<String>) {
            self.texts.clone().unwrap_or((None, None))
        }
        fn say(&self, line: &str) {
            self.events.borrow_mut().push(format!("say {line}"));
        }
        fn push(&self, b: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("push {b}"));
            Ok(())
        }
        fn delete(&self, b: &str) {
            self.calls.borrow_mut().push(format!("delete {b}"));
        }
        fn dispatch(&self, b: &str, force: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("dispatch {b}{}", if force { " force" } else { "" }));
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
            self.events.borrow_mut().push("sleep".into());
            self.clock.set(self.clock.get() + s);
        }
    }

    #[test_id(tcbn87a)]
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

    fn base_of(row: &str, name: &str) -> Option<String> {
        spira_config::repos::Registry::new(Some(row), &std::collections::BTreeMap::new(), std::path::Path::new("x")).base(name)
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(tg9lmgv)]
    #[test]
    fn a_branch_is_judged_against_its_registered_landing_ref_not_a_stale_local_main() {
        let mut f = fake(&[], vec![None]);
        f.landref = base_of("dumpstar | /x/dumpstar | queue.forge | origin/main | | | plan\n", "dumpstar");
        f.by_base = vec![
            ("origin/main".into(), paths(&["crates/a/src/decks.rs", "crates/a/src/mcp.rs", "crates/a/src/lib.rs"])),
            ("local/main".into(), paths(&["rust-toolchain.toml", "crates/a/src/decks.rs"])),
        ];
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Skipped));
        assert!(f.calls.borrow().is_empty());
        assert_eq!(*f.asked.borrow(), vec!["origin/main".to_string()]);
    }

    #[test_id(tv8pdcc)]
    #[test]
    fn a_repository_without_the_image_workflow_is_never_diffed_or_dispatched() {
        let mut f = fake(&["rust-toolchain.toml"], vec![Some(Build::NotStarted)]);
        f.recipe = false;
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Skipped));
        assert!(f.calls.borrow().is_empty());
        assert!(f.asked.borrow().is_empty());
    }

    #[test_id(t6zhxc4)]
    #[test]
    fn spira_editing_a_recipe_input_against_local_main_still_dispatches() {
        let mut f = fake(&[], vec![Some(Build::NotStarted), Some(Build::Running("7".into())), Some(Build::Failure("7".into(), vec!["build / Build, image and tag".into()]))]);
        f.landref = base_of("spira | /x/harness | queue.local | local/main | | | plan\n", "spira");
        f.by_base = vec![("local/main".into(), paths(&["round-vm/ami-setup.sh"]))];
        assert!(gate(&f, "sp-x", "0123456789abcdef", 7, 5400).is_err());
        assert_eq!(
            f.calls.borrow().join(";"),
            "push spira-round-candidate/sp-x-0123456789ab-7;dispatch spira-round-candidate/sp-x-0123456789ab-7;delete spira-round-candidate/sp-x-0123456789ab-7"
        );
    }

    const CF: &str = "spira/testenv/Containerfile";

    #[test_id(tkgn9ez)]
    #[test]
    fn a_comment_only_containerfile_edit_is_skipped_with_no_forge_call() {
        let mut f = fake(&[CF], vec![None]);
        f.texts = Some((Some("# a\nFROM x\n".into()), Some("# b\nFROM x\n".into())));
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Skipped));
        assert!(f.calls.borrow().is_empty() && f.events.borrow().is_empty());
    }

    #[test_id(tk5u3yi)]
    #[test]
    fn a_run_line_edit_dispatches_and_unreadable_text_counts_as_changed() {
        for texts in [(Some("FROM x\nRUN a\n".to_string()), Some("FROM x\nRUN b\n".to_string())), (Some("FROM x\n".to_string()), None), (None, None)] {
            let mut f = fake(&[CF], vec![Some(Build::NotStarted), Some(Build::Success("3".into()))]);
            f.texts = Some(texts);
            assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Built("3".into())));
            assert!(f.calls.borrow().iter().any(|c| c.starts_with("dispatch ")));
        }
    }

    #[test_id(tfbfxn3)]
    #[test]
    fn the_start_and_the_run_are_said_once_each_before_the_next_sleep() {
        let f = fake(&[CF], vec![Some(Build::NotStarted), Some(Build::Running("7".into())), Some(Build::Running("7".into())), Some(Build::Success("7".into()))]);
        gate(&f, "sp-x", "abc", 1, 5400).unwrap();
        let ev = f.events.borrow().clone();
        assert_eq!(
            ev,
            vec![
                "say work submit: spira-round-candidate/sp-x-abc-1: an image build of up to 90 min has started".to_string(),
                "sleep".into(),
                "say work submit: the round image build is running as run 7".into(),
                "sleep".into(),
                "sleep".into(),
            ]
        );
    }

    #[test_id(tmlcj70)]
    #[test]
    fn an_unresolvable_landing_ref_refuses_naming_it_and_asks_the_forge_nothing() {
        let mut f = fake(&["round-vm/ami-setup.sh"], vec![Some(Build::NotStarted)]);
        f.landref = None;
        let e = gate(&f, "sp-x", "abc", 1, 5400).unwrap_err();
        assert!(e.contains("landing ref"), "{e}");
        assert!(f.calls.borrow().is_empty());
    }

    #[test_id(tnuu3cu)]
    #[test]
    fn a_successful_image_build_lets_the_submit_proceed() {
        let f = fake(&["round-vm/ssm-commands.jq"], vec![Some(Build::NotStarted), Some(Build::Success("8".into()))]);
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Built("8".into())));
    }

    #[test_id(tyil6xq)]
    #[test]
    fn a_branch_changing_only_the_setup_waiter_pushes_and_forces_the_image_build() {
        assert_forced("round-vm/wait-setup.sh");
    }

    #[test_id(ty6aww0)]
    #[test]
    fn a_branch_changing_only_the_image_waiter_forces_the_image_build() {
        assert_forced("round-vm/wait-image.sh");
    }

    /// A path outside the recipe hash leaves an image already built, so an unforced run skips.
    fn assert_forced(path: &str) {
        let f = fake(&[path], vec![Some(Build::NotStarted), Some(Build::Success("8".into()))]);
        assert_eq!(gate(&f, "sp-x", "0123456789abcdef", 7, 5400), Ok(Verdict::Built("8".into())));
        let b = "spira-round-candidate/sp-x-0123456789ab-7";
        assert_eq!(f.calls.borrow().join(";"), format!("push {b};dispatch {b} force;delete {b}"));
    }

    #[test_id(tz6bi4n)]
    #[test]
    fn an_unrelated_branch_neither_pushes_nor_dispatches() {
        let f = fake(&["work/src/main.rs", "round-vm/src/cli.rs"], vec![None]);
        assert_eq!(gate(&f, "sp-x", "abc", 1, 5400), Ok(Verdict::Skipped));
        assert!(f.calls.borrow().is_empty());
    }

    #[test_id(taw1f0i)]
    #[test]
    fn an_unreadable_forge_refuses_rather_than_passes() {
        let f = fake(&["round-vm/ami-setup.sh"], vec![None]);
        assert!(gate(&f, "sp-x", "abc", 1, 5400).unwrap_err().contains("forge cannot be read for the round image build: route stub: the forge answered `?`"));
    }

    #[test_id(t2tvgth)]
    #[test]
    fn a_build_that_never_finishes_refuses_at_the_wait_bound() {
        let f = fake(&[".github/workflows/round-ami-build.yml"], vec![Some(Build::Running("9".into()))]);
        assert!(gate(&f, "sp-x", "abc", 1, 180).unwrap_err().contains("did not finish within 180s"));
    }

    #[test_id(tzor43z)]
    #[test]
    fn an_unreadable_diff_refuses() {
        let mut f = fake(&[], vec![None]);
        f.changed = None;
        assert!(gate(&f, "sp-x", "abc", 1, 1).is_err());
    }

    #[test_id(tbhf11p)]
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

    #[test_id(t206lg1)]
    #[test]
    fn a_dispatch_that_never_starts_refuses_and_removes_its_branch() {
        let f = fake(&["flake.lock"], vec![Some(Build::NotStarted)]);
        assert!(gate(&f, "sp-x", "abc", 1, 5400).unwrap_err().contains("never started"));
        assert_eq!(f.calls.borrow().last().map(String::as_str), Some("delete spira-round-candidate/sp-x-abc-1"));
    }

    #[test_id(t2vchjf)]
    #[test]
    fn a_timeout_removes_the_pushed_branch() {
        let f = fake(&["flake.lock"], vec![Some(Build::NotStarted), Some(Build::Running("9".into()))]);
        assert!(gate(&f, "sp-x", "abc", 1, 120).is_err());
        assert!(f.calls.borrow().iter().any(|c| c.starts_with("delete ")));
    }

    #[test_id(tj1f3t3)]
    #[test]
    fn forge_answers_parse_and_an_unreadable_one_does_not() {
        assert_eq!(Build::parse("failure 9\nfailed-step: build / x\n"), Some(Build::Failure("9".into(), vec!["build / x".into()])));
        assert_eq!(Build::parse("?\n"), None);
    }
}
