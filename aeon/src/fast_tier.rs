//! The in-session fast tier: the checks the landing gate runs that need no host-wide
//! admission. The teardown runs it before a handoff; `aeon fast-tier` runs the same code so
//! acceptance can exercise it on a release without a model session.

use std::path::Path;

use crate::ports::{Exec, Git};

/// The fast tier's commands, in order. spira-lint judges only the branch's own diff, so a
/// finding already on the base never reds a bead; the round lints the whole tree. The build
/// fence gets the base through SPIRA_GATE_BASE.
pub fn steps(base_fq: &str, lint_from_tree: bool) -> Vec<(&'static str, Vec<String>)> {
    let base = format!("SPIRA_GATE_BASE={base_fq}");
    let lint: Vec<&str> = if lint_from_tree { vec!["cargo", "run", "--quiet", "-p", "spira-lint", "--"] } else { vec!["spira-lint"] };
    let mut lint_args = vec![base.clone()];
    lint_args.extend(lint.into_iter().map(String::from));
    lint_args.extend(["--diff".to_string(), base_fq.to_string()]);
    vec![
        ("env", lint_args),
        ("env", vec![base, "bash".to_string(), "spira/build-fence.sh".to_string()]),
    ]
}

/// Whether the branch changes what spira-lint compiles in (the key registry), so the installed
/// binary would judge the branch against keys it predates.
fn changes_compiled_registry(git: &dyn Git, work: &Path, base_fq: &str) -> bool {
    git.git(work, &["diff", "--name-only", base_fq, "HEAD"])
        .stdout
        .lines()
        .any(|f| f.starts_with("spira/conf.d/") || f.starts_with("spira-config/") || f.starts_with("spira-lint/"))
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
    for (prog, args) in steps(base_fq, changes_compiled_registry(git, work, base_fq)) {
        let o = exec.exec(prog, &args, None, Some(work));
        if o.code != 0 && (o.code != 127 || strict) {
            let name = if args.iter().any(|a| a == "spira/build-fence.sh") { "spira/build-fence.sh" } else { "spira-lint" };
            return Some(format!("{name} failed (rc={}):\n{}{}", o.code, o.stdout, o.stderr));
        }
    }
    None
}

pub const KIND: &str = "fast-tier-red";
pub const DIGEST_MAX_LINES: usize = 60;

/// The lines of a red that say what to fix: the failing step, compiler errors with their
/// `-->` locations, and a rebase conflict's paths. Warnings and the rest are dropped.
pub fn digest(red: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_error = false;
    for (i, line) in red.lines().enumerate() {
        let t = line.trim_start();
        let keep = if i == 0 {
            in_error = false;
            true
        } else if t.starts_with("error") || t.contains("CONFLICT") {
            in_error = true;
            true
        } else if t.starts_with("warning") {
            in_error = false;
            false
        } else if t.starts_with("-->") {
            in_error
        } else {
            false
        };
        if keep {
            out.push(line);
        }
    }
    out.truncate(DIGEST_MAX_LINES);
    out.join("\n")
}

/// Whether a digest names a compile error, which `cargo check` would reproduce.
pub fn names_compile_error(digest: &str) -> bool {
    digest.lines().any(|l| l.trim_start().starts_with("error[E") || l.trim_start().starts_with("error:") && !l.contains("aborting") && !l.contains("could not compile"))
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
        let steps = steps("refs/heads/local/main", false);
        assert_eq!(steps.len(), 2);
        for (prog, args) in &steps {
            assert_eq!(*prog, "env");
            assert_eq!(args[0], "SPIRA_GATE_BASE=refs/heads/local/main", "{args:?}");
        }
        assert_eq!(steps[0].1[1..], ["spira-lint".to_string(), "--diff".to_string(), "refs/heads/local/main".to_string()]);
        assert_eq!(steps[1].1[1..], ["bash".to_string(), "spira/build-fence.sh".to_string()]);
    }

    #[test]
    fn a_registry_changing_branch_lints_with_a_binary_built_from_its_tree() {
        let steps = steps("base", true);
        assert_eq!(steps[0].1[1..], ["cargo", "run", "--quiet", "-p", "spira-lint", "--", "--diff", "base"].map(String::from));
        let w = tree("registry", true);
        struct D;
        impl Git for D {
            fn git(&self, _: &Path, _: &[&str]) -> Out {
                Out { code: 0, stdout: "spira/conf.d/SPIRA_FOO\n".into(), stderr: String::new() }
            }
        }
        let e = E(0, Mutex::new(vec![]));
        assert_eq!(red(&D, &e, &w.join("."), &w.join("."), "b", "base", true), None);
        assert!(e.1.lock().unwrap()[0].contains(&"cargo".to_string()));
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

    #[test]
    fn the_digest_keeps_two_errors_and_their_locations_out_of_fifty_warnings() {
        let mut red = String::from("spira/build-fence.sh failed (rc=101):\n");
        for i in 0..50 {
            red.push_str(&format!("warning: unused variable `v{i}`\n  --> src/w{i}.rs:1:1\n   |\n1  | let v = 1;\n\n"));
        }
        red.push_str("error[E0432]: unresolved import `crate::nope`\n  --> src/a.rs:3:5\n   |\n3  | use crate::nope;\n\nerror[E0063]: missing field `x`\n  --> src/b.rs:9:1\n");
        let d = digest(&red);
        assert!(!d.contains("warning") && !d.contains("src/w"), "{d}");
        assert_eq!(d.lines().collect::<Vec<_>>(), [
            "spira/build-fence.sh failed (rc=101):",
            "error[E0432]: unresolved import `crate::nope`",
            "  --> src/a.rs:3:5",
            "error[E0063]: missing field `x`",
            "  --> src/b.rs:9:1",
        ]);
        assert!(names_compile_error(&d));
    }

    #[test]
    fn a_rebase_conflict_digest_keeps_its_paths_and_is_capped() {
        let red = format!("b does not rebase onto base cleanly:\n{}", (0..200).map(|i| format!("CONFLICT (content): Merge conflict in f{i}.rs\n")).collect::<String>());
        let d = digest(&red);
        assert_eq!(d.lines().count(), DIGEST_MAX_LINES);
        assert!(d.contains("in f1.rs") && !names_compile_error(&d));
    }
}
