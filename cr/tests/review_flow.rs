//! `cr open` then `cr seal` over a real git repository: the posted files are the source
//! commit's bytes, even after the landing ref has moved on, and the ledger row is committed.

#[macro_use]
extern crate spira_test_id;

use cr::cli::run;
use cr::ports::{Config, GitTree, Ports, VirtualClock};
use cr::record;
use margin_client::FakeMargin;
use std::path::Path;
use std::process::Command;

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

#[test_id(tz9b937)]
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

    let findings = testkit::TempDir::new("cr-flow-findings");
    git(findings.path(), &["init", "-q", "-b", "main"]);
    git(findings.path(), &["commit", "-q", "--allow-empty", "-m", "root"]);
    let margin = FakeMargin::default();
    let _env = testkit::env(&[("GIT_AUTHOR_NAME", Some("a")), ("GIT_AUTHOR_EMAIL", Some("a@a")), ("GIT_COMMITTER_NAME", Some("a")), ("GIT_COMMITTER_EMAIL", Some("a@a"))]);
    let ports = Ports {
        run_dir: run_dir.path().to_path_buf(),
        margin: Box::new(margin.clone()),
        findings_dir: findings.path().to_str().unwrap().to_string(),
        committer: Box::new(cr::ports::GitCommitter),
        source_repo: "k".into(),
        source_url: Ok("https://github.com/acme/k".into()),
        stdin: String::new(),
        clock: Box::new(VirtualClock::new(1_700_000_000_000)),
        tree: Box::new(GitTree::new(p.to_str().unwrap(), "main").unwrap()),
        beads: Box::new(cr::ports::FixtureBeads::default()),
        registry: Box::new(cr::ports::FixtureRegistry::default()),
        config: Config { size_unit_lines: 60, exclude: vec![], churn_days: 60, rewrite_days: 90, count_limit: 50, status_limit: 10 },
        env_seed: None,
    };
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let o = run(&args(&["open", "--aim", "k/src/lib.rs", "--seed", "1"]), &ports);
    assert_eq!(o.code, 0, "{}", o.stderr);

    std::fs::write(p.join("k/src/lib.rs"), "moved on\n").unwrap();
    git(p, &["commit", "-q", "-am", "two"]);

    let r = record::load(run_dir.path(), 1).unwrap();
    assert_eq!(r.source_sha, sha);
    let half = format!(r#"[{{"path":"{}","line":1,"body":"b"}}]"#, r.unit.files[0].path);
    let comment = format!(r#"{{"does":"d","look_first":"l","lens":{half},"fresh":{half}}}"#);
    let mut sealing = ports;
    sealing.stdin = comment;
    let o = run(&args(&["seal", "--review", "001"]), &sealing);
    assert_eq!(o.code, 0, "{}", o.stderr);

    let (_, _, docs) = margin.collection_posted(0);
    assert_eq!(docs.len(), r.unit.files.len());
    for (f, d) in r.unit.files.iter().zip(&docs) {
        let want = git(p, &["show", &format!("{sha}:{}", f.path)]);
        assert_eq!(d.text.as_bytes(), want.as_slice(), "{}", f.path);
    }
    let ledger = std::fs::read_to_string(findings.path().join("ledger.jsonl")).unwrap();
    assert_eq!(ledger.lines().count(), 1);
    assert!(git(findings.path(), &["log", "--format=%s", "--", "ledger.jsonl"]).starts_with(b"ledger: 001 opened"));
}
