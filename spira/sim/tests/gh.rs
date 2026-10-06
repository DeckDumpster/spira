use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct W {
    _t: testkit::TempDir,
    dir: PathBuf,
    repo: PathBuf,
}

fn sh(dir: &Path, prog: &str, args: &[&str]) -> String {
    let o = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t.invalid")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t.invalid")
        .output()
        .unwrap();
    assert!(o.status.success(), "{prog} {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn world() -> W {
    let t = testkit::TempDir::new("simgh");
    let dir = t.join("world");
    let repo = t.join("work");
    for d in ["gh", "bin"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_sim"), dir.join("bin/gh")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git", &["init", "-q", "--initial-branch=main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    sh(&repo, "git", &["add", "-A"]);
    sh(&repo, "git", &["commit", "-q", "-m", "seed"]);
    sh(&repo, "git", &["branch", "spira/publish/x"]);
    W { _t: t, dir, repo }
}

struct Res {
    code: i32,
    out: String,
    err: String,
}

impl W {
    fn gh_in(&self, cwd: &Path, args: &[&str], stdin: &str) -> Res {
        let mut c = Command::new(self.dir.join("bin/gh"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("SIM_GH_DIR", self.dir.join("gh"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        Res { code: o.status.code().unwrap(), out: String::from_utf8(o.stdout).unwrap(), err: String::from_utf8(o.stderr).unwrap() }
    }
    fn gh(&self, args: &[&str]) -> Res {
        self.gh_in(&self.repo, args, "")
    }
    fn ok(&self, args: &[&str]) -> String {
        let r = self.gh(args);
        assert_eq!(r.code, 0, "{args:?}: {}", r.err);
        r.out
    }
    fn ctl(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_sim")).arg("ghctl").arg(self.dir.join("gh")).args(args).output().unwrap();
        assert!(o.status.success(), "ghctl {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
    fn head(&self, rev: &str) -> String {
        sh(&self.repo, "git", &["rev-parse", rev])
    }
    fn state(&self) -> String {
        std::fs::read_to_string(self.dir.join("gh/state.json")).unwrap()
    }
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn pr_create_view_list_close_comment() {
    let w = world();
    let sha = w.head("spira/publish/x");
    let url = w.gh_in(&w.repo, &["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "publish x", "--body-file", "-"], "the body\n");
    assert_eq!(url.code, 0, "{}", url.err);
    assert_eq!(url.out.trim(), "https://github.com/sim/sim/pull/1");

    assert_eq!(w.ok(&["pr", "view", "spira/publish/x", "--json", "number", "-q", ".number"]).trim(), "1");
    assert_eq!(w.ok(&["pr", "view", "1", "--json", "state", "-q", ".state"]).trim(), "OPEN");
    let v = json(&w.ok(&["pr", "view", "1", "--json", "mergeable,mergeStateStatus,headRefOid,body"]));
    assert_eq!((v["mergeable"].as_str(), v["mergeStateStatus"].as_str(), v["headRefOid"].as_str()), (Some("MERGEABLE"), Some("CLEAN"), Some(sha.as_str())));
    assert_eq!(v["body"], "the body\n");
    let l = json(&w.ok(&["pr", "list", "--state", "open", "--json", "number,headRefName"]));
    assert_eq!(l, serde_json::json!([{"number": 1, "headRefName": "spira/publish/x"}]));

    let dup = w.gh(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "again", "--body", "b"]);
    assert_eq!(dup.code, 1);
    assert!(dup.err.contains("already exists"), "{}", dup.err);

    assert_eq!(w.gh(&["pr", "comment", "1", "--body", "hello"]).code, 0);
    assert_eq!(w.gh(&["pr", "merge", "--auto", "--squash", "1"]).code, 0);
    assert!(w.state().contains("SQUASH"));

    assert_eq!(w.gh(&["pr", "close", "1"]).code, 0);
    assert_eq!(w.ok(&["pr", "view", "1", "--json", "state", "-q", ".state"]).trim(), "CLOSED");
    assert_eq!(json(&w.ok(&["pr", "list", "--state", "open", "--json", "number"])), serde_json::json!([]));
    assert_eq!(w.gh(&["pr", "close", "1"]).code, 1);
    assert_eq!(w.gh(&["pr", "merge", "--auto", "--squash", "1"]).code, 1);
}

#[test]
fn pr_create_for_a_branch_that_does_not_exist_refuses() {
    let w = world();
    let r = w.gh(&["pr", "create", "--head", "nope", "--base", "main", "--title", "t", "--body", "b"]);
    assert_ne!(r.code, 0);
    assert!(w.gh(&["pr", "view", "1", "--json", "number"]).code != 0);
}

#[test]
fn a_merged_pr_reports_merged_and_leaves_the_open_list() {
    let w = world();
    w.ok(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "t", "--body", "b"]);
    w.ctl(&["pr-merge", "1"]);
    assert_eq!(w.ok(&["pr", "view", "1", "--json", "state", "-q", ".state"]).trim(), "MERGED");
    assert_eq!(json(&w.ok(&["pr", "list", "--state", "merged", "--json", "number"])), serde_json::json!([{"number": 1}]));
    w.ctl(&["pr-mergeable", "1", "CONFLICTING", "DIRTY"]);
}

#[test]
fn run_list_and_view_answer_as_forge_asks() {
    let w = world();
    let sha = w.head("main");
    let id = w.ctl(&["run-add", "--workflow", "gate.yml", "--branch", "spira/publish/x", "--sha", &sha, "--status", "in_progress"]);
    assert_eq!(id.trim(), "1");
    let other = w.ctl(&["run-add", "--workflow", "other.yml", "--branch", "spira/publish/x", "--sha", &sha, "--status", "completed", "--conclusion", "success"]);
    assert_eq!(other.trim(), "2");
    w.ctl(&["run-complete", "1", "failure"]);

    let l = json(&w.ok(&["run", "list", "--branch", "spira/publish/x", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"]));
    assert_eq!(l.as_array().unwrap().len(), 1);
    assert_eq!((l[0]["databaseId"].as_u64(), l[0]["status"].as_str(), l[0]["conclusion"].as_str()), (Some(1), Some("completed"), Some("failure")));
    assert_eq!(l[0]["headSha"], sha.as_str());
    assert_eq!(w.ok(&["run", "list", "--branch", "spira/publish/x", "--json", "databaseId", "--limit", "1", "-q", ".[0].databaseId"]).trim(), "2");
    assert_eq!(w.ok(&["run", "list", "--branch", "nobody", "--json", "databaseId"]).trim(), "[]");
    assert_eq!(w.ok(&["run", "view", "1", "--json", "status", "-q", ".status"]).trim(), "completed");

    assert_eq!(w.gh(&["run", "rerun", "1"]).code, 0);
    assert_eq!(w.ok(&["run", "view", "1", "--json", "status", "-q", ".status"]).trim(), "queued");
    assert_eq!(w.gh(&["run", "cancel", "1"]).code, 0);
    assert_eq!(w.ok(&["run", "view", "1", "--json", "conclusion", "-q", ".conclusion"]).trim(), "cancelled");
    assert_ne!(w.gh(&["run", "view", "99", "--json", "status"]).code, 0);
}

#[test]
fn workflow_run_creates_a_queued_dispatch_run_with_its_inputs() {
    let w = world();
    let r = w.gh(&["workflow", "run", "Gate", "--ref", "main", "-f", "suites=a b"]);
    assert_eq!(r.code, 0, "{}", r.err);
    let rs = json(&w.ok(&["run", "list", "--workflow", "Gate", "--json", "event,status,headSha,headBranch"]));
    assert_eq!(rs, serde_json::json!([{"event": "workflow_dispatch", "status": "queued", "headSha": w.head("main"), "headBranch": "main"}]));
    assert!(w.state().contains("a b"));
    let d = w.gh(&["workflow", "run", "gate.yml", "--ref", "main", "-f", "cut=true"]);
    assert_eq!(d.code, 0, "{}", d.err);
    assert_eq!(w.ok(&["run", "list", "--workflow", "gate.yml", "--json", "databaseId"]).matches("databaseId").count(), 2);
    assert_ne!(w.gh(&["workflow", "run", "Gate", "--ref", "no-such-ref"]).code, 0);
}

#[test]
fn api_answers_commit_pulls_check_runs_and_actions() {
    let w = world();
    let sha = w.head("spira/publish/x");
    w.ok(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "t", "--body", "b"]);
    w.ctl(&["check-run", &sha, "gate", "completed", "success"]);
    w.ctl(&["check-run", &sha, "lint", "completed", "failure"]);
    let rid = w.ctl(&["run-add", "--branch", "spira/publish/x", "--sha", &sha, "--status", "completed", "--conclusion", "success", "--job", "build:completed:success", "--artifact", "batch-results-1:a.result,b.result"]);
    let rid = rid.trim();

    assert_eq!(w.ok(&["api", &format!("repos/o/r/commits/{sha}/pulls"), "--jq", ".[0].head.ref // \"\""]).trim(), "spira/publish/x");
    assert_eq!(w.ok(&["api", &format!("repos/o/r/commits/{sha}/check-runs"), "--jq", "[.check_runs[] | select(.name == \"gate\" and .conclusion == \"success\")] | length"]).trim(), "1");
    let runs = w.ok(&["api", &format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={sha}&event=pull_request&status=completed"), "--jq", ".workflow_runs[] | select(.name == \"Gate\" and .conclusion == \"success\") | .id"]);
    assert_eq!(runs.trim(), rid);
    assert_eq!(w.ok(&["api", "repos/{owner}/{repo}/actions/runs?per_page=100", "--jq", ".workflow_runs | length"]).trim(), "1");
    let jobs = json(&w.ok(&["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{rid}/jobs")]));
    assert_eq!(jobs["jobs"][0]["name"], "build");
    let arts = w.ok(&["api", &format!("repos/o/r/actions/runs/{rid}/artifacts"), "--jq", ".artifacts[] | select(.name | startswith(\"batch-results-\")) | .id"]);
    assert_eq!(arts.trim(), "1");
    assert_eq!(json(&w.ok(&["api", "repos/{owner}/{repo}/actions/runners?per_page=100"]))["runners"], serde_json::json!([]));

    let nothing = w.ok(&["api", "repos/o/r/commits/0000/pulls", "--jq", ".[0].number // \"\""]);
    assert_eq!(nothing.trim(), "");
    let r = w.gh(&["api", "repos/o/r/no/such/route"]);
    assert_eq!((r.code, r.err.contains("404")), (1, true));
}

#[test]
fn api_tag_delete_and_branch_protection() {
    let w = world();
    let sha = w.head("main");
    w.ctl(&["tag", "spira-release-x-1", &sha]);
    assert_eq!(w.gh(&["api", "--method", "DELETE", "repos/o/r/git/refs/tags/spira-release-x-1"]).code, 0);
    assert_eq!(w.gh(&["api", "--method", "DELETE", "repos/o/r/git/refs/tags/spira-release-x-1"]).code, 1);
    assert_eq!(w.gh(&["api", "repos/o/r/branches/main/protection"]).code, 1);
    let put = w.gh_in(&w.repo, &["api", "repos/o/r/branches/main/protection", "--method", "PUT", "--input", "-"], "{\"enforce_admins\":true}");
    assert_eq!(put.code, 0, "{}", put.err);
    assert_eq!(json(&w.ok(&["api", "repos/o/r/branches/main/protection"]))["enforce_admins"], true);
}

#[test]
fn release_list_and_view() {
    let w = world();
    w.ctl(&["release", "spira-release-x-1", "--asset", "bundle.tar"]);
    w.ctl(&["release", "spira-release-x-2", "--draft"]);
    let l = json(&w.ok(&["--repo", "o/r", "release", "list", "--json", "tagName,isDraft"]));
    assert_eq!(l, serde_json::json!([{"tagName": "spira-release-x-2", "isDraft": true}, {"tagName": "spira-release-x-1", "isDraft": false}]));
    assert_eq!(w.ok(&["--repo", "o/r", "release", "view", "spira-release-x-2", "--json", "isDraft", "-q", ".isDraft"]).trim(), "true");
    let a = json(&w.ok(&["release", "view", "spira-release-x-1", "--json", "assets"]));
    assert_eq!(a["assets"][0]["name"], "bundle.tar");
    assert_eq!(w.gh(&["release", "view", "nope", "--json", "isDraft"]).code, 1);
}

fn cut_step(sha: &str) -> String {
    let yml = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/gate.yml")).unwrap();
    let cut = yml.split("\n  cut:\n").nth(1).expect("the cut job");
    let step = cut.split("- name: Assert gate check").nth(1).expect("the assert step");
    let run = step.split("        run: |\n").nth(1).expect("its script");
    let mut script = Vec::new();
    for l in run.lines() {
        if !l.is_empty() && !l.starts_with("          ") {
            break;
        }
        script.push(l.get(10..).unwrap_or(""));
    }
    script.join("\n").replace("${{ github.sha }}", sha)
}

fn run_cut_assert(w: &W, sha: &str) -> Res {
    let shim = w._t.join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    testkit::write_exe(shim.join("unzip"), "#!/bin/sh\nexec python3 -m zipfile -e \"$2\" \"$4\"\n");
    let mut c = Command::new("bash");
    c.arg("-c").arg(cut_step(sha)).current_dir(&w.repo).env_clear();
    c.env("PATH", format!("{}:{}:{}", w.dir.join("bin").display(), shim.display(), std::env::var("PATH").unwrap()));
    c.env("SIM_GH_DIR", w.dir.join("gh")).env("GITHUB_REPOSITORY", "owner/repo").env("HOME", &w._t.path());
    let o = c.output().unwrap();
    Res { code: o.status.code().unwrap(), out: String::from_utf8(o.stdout).unwrap(), err: String::from_utf8(o.stderr).unwrap() }
}

#[test]
fn the_release_cut_assertion_runs_unmodified_and_refuses_what_it_should() {
    let w = world();
    let sha = w.head("spira/publish/x");

    let r = run_cut_assert(&w, &sha);
    assert_eq!(r.code, 1, "no PR must refuse the cut: {}", r.err);
    assert!(r.err.contains("not the head of a queue batch PR"), "{}", r.err);

    w.ok(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "t", "--body", "b"]);
    let r = run_cut_assert(&w, &sha);
    assert!(r.err.contains("no green gate check"), "{}", r.err);

    w.ctl(&["check-run", &sha, "gate", "completed", "success"]);
    let r = run_cut_assert(&w, &sha);
    assert!(r.err.contains("no completed pull_request Gate run"), "{}", r.err);

    w.ctl(&["run-add", "--branch", "spira/publish/x", "--sha", &sha, "--conclusion", "success"]);
    let r = run_cut_assert(&w, &sha);
    assert!(r.err.contains("carries a batch-results artifact"), "{}", r.err);

    w.ctl(&["run-add", "--branch", "spira/publish/x", "--sha", &sha, "--conclusion", "success", "--artifact", "batch-results-1:a.result,b.result"]);
    let r = run_cut_assert(&w, &sha);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("2 suite results found in queue PR run 2"), "{}", r.out);
}

#[test]
fn the_cut_pr_lookup_resolves_a_merged_pr_by_its_head_sha() {
    let w = world();
    let sha = w.head("spira/publish/x");
    w.ok(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "t", "--body", "b"]);
    w.ctl(&["pr-merge", "1"]);
    assert_eq!(w.ok(&["api", &format!("repos/o/r/commits/{sha}/pulls"), "--jq", ".[0].number // \"\""]).trim(), "1");
}

#[test]
fn it_fails_closed() {
    let w = world();
    assert_eq!(w.gh(&["pr", "frobnicate"]).code, 1);
    assert_eq!(w.gh(&["pr", "list", "--no-such-flag"]).code, 1);
    assert_eq!(w.gh(&["pr", "view", "1", "--json", "noSuchField"]).code, 1);
    assert_eq!(w.gh(&["pr", "merge", "1"]).code, 1, "a plain merge is the simulator's decision");
    let o = Command::new(w.dir.join("bin/gh")).args(["pr", "list"]).env_clear().output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("SIM_GH_DIR"));
}

#[test]
fn the_same_script_makes_the_same_state() {
    let run = || {
        let w = world();
        w.ok(&["pr", "create", "--head", "spira/publish/x", "--base", "main", "--title", "t", "--body", "b"]);
        w.ctl(&["clock", "2001-02-03T04:05:06Z"]);
        w.ctl(&["run-add", "--branch", "spira/publish/x", "--sha", &w.head("main"), "--job", "build:completed:success"]);
        w.ctl(&["check-run", &w.head("main"), "gate", "completed", "success"]);
        w.state()
    };
    let (a, b) = (run(), run());
    assert!(a.contains("2001-02-03T04:05:06Z") && a.contains("2000-01-01T00:00:00Z"));
    assert_eq!(a, b);
}
