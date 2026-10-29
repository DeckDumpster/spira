//! `release vm-repro` end to end under no inherited environment: a real fixture repository
//! with a member branch, a real detached worktree, and a release whose `cargo` builds
//! nothing and whose `testenv` stands for the tree's own runner.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_CARGO: &str = r#"#!/bin/sh
echo "cargo $*"
if [ -e PLANT_BUILD_FAILURE ]; then echo "error[E0425]: cannot find value"; exit 101; fi
echo "RUSTC_WRAPPER=$RUSTC_WRAPPER SPIRA_RELEASE=$SPIRA_RELEASE SPIRA_TOML=$SPIRA_TOML"
mkdir -p target/release
cat > target/release/testenv <<'T'
#!/bin/sh
echo "testenv $*"
if [ -e PLANT_INSTALL_FAILURE ]; then
    echo "install: unit spira-fixture.service failed to start"
    echo "VERDICT FAULT rc=2 reason=install"
    exit 2
fi
if [ -e PLANT_RED ]; then
    echo "VERDICT RED rc=1 red=test-one.sh"
    exit 1
fi
echo "  test-one.sh                      ok      1s"
echo "VERDICT GREEN rc=0"
T
chmod +x target/release/testenv
"#;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).status().unwrap();
    assert!(st.success(), "git {args:?}");
}

struct Fixture {
    _tmp: testkit::TempDir,
    root: PathBuf,
    repo: PathBuf,
    release: PathBuf,
    run: PathBuf,
    toml: String,
}

fn fixture(name: &str, files: &[(&str, &str)]) -> Fixture {
    let tmp = testkit::TempDir::new(&format!("vm-repro-{name}"));
    let root = tmp.path().to_path_buf();
    let (repo, release, run) = (root.join("repo"), root.join("release"), root.join("run"));
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(release.join("bin")).unwrap();
    spira_config::process::fixture_release(&root, true);
    testkit::write_exe(release.join("bin/cargo"), FAKE_CARGO);
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README"), "x").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "spira/sp-fix"]);
    for (n, body) in files {
        fs::create_dir_all(repo.join(n).parent().unwrap()).unwrap();
        fs::write(repo.join(n), body).unwrap();
    }
    fs::write(repo.join("member"), "y").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "member"]);
    git(&repo, &["checkout", "-q", "main"]);
    let over = root.join("over.toml");
    fs::write(&over, format!("[spira]\nrun = \"{}\"\npath = \"\"\n", run.display())).unwrap();
    let complete = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/testlib/complete.toml");
    let toml = format!("{}:{}", complete.display(), over.display());
    Fixture { _tmp: tmp, root, repo, release, run, toml }
}

fn vm_repro(f: &Fixture, extra: &[&str]) -> Output {
    let home = f.root.join("home");
    fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_release"))
        .env_clear()
        .env("HOME", &home)
        .env("SPIRA_TOML", &f.toml)
        .arg("vm-repro")
        .arg("sp-fix")
        .arg("--release")
        .arg(&f.release)
        .arg("--repo")
        .arg(&f.repo)
        .args(extra)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn a_member_branch_builds_runs_its_suite_and_prints_green() {
    let f = fixture("green", &[]);
    let o = vm_repro(&f, &["--suites", "test-one.sh"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("VERDICT GREEN"), "{out}");
    assert!(out.contains("executed 1 suite(s), lanes: container(1)") && out.contains("NOT REPRODUCED"), "{out}");
    let logs = f.run.join("vm-repro/sp-fix");
    let build = fs::read_to_string(logs.join("build.log")).unwrap();
    assert!(build.contains("cargo build --profile release --workspace --config profile.release.incremental=false"), "{build}");
    assert!(build.contains(&format!("SPIRA_RELEASE={}", f.release.display())), "{build}");
    assert!(build.contains("SPIRA_TOML=") && build.contains("over.toml"), "{build}");
    let t = fs::read_to_string(logs.join("testenv.log")).unwrap();
    let tree = f.run.join("worktree/vm-repro-sp-fix");
    let want = format!("--mode parallel --artifacts {}/target/release --suites test-one.sh --deadline 600 spira/sp-fix {}", tree.display(), f.repo.display());
    assert!(t.contains(&want), "{t}");
    assert!(!tree.exists(), "the worktree is removed when it finishes");
    let listed = Command::new("git").arg("-C").arg(&f.repo).args(["worktree", "list"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("vm-repro-sp-fix"));
}

#[test]
fn a_planted_install_failure_prints_fault_and_keeps_the_install_output() {
    let f = fixture("fault", &[("PLANT_INSTALL_FAILURE", "")]);
    let o = vm_repro(&f, &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(3), "{out}");
    assert!(out.contains("VERDICT FAULT"), "{out}");
    assert!(out.lines().any(|l| l == "REPRODUCED"), "{out}");
    let t = fs::read_to_string(f.run.join("vm-repro/sp-fix/testenv.log")).unwrap();
    assert!(t.contains("install: unit spira-fixture.service failed to start"), "{t}");
    assert!(!t.contains("--suites"), "no --suites unless given: {t}");
}

#[test]
fn a_branch_that_does_not_exist_is_refused_before_anything_is_built() {
    let f = fixture("nobranch", &[]);
    let o = Command::new(env!("CARGO_BIN_EXE_release"))
        .env_clear()
        .env("HOME", &f.root)
        .env("SPIRA_TOML", &f.toml)
        .args(["vm-repro", "sp-missing", "--release"])
        .arg(&f.release)
        .arg("--repo")
        .arg(&f.repo)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("spira/sp-missing"), "{}", text(&o));
    assert!(!f.run.join("worktree").exists());
}

#[test]
fn a_failing_build_is_a_fault_with_its_errors_kept_and_the_worktree_removed() {
    let f = fixture("build", &[("PLANT_BUILD_FAILURE", "")]);
    let o = vm_repro(&f, &[]);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(text(&o).contains("VERDICT FAULT reason=build rc=101"), "{}", text(&o));
    assert!(fs::read_to_string(f.run.join("vm-repro/sp-fix/build.log")).unwrap().contains("error[E0425]"));
    assert!(!f.run.join("worktree/vm-repro-sp-fix").exists());
    assert!(!f.run.join("vm-repro/sp-fix/testenv.log").exists());
}

#[test]
fn a_red_suite_exits_one_and_still_removes_the_worktree() {
    let f = fixture("red", &[("PLANT_RED", "")]);
    let o = vm_repro(&f, &["--suites", "test-one.sh"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("VERDICT RED"), "{}", text(&o));
    assert!(!f.run.join("worktree/vm-repro-sp-fix").exists());
}

#[test]
fn a_worktree_left_by_a_killed_run_is_replaced() {
    let f = fixture("stale", &[]);
    let stale = f.run.join("worktree/vm-repro-sp-fix");
    fs::create_dir_all(f.run.join("worktree")).unwrap();
    git(&f.repo, &["worktree", "add", "--detach", stale.to_str().unwrap(), "main"]);
    fs::write(stale.join("leftover"), "x").unwrap();
    let o = vm_repro(&f, &["--suites", "test-one.sh"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(!stale.exists());
}

#[test]
fn the_suites_cover_the_sim_lane_and_a_suite_off_the_skip_allowlist() {
    let f = fixture(
        "lanes",
        &[
            ("spira/test-a-skips.sh", "#!/bin/bash\nset -uo pipefail\n"),
            ("spira/test-b-sim.sh", "#!/bin/bash\n# lane: sim\nset -uo pipefail\n"),
            ("spira/test-c-plain.sh", "#!/bin/bash\nset -uo pipefail\n"),
            ("spira/skip-allowlist.tsv", "# comment\ntest-a-skips.sh\tskip:x\twhy\n"),
        ],
    );
    let o = vm_repro(&f, &["--suites", "test-a-skips.sh"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("added test-b-sim.sh") && out.contains("added test-c-plain.sh"), "{out}");
    let t = fs::read_to_string(f.run.join("vm-repro/sp-fix/testenv.log")).unwrap();
    assert!(t.contains("--suites test-a-skips.sh,test-b-sim.sh,test-c-plain.sh --deadline"), "{t}");
}

fn tree_with_suites() -> Vec<(&'static str, &'static str)> {
    vec![
        ("spira/test-a-skips.sh", "#!/bin/bash\nset -uo pipefail\n"),
        ("spira/test-b-sim.sh", "#!/bin/bash\n# lane: sim\nset -uo pipefail\n"),
        ("spira/test-c-plain.sh", "#!/bin/bash\nset -uo pipefail\n"),
        ("spira/skip-allowlist.tsv", "test-a-skips.sh\tskip:x\twhy\n"),
    ]
}

#[test]
fn without_suites_the_rounds_red_suites_are_run() {
    let f = fixture("round-red", &tree_with_suites());
    let batches = r#"[{"batch_id":"b","members":[{"bead_id":"sp-fix"}],"passes":[{"n":1,"verdict":"red","red_suites":["test-a-skips.sh"]}]}]"#;
    testkit::write_exe(f.release.join("bin/spira-lc"), &format!("#!/bin/sh\n[ \"$1 $2\" = 'list --batches' ] || exit 1\ncat <<'J'\n{batches}\nJ\n"));
    let o = vm_repro(&f, &[]);
    let out = text(&o);
    assert!(out.contains("defaulted to the round's red suites: test-a-skips.sh"), "{out}");
    let t = fs::read_to_string(f.run.join("vm-repro/sp-fix/testenv.log")).unwrap();
    assert!(t.contains("--suites test-a-skips.sh,test-b-sim.sh,test-c-plain.sh --deadline"), "{t}");
}

#[test]
fn an_unreadable_round_is_said_and_the_default_selection_runs() {
    let f = fixture("round-unreadable", &tree_with_suites());
    testkit::write_exe(f.release.join("bin/spira-lc"), "#!/bin/sh\necho down >&2\nexit 3\n");
    let o = vm_repro(&f, &[]);
    let out = text(&o);
    assert!(out.contains("could not read the round's red suites"), "{out}");
    let t = fs::read_to_string(f.run.join("vm-repro/sp-fix/testenv.log")).unwrap();
    assert!(t.contains("--suites test-b-sim.sh,test-c-plain.sh --deadline"), "{t}");
}

#[test]
fn a_round_with_no_red_pass_is_said_and_the_default_selection_runs() {
    let f = fixture("round-none", &tree_with_suites());
    testkit::write_exe(f.release.join("bin/spira-lc"), "#!/bin/sh\necho '[]'\n");
    let out = text(&vm_repro(&f, &[]));
    assert!(out.contains("no red suites recorded for sp-fix in the last 14 days"), "{out}");
}
