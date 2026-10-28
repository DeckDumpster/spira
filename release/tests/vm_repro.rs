//! `release vm-repro` end to end under no inherited environment: a real fixture repository
//! with a member branch, a real detached worktree, and a release whose `cargo` builds
//! nothing and whose `testenv` stands for the tree's own runner.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_CARGO: &str = r#"#!/bin/sh
echo "cargo $*"
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

fn fixture(name: &str, plant: bool) -> Fixture {
    let tmp = testkit::TempDir::new(&format!("vm-repro-{name}"));
    let root = tmp.path().to_path_buf();
    let (repo, release, run) = (root.join("repo"), root.join("release"), root.join("run"));
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(release.join("bin")).unwrap();
    fs::create_dir_all(release.join("spira")).unwrap();
    testkit::write_exe(release.join("bin/cargo"), FAKE_CARGO);
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README"), "x").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "spira/sp-fix"]);
    if plant {
        fs::write(repo.join("PLANT_INSTALL_FAILURE"), "x").unwrap();
    }
    fs::write(repo.join("member"), "y").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "member"]);
    git(&repo, &["checkout", "-q", "main"]);
    let over = root.join("over.toml");
    fs::write(&over, format!("[spira]\nrun = \"{}\"\npath = \"\"\n", run.display())).unwrap();
    let complete = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml");
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
    let f = fixture("green", false);
    let o = vm_repro(&f, &["--suites", "test-one.sh"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("VERDICT GREEN"), "{out}");
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
    let f = fixture("fault", true);
    let o = vm_repro(&f, &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(3), "{out}");
    assert!(out.contains("VERDICT FAULT"), "{out}");
    let t = fs::read_to_string(f.run.join("vm-repro/sp-fix/testenv.log")).unwrap();
    assert!(t.contains("install: unit spira-fixture.service failed to start"), "{t}");
    assert!(!t.contains("--suites"), "no --suites unless given: {t}");
}

#[test]
fn a_branch_that_does_not_exist_is_refused_before_anything_is_built() {
    let f = fixture("nobranch", false);
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
