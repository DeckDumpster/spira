//! `gate fixture-config --tree <dir> [--base <rev>]`: the tree's complete test fixture config
//! must resolve against every key the tree's registry reads, and against the registry the
//! landing ref (`--base`) reads — a round runs the code in force against the tree's fixture,
//! so a key the landing ref still requires cannot leave it. A refusal names the key, instead
//! of surfacing as a harness fault when a round computes its image.

use spira_config::resolve::{default_conf_d, resolve, ResolveInput, Resolved};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub const FIXTURE: &str = "spira/testlib/complete.toml";
pub const FENCE: &str = "fixture-config";
static SCRATCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The number of registry keys the fixture resolved, or the refusal naming the key it cannot.
pub fn check(tree: &Path, base: Option<&str>) -> Result<usize, String> {
    let fixture = tree.join(FIXTURE);
    let home = tree.join("spira");
    let doc = tree_doc(&fixture, &home)?;
    let resolved = resolve_fixture(tree, &doc, &default_conf_d(&home), "this tree's config keys")?;
    if let Some(base) = base {
        against_rev(tree, base, &format!("the landing ref {base}"))?;
        merged_delta(tree, base)?;
    }
    Ok(resolved.values.len())
}

fn resolve_fixture(tree: &Path, doc: &spira_config::SpiraToml, conf_d: &Path, what: &str) -> Result<Resolved, String> {
    let env = BTreeMap::new();
    resolve(ResolveInput { env: &env, home: &tree.join("spira"), repo: tree, toml: Some(doc), conf_d })
        .map_err(|e| format!("{FIXTURE} does not resolve against {what}: {e}"))
}

/// The tree's fixture must resolve against the key registry of `rev`, the code that reads it;
/// `what` names `rev` in a refusal.
pub fn against_rev(tree: &Path, rev: &str, what: &str) -> Result<(), String> {
    let fixture = tree.join(FIXTURE);
    let home = tree.join("spira");
    let scratch = std::env::temp_dir().join(format!("gate-fixture-config-{}-{}", std::process::id(), SCRATCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch for {what}'s keys: {e}"))?;
    // batch-job: a gate step that extracts the landing ref under full gate load
    let archive = Command::new("timeout").args(["60", "git", "-C"]).arg(tree).args(["archive", rev, "spira/conf.d"]).output()
        .map_err(|e| format!("git archive {rev}: {e}"))?;
    if !archive.status.success() {
        return Err(format!("git archive {rev} spira/conf.d: {}", String::from_utf8_lossy(&archive.stderr).trim()));
    }
    // batch-job: a gate step that extracts the landing ref under full gate load
    let mut tar = Command::new("timeout").args(["60", "tar", "-x"]).arg("-C").arg(&scratch).stdin(std::process::Stdio::piped()).spawn()
        .map_err(|e| format!("tar: {e}"))?;
    let written = std::io::Write::write_all(&mut tar.stdin.take().unwrap(), &archive.stdout);
    let unpacked = tar.wait().map_err(|e| format!("tar: {e}"))?.success();
    if let (Err(e), false) = (&written, unpacked) {
        return Err(format!("tar did not unpack {what}'s config keys: {e}"));
    }
    let landed = unpacked.then(|| landing_doc(&fixture, &scratch.join("spira")).and_then(|landing| resolve_fixture(tree, &landing, &scratch.join("spira/conf.d"), &format!("{what}'s config keys (a round runs that code against this fixture)"))));
    let landed = landed.map(|r| r.map_err(|e| match key_new_on_branch(&e, &home.join("conf.d"), &scratch.join("spira/conf.d")) {
        Some(k) => format!("{e}; {k} is new on this branch (spira/conf.d/{k} is not on {rev}): let it resolve from its conf.d file in the generic pass, with no hand-written step in spira-config/src/resolve.rs and no COMPUTED_KEYS entry in spira-config/src/process.rs"),
        None => e,
    }));
    let _ = std::fs::remove_dir_all(&scratch);
    landed.unwrap_or_else(|| Err(format!("could not unpack {what}'s config keys")))?;
    Ok(())
}

/// The key a resolve refusal is about, when only the tree registers it.
fn key_new_on_branch<'a>(refusal: &'a str, tree_conf_d: &Path, base_conf_d: &Path) -> Option<&'a str> {
    let (before, _) = refusal.split_once(spira_config::resolve::NOT_IN_REGISTRY)?;
    let key = before.trim_end().rsplit(' ').next()?;
    (tree_conf_d.join(key).is_file() && !base_conf_d.join(key).exists()).then_some(key)
}

pub const DELTA: &str = "spira/config-delta.toml";

/// The config delta of this tree's commit merged with the landing ref must still parse. Two
/// branches that each add a `removed` line merge without a conflict into a file with two, which
/// no round can build; the branch that is behind is told here, before a round carries it.
fn merged_delta(tree: &Path, base: &str) -> Result<(), String> {
    let git = |args: &[&str]| Command::new("timeout").args(["5", "git", "-C"]).arg(tree).args(args).output().map_err(|e| format!("git {}: {e}", args.join(" ")));
    // A tree that already contains the landing ref (a round's merged tree) is its own merge.
    let merged = if git(&["merge-base", "--is-ancestor", base, "HEAD"])?.status.success() {
        "HEAD".to_string()
    } else {
        let merge = git(&["-c", &format!("attr.tree={base}"), "merge-tree", "--write-tree", base, "HEAD"])?;
        match merge.status.code() {
            Some(0) => String::from_utf8_lossy(&merge.stdout).lines().next().unwrap_or("").trim().to_string(),
            // A textual conflict is the merge's own refusal, made where the branch is merged.
            Some(1) => return Ok(()),
            // The deadline, not a verdict: a round judges its merged tree's delta in its own pass.
            Some(124) | None => {
                eprintln!("gate fixture-config: merging this tree with {base} took over 5 s; the merged config delta is not judged here");
                return Ok(());
            }
            Some(rc) => return Err(format!("could not merge this tree with the landing ref {base} to read its config delta (git merge-tree exited {rc}): {}", String::from_utf8_lossy(&merge.stderr).trim())),
        }
    };
    let fixture = git(&["show", &format!("{merged}:{FIXTURE}")])?;
    if fixture.status.success() {
        merged_fixture_parses(&String::from_utf8_lossy(&fixture.stdout), base)?;
    }
    let shown = git(&["show", &format!("{merged}:{DELTA}")])?;
    if !shown.status.success() {
        return Ok(());
    }
    merged_delta_parses(&String::from_utf8_lossy(&shown.stdout), base)
}

fn merged_fixture_parses(text: &str, base: &str) -> Result<(), String> {
    text.parse::<toml::Table>().map(drop).map_err(|e| {
        format!("{FIXTURE} of this tree merged with the landing ref {base} does not parse: {} — rebase onto the landing ref and keep one definition of the key", e.message())
    })
}

fn merged_delta_parses(text: &str, base: &str) -> Result<(), String> {
    spira_config::delta_added(text).map(drop).map_err(|e| {
        format!("{DELTA} of this tree merged with the landing ref {base} does not parse: {e} — rebase onto the landing ref and fold your entries into the lines it already has")
    })
}

/// The fixture read against this tree's own registry, never the running binary's: a key the tree
/// adds (with its conf.d file and delta) is not in the binary a round runs the fence with.
fn tree_doc(fixture: &Path, home: &Path) -> Result<spira_config::SpiraToml, String> {
    let text = std::fs::read_to_string(fixture).map_err(|e| format!("{FIXTURE}: {e}"))?;
    let reg = spira_config::registry::Registry::load(home)?;
    spira_config::validate_with(&text, &reg).map(|(doc, _)| doc).map_err(|e| format!("{FIXTURE}: {e}"))
}

/// The fixture read against the landing ref's registry, so a key the tip's registry retired is
/// still declared for the code that has not yet.
fn landing_doc(fixture: &Path, home: &Path) -> Result<spira_config::SpiraToml, String> {
    let text = std::fs::read_to_string(fixture).map_err(|e| format!("{FIXTURE}: {e}"))?;
    std::fs::create_dir_all(home.join("conf.toml.d")).map_err(|e| format!("scratch for the landing ref's keys: {e}"))?;
    let reg = spira_config::registry::Registry::load(home)?;
    spira_config::validate_with(&text, &reg).map(|(doc, _)| doc).map_err(|e| format!("{FIXTURE}: {e}"))
}

pub fn main(args: &[String]) -> i32 {
    let (tree, base) = match args {
        [flag, tree] if flag == "--tree" => (tree, None),
        [f1, tree, f2, base] if f1 == "--tree" && f2 == "--base" => (tree, Some(base.as_str())),
        _ => {
            eprintln!("usage: gate fixture-config --tree <dir> [--base <rev>]");
            return 2;
        }
    };
    match check(Path::new(tree), base) {
        Ok(n) => {
            println!("fence: {FENCE} checked {n} keys");
            0
        }
        Err(e) => {
            eprintln!("gate fixture-config: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_tree() -> testkit::TempDir {
        let t = testkit::TempDir::new("gate-fixture-config");
        let src = spira_config::resolve::locate_home_for_process().unwrap();
        let dst = t.path().join("spira");
        std::fs::create_dir_all(dst.join("testlib")).unwrap();
        std::fs::copy(src.join("testlib/complete.toml"), dst.join("testlib/complete.toml")).unwrap();
        for dir in ["conf.d", "conf.toml.d"] {
            std::fs::create_dir_all(dst.join(dir)).unwrap();
            for e in std::fs::read_dir(src.join(dir)).unwrap() {
                let e = e.unwrap();
                std::fs::copy(e.path(), dst.join(dir).join(e.file_name())).unwrap();
            }
        }
        t
    }

    #[test_id(typ5138)]
    #[test]
    fn the_shipped_fixture_resolves_every_key() {
        let t = copy_tree();
        assert!(check(t.path(), None).unwrap() > 0);
    }

    #[test_id(t02fobo)]
    #[test]
    fn a_tip_that_retires_a_fixture_key_the_tree_still_reads_is_refused_naming_it() {
        let t = copy_tree();
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
        assert_ne!(text.len(), kept.len(), "the planted offender must exist");
        std::fs::write(&f, kept).unwrap();
        let e = check(t.path(), None).unwrap_err();
        assert!(e.contains("SPIRA_ARCHIVIST_EVERY"), "{e}");
    }

    #[test_id(tyh3k3v)]
    #[test]
    fn a_tip_that_retires_a_fixture_key_the_landing_ref_still_requires_is_refused_naming_it() {
        let t = copy_tree();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        let key = t.path().join("spira/conf.d/SPIRA_ARCHIVIST_EVERY");
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
        assert_ne!(text.len(), kept.len(), "the planted offender must exist");
        std::fs::write(&f, kept).unwrap();
        std::fs::remove_file(&key).unwrap();
        check(t.path(), None).expect("the tip alone is consistent: it retired the key from both");
        let e = check(t.path(), Some("main")).unwrap_err();
        assert!(e.contains("SPIRA_ARCHIVIST_EVERY") && e.contains("landing ref"), "{e}");
    }

    #[test_id(tv0nmvr)]
    #[test]
    fn a_delta_with_two_removed_lines_is_refused_naming_the_landing_ref() {
        let e = merged_delta_parses("removed = [\"spira.a\"]\nremoved = [\"spira.b\"]\n[spira]\nx = \"1\"\n", "main").unwrap_err();
        assert!(e.contains("merged with the landing ref main") && e.contains("rebase"), "{e}");
        merged_delta_parses("removed = [\"spira.a\", \"spira.b\"]\n[spira]\nx = \"1\"\n", "main").expect("one removed line parses");
    }

    #[test_id(t9zjiot)]
    #[test]
    fn a_tip_whose_delta_only_breaks_once_merged_with_the_landing_ref_is_refused() {
        let t = copy_tree();
        let delta = t.path().join(DELTA);
        std::fs::write(&delta, "# one\n# two\n# three\n# four\n[spira]\nx = \"1\"\n").unwrap();
        let git = |args: &[&str]| testkit::git(t.path(), args);
        let commit = |msg: &str| git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", msg]);
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        commit("base");
        git(&["checkout", "-q", "-b", "tip"]);
        std::fs::write(&delta, "# one\n# two\n# three\n# four\nremoved = [\"spira.b\"]\n[spira]\nx = \"1\"\n").unwrap();
        commit("the tip retires a key");
        check(t.path(), Some("main")).expect("alone, and on its own base, the tip's delta parses");
        git(&["checkout", "-q", "main"]);
        std::fs::write(&delta, "# one\nremoved = [\"spira.a\"]\n# two\n# three\n# four\n[spira]\nx = \"1\"\n").unwrap();
        commit("the landing ref retires another");
        git(&["checkout", "-q", "tip"]);
        let e = check(t.path(), Some("main")).unwrap_err();
        assert!(e.contains(DELTA) && e.contains("merged with the landing ref main") && e.contains("duplicate key"), "{e}");
    }

    #[test_id(ti8tblx)]
    #[test]
    fn a_merge_that_defines_a_fixture_key_twice_is_refused_naming_the_file_and_key() {
        let t = copy_tree();
        let f = t.path().join(FIXTURE);
        std::fs::write(t.path().join(".gitattributes"), format!("{FIXTURE} merge=union\n")).unwrap();
        let git = |args: &[&str]| testkit::git(t.path(), args);
        let commit = |msg: &str| git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", msg]);
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        commit("base");
        git(&["checkout", "-q", "-b", "tip"]);
        let base = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&f, base.replacen("[spira]\n", "[spira]\nplanted_twice = \"1\"\n", 1)).unwrap();
        commit("the tip defines the key");
        git(&["checkout", "-q", "main"]);
        std::fs::write(&f, base.replacen("task_modes = ", "planted_twice = \"2\"\ntask_modes = ", 1)).unwrap();
        commit("the landing ref defines it too, far from the tip's line");
        git(&["checkout", "-q", "tip"]);
        let e = merged_delta(t.path(), "main").unwrap_err();
        assert!(e.contains(FIXTURE) && e.contains("planted_twice"), "{e}");
    }

    #[test_id(te49bfy)]
    #[test]
    fn a_tree_that_contains_the_landing_ref_is_judged_on_its_own_delta_without_a_merge() {
        // A round's merged tree: the base is an ancestor, so nothing is merged (a slow merge
        // once timed out here and read as a red fence for a whole round).
        let t = copy_tree();
        let delta = t.path().join(DELTA);
        std::fs::write(&delta, "[spira]\nx = \"1\"\n").unwrap();
        let git = |args: &[&str]| testkit::git(t.path(), args);
        let commit = |msg: &str| git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", msg]);
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        commit("base");
        git(&["checkout", "-q", "-b", "round"]);
        std::fs::write(&delta, "removed = [\"spira.a\"]\n[spira]\nx = \"1\"\n").unwrap();
        commit("a member retires a key");
        check(t.path(), Some("main")).expect("one removed line on top of the base parses");
        std::fs::write(&delta, "removed = [\"spira.a\"]\nremoved = [\"spira.b\"]\n[spira]\nx = \"1\"\n").unwrap();
        commit("a second member's line was merged in beside it");
        let e = check(t.path(), Some("main")).unwrap_err();
        assert!(e.contains("duplicate key") && e.contains(DELTA), "{e}");
    }

    #[test_id(tgalwjw)]
    #[test]
    fn a_tip_that_deleted_a_key_keeps_its_fixture_line_while_the_landing_ref_requires_it() {
        let t = copy_tree();
        let planted = t.path().join("spira/conf.d/SPIRA_PLANTED_ONLY_HERE");
        std::fs::write(&planted, "TYPE=string\nGROUP=archivist\nDOC=planted\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_PLANTED_ONLY_HERE:=x}\"\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let with_key = text.replacen("archivist_every = ", "planted_only_here = \"x\"\narchivist_every = ", 1);
        std::fs::write(&f, &with_key).unwrap();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        std::fs::remove_file(&planted).unwrap();
        check(t.path(), Some("main")).expect("the fixture still declares the key the landing ref requires");
    }

    #[test_id(tcpgztd)]
    #[test]
    fn a_retired_key_passes_the_fence_against_the_landing_ref_that_still_registers_it() {
        let t = copy_tree();
        std::fs::copy(spira_config::resolve::locate_home_for_process().unwrap().join("config-delta.toml"), t.path().join("spira/config-delta.toml")).unwrap();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        spira_config::retire::retire_key(t.path(), "spira.archivist_every", "sp-x").unwrap();
        assert!(!t.path().join("spira/conf.d/SPIRA_ARCHIVIST_EVERY").exists(), "the key is retired at the tip");
        check(t.path(), Some("main")).expect("the fixture line stays while the landing ref requires it");
    }

    #[test_id(t9r7nnr)]
    #[test]
    fn a_key_the_tree_adds_with_its_conf_d_file_resolves_though_the_running_binary_never_registered_it() {
        let t = copy_tree();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&f, text.replacen("archivist_every = ", "key_only_in_delta = \"3\"\narchivist_every = ", 1)).unwrap();
        std::fs::write(t.path().join(DELTA), "[spira]\nkey_only_in_delta = \"3\"\n").unwrap();
        std::fs::write(t.path().join("spira/conf.d/SPIRA_KEY_ONLY_IN_DELTA"), "TYPE=string\nGROUP=archivist\nDOC=planted\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_KEY_ONLY_IN_DELTA:=3}\"\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        check(t.path(), Some("main")).expect("the tree declares the key in conf.d, the fixture and the delta; the landing ref lacks it");
    }

    #[test_id(tdtemym)]
    #[test]
    fn a_key_new_on_this_branch_that_a_hand_written_step_asks_for_is_refused_naming_the_step_and_the_way_through() {
        let t = copy_tree();
        let key = t.path().join("spira/conf.d/SPIRA_CZAR_OUTCOME_MINS");
        let registered = std::fs::read(&key).unwrap();
        std::fs::remove_file(&key).unwrap();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        std::fs::write(&key, registered).unwrap();
        let e = check(t.path(), Some("main")).unwrap_err();
        for want in ["SPIRA_CZAR_OUTCOME_MINS", "new on this branch", "hand-written step", "resolve.rs"] {
            assert!(e.contains(want), "{want}: {e}");
        }
    }
}
