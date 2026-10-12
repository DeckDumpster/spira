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
    let mut lint_args = git_env_unset();
    lint_args.push(base.clone());
    lint_args.extend(lint.into_iter().map(String::from));
    lint_args.extend(["--diff".to_string(), base_fq.to_string()]);
    vec![
        ("env", lint_args),
        ("env", git_env_unset().into_iter().chain([base, "bash".to_string(), "spira/build-fence.sh".to_string()]).collect()),
    ]
}

/// Both steps resolve git from the work tree's cwd, as the build does, never from a git
/// location the aeon's own environment carries.
fn git_env_unset() -> Vec<String> {
    ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_PREFIX"].iter().flat_map(|k| ["-u".to_string(), k.to_string()]).collect()
}

/// spira-lint's exit when its own probe (git) failed, not when it found something.
pub const LINT_PROBE_FAILED: i32 = 4;

/// A fast-tier red. `harness` marks a tool failure that says nothing about the work.
#[derive(Debug, PartialEq)]
pub struct Red {
    pub harness: bool,
    pub text: String,
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
pub fn red(git: &dyn Git, exec: &dyn Exec, repo: &Path, work: &Path, branch: &str, base_fq: &str, strict: bool) -> Option<Red> {
    let mt = git.git(repo, &["merge-tree", "--write-tree", base_fq, branch]);
    if mt.code == 1 {
        return Some(Red { harness: false, text: format!("{branch} does not rebase onto {base_fq} cleanly:\n{}", mt.text()) });
    }
    if !work.join("spira/build-fence.sh").is_file() {
        return strict.then(|| Red { harness: false, text: format!("{} carries no spira/build-fence.sh — there is no fast tier to run", work.display()) });
    }
    for (prog, args) in steps(base_fq, changes_compiled_registry(git, work, base_fq)) {
        let o = exec.exec(prog, &args, None, Some(work));
        if o.code != 0 && (o.code != 127 || strict) {
            let name = if args.iter().any(|a| a == "spira/build-fence.sh") { "spira/build-fence.sh" } else { "spira-lint" };
            let harness = name == "spira-lint" && o.code == LINT_PROBE_FAILED;
            return Some(Red { harness, text: format!("{name} failed (rc={}):\n{}{}", o.code, o.stdout, o.stderr) });
        }
    }
    None
}

/// The paths a rebase-conflict red names; empty for any other red.
pub fn conflict_paths(red: &str) -> Vec<String> {
    if !red.contains(" does not rebase onto ") {
        return Vec::new();
    }
    red.lines()
        .filter(|l| l.starts_with("CONFLICT"))
        .filter_map(|l| {
            if let Some((_, p)) = l.split_once("Merge conflict in ") {
                return Some(p.trim());
            }
            let rest = l.split_once("): ")?.1;
            rest.split_once(" deleted in ").or_else(|| rest.split_once(" renamed ")).map(|(p, _)| p.trim())
        })
        .map(String::from)
        .collect()
}

pub const KIND: &str = "fast-tier-red";
/// The reopen cause of a handoff refused for a conflict with an unlanded blocker; its name
/// carries "conflict", which the attempt census reads as a rebase return, never charged.
pub const BLOCKER_CONFLICT_CAUSE: &str = "blocker-conflict";
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
    if out.len() <= 1 {
        out = red.lines().collect();
    }
    out.truncate(DIGEST_MAX_LINES);
    out.join("\n")
}

const TIP_PREFIX: &str = "\ntip: ";

/// A digest as recorded on the bead: the tip it judged, so a resubmit of that tip is recognised.
pub fn with_tip(digest: &str, tip: &str) -> String {
    format!("{digest}{TIP_PREFIX}{tip}")
}

/// The tip a recorded digest judged, and the digest without it.
pub fn split_tip(recorded: &str) -> (&str, Option<&str>) {
    match recorded.rsplit_once(TIP_PREFIX) {
        Some((d, t)) if !t.contains('\n') && !t.is_empty() => (d, Some(t.trim())),
        _ => (recorded, None),
    }
}

/// Whether a digest is a spira-lint red, which `cargo check` does not reproduce.
pub fn names_lint(digest: &str) -> bool {
    digest.starts_with("spira-lint failed")
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
            assert!(args.contains(&"SPIRA_GATE_BASE=refs/heads/local/main".to_string()), "{args:?}");
            assert!(args.windows(2).any(|w| w == ["-u", "GIT_DIR"]), "{args:?}");
        }
        assert_eq!(steps[0].1[steps[0].1.len() - 3..], ["spira-lint".to_string(), "--diff".to_string(), "refs/heads/local/main".to_string()]);
        assert_eq!(steps[1].1[steps[1].1.len() - 2..], ["bash".to_string(), "spira/build-fence.sh".to_string()]);
    }

    #[test]
    fn a_registry_changing_branch_lints_with_a_binary_built_from_its_tree() {
        let steps = steps("base", true);
        assert_eq!(steps[0].1[steps[0].1.len() - 8..], ["cargo", "run", "--quiet", "-p", "spira-lint", "--", "--diff", "base"].map(String::from));
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
        assert!(r.text.starts_with("spira-lint failed (rc=3)"), "{r:?}");
        assert!(!r.harness);
    }

    #[test]
    fn a_lint_probe_failure_is_harness_and_a_finding_is_not() {
        let w = tree("probe", true);
        let r = red(&G(0), &E(LINT_PROBE_FAILED, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false).unwrap();
        assert!(r.harness, "{r:?}");
        assert!(r.text.contains("boom"), "stderr is recorded verbatim: {r:?}");
        let r = red(&G(0), &E(1, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false).unwrap();
        assert!(!r.harness, "{r:?}");
    }

    #[test]
    fn a_rebase_conflict_is_red() {
        let w = tree("rebase", true);
        let r = red(&G(1), &E(0, Mutex::new(vec![])), &w.join("."), &w.join("."), "b", "base", false).unwrap();
        assert!(r.text.contains("does not rebase onto base cleanly"), "{r:?}");
    }

    #[test]
    fn conflict_paths_are_read_from_conflict_lines_only() {
        let red = "b does not rebase onto base cleanly:\nAuto-merging x\nCONFLICT (content): Merge conflict in deploy/ci.sh\nCONFLICT (add/add): Merge conflict in a/__init__.py\nCONFLICT (modify/delete): gone.rs deleted in HEAD and modified in b.\n";
        assert_eq!(conflict_paths(red), ["deploy/ci.sh", "a/__init__.py", "gone.rs"]);
        assert!(conflict_paths("spira-lint failed (rc=1):\nCONFLICT (content): Merge conflict in x\n").is_empty(), "a lint red is not a rebase conflict");
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
        assert!(red(&G(0), &e, &w.join("."), &w.join("."), "b", "base", true).unwrap().text.contains("no fast tier"));
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

    #[test]
    fn a_multi_line_lint_red_is_recorded_whole_and_the_tip_round_trips() {
        let red = "spira-lint failed (rc=1):\nrule-a: f.rs:3: bad thing\nrule-b: g.rs:9: other\n";
        let d = digest(red);
        assert_eq!(d.lines().count(), 3, "{d}");
        assert!(d.contains("g.rs:9") && names_lint(&d));
        let rec = with_tip(&d, "abc123");
        assert_eq!(split_tip(&rec), (d.as_str(), Some("abc123")));
        assert_eq!(split_tip(&d).1, None);
    }
}
