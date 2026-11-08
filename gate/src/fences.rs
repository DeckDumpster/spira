//! `gate fences --tree <dir> [--bin-dir <dir>]`: the fences a round runs on its merged tree,
//! one `name<TAB>command` line per step of the tree's [`crate::def::PATH`] that a suites-off
//! gate runs ([`crate::compose::without_suites`]), less the two steps the round runs as its own:
//! the spira-lint step, and the build fence, whose compile check is the round's workspace build
//! and unit tier (as in a unit composition, [`crate::compose::gate_string`]). A second
//! whole-workspace build on the VM, just before the unit tier, only competes with it. A runner takes this list (law-a-runner-takes-a-list); nothing else decides
//! which fences a round runs.

use crate::def::{Bin, Def};

/// The package whose step the round runs as its own lint, never as a listed fence.
pub const LINT_PACKAGE: &str = "spira-lint";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fence {
    /// What the round names the fence's result after: `fence-<name>`.
    pub name: String,
    pub command: String,
}

fn reads(step: &str, b: &Bin) -> bool {
    let plain = format!("${}", b.var);
    let braced = format!("${{{}}}", b.var);
    step.contains(&braced)
        || step.match_indices(&plain).any(|(at, _)| {
            !step[at + plain.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn file_name_ok(n: &str) -> bool {
    !n.is_empty() && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// The step's name: the bin package of a `$VAR` step, else its script's basename without the
/// extension, else its first word's.
fn name_of(d: &Def, step: &str) -> Result<String, String> {
    let stem = |w: &str| {
        let base = w.rsplit('/').next().unwrap_or(w);
        base.rsplit_once('.').map_or(base, |(s, _)| s).to_string()
    };
    let name = match d.bins.iter().find(|b| reads(step, b)) {
        Some(b) => b.package.clone(),
        None => match crate::parse::bash_paths(step).first() {
            Some(p) => stem(p),
            None => stem(step.split_whitespace().next().unwrap_or("").trim_matches('"')),
        },
    };
    if file_name_ok(&name) {
        Ok(name)
    } else {
        Err(format!("step `{step}` names no fence a result file can carry (got {name:?})"))
    }
}

/// The round's fences, in gate order. With `bin_dir`, each command first exports every bin it
/// reads as `<bin_dir>/<package>`, the tree's own build of it.
pub fn list(d: &Def, bin_dir: Option<&str>) -> Result<Vec<Fence>, String> {
    if let Some(dir) = bin_dir {
        if dir.is_empty() || !dir.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
            return Err(format!("--bin-dir {dir:?} is not a plain path"));
        }
    }
    let mut out: Vec<Fence> = Vec::new();
    for step in d.steps.iter().filter(|s| !crate::compose::is_suites_step(s) && s.as_str() != crate::compose::BUILD_FENCE_STEP) {
        let bins: Vec<&Bin> = d.bins.iter().filter(|b| reads(step, b)).collect();
        if bins.iter().any(|b| b.package == LINT_PACKAGE) {
            continue;
        }
        let name = name_of(d, step)?;
        if out.iter().any(|f| f.name == name) {
            return Err(format!("two steps are both the fence {name:?}; their results would collide"));
        }
        let command = match bin_dir {
            Some(dir) => bins.iter().fold(String::new(), |acc, b| acc + &format!("export {}={dir}/{}; ", b.var, b.package)) + step,
            None => step.clone(),
        };
        out.push(Fence { name, command });
    }
    Ok(out)
}

/// `name<TAB>command` lines, one per fence.
pub fn render(fences: &[Fence]) -> String {
    fences.iter().map(|f| format!("{}\t{}\n", f.name, f.command)).collect()
}

/// The subcommand: reads `<tree>/gate.steps` and prints its fences. A tree whose definition is
/// missing or unreadable is refused, never read as one with no fences.
pub fn main(args: &[String]) -> i32 {
    let (mut tree, mut bin_dir) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tree" => tree = it.next().cloned(),
            "--bin-dir" => bin_dir = it.next().cloned(),
            _ => {
                eprintln!("usage: gate fences --tree <dir> [--bin-dir <dir>]");
                return 2;
            }
        }
    }
    let Some(tree) = tree else {
        eprintln!("usage: gate fences --tree <dir> [--bin-dir <dir>]");
        return 2;
    };
    let path = std::path::Path::new(&tree).join(crate::def::PATH);
    let listed = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e}", path.display()))
        .and_then(|t| crate::def::parse(&t))
        .and_then(|d| list(&d, bin_dir.as_deref()));
    match listed {
        Ok(f) => {
            print!("{}", render(&f));
            0
        }
        Err(e) => {
            eprintln!("gate fences: {e}");
            1
        }
    }
}

/// `gate fence-run <name> --repo <root> --tip <sha> --base <ref>`: one fence of the tip's own
/// `gate.steps`, run in a throwaway checkout of the tip. A bin the fence reads is the installed
/// one by bare name; the round, which builds the tree's own, is what finally judges. Exit 0 the
/// fence passed, 1 it failed, 2 it could not be run (unknown name, no checkout).
pub fn run_main(args: &[String]) -> i32 {
    const USAGE: &str = "usage: gate fence-run <name> --repo <root> --tip <sha> --base <ref>";
    let (mut name, mut repo, mut tip, mut base) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--repo" => repo = it.next().cloned(),
            "--tip" => tip = it.next().cloned(),
            "--base" => base = it.next().cloned(),
            n if !n.starts_with('-') && name.is_none() => name = Some(n.to_string()),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let (Some(name), Some(repo), Some(tip), Some(base)) = (name, repo, tip, base) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let tree = std::env::temp_dir().join(format!("gate-fence-run-{}-{}", std::process::id(), name));
    let git = |args: &[&str]| {
        // batch-job: one worktree add or remove
        spira_config::bounded::bounded_for("git", "60").arg("-C").arg(&repo).args(args).output()
    };
    let added = git(&["worktree", "add", "-q", "--detach", &tree.display().to_string(), &tip]);
    if !added.as_ref().is_ok_and(|o| o.status.success()) {
        eprintln!("gate fence-run: cannot check out {tip} in {repo}");
        return 2;
    }
    let rc = run_in(&tree, &name, &base);
    let _ = git(&["worktree", "remove", "--force", &tree.display().to_string()]);
    rc
}

fn run_in(tree: &std::path::Path, name: &str, base: &str) -> i32 {
    let path = tree.join(crate::def::PATH);
    let fences = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e}", path.display()))
        .and_then(|t| crate::def::parse(&t))
        .and_then(|d| {
            let prefix = |f: &Fence| {
                d.bins.iter().filter(|b| f.command.contains(&b.var)).fold(String::new(), |acc, b| acc + &format!("export {}={}; ", b.var, b.package))
            };
            list(&d, None).map(|l| l.into_iter().map(|f| Fence { command: prefix(&f) + &f.command, name: f.name }).collect::<Vec<_>>())
        });
    let fences = match fences {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gate fence-run: {e}");
            return 2;
        }
    };
    let Some(fence) = fences.iter().find(|f| f.name == name) else {
        let known: Vec<&str> = fences.iter().map(|f| f.name.as_str()).collect();
        eprintln!("gate fence-run: {name} is not a fence of this tree; its fences: {}", known.join(" "));
        return 2;
    };
    // batch-job: the fence's own command, bounded by the caller's timeout
    let status = std::process::Command::new("bash")
        .args(["-c", &fence.command])
        .current_dir(tree)
        .env("SPIRA_GATE_BASE", base)
        .env("SPIRA_GATE_BRANCH", "HEAD")
        .env_remove("RUSTC_WRAPPER")
        .stdin(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => 0,
        Ok(_) => 1,
        Err(e) => {
            eprintln!("gate fence-run: cannot run {name}: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn this_trees_fences_are_its_suites_off_steps_less_the_lint() {
        let text = std::fs::read_to_string(root().join(crate::def::PATH)).unwrap();
        let d = crate::def::parse(&text).unwrap();
        let got = list(&d, None).unwrap();
        let kept: Vec<String> = crate::compose::chain_steps(&crate::compose::without_suites(&d.command()))
            .unwrap()
            .into_iter()
            .filter(|s| !s.contains("SPIRA_LINT_BIN") && s != crate::compose::BUILD_FENCE_STEP)
            .collect();
        assert_eq!(got.iter().map(|f| f.command.clone()).collect::<Vec<_>>(), kept);
        let names: Vec<&str> = got.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"lifecycle-guard") && names.contains(&"boundary"), "{names:?}");
        assert!(!names.contains(&LINT_PACKAGE) && !names.contains(&"build-fence"), "{names:?}");
        for f in &got {
            assert!(!f.command.starts_with("bin ") && !f.command.contains("SPIRA_SELECT_BIN") && !f.command.contains("SPIRA_TESTENV_BIN"), "{f:?}");
        }
    }

    #[test]
    fn names_are_the_bin_package_or_the_script_stem() {
        let d = crate::def::parse(
            "bin SPIRA_LINT_BIN spira-lint\nbin SPIRA_GUARD_BIN lifecycle-guard\nbin SPIRA_SELECT_BIN suite-select\n\
             step \"$SPIRA_LINT_BIN\"\nstep \"${SPIRA_GUARD_BIN}\" --gate .\nstep bash spira/boundary.sh check\nstep true\n\
             step { \"$SPIRA_SELECT_BIN\" gate; }\n",
        )
        .unwrap();
        let got = list(&d, Some("target/release")).unwrap();
        assert_eq!(
            render(&got),
            "lifecycle-guard\texport SPIRA_GUARD_BIN=target/release/lifecycle-guard; \"${SPIRA_GUARD_BIN}\" --gate .\n\
             boundary\tbash spira/boundary.sh check\ntrue\ttrue\n"
        );
        assert!(list(&d, Some("a b")).is_err());
        let dup = crate::def::parse("step bash a/x.sh\nstep bash b/x.sh\n").unwrap();
        assert!(list(&dup, None).is_err(), "two fences named x would overwrite one result");
    }

    #[test]
    fn a_tree_without_a_readable_definition_is_refused() {
        let t = testkit::TempDir::new("gate-fences");
        let d = t.path();
        let tree = d.display().to_string();
        assert_eq!(main(&["--tree".into(), tree.clone()]), 1);
        std::fs::write(d.join(crate::def::PATH), "nonsense\n").unwrap();
        assert_eq!(main(&["--tree".into(), tree.clone()]), 1);
        std::fs::write(d.join(crate::def::PATH), "step bash spira/x.sh\n").unwrap();
        assert_eq!(main(&["--tree".into(), tree]), 0);
        assert_eq!(main(&[]), 2);
    }

    fn repo_with_fences(tag: &str, steps: &str) -> (testkit::TempDir, String, String) {
        let t = testkit::TempDir::new(tag);
        let r = t.path();
        testkit::git(r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join(crate::def::PATH), steps).unwrap();
        std::fs::create_dir_all(r.join("spira")).unwrap();
        std::fs::write(r.join("spira/ok.sh"), "echo \"on $SPIRA_GATE_BRANCH vs $SPIRA_GATE_BASE\"\n").unwrap();
        std::fs::write(r.join("spira/bad.sh"), "exit 7\n").unwrap();
        testkit::git(r, &["add", "."]);
        testkit::git(r, &["commit", "-q", "-m", "x"]);
        let tip = testkit::git(r, &["rev-parse", "HEAD"]);
        let root = r.display().to_string();
        (t, root, tip)
    }

    fn fence_run(root: &str, tip: &str, name: &str) -> i32 {
        run_main(&[name.into(), "--repo".into(), root.into(), "--tip".into(), tip.into(), "--base".into(), "main".into()])
    }

    #[test]
    fn one_named_fence_runs_at_the_tip_and_its_exit_is_the_answer() {
        let (_t, root, tip) = repo_with_fences("fence-run", "step bash spira/ok.sh\nstep bash spira/bad.sh\n");
        assert_eq!(fence_run(&root, &tip, "ok"), 0);
        assert_eq!(fence_run(&root, &tip, "bad"), 1, "any red exit of the fence is a red");
        assert_eq!(fence_run(&root, &tip, "fence-ok"), 2, "the round's `fence-` prefix is not part of the fence's name");
        assert_eq!(fence_run(&root, &tip, "nonesuch"), 2, "an unknown fence cannot be run, which is not a pass");
        assert_eq!(fence_run(&root, "0000000000000000000000000000000000000000", "ok"), 2, "a tip the repository lacks cannot be run");
        let trees = testkit::git(std::path::Path::new(&root), &["worktree", "list"]);
        assert_eq!(trees.lines().count(), 1, "the throwaway checkout is removed: {trees}");
    }
}
