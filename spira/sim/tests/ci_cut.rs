//! The cut half of the world scenarios `publish-then-cut` and `cut-in-bare-env` (sp-2adk7),
//! which drove `scenarios/steps/cut.sh` after a landing: merge the open publish PR as GitHub
//! would, then run `.github/workflows/gate.yml`'s `cut` job on the merged head through the sim
//! runner (`spira_sim::ci`), once in a bare environment and once with the release's tools on
//! PATH. The loop's half (a bead lands and a publish PR opens at the landed head) is
//! `loop_ports.rs`'s happy-path test; the world that ran both halves as actor processes cannot
//! load since ruling D6, so the cut half runs here on a scripted git and a scripted forge.
//!
//! The scripted git is a scratch repository holding this tree's `.github`, `spira` and
//! `spira-config` (as cut.sh's sparse checkout did), an origin, and a landed commit on a
//! `spira/publish/*` branch. The scripted forge is the sim gh state `sim ghctl` writes: the
//! merged publish PR, a green `gate` check-run, and a successful pull_request Gate run whose
//! `batch-results-*` artifact holds one result per suite in the corpus.
//!
//! The defects these were written for: the cut refused every publish-PR head (sp-3e2wt: it
//! accepted only `spira/queue/*`), and the cut job could not run release.sh on a hosted runner
//! because nothing built spira-config (sp-y5wki). Each has a control below that is red.
//!
//! "The release's bin" is the target directory this test binary was built into: cargo stages no
//! release, so the honest equivalent is the workspace's own built binaries, spira-config among
//! them (built here if this `cargo test` did not build it). It is both what the sim `cargo`
//! copies when the job "builds" spira-config and, for the with-release run, a directory first
//! on the runner's PATH.

use spira_sim::{ci, gh};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const BEAD: &str = "sp-cu01";
const BRANCH: &str = "spira/publish/20270115T080000Z";
const SKIP: &str = "Mint App token for tag push";
/// The workspace, named for the repository as `$GITHUB_WORKSPACE` is (`.../spira/spira`).
const WS: &str = "spira";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// `git` that reads no global or system config and commits under a fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("spawn git");
    assert!(o.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The workspace's built binaries, spira-config among them: `<target>/debug`, beside `sim`.
fn release_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = Path::new(env!("CARGO_BIN_EXE_sim")).parent().unwrap().to_path_buf();
        let target = bin.parent().unwrap();
        let o = Command::new(env!("CARGO"))
            .args(["build", "--offline", "-q", "-p", "spira-config", "--bin", "spira-config", "--target-dir"])
            .arg(target)
            .current_dir(root())
            .output()
            .expect("spawn cargo");
        assert!(o.status.success(), "cargo build -p spira-config: {}", String::from_utf8_lossy(&o.stderr));
        assert!(bin.join("spira-config").is_file(), "no spira-config in {}", bin.display());
        bin
    })
}

/// Copies the tracked files under `dirs` from this tree's working copy into `to`.
fn copy_tree(to: &Path, dirs: &[&str]) {
    let r = root();
    let listed = git(&r, &[&["ls-files", "-z", "--"][..], dirs].concat());
    for rel in listed.split('\0').filter(|p| !p.is_empty()) {
        let (from, dest) = (r.join(rel), to.join(rel));
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        match std::fs::symlink_metadata(&from) {
            Ok(m) if m.file_type().is_symlink() => std::os::unix::fs::symlink(std::fs::read_link(&from).unwrap(), &dest).unwrap(),
            Ok(_) => {
                std::fs::copy(&from, &dest).unwrap();
            }
            Err(_) => {} // deleted in the working copy and not yet committed
        }
    }
}

/// What the forge says about the landed head.
#[derive(Clone, Copy)]
struct Forge {
    /// The head branch of the merged PR.
    branch: &'static str,
    /// A green `gate` check-run on the head.
    green_gate: bool,
    /// Suites in the run's batch-results artifact short of the corpus.
    missing_results: usize,
}

const GITHUB: Forge = Forge { branch: BRANCH, green_gate: true, missing_results: 0 };

struct Cut {
    t: testkit::TempDir,
    sha: String,
}

impl Cut {
    /// A landed commit on a publish branch of a repository holding this tree, its PR merged
    /// as GitHub would merge it (main fast-forwarded to the head), and the forge state `f`.
    fn new(f: Forge) -> Cut {
        release_bin();
        let t = testkit::TempDir::new("ci-cut");
        let (src, origin, gh_dir) = (t.join("src"), t.join("origin.git"), t.join("gh"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&gh_dir).unwrap();
        copy_tree(&src, &[".github", "spira", "spira-config"]);
        git(&src, &["init", "-q", "-b", "main"]);
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "-q", "-m", "seed: the tree at a commit"]);
        git(&t, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
        git(&src, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(&src, &["push", "-q", "origin", "main"]);
        git(&src, &["checkout", "-q", "-b", f.branch]);
        std::fs::create_dir_all(src.join("out")).unwrap();
        std::fs::write(src.join("out/a.txt"), "a\n").unwrap();
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "-q", "-m", &format!("{BEAD}: land out/a.txt")]);
        git(&src, &["push", "-q", "origin", f.branch]);
        let sha = git(&src, &["rev-parse", "HEAD"]);

        let say = |args: &[&str]| -> String {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let out = gh::run(&gh_dir, &src, &args).unwrap_or_else(|e| panic!("sim gh {args:?}: {e}"));
            assert_eq!(out.code, 0, "sim gh {args:?}: {}", out.stderr);
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let pr = say(&["pr", "create", "--head", f.branch, "--base", "main", "--title", "publish"]);
        let pr = pr.trim().rsplit('/').next().unwrap().to_string();
        let ctl = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            gh::ctl(&gh_dir, &args).unwrap_or_else(|e| panic!("sim ghctl {args:?}: {e}"))
        };
        if f.green_gate {
            ctl(&["check-run", &sha, "gate", "completed", "success"]);
        }
        let mut suites: Vec<String> = std::fs::read_dir(src.join("spira"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
            .map(|n| format!("{n}.result"))
            .collect();
        assert!(suites.len() > f.missing_results + 1, "the corpus has suites: {suites:?}");
        suites.truncate(suites.len() - f.missing_results);
        let artifact = format!("batch-results-1:{}", suites.join(","));
        ctl(&["run-add", "--branch", f.branch, "--sha", &sha, "--conclusion", "success", "--artifact", &artifact]);
        ctl(&["pr-merge", &pr]);
        git(&src, &["push", "-q", "origin", &format!("{sha}:refs/heads/main")]);

        // actions/checkout with ref: <sha>, fetch-depth: 0, fetch-tags: true, into a directory
        // named for the repository as a hosted runner's workspace is.
        let ws = t.join(WS);
        git(&t, &["clone", "-q", "--no-checkout", origin.to_str().unwrap(), ws.to_str().unwrap()]);
        git(&ws, &["checkout", "-q", "-B", "main", &sha]);
        Cut { t, sha }
    }

    /// Runs the shipped cut job on the head; its exit code and the runner's log.
    fn run(&self, path: &[PathBuf], skip: &[&str]) -> (i32, String) {
        let yml = std::fs::read_to_string(self.t.join(WS).join(".github/workflows/gate.yml")).unwrap();
        let steps = ci::steps(&yml, "cut").unwrap();
        let skip: Vec<String> = skip.iter().map(|s| s.to_string()).collect();
        for s in &skip {
            assert!(steps.iter().any(|st| &st.name == s), "the cut job has a step {s:?} to skip");
        }
        let scratch = self.t.join("runner");
        std::fs::create_dir_all(&scratch).unwrap();
        let (ws, gh_dir) = (self.t.join(WS), self.t.join("gh"));
        let job = ci::Job {
            steps: &steps,
            skip: &skip,
            cx: ci::Context { sha: &self.sha, workspace: &ws, temp: &scratch },
            scratch: &scratch,
            gh_dir: &gh_dir,
            release_bin: release_bin(),
            path,
            env: &[],
        };
        let mut log = String::new();
        let code = ci::run_job(&job, Path::new(env!("CARGO_BIN_EXE_sim")), |l| log.push_str(l)).unwrap_or_else(|e| panic!("the runner ran: {e}\n{log}"));
        (code, log)
    }

    /// The release tags origin holds, each with the commit it names.
    fn origin_tags(&self) -> Vec<(String, String)> {
        let out = git(&self.t.join("origin.git"), &["for-each-ref", "--format=%(refname:short) %(*objectname)", "refs/tags"]);
        out.lines().filter_map(|l| l.split_once(' ')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn tag_message(&self, tag: &str) -> String {
        git(&self.t.join("origin.git"), &["tag", "-l", "--format=%(contents)", tag])
    }
}

fn timed<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = f();
    let wall = started.elapsed();
    println!("{name}: wall {wall:?}");
    assert!(wall < Duration::from_secs(30), "{name} ran in {wall:?}");
    out
}

/// The cut tagged the landed head, pushed the tag to origin, and named the bead and the PR.
fn assert_cut(c: &Cut, code: i32, log: &str) {
    assert_eq!(code, 0, "the cut job exits 0:\n{log}");
    let tags = c.origin_tags();
    let [(tag, at)] = tags.as_slice() else { panic!("origin holds one release tag, not {tags:?}:\n{log}") };
    assert_eq!(tag, "spira-0.1.1", "the home repository's first numbered release");
    assert_eq!(at, &c.sha, "the tag names the merged head");
    let msg = c.tag_message(tag);
    // Not asserted: a `branch: <publish branch>` line. release.sh writes the --branches list
    // through `tr ',' '\n' | while read`, which drops the last entry (it has no newline), so a
    // single-branch cut records none; no reader of the line exists today.
    assert!(msg.starts_with("spira release: spira\n") && msg.contains("\npr: 1\n") && msg.contains(&format!("bead: {BEAD}")), "the tag names the repository, the PR and the bead:\n{msg}");
    assert!(log.contains("suite results found in queue PR run"), "the gate assertion ran:\n{log}");
}

/// Replaces `cut-in-bare-env` (sp-2adk7, for sp-y5wki): with only what the job's own steps
/// provide, the cut job builds spira-config, runs release.sh, and pushes the tag.
#[test]
fn cut_in_bare_env_the_cut_job_tags_the_merged_publish_head() {
    let c = Cut::new(GITHUB);
    let (code, log) = timed("cut-in-bare-env", || c.run(&[], &[SKIP]));
    assert_cut(&c, code, &log);
}

/// Replaces `publish-then-cut` (sp-2adk7, for sp-3e2wt): a merged `spira/publish/*` head cuts,
/// on a runner whose PATH already holds the release's tools.
#[test]
fn publish_then_cut_with_the_release_on_path_tags_the_merged_publish_head() {
    let c = Cut::new(GITHUB);
    let (code, log) = timed("publish-then-cut", || c.run(&[release_bin().to_path_buf()], &[SKIP]));
    assert_cut(&c, code, &log);
}

/// The bare env is bare: without the step that builds spira-config, release.sh finds none and
/// the cut refuses (sp-y5wki's own failure), so nothing outside the job's steps supplied it.
#[test]
fn cut_in_bare_env_without_the_spira_config_build_step_refuses() {
    let c = Cut::new(GITHUB);
    let (code, log) = timed("bare-env-no-build", || c.run(&[], &[SKIP, "Build spira-config for release.sh"]));
    assert_ne!(code, 0, "the cut refuses without spira-config:\n{log}");
    assert!(log.contains("release.sh refused") && log.contains("spira-config"), "release.sh refused for want of spira-config:\n{log}");
    assert!(c.origin_tags().is_empty(), "no tag reached origin");
}

/// The gate assertion is a positive control: each way the forge falls short refuses the cut
/// before anything is tagged.
#[test]
fn the_cut_refuses_a_head_the_forge_does_not_vouch_for() {
    let cases = [
        (Forge { missing_results: 1, ..GITHUB }, "not a full-suite run"),
        (Forge { green_gate: false, ..GITHUB }, "has no green gate check"),
        (Forge { branch: "feature/a", ..GITHUB }, "is not the head of a queue batch PR"),
    ];
    for (f, said) in cases {
        let c = Cut::new(f);
        let (code, log) = timed(said, || c.run(&[], &[SKIP]));
        assert_eq!(code, 1, "{said}: the cut refuses:\n{log}");
        assert!(log.contains(said), "{said}: the refusal says why:\n{log}");
        assert!(c.origin_tags().is_empty(), "{said}: no tag reached origin");
    }
}
