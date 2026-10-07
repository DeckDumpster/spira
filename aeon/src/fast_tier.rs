//! The in-session fast tier: the checks the landing gate runs that need no host-wide
//! admission. The teardown runs it before a handoff; `aeon fast-tier` runs the same code so
//! acceptance can exercise it on a release without a model session.

use std::path::Path;

use crate::ports::{Exec, Git};

/// The fast tier's commands, in order. spira-lint judges only the branch's own diff, so a
/// finding already on the base never reds a bead; the round lints the whole tree. The build
/// fence gets the base through SPIRA_GATE_BASE.
pub fn steps(base_fq: &str) -> Vec<(&'static str, Vec<String>)> {
    let base = format!("SPIRA_GATE_BASE={base_fq}");
    vec![
        ("env", vec![base.clone(), "spira-lint".to_string(), "--diff".to_string(), base_fq.to_string()]),
        ("env", vec![base, "bash".to_string(), "spira/build-fence.sh".to_string()]),
    ]
}

/// The first red, with its text: a rebase conflict of `branch` against `base_fq`, then (where
/// the tree carries the build fence) spira-lint and the fence. `strict` makes an absent tool
/// (127) red; otherwise it is skipped.
pub fn red(git: &dyn Git, exec: &dyn Exec, repo: &Path, work: &Path, branch: &str, base_fq: &str, strict: bool) -> Option<String> {
    let mt = git.git(repo, &["merge-tree", "--write-tree", base_fq, branch]);
    if mt.code == 1 {
        return Some(format!("{branch} does not rebase onto {base_fq} cleanly:\n{}", mt.text()));
    }
    if !work.join("spira/build-fence.sh").is_file() {
        return strict.then(|| format!("{} carries no spira/build-fence.sh — there is no fast tier to run", work.display()));
    }
    for (prog, args) in steps(base_fq) {
        let o = exec.exec(prog, &args, None, Some(work));
        if o.code != 0 && (o.code != 127 || strict) {
            let name = if args.iter().any(|a| a == "spira/build-fence.sh") { "spira/build-fence.sh" } else { "spira-lint" };
            return Some(format!("{name} failed (rc={}):\n{}{}", o.code, o.stdout, o.stderr));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::sync::Mutex;

    struct G(i32);
    impl Git for G {
        fn git(&self, _: &Path, _: &[&str]) -> Out {
            Out { code: self.0, stdout: "CONFLICT".into(), stderr: String::new() }
        }
    }

    struct E(i32, Mutex<Vec<Vec<String>>>);
    impl Exec for E {
        fn exec(&self, _: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.1.lock().unwrap().push(args.to_vec());
            Out { code: self.0, stdout: String::new(), stderr: "boom".into() }
        }
    }

    fn tree(name: &str, with_fence: bool) -> testkit::TempDir {
        let d = testkit::TempDir::new(name);
        std::fs::create_dir_all(d.join("spira")).unwrap();
        if with_fence {
            std::fs::write(d.join("spira/build-fence.sh"), "").unwrap();
        }
        d
    }

    #[test]
    fn spira_lint_and_the_build_fence_both_run_against_the_branch_base() {
        let steps = steps("refs/heads/local/main");
        assert_eq!(steps.len(), 2);
        for (prog, args) in &steps {
            assert_eq!(*prog, "env");
            assert_eq!(args[0], "SPIRA_GATE_BASE=refs/heads/local/main", "{args:?}");
        }
        assert_eq!(steps[0].1[1..], ["spira-lint".to_string(), "--diff".to_string(), "refs/heads/local/main".to_string()]);
        assert_eq!(steps[1].1[1..], ["bash".to_string(), "spira/build-fence.sh".to_string()]);
    }

    #[test]
    fn a_green_tier_runs_both_steps_and_is_not_red() {
        let w = tree("green", true);
        let e = E(0, Mutex::new(vec![]));
        assert_eq!(red(&G(0), &e, &w.join("."), &w.join("."), "b", "base", true), None);
        assert_eq!(e.1.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_failing_step_is_red_and_names_itself() {
        let w = tree("failing", true);
        let r = red(&G(0), &E(3, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false).unwrap();
        assert!(r.starts_with("spira-lint failed (rc=3)"), "{r}");
    }

    #[test]
    fn a_rebase_conflict_is_red() {
        let w = tree("rebase", true);
        let r = red(&G(1), &E(0, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false).unwrap();
        assert!(r.contains("does not rebase onto base cleanly"), "{r}");
    }

    #[test]
    fn an_absent_tool_is_skipped_unless_strict() {
        let w = tree("absent", true);
        assert_eq!(red(&G(0), &E(127, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false), None);
        assert!(red(&G(0), &E(127, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", true).is_some());
    }

    #[test]
    fn a_tree_without_the_fence_is_skipped_unless_strict() {
        let w = tree("nofence", false);
        let e = E(0, Mutex::new(vec![]));
        assert_eq!(red(&G(0), &e, &w.join("."), &w.join("."), "b", "base", false), None);
        assert!(red(&G(0), &e, &w.join("."), &w.join("."), "b", "base", true).unwrap().contains("no fast tier"));
        assert!(e.1.lock().unwrap().is_empty());
    }
}
