use serde_json::Value;
use spira_sim::runstate::{flocked_inodes, read, unit_row, verdict_row, TABLES};
use std::path::Path;

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn rows<'a>(all: &'a [(&'static str, Value)], t: &str) -> Vec<&'a Value> {
    all.iter().filter(|(n, _)| *n == t).map(|(_, v)| v).collect()
}

#[test]
fn a_verdict_file_is_a_row_keyed_by_its_name() {
    let r = verdict_row("abc", "when=2026-10-24T13:55:20Z\nat=1792850120\nby=spira/sp-x\nrepo=spira\nbranch=spira/sp-x\nsuites=-\ncompose=unit\n");
    assert_eq!((r["key"].as_str(), r["stamped"].as_i64(), r["compose"].as_str(), r["suites"].as_str()), (Some("abc"), Some(1792850120), Some("unit"), Some("-")));
    assert!(verdict_row("k", "garbage")["branch"].is_null());
}

#[test]
fn flocked_inodes_reads_only_flock_rows() {
    let locks = "1: FLOCK  ADVISORY  WRITE 4242 08:01:131077 0 EOF\n2: POSIX  ADVISORY  WRITE 4243 08:01:999 0 EOF\n3: FLOCK  ADVISORY  READ 1 00:2c:7 0 EOF\n";
    assert_eq!(flocked_inodes(locks).into_iter().collect::<Vec<_>>(), vec![7, 131077]);
}

#[test]
fn the_run_directory_reads_as_one_row_set_per_family() {
    let w = testkit::TempDir::new("sim-runstate");
    put(&w, "verdicts/k1", "repo=sim\nbranch=spira/sp-a\nat=5\nsuites=test-a.sh\ncompose=unit\nby=x\n");
    put(&w, "queue/sim/publish", "pr=1\nmembers=sp-a:t\n");
    put(&w, "queue/sim/round-seq", "7\n");
    put(&w, "queue/sim/lock", "pid=99 verb=round certify\n");
    put(&w, "queue/sim/step.lock", "");
    put(&w, "ejected/sp-a", "test-a.sh,test-b.sh");
    put(&w, "landing.lock", "");
    put(&w, "worktree/.gate.repo.lock", "4 5\n");
    put(&w, "gate.log", "2026-10-25T03:31:26Z sim spira/sp-a rc=0\n2026-10-25T03:32:00Z sim spira/sp-b rc=1\n");
    put(&w, "landing.log", "QUEUE ROUND-LAND 1 repo=sim batch=b members=sp-a:t\n");
    put(&w, "sim-units/round-controller-run-b1-3-0", "inactive\nexit=7\n");
    put(&w, "verdicts/trees/sim/8c7a9232", "repo=sim\nbranch=round/sim-1\nby=round GREEN\n");
    let all = read(&w, "");
    let keys: Vec<&str> = rows(&all, "run_verdicts").iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["k1", "trees/sim/8c7a9232"], "a round's certificate lives under trees/<repo>/<tree>");
    let q: Vec<(String, String)> = rows(&all, "run_queue").iter().map(|r| (r["file"].as_str().unwrap().into(), r["key"].as_str().unwrap_or("-").into())).collect();
    assert!(q.contains(&("publish".into(), "members".into())) && q.contains(&("round-seq".into(), "-".into())), "{q:?}");
    assert!(!q.iter().any(|(f, _)| f.contains("lock")), "lock files belong to run_locks: {q:?}");
    assert_eq!(rows(&all, "run_ejected")[0]["suites"], "test-a.sh,test-b.sh");
    let locks: Vec<(&str, Option<&str>)> = rows(&all, "run_locks").iter().map(|r| (r["name"].as_str().unwrap(), r["holder"].as_str())).collect();
    assert_eq!(locks, vec![("queue/sim/lock", Some("99")), ("queue/sim/step.lock", None), ("landing.lock", None), (".gate.repo.lock", Some("4"))]);
    assert!(rows(&all, "run_locks").iter().all(|r| r["held"] == false));
    assert_eq!(rows(&all, "run_gate_log").iter().map(|r| r["rc"].as_i64().unwrap()).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(rows(&all, "run_landing_log")[0]["event"], "ROUND-LAND");
    let u = rows(&all, "run_units");
    assert_eq!((u.len(), u[0]["name"].as_str(), u[0]["state"].as_str(), u[0]["exit"].as_str()), (1, Some("round-controller-run-b1-3-0"), Some("inactive"), Some("7")));
    assert!(unit_row("u", "active\n")["exit"].is_null());
}

#[test]
fn an_absent_run_directory_reads_as_nothing() {
    let w = testkit::TempDir::new("sim-runstate-empty");
    assert!(read(&w.join("no-such-run"), "").is_empty());
}

#[test]
fn every_table_is_declared_once() {
    let mut names: Vec<_> = TABLES.iter().map(|t| t.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), TABLES.len());
}
