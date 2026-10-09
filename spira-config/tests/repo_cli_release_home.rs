//! `spira-config repo landref <name>` through the real binary, for a home repo whose registry
//! row names a checkout while `SPIRA_HOME`/`SPIRA_REPO` are a release directory (no `.git`).

use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed");
}

fn repo_cmd(verb: &str, release: &Path, toml: &Path, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("SPIRA_HOME", release)
        .env("SPIRA_REPO", release)
        .env("SPIRA_TOML", toml)
        .args(["repo", verb, "sim"])
        .output()
        .expect("spira-config runs")
}

#[test]
fn a_release_home_resolves_the_mapped_checkouts_land_ref() {
    let t = testkit::TempDir::new("spira-config-repo-cli-release-home");
    let checkout = t.join("work");
    std::fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "-q", "--initial-branch=main"]);
    git(&checkout, &["commit", "-q", "--allow-empty", "-m", "seed"]);
    git(&checkout, &["branch", "local/main", "main"]);
    let map = t.join("rows");
    std::fs::write(&map, format!("sim|{}|queue.local|local/main|||plan\n", checkout.display())).unwrap();
    let release = t.join("release/spira");
    std::fs::create_dir_all(&release).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"), release.join("conf.d")).unwrap();
    let toml = spira_config::process::fixture_toml(
        &t.path(),
        &[("SPIRA_REPO_MAP", &map.display().to_string()), ("SPIRA_HOME_REPO", "sim")],
    );
    let home = t.join("userhome");

    let root = repo_cmd("root", &release, &toml, &home);
    assert_eq!(String::from_utf8_lossy(&root.stdout).trim(), checkout.display().to_string(), "{}", String::from_utf8_lossy(&root.stderr));
    let o = repo_cmd("landref", &release, &toml, &home);
    assert!(o.status.success(), "landref refused: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "local/main");
}
