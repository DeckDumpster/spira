//! Every test binary runs under cargo's target runner, `tools/test-sandbox/runner`, which
//! `.cargo/config.toml` names: a bubblewrap sandbox in which the caller's installation is out of
//! reach (sp-bpenvh). What a binary can reach inside it is the test-sandbox suite's to prove
//! with a probe binary; these tests read files and spawn at most the runner's own `--print-args`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

#[test]
fn cargo_runs_every_test_binary_through_the_sandbox_runner() {
    let ws = workspace();
    let text = fs::read_to_string(ws.join(".cargo/config.toml")).expect("the workspace's .cargo/config.toml");
    let conf: toml::Table = text.parse().expect(".cargo/config.toml parses");
    let target = conf.get("target").and_then(toml::Value::as_table).expect("a [target] table");
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let runner = target.get(triple).and_then(|t| t.get("runner")).and_then(toml::Value::as_str);
        assert_eq!(runner, Some("tools/test-sandbox/runner"), "[target.{triple}] names the sandbox runner");
    }
    assert!(target.keys().all(|k| !k.starts_with("cfg(")), "a cfg(...) key loses to a triple key in ~/.cargo/config.toml: {target:?}");
    let mode = fs::metadata(ws.join("tools/test-sandbox/runner")).expect("the runner exists").permissions().mode();
    assert_ne!(mode & 0o111, 0, "the runner is executable");
}

/// The repository's own git directory, which both sandboxes bind back read-only so a test can
/// run git against its checkout: a worktree's `.git` names `<common>/worktrees/<name>`.
fn git_common_dir() -> Option<PathBuf> {
    let dotgit = workspace().join(".git");
    let text = fs::read_to_string(&dotgit).ok()?;
    let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() { gitdir } else { workspace().join(gitdir) };
    Some(match gitdir.parent() {
        Some(p) if p.file_name().is_some_and(|n| n == "worktrees") => p.parent()?.to_path_buf(),
        _ => gitdir,
    })
}

/// Every path under `dir` a test can see, except the way down to each of `keep` and the kept
/// paths themselves.
fn visible_beyond(dir: &Path, keep: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries {
        let p = e.unwrap().path();
        if keep.contains(&p) {
            continue;
        }
        if keep.iter().any(|k| k.starts_with(&p)) {
            visible_beyond(&p, keep, out);
        } else {
            out.push(p);
        }
    }
}

/// What the runner binds back on purpose: the git common dir (read-only), the workspace and
/// CARGO_TARGET_DIR. When a checkout or its target lives under $HOME/spira (a worktree under
/// run/worktree/, a gate's own target under run/), the way down to it is mount scaffolding,
/// not the installation.
fn bound_back() -> Vec<PathBuf> {
    let canon = |p: PathBuf| fs::canonicalize(&p).unwrap_or(p);
    let mut keep: Vec<PathBuf> = git_common_dir().into_iter().map(canon).collect();
    keep.push(canon(workspace()));
    if let Some(t) = std::env::var_os("CARGO_TARGET_DIR") {
        keep.push(canon(PathBuf::from(t)));
    }
    keep
}

#[test]
fn a_test_finds_no_installation_under_its_home() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("the runner sets HOME"));
    let mut seen = Vec::new();
    visible_beyond(&home.join("spira"), &bound_back(), &mut seen);
    assert!(seen.is_empty(), "under $HOME/spira a test sees {seen:?}; only the repository's git directory, workspace and target are bound in");
    for p in [".config/spira", ".local/bin/work"] {
        assert!(!home.join(p).exists(), "$HOME/{p} is visible inside the sandbox");
    }
}

fn print_args(args: &[&str]) -> Vec<String> {
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .arg("--print-args")
        .args(args)
        .output()
        .expect("the runner spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect()
}

#[test]
fn around_binds_each_target_link_destination_writable_and_keeps_the_environment() {
    let ws = workspace();
    let scratch = testkit::TempDir::new("around-dest");
    let dest = scratch.path().to_path_buf();
    let (target, link) = (ws.join("target"), ws.join("target/debug"));
    let made_target = !target.exists();
    fs::create_dir_all(&target).unwrap();
    let had_link = fs::symlink_metadata(&link).is_ok();
    if !had_link {
        std::os::unix::fs::symlink(&dest, &link).unwrap();
    }
    let args = print_args(&["--around", "cargo", "nextest", "run"]);
    if !had_link {
        fs::remove_file(&link).unwrap();
    }
    if made_target {
        let _ = fs::remove_dir(&target);
    }
    let want = fs::canonicalize(if had_link { &link } else { &dest }).unwrap().display().to_string();
    assert!(args.windows(3).any(|w| w[0] == "--bind" && w[1] == want && w[2] == want), "{want} bound writable: {args:?}");
    assert!(!args.iter().any(|a| a == "--clearenv"), "the caller's environment is kept: {args:?}");
    assert!(args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "GIT_CONFIG_SYSTEM"), "{args:?}");
    assert_eq!(&args[args.len() - 4..], ["--", "cargo", "nextest", "run"]);
}

#[test]
fn the_held_decision_comes_before_any_work_a_held_call_does_not_need() {
    let text = fs::read_to_string(workspace().join("tools/test-sandbox/runner")).unwrap();
    let held = text.find("[ -z \"$print\" ] && held").expect("the runner decides held");
    for later in ["readlink -f", "sed -n", "/bwrap"] {
        let first_use = text.find(later).unwrap();
        assert!(held < first_use, "`{later}` runs at byte {first_use}, before the held decision at {held}");
    }
}

#[test]
fn print_args_for_a_binary_is_what_the_landing_ref_printed() {
    let ws = workspace();
    let old = std::process::Command::new("git").arg("-C").arg(&ws).args(["show", "local/main:tools/test-sandbox/runner"]).output();
    let Some(old) = old.ok().filter(|o| o.status.success()) else { return };
    let copy = ws.join("tools/test-sandbox/runner.landing-ref");
    testkit::write_exe(&copy, &String::from_utf8(old.stdout).unwrap());
    let before = std::process::Command::new(&copy).args(["--print-args", "/usr/bin/true", "a", "b"]).output();
    fs::remove_file(&copy).unwrap();
    let before = String::from_utf8(before.unwrap().stdout).unwrap();
    assert_eq!(print_args(&["/usr/bin/true", "a", "b"]), before.lines().collect::<Vec<_>>());
}
