//! `spira-lc unpublished <repo>` against real git repositories: the exact commit count, a forge tip
//! fetched at the call, the per-repo publish target, and a refusal wherever it cannot measure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    dir: testkit::TempDir,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn git(&self, repo: &str, args: &[&str], landed_ago: Option<u64>) -> String {
        let mut c = Command::new("git");
        c.arg("-C").arg(self.p(repo)).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t");
        if let Some(ago) = landed_ago {
            c.env("GIT_COMMITTER_DATE", format!("@{} +0000", now() - ago));
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, repo: &str, msg: &str, landed_ago: u64) {
        fs::write(self.p(repo).join("f.txt"), format!("{msg}\n")).unwrap();
        self.git(repo, &["add", "f.txt"], None);
        self.git(repo, &["commit", "-q", "-m", msg], Some(landed_ago));
    }

    fn build(name: &str, publish: &str) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("unpub-{name}")) };
        for d in ["run", "repo", "other"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        f.git("", &["init", "-q", "--bare", "-b", "main", "remote.git"], None);
        f.git("", &["init", "-q", "--bare", "-b", "release", "second.git"], None);
        f.git("repo", &["init", "-q", "-b", "main"], None);
        f.git("repo", &["commit", "-q", "--allow-empty", "-m", "base"], None);
        f.git("repo", &["remote", "add", "origin", f.p("remote.git").to_str().unwrap()], None);
        f.git("repo", &["remote", "add", "upstream", f.p("second.git").to_str().unwrap()], None);
        f.git("repo", &["push", "-q", "origin", "main"], None);
        f.git("repo", &["push", "-q", "upstream", "main:release"], None);
        f.git("repo", &["fetch", "-q", "--all"], None);
        f.git("repo", &["branch", "local/main", "main"], None);
        f.git("repo", &["checkout", "-q", "local/main"], None);
        fs::write(f.p("rows"), format!("fixlocal | {} | queue.local | local/main | | |\n", f.p("repo").display())).unwrap();
        fs::write(
            f.p("layer.toml"),
            format!("[spira]\nrun = \"{}\"\nrepo_map = \"{}\"\nhome_repo = \"fixlocal\"\n{publish}", f.p("run").display(), f.p("rows").display()),
        )
        .unwrap();
        f
    }

    fn lc(&self, repo: &str) -> (i32, String) {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let out = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
            .args(["unpublished", repo])
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("SPIRA_TOML", format!("{}:{}", home.join("testlib/complete.toml").display(), self.p("layer.toml").display()))
            .env("SPIRA_HOME", &home)
            .env("SPIRA_REPO", self.p("repo"))
            .output()
            .unwrap();
        (out.status.code().unwrap(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    fn range(&self, repo: &str) -> serde_json::Value {
        let (code, out) = self.lc(repo);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }
}

#[test]
fn a_landing_of_three_commits_is_three_not_one_and_arrives_when_its_oldest_did() {
    let f = Fixture::build("multi", "");
    f.git("repo", &["checkout", "-q", "-b", "side"], None);
    for (i, m) in ["a", "b", "c"].iter().enumerate() {
        f.commit("repo", m, 9_000 - i as u64);
    }
    f.git("repo", &["checkout", "-q", "local/main"], None);
    f.git("repo", &["merge", "-q", "--ff-only", "side"], Some(3_600));
    let v = f.range("fixlocal");
    assert_eq!(v["count"], 3, "{v}");
    assert_eq!(v["landing_tip"], f.git("repo", &["rev-parse", "local/main"], None));
    assert_eq!(v["forge_tip"], f.git("repo", &["rev-parse", "origin/main"], None));
    let arrival = v["oldest_arrival"].as_u64().unwrap();
    assert!(arrival.abs_diff(now() - 3_600) < 120, "the reflog entry of the landing, not the author date: {v}");
}

#[test]
fn nothing_unpublished_is_a_measured_zero_with_no_arrival() {
    let f = Fixture::build("zero", "");
    let v = f.range("fixlocal");
    assert_eq!((v["count"].as_u64(), v["oldest_arrival"].is_null()), (Some(0), true), "{v}");
}

#[test]
fn the_forge_tip_is_fetched_at_the_call_not_read_from_the_last_fetch() {
    let f = Fixture::build("fresh", "");
    f.commit("repo", "c1", 60);
    assert_eq!(f.range("fixlocal")["count"], 1);
    f.git("repo", &["push", "-q", "origin", "local/main:main"], None);
    f.git("repo", &["update-ref", "-d", "refs/remotes/origin/main"], None);
    let pushed = f.git("repo", &["rev-parse", "local/main"], None);
    let v = f.range("fixlocal");
    assert_eq!((v["count"].as_u64(), v["forge_tip"].as_str()), (Some(0), Some(pushed.as_str())), "{v}");
}

#[test]
fn a_per_repo_publish_target_is_measured_not_the_default_remote() {
    let f = Fixture::build("override", "[publish.fixlocal]\nremote = \"upstream\"\nbranch = \"release\"\n");
    f.commit("repo", "c1", 60);
    f.git("repo", &["push", "-q", "origin", "local/main:main"], None);
    let v = f.range("fixlocal");
    assert_eq!(v["count"], 1, "origin is level but the declared target is not: {v}");
    assert_eq!(v["forge_tip"], f.git("repo", &["rev-parse", "upstream/release"], None));
}

#[test]
fn what_it_cannot_measure_is_refused_never_a_zero() {
    let f = Fixture::build("blind", "");
    f.commit("repo", "c1", 60);
    fs::remove_file(f.p("repo/.git/logs/refs/heads/local/main")).unwrap();
    let (code, out) = f.lc("fixlocal");
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("reflog"), "{out}");

    let (code, out) = f.lc("nosuch");
    assert_eq!(code, 2, "{out}");

    let h = Fixture::build("unreachable", "");
    h.git("repo", &["remote", "set-url", "origin", "/nonexistent/forge.git"], None);
    let (code, out) = h.lc("fixlocal");
    assert_eq!(code, 2, "a forge that cannot be fetched is not a zero backlog: {out}");
    assert!(out.contains("fetch of origin/main failed"), "{out}");
}

#[test]
fn a_repo_declared_to_publish_nowhere_is_skipped_not_refused() {
    let g = Fixture::build("nowhere", "[publish.fixlocal]\nremote = \"none\"\n");
    g.commit("repo", "c1", 60);
    let v = g.range("fixlocal");
    assert_eq!(v["declared_unpublished"], true, "{v}");
    assert!(v.get("count").is_none(), "a declared-unpublished repo has no backlog to count: {v}");
}

#[test]
fn a_push_mode_repo_is_measured_against_its_remote_branch() {
    let f = Fixture::build("push", "[publish.fixlocal]\nremote = \"upstream\"\nbranch = \"release\"\nmode = \"push\"\n");
    f.commit("repo", "c1", 60);
    f.git("repo", &["push", "-q", "origin", "local/main:main"], None);
    let v = f.range("fixlocal");
    assert_eq!(v["count"], 1, "origin is level but the push target is not: {v}");
    assert_eq!(v["forge_tip"], f.git("repo", &["rev-parse", "upstream/release"], None));
    f.git("repo", &["push", "-q", "upstream", "local/main:release"], None);
    assert_eq!(f.range("fixlocal")["count"], 0);
}
