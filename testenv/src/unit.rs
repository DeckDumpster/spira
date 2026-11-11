//! `testenv --suites unit-tests <tip> <repo>`: the Rust unit tests of the crates a tip's diff
//! affects, run in the fixture container by `spira/unit-tests.sh`. The host computes the crate
//! list (the container has no history) and leaves it in the tree for the script.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The name callers use; the script that runs it is [`SCRIPT`].
pub const NAME: &str = "unit-tests";
pub const SCRIPT: &str = "unit-tests.sh";
pub const CRATES_REL: &str = "target/unit-tests.crates";
pub const BINS_REL: &str = "target/unit-tests.bins";
pub const FILTER_REL: &str = "target/unit-tests.filter";
pub const SKIP_REL: &str = "target/unit-tests.skip";
const MAX_FAILING: usize = 5;
const MAX_DETAIL: usize = 3;
const MAX_LINE: usize = 160;

pub fn resolve(list: &str) -> String {
    list.split(',').map(|n| if n.trim() == NAME { SCRIPT } else { n }).collect::<Vec<_>>().join(",")
}

pub fn selected(names: &[String]) -> bool {
    names.iter().any(|n| n == SCRIPT)
}

pub fn crates_path(tree: &Path) -> PathBuf {
    tree.join(CRATES_REL)
}

pub fn bins_path(tree: &Path) -> PathBuf {
    tree.join(BINS_REL)
}

/// Leaves `--tests`' names for the script, one per line, or removes a filter an earlier run
/// left in this tree, so an unfiltered run is never quietly filtered.
pub fn write_filter(tree: &Path, tests: Option<&[String]>) -> Result<(), String> {
    let path = tree.join(FILTER_REL);
    match tests {
        Some(names) => std::fs::create_dir_all(path.parent().unwrap_or(tree))
            .and_then(|_| std::fs::write(&path, names.iter().map(|n| format!("{n}\n")).collect::<String>())),
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
    .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Leaves the names the script must skip, one per line, or removes the file when there are
/// none, so a later run of this tree skips nothing it was not told to.
pub fn write_skip(tree: &Path, names: &[String]) -> Result<(), String> {
    let path = tree.join(SKIP_REL);
    if names.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(path.parent().unwrap_or(tree))
        .and_then(|_| std::fs::write(&path, names.iter().map(|n| format!("{n}\n")).collect::<String>()))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Each failing test's name and the first lines of its `---- name stdout ----` block.
pub fn failing(output: &str) -> Vec<(String, Vec<String>)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut names: Vec<String> = lines.iter().filter_map(|l| l.trim().strip_prefix("test ")?.strip_suffix(" ... FAILED").map(str::to_string)).collect();
    names.dedup();
    names
        .into_iter()
        .map(|n| {
            let head = format!("---- {n} stdout ----");
            let detail = lines
                .iter()
                .position(|l| l.trim() == head)
                .map(|i| {
                    lines[i + 1..]
                        .iter()
                        .take_while(|l| !l.starts_with("---- ") && !l.starts_with("failures:"))
                        .filter(|l| !l.trim().is_empty())
                        .take(MAX_DETAIL)
                        .map(|l| {
                            let l = l.trim();
                            if l.chars().count() > MAX_LINE { format!("{}…", l.chars().take(MAX_LINE).collect::<String>()) } else { l.to_string() }
                        })
                        .collect()
                })
                .unwrap_or_default();
            (n, detail)
        })
        .collect()
}

/// The log lines that name a red run's failing tests, at most [`MAX_FAILING`] of them.
pub fn failing_report(output: &str) -> Vec<String> {
    let failed = failing(output);
    if failed.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("unit-tests: {} failing test(s):", failed.len())];
    for (name, detail) in failed.iter().take(MAX_FAILING) {
        out.push(format!("  {name}"));
        out.extend(detail.iter().map(|d| format!("    {d}")));
    }
    if failed.len() > MAX_FAILING {
        out.push(format!("  (+{} more)", failed.len() - MAX_FAILING));
    }
    out
}

/// The tests libtest reports it ran, from its `test <name> ... ok|FAILED` lines; an ignored
/// test did not run. A `failures:` section is a failed test's captured output, never a report.
pub fn tests_ran(output: &str) -> Vec<String> {
    let mut in_failures = false;
    output
        .lines()
        .filter(|l| {
            if *l == "failures:" {
                in_failures = true;
            } else if l.starts_with("test result: ") {
                in_failures = false;
            }
            !in_failures
        })
        .filter_map(|l| {
            let (name, result) = l.strip_prefix("test ")?.split_once(" ... ")?;
            (result.starts_with("ok") || result.starts_with("FAILED")).then(|| name.to_string())
        })
        .collect()
}

/// Workspace crates whose binaries the selected crates' tests launch, declared as
/// `[package.metadata.spira] test-bins = [..]`; sorted, each once.
pub fn test_bins(metadata_json: &str, crates: &[String]) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(metadata_json).map_err(|e| format!("cargo metadata: {e}"))?;
    let mut out = std::collections::BTreeSet::new();
    for p in v["packages"].as_array().into_iter().flatten() {
        if !p["name"].as_str().is_some_and(|n| crates.iter().any(|c| c == n)) {
            continue;
        }
        let name = p["name"].as_str().unwrap_or_default();
        match &p["metadata"]["spira"]["test-bins"] {
            serde_json::Value::Null => {}
            serde_json::Value::Array(a) => {
                for b in a {
                    let b = b.as_str().ok_or_else(|| format!("{name}: test-bins must be strings"))?;
                    if !v["packages"].as_array().into_iter().flatten().any(|q| q["name"] == b) {
                        return Err(format!("{name}: test-bins names {b}, which is not a workspace crate"));
                    }
                    out.insert(b.to_string());
                }
            }
            _ => return Err(format!("{name}: test-bins must be an array")),
        }
    }
    Ok(out.into_iter().collect())
}

fn output(cmd: &mut Command) -> Result<Vec<u8>, String> {
    // batch-job: part of a testenv trial, bounded by the trial deadline
    let o = cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// The crates the unit tier runs: `affected` is what the diff selects (touched plus every
/// transitive dependent); `crates` is what is written for the script, the same list, or under
/// `--tests` only the affected crates that hold a named test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub affected: Vec<String>,
    pub crates: Vec<String>,
}

/// Writes the crates the tier runs, one per line, and the test-bins those crates declare; an
/// empty file means the diff touches no crate. Under `--tests` the list is narrowed to the
/// affected crates that hold a named test ([`holding`]), so `cargo test -p` builds those
/// crates and their dependencies, never every dependent of the diff.
pub fn write_crates(repo: &Path, base: &str, rev: &str, tree: &Path, tests: Option<&[String]>) -> Result<Picked, String> {
    let diff = output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--raw", "-z", "--no-renames", &format!("{base}...{rev}")])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE"),
    )
    .map_err(|e| format!("git diff {base}...{rev}: {e}"))?;
    let meta = output(Command::new("cargo").current_dir(tree).args(["metadata", "--format-version", "1", "--no-deps", "--offline"]))
        .map_err(|e| format!("cargo metadata: {e}"))?;
    let meta = String::from_utf8_lossy(&meta);
    let affected = gate::compose::select_crates(&diff, &meta, true)?;
    let crates = match tests {
        Some(names) => holding(tree, &gate::compose::parse_metadata(&meta)?, &affected, names),
        None => affected.clone(),
    };
    let path = crates_path(tree);
    let body: String = crates.iter().map(|c| format!("{c}\n")).collect();
    std::fs::create_dir_all(path.parent().unwrap_or(tree))
        .and_then(|_| std::fs::write(&path, body))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let bins: String = test_bins(&meta, &crates)?.iter().map(|c| format!("{c}\n")).collect();
    let path = bins_path(tree);
    std::fs::write(&path, bins).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Picked { affected, crates })
}

/// The crates of `crates` whose own Rust sources name one of `tests`, in their order. A name
/// is cargo's substring filter over a test's path, so its last `::` segment is part of the
/// test function's name or of a module's, and either appears in the crate's sources or, for a
/// module a build script generates from a file stem, in a source path: a crate where neither
/// contains it holds no test the name can match. A nested member's directory and `target/`
/// are not the crate's sources. A name with no segment (`::`) matches every test, so it keeps
/// every crate.
pub fn holding(tree: &Path, members: &[gate::compose::Member], crates: &[String], tests: &[String]) -> Vec<String> {
    let needles: Vec<&str> = tests
        .iter()
        .filter_map(|t| t.rsplit("::").find(|s| !s.is_empty()))
        .collect();
    if needles.len() < tests.len() {
        return crates.to_vec();
    }
    crates
        .iter()
        .filter(|c| {
            let Some(m) = members.iter().find(|m| &m.name == *c) else {
                return true;
            };
            let others: Vec<PathBuf> = members
                .iter()
                .filter(|o| o.name != m.name)
                .map(|o| tree.join(&o.dir))
                .collect();
            names_any(&tree.join(&m.dir), &others, &needles)
        })
        .cloned()
        .collect()
}

fn names_any(dir: &Path, others: &[PathBuf], needles: &[&str]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        let named = || needles.iter().any(|n| name.contains(n));
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                name != "target"
                    && !name.starts_with('.')
                    && !others.contains(&path)
                    && (named() || names_any(&path, others, needles))
            }
            Ok(t) if t.is_file() && name.ends_with(".rs") => {
                named() || std::fs::read_to_string(&path).is_ok_and(|src| needles.iter().any(|n| src.contains(n)))
            }
            _ => false,
        }
    })
}

/// Why a crate list that came out empty cannot be believed, or None for a diff that genuinely
/// touches no crate (docs, scripts). An empty diff is a base that is the tip itself, and a Rust
/// source or manifest outside every crate is a selection that missed it.
pub fn no_crate_doubt(repo: &Path, base: &str, rev: &str) -> Result<Option<String>, String> {
    let names = output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--name-only", "-z", "--no-renames", &format!("{base}...{rev}")])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE"),
    )
    .map_err(|e| format!("git diff {base}...{rev}: {e}"))?;
    let paths: Vec<String> = names.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    Ok(doubt(&paths))
}

fn doubt(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return Some("selects no crate and is empty".into());
    }
    let rust: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| p.ends_with(".rs") || p.rsplit('/').next() == Some("Cargo.toml"))
        .collect();
    (!rust.is_empty()).then(|| format!("selects no crate but touches Rust sources: {}", rust.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = "running 2 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nfailures:\n\n---- a::bad stdout ----\nthread 'a::bad' panicked at src/a.rs:9:5:\nassertion `left == right` failed\n  left: 1\n right: 2\n\nfailures:\n    a::bad\n\nVERDICT RED ran=1 red=unit-tests.sh\n";

    #[test]
    fn a_red_output_names_the_failing_test_and_its_first_three_lines() {
        let f = failing(RED);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, "a::bad");
        assert_eq!(f[0].1.len(), 3);
        assert!(f[0].1[1].contains("assertion"));
        assert_eq!(failing_report(RED)[0], "unit-tests: 1 failing test(s):");
    }

    #[test]
    fn a_green_output_names_no_failing_test() {
        assert!(failing("test a::ok ... ok\ntest result: ok.\n").is_empty());
        assert!(failing_report("test a::ok ... ok\n").is_empty());
    }

    #[test]
    fn write_skip_writes_the_names_and_removes_the_file_when_none_remain() {
        let dir = testkit::TempDir::new("skip");
        let path = dir.path().join(SKIP_REL);
        write_skip(dir.path(), &["a".into(), "b".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
        write_skip(dir.path(), &[]).unwrap();
        assert!(!path.exists());
        write_skip(dir.path(), &[]).unwrap();
    }

    #[test]
    fn an_empty_crate_list_is_doubted_for_an_empty_diff_or_a_rust_path_and_believed_for_docs() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(doubt(&[]).as_deref(), Some("selects no crate and is empty"));
        assert_eq!(
            doubt(&p(&["notes.md", "tools/gen.rs", "x/Cargo.toml"])).as_deref(),
            Some("selects no crate but touches Rust sources: tools/gen.rs x/Cargo.toml")
        );
        assert_eq!(doubt(&p(&["notes.md", "spira/x.sh", "Cargo.toml.md"])), None);
    }

    #[test]
    fn the_caller_name_resolves_to_the_script_and_other_names_pass_through() {
        assert_eq!(resolve("unit-tests"), "unit-tests.sh");
        assert_eq!(resolve("test-a.sh, unit-tests"), "test-a.sh,unit-tests.sh");
        assert_eq!(resolve("test-a.sh"), "test-a.sh");
        assert!(selected(&["unit-tests.sh".to_string()]));
        assert!(!selected(&["test-a.sh".to_string()]));
    }

    const META: &str = r#"{"packages":[
        {"name":"rt","metadata":{"spira":{"test-bins":["lc"]}}},
        {"name":"lc","metadata":null},
        {"name":"plain","metadata":null},
        {"name":"bad","metadata":{"spira":{"test-bins":["ghost"]}}}]}"#;

    #[test]
    fn a_selected_crate_declaring_test_bins_names_them_and_one_declaring_nothing_adds_none() {
        assert_eq!(test_bins(META, &["rt".into(), "plain".into()]).unwrap(), ["lc"]);
        assert!(test_bins(META, &["plain".into()]).unwrap().is_empty());
        assert!(test_bins(META, &["lc".into()]).unwrap().is_empty());
    }

    #[test]
    fn the_tests_libtest_ran_are_its_ok_and_failed_lines_and_never_an_ignored_one() {
        let out = "running 3 tests\n\
            test tests::one_name ... ok\n\
            test tests::two_name ... FAILED\n\
            test tests::slow_one ... ignored, needs a database\n\
            test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n\
            test src/lib.rs - doc (line 3) ... ok\n";
        assert_eq!(tests_ran(out), ["tests::one_name", "tests::two_name", "src/lib.rs - doc (line 3)"]);
        let failing = "test tests::real ... FAILED\n\nfailures:\n\n---- tests::real stdout ----\ntest tests::printed ... ok\n\nfailures:\n    tests::real\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert_eq!(tests_ran(failing), ["tests::real"]);
        assert!(tests_ran("running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out\n").is_empty());
    }

    #[test]
    fn a_crate_holds_a_name_its_own_sources_or_source_paths_contain_and_never_one_only_a_nested_member_or_target_contains() {
        let t = testkit::TempDir::new("testenv-unit-holding");
        let put = |rel: &str, body: &str| {
            let p = t.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        put("outer/src/lib.rs", "mod sweep;\n");
        put("outer/src/sweep/tests.rs", "#[test]\nfn a_stale_slot_is_swept() {}\n");
        put("outer/inner/src/lib.rs", "#[test]\nfn only_inner_has_this() {}\n");
        put("outer/target/debug/build/gen.rs", "fn only_target_has_this() {}\n");
        put("plain/src/lib.rs", "include!(concat!(env!(\"OUT_DIR\"), \"/rules.rs\"));\n");
        put("plain/src/rules/no_raw_pr.rs", "#[test]\nfn refused() {}\n");
        let m = |name: &str, dir: &str| gate::compose::Member { name: name.into(), dir: dir.into(), deps: vec![], build_deps: vec![] };
        let members = [m("outer", "outer"), m("inner", "outer/inner"), m("plain", "plain")];
        let crates: Vec<String> = ["outer", "inner", "plain"].map(String::from).to_vec();
        let held = |names: &[&str]| holding(t.path(), &members, &crates, &names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(held(&["sweep::tests::a_stale_slot"]), ["outer"]);
        assert_eq!(held(&["sweep"]), ["outer"]);
        assert_eq!(held(&["only_inner_has_this"]), ["inner"]);
        assert!(held(&["only_target_has_this"]).is_empty());
        assert_eq!(held(&["a_stale_slot_is_swept", "only_inner_has_this"]), ["outer", "inner"]);
        // a module a build script makes from a file stem is named by the path alone
        assert_eq!(held(&["rules::no_raw_pr"]), ["plain"]);
        assert_eq!(held(&["::"]), ["outer", "inner", "plain"]);
    }

    #[test]
    fn a_test_bin_that_is_not_a_workspace_crate_is_refused() {
        assert!(test_bins(META, &["bad".into()]).unwrap_err().contains("ghost"));
    }
}
