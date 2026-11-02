//! cargo's target runner (`tools/test-sandbox/runner`, named by `.cargo/config.toml`) builds the
//! same sandbox testenv's sandbox unit runner does (`testenv::sandbox::Profile`), so a test sees
//! one policy however it was started. What a binary can reach inside it is
//! spira/test-test-sandbox.sh's to prove: a Rust test runs inside the runner already.
//!
//! THIS TEST SPAWNS A BINARY, an exception to law-a-unit-test-calls-no-binary written here as it
//! requires: the runner is a shell script cargo executes, so its arguments exist only as what it
//! prints. It runs in `--print-args` mode, which starts nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use testenv::sandbox::Profile;
use testkit::TempDir;

/// The arguments following each `flag`, in order.
fn after(a: &[String], flag: &str, n: usize) -> Vec<Vec<String>> {
    a.windows(n + 1).filter(|w| w[0] == flag).map(|w| w[1..].to_vec()).collect()
}

#[test]
fn the_runner_builds_the_sandbox_testenvs_profile_builds() {
    let d = TempDir::new("test-sandbox-profile");
    let home = d.join("home");
    for p in [".rustup", ".cargo/bin", ".cargo/registry", ".cargo/git", ".local/lib/spira-env"] {
        fs::create_dir_all(home.join(p)).unwrap();
    }
    let target = d.join("target");
    fs::create_dir_all(&target).unwrap();
    let me = std::env::current_exe().unwrap();
    let h = |r: &str| home.join(r).to_str().unwrap().to_string();
    let runner = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/test-sandbox/runner");
    // batch-job: the runner only prints its arguments, bounded by its own exit
    let o = Command::new(runner)
        .arg("--print-args")
        .arg(&me)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("RUSTUP_HOME", h(".rustup"))
        .env("CARGO_HOME", h(".cargo"))
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .expect("start the runner");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ours: Vec<String> = String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect();
    let theirs = Profile::for_run(&d.join("tree"), &target, &home, 4).bwrap_args();

    let flags = |a: &[String]| a.iter().filter(|x| x.starts_with("--unshare") || ["--die-with-parent", "--new-session", "--clearenv"].contains(&x.as_str())).cloned().collect::<Vec<_>>();
    for f in flags(&theirs) {
        assert!(ours.contains(&f), "testenv's sandbox has {f}, the runner's does not: {ours:?}");
    }
    assert_eq!(after(&ours, "--ro-bind", 2).first(), after(&theirs, "--ro-bind", 2).first(), "the root is bound read-only first in both");
    for t in after(&theirs, "--tmpfs", 1) {
        assert!(after(&ours, "--tmpfs", 1).contains(&t), "testenv's sandbox puts a tmpfs on {t:?}, the runner does not: {ours:?}");
    }
    let under_home = |a: &[String], flag: &str| {
        let mut v: Vec<String> = after(a, flag, 2).into_iter().map(|p| p[0].clone()).filter(|p| p.starts_with(home.to_str().unwrap())).collect();
        v.sort();
        v
    };
    assert_eq!(under_home(&ours, "--ro-bind"), under_home(&theirs, "--ro-bind"), "the toolchain bound back read-only");
    assert_eq!(under_home(&ours, "--bind"), under_home(&theirs, "--bind"), "the toolchain bound back writable");
    assert!(after(&ours, "--bind", 2).contains(&vec![target.to_str().unwrap().to_string(); 2]), "the target is writable: {ours:?}");

    let env = |a: &[String]| after(a, "--setenv", 2).into_iter().map(|p| (p[0].clone(), p[1].clone())).collect::<std::collections::BTreeMap<_, _>>();
    let (ours_env, theirs_env) = (env(&ours), env(&theirs));
    for k in ["PATH", "HOME", "USER", "TMPDIR", "LANG", "CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR", "CARGO_NET_OFFLINE", "CARGO_TERM_COLOR"] {
        assert_eq!(ours_env.get(k), theirs_env.get(k), "{k} differs between the runner and testenv's sandbox");
    }
    assert!(ours_env.contains_key("GIT_CONFIG_SYSTEM"), "the runner names no system git identity: {ours_env:?}");
    let spira: Vec<&String> = ours_env.keys().filter(|k| k.starts_with("SPIRA")).collect();
    assert_eq!(spira, ["SPIRA_HOME"], "only the workspace's own SPIRA_HOME is set: {ours_env:?}");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().canonicalize().unwrap();
    assert_eq!(ours_env.get("SPIRA_HOME").map(PathBuf::from), Some(workspace.join("spira")), "SPIRA_HOME is the workspace's, as .cargo/config.toml's [env] declares");
}
