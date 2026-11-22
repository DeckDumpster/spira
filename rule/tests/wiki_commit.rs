// Exception to law-a-unit-test-calls-no-binary: the contract under test is a bash script driving
// real git, so these tests spawn `bash` and `git`. A `git` shim fixes the order of lock events.

#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use testkit::TempDir;

fn script() -> PathBuf {
    testkit::manifest_dir().join("../spira/wiki-commit.sh")
}

fn make_executable(p: &Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

fn real_git() -> String {
    let out = Command::new("bash").args(["-c", "command -v git"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn git(w: &Path, args: &[&str]) -> String {
    let o = Command::new(real_git()).arg("-C").arg(w).args(args).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn wiki(tag: &str) -> (TempDir, PathBuf) {
    let t = TempDir::new(tag);
    let w = t.path().join("wiki");
    fs::create_dir(&w).unwrap();
    let bare = t.path().join("origin.git");
    git(t.path(), &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    git(&w, &["init", "-q", "-b", "main"]);
    git(&w, &["config", "user.email", "t@example.com"]);
    git(&w, &["config", "user.name", "t"]);
    git(&w, &["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(w.join("seed.md"), "seed\n").unwrap();
    git(&w, &["add", "seed.md"]);
    git(&w, &["commit", "-qm", "seed"]);
    git(&w, &["push", "-q", "-u", "origin", "main"]);
    (t, w)
}

fn head(w: &Path) -> String {
    git(w, &["log", "-1", "--format=%s"])
}

fn run(w: &Path, paths: &str, msg: &str, wait: &str, path_env: Option<&str>) -> Output {
    let mut c = Command::new("bash");
    c.arg(script()).arg(w).arg(msg).env("WIKI_COMMIT_LOCK_WAIT", wait);
    if let Some(p) = path_env {
        c.env("PATH", p);
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(paths.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A `git` that counts `add` calls and removes the lock on attempt `n`, `before` or `after` real git runs.
/// Returns (PATH value, counter file).
fn shim(dir: &Path, w: &Path, n: u32, point: &str) -> (String, PathBuf) {
    let bin = dir.join("shimbin");
    fs::create_dir_all(&bin).unwrap();
    let counter = dir.join("adds");
    let script = format!(
        r#"#!/bin/bash
real={real}
sub=
for a in "$@"; do case "$a" in -C) ;; -*) ;; *) [ -d "$a" ] || {{ sub=$a; break; }} ;; esac; done
if [ "$sub" != add ]; then exec "$real" "$@"; fi
c=$(cat {counter} 2>/dev/null || echo 0); c=$((c+1)); echo $c > {counter}
[ "$c" -eq {n} ] && [ {point} = before ] && rm -f {lock}
"$real" "$@"; rc=$?
[ "$c" -eq {n} ] && [ {point} = after ] && rm -f {lock}
exit $rc
"#,
        real = real_git(),
        counter = counter.display(),
        n = n,
        point = point,
        lock = w.join(".git/index.lock").display()
    );
    let g = bin.join("git");
    fs::write(&g, script).unwrap();
    make_executable(&g);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    (path, counter)
}

#[test_id(t5sbnpb)]
#[test]
fn a_lock_cleared_between_the_failed_add_and_the_existence_check_is_retried() {
    let (t, w) = wiki("wc-window");
    fs::write(w.join("r.md"), "r\n").unwrap();
    fs::write(w.join(".git/index.lock"), "").unwrap();
    let (path, counter) = shim(t.path(), &w, 1, "after");
    let o = run(&w, "r.md\n", "retried", "60", Some(&path));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(head(&w), "retried");
    assert_eq!(fs::read_to_string(counter).unwrap().trim(), "2");
}

#[test_id(tofz1hs)]
#[test]
fn a_lock_held_across_retries_is_waited_out_and_the_file_committed() {
    let (t, w) = wiki("wc-held");
    fs::write(w.join("r.md"), "r\n").unwrap();
    fs::write(w.join(".git/index.lock"), "").unwrap();
    let (path, counter) = shim(t.path(), &w, 3, "before");
    let o = run(&w, "r.md\n", "retried", "60", Some(&path));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(head(&w), "retried");
    assert_eq!(fs::read_to_string(counter).unwrap().trim(), "3");
}

#[test_id(t1u5d84)]
#[test]
fn a_stale_lock_is_refused_with_its_age_and_never_removed() {
    let (_t, w) = wiki("wc-stale");
    fs::write(w.join("page.md"), "x\n").unwrap();
    fs::write(w.join(".git/index.lock"), "").unwrap();
    let o = run(&w, "page.md\n", "msg", "1", None);
    let err = stderr(&o);
    assert_ne!(o.status.code(), Some(0));
    assert!(err.contains("index.lock"), "{err}");
    assert!(err.contains("s old"), "{err}");
    assert_eq!(head(&w), "seed");
    assert!(w.join(".git/index.lock").exists());
}

#[test_id(tgdcz8d)]
#[test]
fn no_lock_commits_the_file() {
    let (_t, w) = wiki("wc-plain");
    fs::write(w.join("page.md"), "x\n").unwrap();
    let o = run(&w, "page.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(head(&w), "msg");
}

#[test_id(t1k8k2a)]
#[test]
fn an_already_committed_path_exits_zero() {
    let (_t, w) = wiki("wc-done");
    let o = run(&w, "seed.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(head(&w), "seed");
}

#[test_id(thvi2nq)]
#[test]
fn an_unknown_path_is_refused() {
    let (_t, w) = wiki("wc-unknown");
    let o = run(&w, "missing.md\n", "msg", "1", None);
    assert_ne!(o.status.code(), Some(0));
}

#[test_id(tdcda7x)]
#[test]
fn a_gitignored_path_is_refused() {
    let (_t, w) = wiki("wc-ignored");
    fs::write(w.join(".gitignore"), "ign.md\n").unwrap();
    fs::write(w.join("ign.md"), "i\n").unwrap();
    let o = run(&w, "ign.md\n", "m", "1", None);
    assert_ne!(o.status.code(), Some(0));
}

#[test_id(tmfptdt)]
#[test]
fn a_failing_pre_commit_hook_fails_the_script_and_its_text_reaches_stderr() {
    let (_t, w) = wiki("wc-hook");
    fs::write(w.join("hook.md"), "h\n").unwrap();
    let hook = w.join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\necho \"hook-says-no-commit\" >&2\nexit 1\n").unwrap();
    make_executable(&hook);
    let o = run(&w, "hook.md\n", "hooked", "1", None);
    assert_ne!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("hook-says-no-commit"), "{}", stderr(&o));
}

fn remote_main(t: &TempDir) -> String {
    git(&t.path().join("origin.git"), &["rev-parse", "main"])
}

fn refuse_pushes(t: &TempDir) {
    let hook = t.path().join("origin.git/hooks/pre-receive");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    testkit::write_exe(&hook, "#!/bin/sh\necho 'REJECTED — refusing for the test'\nexit 1\n");
}

#[test_id(tjkwkgd)]
#[test]
fn a_hook_refusal_undoes_the_commit_and_keeps_the_work_unstaged() {
    let (t, w) = wiki("wc-refused");
    refuse_pushes(&t);
    fs::write(w.join("page.md"), "x\n").unwrap();
    fs::write(w.join("other.md"), "o\n").unwrap();
    git(&w, &["add", "other.md"]);
    let pre = git(&w, &["rev-parse", "HEAD"]);
    let o = run(&w, "page.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("push failed:"), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("REJECTED — refusing for the test"),
        "{}",
        stderr(&o)
    );
    assert_eq!(git(&w, &["rev-parse", "HEAD"]), pre);
    assert_eq!(pre, remote_main(&t));
    let status = git(&w, &["status", "--porcelain"]);
    assert!(status.lines().any(|l| l == "?? page.md"), "{status}");
    assert!(status.lines().any(|l| l == "A  other.md"), "{status}");
}

#[test_id(tzpgrzd)]
#[test]
fn an_unreachable_remote_undoes_the_commit() {
    let (t, w) = wiki("wc-unreachable");
    git(
        &w,
        &[
            "remote",
            "set-url",
            "origin",
            t.path().join("missing.git").to_str().unwrap(),
        ],
    );
    fs::write(w.join("page.md"), "x\n").unwrap();
    let pre = git(&w, &["rev-parse", "HEAD"]);
    let o = run(&w, "page.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("push failed:"), "{}", stderr(&o));
    assert_eq!(git(&w, &["rev-parse", "HEAD"]), pre);
    assert!(git(&w, &["status", "--porcelain"]).lines().any(|l| l == "?? page.md"));
}

#[test_id(tlkpo9t)]
#[test]
fn a_head_that_moved_during_the_transfer_is_left_in_place() {
    let (t, w) = wiki("wc-moved");
    refuse_pushes(&t);
    let hook = w.join(".git/hooks").join(["pre", "push"].join("-"));
    testkit::write_exe(&hook, "#!/bin/sh\ngit commit -q --allow-empty -m intruder\nexit 0\n");
    fs::write(w.join("page.md"), "x\n").unwrap();
    let o = run(&w, "page.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("commit left in place"), "{}", stderr(&o));
    assert_eq!(git(&w, &["log", "-2", "--format=%s"]), "intruder\nmsg");
}

#[test_id(tsiidab)]
#[test]
fn a_good_push_leaves_the_remote_at_head() {
    let (t, w) = wiki("wc-pushed");
    fs::write(w.join("page.md"), "x\n").unwrap();
    let o = run(&w, "page.md\n", "msg", "1", None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(git(&w, &["rev-parse", "HEAD"]), remote_main(&t));
    assert_eq!(head(&w), "msg");
}
