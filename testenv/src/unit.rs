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
pub const BUILD_REL: &str = "target/unit-tests.build";
pub const FILTER_REL: &str = "target/unit-tests.filter";
pub const SKIP_REL: &str = "target/unit-tests.skip";
pub const DENY_REL: &str = "target/unit-tests.deny";
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
    write_names(&tree.join(SKIP_REL), tree, names)
}

/// `::<id>` for each `test:` row of the deny list, the substring `unit-tests.sh` skips; the file
/// is removed when there are none. Returns the names written.
pub fn write_deny(tree: &Path, deny: &spira_config::unit_tier::DenyList) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for (_, id, _) in deny.test_rows() {
        let name = format!("::{id}");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    write_names(&tree.join(DENY_REL), tree, &names)?;
    Ok(names)
}

fn write_names(path: &Path, tree: &Path, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(path.parent().unwrap_or(tree))
        .and_then(|_| std::fs::write(path, names.iter().map(|n| format!("{n}\n")).collect::<String>()))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Each failing test's name and the first lines of its `---- name stdout ----` block.
pub fn failing(output: &str) -> Vec<(String, Vec<String>)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut names: Vec<String> = Vec::new();
    for n in lines.iter().filter_map(|l| l.trim().strip_prefix("test ")?.strip_suffix(" ... FAILED")) {
        if !names.iter().any(|seen| seen == n) {
            names.push(n.to_string());
        }
    }
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
        for b in gate::compose::package_test_bins(p)? {
            if !v["packages"].as_array().into_iter().flatten().any(|q| q["name"] == b.as_str()) {
                return Err(format!("{name}: test-bins names {b}, which is not a workspace crate"));
            }
            out.insert(b);
        }
    }
    Ok(out.into_iter().collect())
}

/// The test-bins of `packages` in `tree`; a name that is not a workspace crate adds nothing.
pub fn tree_test_bins(tree: &Path, packages: &[String]) -> Result<Vec<String>, String> {
    test_bins(&workspace_metadata(tree)?, packages)
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

/// Writes the crates the tier runs, one per line, the test-bins those crates declare, and the
/// build line the gate runs for the same crates ([`gate::compose::unit_build_words`], one word
/// per line, read by the script); an
/// empty file means the diff touches no crate. Under `--tests` the list is narrowed to the
/// affected crates that hold a named test ([`holding`]), so `cargo test -p` builds those
/// crates and their dependencies, never every dependent of the diff.
pub fn write_crates(repo: &Path, base: &str, rev: &str, tree: &Path, tests: Option<&[String]>, transitive: bool) -> Result<Picked, String> {
    let diff = output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--raw", "-z", "--no-renames", &format!("{base}...{rev}")])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE"),
    )
    .map_err(|e| format!("git diff {base}...{rev}: {e}"))?;
    let meta = workspace_metadata(tree)?;
    let picked = pick(&diff, &meta, tree, tests, transitive)?;
    write_picked(tree, &meta, picked)
}

fn workspace_metadata(tree: &Path) -> Result<String, String> {
    let meta = output(Command::new("cargo").current_dir(tree).args(["metadata", "--format-version", "1", "--no-deps", "--offline"]))
        .map_err(|e| format!("cargo metadata: {e}"))?;
    Ok(String::from_utf8_lossy(&meta).into_owned())
}

/// Like [`write_crates`] for a list the caller named: the diff selects nothing.
pub fn write_named(tree: &Path, named: &[String], tests: Option<&[String]>) -> Result<Picked, String> {
    let meta = workspace_metadata(tree)?;
    let picked = pick_named(&meta, tree, named, tests)?;
    write_picked(tree, &meta, picked)
}

/// For a diff that touches no crate: every workspace member whose own sources name one of
/// `tests`.
pub fn write_holding(tree: &Path, tests: &[String]) -> Result<Picked, String> {
    let meta = workspace_metadata(tree)?;
    let members = gate::compose::parse_metadata(&meta)?;
    let all: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
    let crates = holding(tree, &members, &all, tests);
    write_picked(tree, &meta, Picked { affected: all, crates })
}

fn write_picked(tree: &Path, meta: &str, picked: Picked) -> Result<Picked, String> {
    let crates = &picked.crates;
    let path = crates_path(tree);
    let body: String = crates.iter().map(|c| format!("{c}\n")).collect();
    std::fs::create_dir_all(path.parent().unwrap_or(tree))
        .and_then(|_| std::fs::write(&path, body))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let bins: String = test_bins(meta, crates)?.iter().map(|c| format!("{c}\n")).collect();
    let path = bins_path(tree);
    std::fs::write(&path, bins).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let words: String = gate::compose::unit_build_words(crates).iter().map(|w| format!("{w}\n")).collect();
    let path = tree.join(BUILD_REL);
    std::fs::write(&path, words).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(picked)
}

/// The crates a diff selects: touched plus their dependents (every transitive one, or only the
/// direct ones), narrowed under `--tests` to those holding a named test.
pub fn pick(diff: &[u8], meta: &str, tree: &Path, tests: Option<&[String]>, transitive: bool) -> Result<Picked, String> {
    let affected = gate::compose::select_crates(diff, meta, transitive)?;
    let crates = match tests {
        Some(names) => holding(tree, &gate::compose::parse_metadata(meta)?, &affected, names),
        None => affected.clone(),
    };
    Ok(Picked { affected, crates })
}

/// The crates a caller named, each checked to be a workspace member, narrowed under `--tests`
/// like a diff's.
pub fn pick_named(meta: &str, tree: &Path, named: &[String], tests: Option<&[String]>) -> Result<Picked, String> {
    let members = gate::compose::parse_metadata(meta)?;
    crate::cli::check_crates(named, &members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>())?;
    let crates = match tests {
        Some(names) => holding(tree, &members, named, names),
        None => named.to_vec(),
    };
    Ok(Picked { affected: named.to_vec(), crates })
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

/// Seconds cargo spent building, summed over its `Finished ... in 1m 06s` / `in 7.92s` lines
/// (the bins build and the test build each print one); None when it printed none.
pub fn build_seconds(output: &str) -> Option<f64> {
    let secs: Vec<f64> = output
        .lines()
        .filter(|l| l.trim_start().starts_with("Finished "))
        .filter_map(|l| duration_secs(l.rsplit_once(" in ")?.1))
        .collect();
    (!secs.is_empty()).then(|| (secs.iter().sum::<f64>() * 10.0).round() / 10.0)
}

fn duration_secs(text: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut any = false;
    for word in text.split_whitespace() {
        let (num, unit) = word.split_at(word.find(|c: char| c.is_alphabetic())?);
        let n: f64 = num.parse().ok()?;
        total += n * match unit {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            _ => return None,
        };
        any = true;
    }
    any.then_some(total)
}

/// The record fields a run's `.out` text states: what was compiled, built, skipped and failed.
pub fn read_back(output: &str, rec: &mut gate::unitrecord::UnitRecord) {
    let count = |p: &str| output.lines().filter(|l| l.trim_start().starts_with(p)).count() as u64;
    rec.compiled = Some(count("Compiling "));
    rec.fresh = Some(count("Fresh ")).filter(|n| *n > 0);
    rec.build_s = build_seconds(output);
    rec.skipped = output
        .lines()
        .find_map(|l| Some(l.strip_prefix("unit-tests: skipped ")?.split_once(": ")?.1.split_whitespace().count() as u64));
    rec.failing = failing(output).into_iter().map(|(n, _)| n).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = "running 2 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nfailures:\n\n---- a::bad stdout ----\nthread 'a::bad' panicked at src/a.rs:9:5:\nassertion `left == right` failed\n  left: 1\n right: 2\n\nfailures:\n    a::bad\n\nVERDICT RED ran=1 red=unit-tests.sh\n";

    #[test_id(t2zkf9t)]
    #[test]
    fn the_repositorys_tree_declares_spira_lc_as_a_test_bin_of_spira_runtime() {
        let manifest = testkit::manifest_dir();
        let tree = manifest.parent().unwrap();
        assert!(tree_test_bins(tree, &["spira-runtime".into(), "no-such-crate".into()]).unwrap().contains(&"spira-lc".to_string()));
        assert!(tree_test_bins(tree, &["no-such-crate".into()]).unwrap().is_empty());
    }

    #[test_id(tinyxmq)]
    #[test]
    fn the_repositorys_tree_declares_the_binaries_its_tests_launch() {
        let tree = testkit::manifest_dir().parent().unwrap().to_path_buf();
        assert!(tree_test_bins(&tree, &["watchtower".into()]).unwrap().contains(&"testenv".to_string()));
        assert!(tree_test_bins(&tree, &["batcher-cut".into()]).unwrap().contains(&"gate".to_string()));
    }

    #[test_id(to4ooud)]
    #[test]
    fn a_red_output_names_the_failing_test_and_its_first_three_lines() {
        let f = failing(RED);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, "a::bad");
        assert_eq!(f[0].1.len(), 3);
        assert!(f[0].1[1].contains("assertion"));
        assert_eq!(failing_report(RED)[0], "unit-tests: 1 failing test(s):");
        assert_eq!(failing("test a ... FAILED\ntest b ... FAILED\ntest a ... FAILED\n").len(), 2);
    }

    #[test_id(tqtcqrc)]
    #[test]
    fn direct_selects_the_touched_crate_and_its_direct_dependents_only() {
        let pkg = |n: &str, dep: Option<&str>| {
            let deps = dep.map(|d| format!(r#"{{"path":"/w/{d}"}}"#)).unwrap_or_default();
            format!(r#"{{"id":"{n}","name":"{n}","manifest_path":"/w/{n}/Cargo.toml","dependencies":[{deps}]}}"#)
        };
        let meta = format!(
            r#"{{"workspace_root":"/w","workspace_members":["leaf","mid","top"],"packages":[{},{},{}]}}"#,
            pkg("leaf", None),
            pkg("mid", Some("leaf")),
            pkg("top", Some("mid"))
        );
        let diff = b":100644 100644 aaa bbb M\0leaf/src/lib.rs\0";
        let root = testkit::TempDir::new("unit-pick");
        let tree = &root.join("nonexistent");
        let names = |transitive| pick(diff, &meta, tree, None, transitive).unwrap().crates;
        assert_eq!(names(false), vec!["leaf", "mid"]);
        assert_eq!(names(true), vec!["leaf", "mid", "top"]);
    }

    #[test_id(t7va1xb)]
    #[test]
    fn named_crates_are_written_whatever_the_diff_touches_and_a_stranger_is_refused() {
        let t = testkit::TempDir::new("testenv-unit-named");
        let tree = t.path();
        let members = ["mail", "queue", "round-vm"];
        let list: String = members.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ");
        std::fs::write(tree.join("Cargo.toml"), format!("[workspace]\nresolver = \"2\"\nmembers = [{list}]\n")).unwrap();
        for m in members {
            std::fs::create_dir_all(tree.join(m).join("src")).unwrap();
            std::fs::write(tree.join(m).join("Cargo.toml"), format!("[package]\nname = \"{m}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")).unwrap();
            std::fs::write(tree.join(m).join("src/lib.rs"), "").unwrap();
        }
        let named = ["mail".to_string(), "queue".to_string()];
        let picked = write_named(tree, &named, None).unwrap();
        assert_eq!(picked.crates, ["mail", "queue"]);
        assert_eq!(std::fs::read_to_string(crates_path(tree)).unwrap(), "mail\nqueue\n");
        let e = write_named(tree, &["nosuch".to_string()], None).unwrap_err();
        assert!(e.contains("nosuch"), "{e}");
    }

    #[test_id(tzngm9p)]
    #[test]
    fn write_holding_writes_the_members_whose_sources_name_the_test() {
        let t = testkit::TempDir::new("testenv-unit-holding");
        let tree = t.path();
        std::fs::write(tree.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"a\", \"b\"]\n").unwrap();
        for m in ["a", "b"] {
            std::fs::create_dir_all(tree.join(m).join("src")).unwrap();
            std::fs::write(tree.join(m).join("Cargo.toml"), format!("[package]\nname = \"{m}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")).unwrap();
            std::fs::write(tree.join(m).join("src/lib.rs"), if m == "b" { "#[test] fn only_in_b() {}" } else { "" }).unwrap();
        }
        let p = write_holding(tree, &["only_in_b".to_string()]).unwrap();
        assert_eq!(p.crates, ["b"]);
        assert_eq!(std::fs::read_to_string(crates_path(tree)).unwrap(), "b\n");
        let p = write_holding(tree, &["nowhere".to_string()]).unwrap();
        assert!(p.crates.is_empty());
        assert_eq!(std::fs::read_to_string(crates_path(tree)).unwrap(), "");
    }

    #[test_id(tbne6ze)]
    #[test]
    fn the_build_line_the_tier_writes_is_the_one_the_gate_runs() {
        let git = |dir: &Path, args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(dir).args(args).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        let t = testkit::TempDir::new("testenv-unit-build-line");
        let repo = t.path();
        std::fs::create_dir_all(repo.join("spira-sim/src")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"spira-sim\"]\n").unwrap();
        std::fs::write(repo.join("spira-sim/Cargo.toml"), "[package]\nname = \"spira-sim\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(repo.join("spira-sim/src/lib.rs"), "").unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "."]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]);
        git(repo, &["checkout", "-q", "-b", "work"]);
        std::fs::write(repo.join("spira-sim/src/lib.rs"), "// changed\n").unwrap();
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "change"]);

        let picked = write_crates(repo, "main", "work", repo, None, true).unwrap();
        assert_eq!(picked.crates, ["spira-sim"]);
        let written: Vec<String> = std::fs::read_to_string(repo.join(BUILD_REL)).unwrap().lines().map(String::from).collect();
        let [gate_build, _] = gate::compose::unit_commands(&["spira-sim".into()], Default::default(), 7, &Default::default());
        assert_eq!(gate_build.1.strip_prefix("CARGO_BUILD_JOBS=7 ").unwrap(), written.join(" "));
    }

    #[test_id(txeiznx)]
    #[test]
    fn a_green_output_names_no_failing_test() {
        assert!(failing("test a::ok ... ok\ntest result: ok.\n").is_empty());
        assert!(failing_report("test a::ok ... ok\n").is_empty());
    }

    #[test_id(tsxmy5x)]
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

    #[test_id(tapvgok)]
    #[test]
    fn write_deny_turns_test_rows_into_id_suffixes_and_removes_the_file_when_none() {
        let dir = testkit::TempDir::new("deny");
        let path = dir.path().join(DENY_REL);
        let d = spira_config::unit_tier::DenyList::parse("disabled\ttest:t0aaaaa\tsp-x\t9999999999\nquarantined\ttest:t0bbbbb\tsp-x\t9999999999\ndisabled\tsuite:test-x.sh\tsp-x\t9999999999\n").unwrap();
        assert_eq!(write_deny(dir.path(), &d).unwrap(), ["::t0aaaaa", "::t0bbbbb"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "::t0aaaaa\n::t0bbbbb\n");
        write_deny(dir.path(), &spira_config::unit_tier::DenyList::default()).unwrap();
        assert!(!path.exists());
    }

    #[test_id(tc4f3k9)]
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

    #[test_id(tavddip)]
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

    #[test_id(tuefyll)]
    #[test]
    fn a_selected_crate_declaring_test_bins_names_them_and_one_declaring_nothing_adds_none() {
        assert_eq!(test_bins(META, &["rt".into(), "plain".into()]).unwrap(), ["lc"]);
        assert!(test_bins(META, &["plain".into()]).unwrap().is_empty());
        assert!(test_bins(META, &["lc".into()]).unwrap().is_empty());
    }

    #[test_id(ttlfbx8)]
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

    #[test_id(tb2k8a5)]
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
        let m = |name: &str, dir: &str| gate::compose::Member { name: name.into(), dir: dir.into(), deps: vec![], build_deps: vec![], targets: Default::default(), test_bins: vec![], bins: vec![] };
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

    #[test_id(tqflkvz)]
    #[test]
    fn a_test_bin_that_is_not_a_workspace_crate_is_refused() {
        assert!(test_bins(META, &["bad".into()]).unwrap_err().contains("ghost"));
    }

    #[test_id(tiestaa)]
    #[test]
    fn the_finished_line_gives_the_build_seconds_and_no_finished_line_gives_none() {
        let fin = |t: &str| format!("   Compiling a v0.1.0\n    Finished `dwarf` profile [unoptimized] target(s) in {t}\n");
        assert_eq!(build_seconds(&fin("1m 06s")), Some(66.0));
        assert_eq!(build_seconds(&fin("7.92s")), Some(7.9));
        assert_eq!(build_seconds(&format!("{}{}", fin("1m 06s"), fin("4.0s"))), Some(70.0));
        assert_eq!(build_seconds("   Compiling a v0.1.0\nerror: could not compile\n"), None);
    }

    #[test_id(t0l3asb)]
    #[test]
    fn read_back_counts_compiled_units_and_names_the_failing_tests() {
        let out = "   Compiling a v1\n   Compiling b v1\n    Finished x in 2.0s\nunit-tests: skipped (needs a running system): t1 t2\ntest m::bad ... FAILED\n";
        let mut r = gate::unitrecord::UnitRecord::default();
        read_back(out, &mut r);
        assert_eq!((r.compiled, r.fresh, r.build_s, r.skipped), (Some(2), None, Some(2.0), Some(2)));
        assert_eq!(r.failing, vec!["m::bad".to_string()]);
    }
}
