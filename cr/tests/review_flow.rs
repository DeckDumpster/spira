//! `cr open` then `cr seal` over a real git repository: the staged files are the source
//! commit's bytes, even after the landing ref has moved on.

#[path = "../src/tests/gh.rs"]
mod gh;

use cr::cli::run;
use cr::ledger::Ledger;
use cr::ports::{Config, GitTree, Ports, VirtualClock};
use cr::record;
use gh::{unbase64, FakeGh, Shared, NAME, OWNER};
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=alpha", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

#[test]
fn seal_stages_the_source_commit_bytes_after_the_landing_ref_has_moved() {
    let repo = testkit::TempDir::new("cr-flow-repo");
    let run_dir = testkit::TempDir::new("cr-flow-run");
    let p = repo.path();
    git(p, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(p.join("k/src")).unwrap();
    std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\"k\"]\n").unwrap();
    std::fs::write(p.join("k/Cargo.toml"), "[package]\nname = \"k\"\n").unwrap();
    std::fs::write(p.join("k/src/lib.rs"), b"//! K.\r\npub fn a() {}\r\n// no final newline").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "one"]);
    let sha = String::from_utf8(git(p, &["rev-parse", "HEAD"])).unwrap().trim().to_string();

    let gh = Rc::new(FakeGh::default());
    let ports = Ports {
        run_dir: run_dir.path().to_path_buf(),
        gh: Box::new(Shared(gh.clone())),
        repo_owner: OWNER.into(),
        repo_name: NAME.into(),
        source_repo: "k".into(),
        stdin: String::new(),
        clock: Box::new(VirtualClock::new(1_700_000_000_000)),
        tree: Box::new(GitTree::new(p.to_str().unwrap(), "main").unwrap()),
        ledger: Ledger::default(),
        config: Config { page_lines: 60, oversize_pages: 6, exclude: vec![], churn_days: 60 },
        env_seed: None,
    };
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let o = run(&args(&["open", "--aim", "k/src/lib.rs", "--seed", "1"]), &ports);
    assert_eq!(o.code, 0, "{}", o.stderr);

    std::fs::write(p.join("k/src/lib.rs"), "moved on\n").unwrap();
    git(p, &["commit", "-q", "-am", "two"]);

    let r = record::load(run_dir.path(), 1).unwrap();
    assert_eq!(r.source_sha, sha);
    let comment = format!(r#"[{{"path":"{}","line":1,"body":"b"}}]"#, r.unit.files[0].path);
    let mut sealing = ports;
    sealing.stdin = comment;
    let o = run(&args(&["seal", "--review", "001"]), &sealing);
    assert_eq!(o.code, 0, "{}", o.stderr);

    let blobs = gh.posted("git/blobs");
    assert_eq!(blobs.len(), r.unit.files.len() + 1);
    for (f, b) in r.unit.files.iter().zip(&blobs) {
        let want = git(p, &["show", &format!("{sha}:{}", f.path)]);
        assert_eq!(unbase64(b["content"].as_str().unwrap()), want, "{}", f.path);
    }
}
