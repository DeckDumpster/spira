#[macro_use]
extern crate spira_test_id;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) {
    let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@t"];
    all.extend_from_slice(args);
    testkit::git(dir, &all);
}

fn commit(dir: &Path, name: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", name]);
    git(dir, &["tag", name]);
}

/// R is the active release; L retires a key from the registry; the branch tip drops its fixture line.
fn fixture() -> testkit::TempDir {
    let d = testkit::TempDir::new("reads-gate");
    let src = spira_config::resolve::locate_home_for_process().unwrap();
    let dst = d.path().join("spira");
    std::fs::create_dir_all(dst.join("testlib")).unwrap();
    std::fs::copy(src.join("testlib/complete.toml"), dst.join("testlib/complete.toml")).unwrap();
    for dir in ["conf.d", "conf.toml.d"] {
        std::fs::create_dir_all(dst.join(dir)).unwrap();
        for e in std::fs::read_dir(src.join(dir)).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), dst.join(dir).join(e.file_name())).unwrap();
        }
    }
    std::fs::write(d.path().join("flake.lock"), "{}\n").unwrap();
    std::fs::write(d.path().join("notes.md"), "n\n").unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    commit(d.path(), "R");
    std::fs::remove_file(dst.join("conf.d/SPIRA_ARCHIVIST_EVERY")).unwrap();
    commit(d.path(), "L");
    git(d.path(), &["checkout", "-q", "-b", "work"]);
    let f = dst.join("testlib/complete.toml");
    let text = std::fs::read_to_string(&f).unwrap();
    let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
    assert_ne!(text.len(), kept.len(), "the planted offender must exist");
    std::fs::write(&f, kept).unwrap();
    commit(d.path(), "T");
    d
}

#[test_id(tiku441)]
#[test]
fn dropping_a_fixture_key_the_active_release_still_requires_is_refused_naming_it_and_the_release() {
    let d = fixture();
    let e = work::reads::gate(d.path(), "L", "R").unwrap_err();
    assert!(e.contains("SPIRA_ARCHIVIST_EVERY") && e.contains("the active release R"), "{e}");
}

#[test_id(t6l0dpo)]
#[test]
fn the_same_removal_passes_once_the_release_carrying_the_first_step_is_in_effect() {
    let d = fixture();
    work::reads::gate(d.path(), "L", "L").unwrap();
}

#[test_id(thuod8m)]
#[test]
fn deleting_a_recipe_input_the_release_declares_is_refused_naming_it() {
    let d = fixture();
    git(d.path(), &["rm", "-q", "flake.lock"]);
    commit(d.path(), "T2");
    let e = work::reads::gate(d.path(), "L", "L").unwrap_err();
    assert!(e.contains("flake.lock"), "{e}");
}

#[test_id(tw7nz70)]
#[test]
fn deleting_only_an_undeclared_file_passes() {
    let d = fixture();
    git(d.path(), &["rm", "-q", "notes.md"]);
    commit(d.path(), "T2");
    work::reads::gate(d.path(), "L", "L").unwrap();
}

#[test_id(t8c7zor)]
#[test]
fn a_branch_forked_before_the_release_gained_a_key_and_dropping_none_passes() {
    let d = fixture();
    let (key, file) = (d.path().join("spira/conf.d/SPIRA_ARCHIVIST_EVERY"), d.path().join("spira/testlib/complete.toml"));
    git(d.path(), &["checkout", "-q", "-b", "stale", "R"]);
    let (registered, complete) = (std::fs::read_to_string(&key).unwrap(), std::fs::read_to_string(&file).unwrap());
    let before: String = complete.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
    std::fs::remove_file(&key).unwrap();
    std::fs::write(&file, &before).unwrap();
    commit(d.path(), "before-the-key");
    std::fs::write(&key, registered).unwrap();
    std::fs::write(&file, complete).unwrap();
    commit(d.path(), "release-with-the-key");
    git(d.path(), &["checkout", "-q", "-b", "forked", "before-the-key"]);
    std::fs::write(&file, before.replacen("[spira]\n", "[spira]\nplanted_by_branch = \"1\"\n", 1)).unwrap();
    commit(d.path(), "adds-a-key");
    work::reads::gate(d.path(), "release-with-the-key", "release-with-the-key").unwrap();
}

#[test_id(t130aa3)]
#[test]
fn a_branch_deleting_an_input_and_dropping_a_required_key_is_told_of_both() {
    let d = fixture();
    git(d.path(), &["rm", "-q", "flake.lock"]);
    commit(d.path(), "T2");
    let e = work::reads::gate(d.path(), "L", "R").unwrap_err();
    assert!(e.contains("flake.lock") && e.contains("SPIRA_ARCHIVIST_EVERY"), "{e}");
}

#[test_id(tgkud0g)]
#[test]
fn the_active_release_is_the_commit_its_directory_is_named_for_through_a_symlink() {
    let d = fixture();
    let sha = testkit::git(d.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let releases = testkit::TempDir::new("releases");
    std::fs::create_dir(releases.path().join(&sha)).unwrap();
    std::os::unix::fs::symlink(releases.path().join(&sha), releases.path().join("current")).unwrap();
    let current = releases.path().join("current");
    assert_eq!(work::reads::release_commit(d.path(), current.to_str()), Ok(sha));
}

#[test_id(te3vmzk)]
#[test]
fn a_release_that_cannot_be_identified_refuses_saying_why() {
    let d = fixture();
    let releases = testkit::TempDir::new("releases");
    std::fs::create_dir(releases.path().join("not-a-commit")).unwrap();
    let why = |release: Option<&str>| work::reads::release_commit(d.path(), release).unwrap_err();
    assert!(why(None).contains("SPIRA_RELEASE is not set"));
    assert!(why(Some("")).contains("SPIRA_RELEASE is not set"));
    assert!(why(releases.path().join("absent").to_str()).contains("cannot be resolved"));
    assert!(why(releases.path().join("not-a-commit").to_str()).contains("finds no commit not-a-commit"));
}
