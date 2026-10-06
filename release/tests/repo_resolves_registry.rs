//! `release build --repo spira` resolves the name through the registry, under a bare env
//! and under a shell whose SPIRA_REPO is the release root.

use release::config::Env;
use release::git::{Git, RealGit};
use std::process::Command;

fn fixture(tag: &str) -> (testkit::TempDir, std::path::PathBuf, String, Env) {
    let t = testkit::TempDir::new(tag);
    let checkout = t.path().join("checkouts/spira");
    std::fs::create_dir_all(&checkout).unwrap();
    let g = |args: &[&str]| {
        let o = Command::new("git").arg("-C").arg(&checkout).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    g(&["init", "-q"]);
    g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
    let sha = g(&["rev-parse", "HEAD"]);
    let map = t.path().join("registry-rows");
    std::fs::write(&map, format!("spira | {} | queue.local | local/main |  |\n", checkout.display())).unwrap();
    let release = t.path().join("spira-releases/deadbeef");
    let rs = release.join("spira");
    std::fs::create_dir_all(&rs).unwrap();
    std::os::unix::fs::symlink(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"), rs.join("conf.d")).unwrap();
    let mut env = Env::new();
    env.insert("HOME".into(), t.path().join("userhome").display().to_string());
    env.insert("SPIRA_RELEASE".into(), release.display().to_string());
    env.insert("SPIRA_REPO_MAP".into(), map.display().to_string());
    env.insert("SPIRA_HOME_REPO".into(), "spira".into());
    (t, checkout, sha, env)
}

#[test]
fn a_repo_name_finds_a_commit_only_the_configured_checkout_has() {
    let (t, checkout, sha, mut env) = fixture("repo-resolve-bare");
    std::env::set_current_dir(t.path()).unwrap();
    // `spira_config::repos::registry_env` (per Ryan 2026-10-05: one source of config) now
    // drops any `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` it finds in the environment outright and
    // resolves both fresh from `$SPIRA_TOML` instead — so `fixture`'s own env entries for
    // them (above) are inert for registry purposes; only read back here to build the real
    // config file `registry_env` now requires. It checks `env.get("SPIRA_TOML")` before
    // falling back to the real process environment, so inserting it into this `env` map is
    // enough — no `testkit::env`/real env var mutation needed for this one.
    let toml = spira_config::process::fixture_toml(t.path(), &[("SPIRA_REPO_MAP", &env["SPIRA_REPO_MAP"]), ("SPIRA_HOME_REPO", &env["SPIRA_HOME_REPO"])]);
    env.insert("SPIRA_TOML".into(), toml.display().to_string());
    let r = release::repo::resolve(Some(std::path::Path::new("spira")), &env).unwrap();
    assert_eq!(r, checkout);
    assert_eq!(RealGit.resolve(&r, &sha).unwrap(), sha);

    env.insert("SPIRA_REPO".into(), env["SPIRA_RELEASE"].clone());
    let r = release::repo::resolve(Some(std::path::Path::new("spira")), &env).unwrap();
    assert_eq!(r, checkout);
    let r = release::repo::resolve(None, &env).unwrap();
    assert_eq!(r, checkout);
}

#[test]
fn a_path_is_left_alone() {
    let (_t, _c, _s, env) = fixture("repo-resolve-path");
    let p = std::path::Path::new("/some/where");
    assert_eq!(release::repo::resolve(Some(p), &env).unwrap(), p);
}
