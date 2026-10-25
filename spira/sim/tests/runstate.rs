use serde_json::{json, Value};
use spira_sim::runstate::{flocked_inodes, gate_log_row, kv, landing_log_row, normalise, read, table, verdict_row, TABLES};
use spira_sim::trace::{violations, Snapshot, Trace};
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
fn a_gate_log_line_yields_its_branch_and_exit_code() {
    let r = gate_log_row(3, "2026-10-25T03:31:26Z spira spira/sp-8jytkl waited=0s ran=198s rc=1 branch-red compose=suites(script)");
    assert_eq!((r["repo"].as_str(), r["branch"].as_str(), r["rc"].as_i64(), r["line"].as_i64()), (Some("spira"), Some("spira/sp-8jytkl"), Some(1), Some(3)));
    assert!(gate_log_row(1, "2026-10-25T03:19:38Z identity spira/x waited=0s")["rc"].is_null());
}

#[test]
fn a_landing_line_is_either_a_timestamped_repo_line_or_a_queue_record() {
    let a = landing_log_row("landing.log", 1, "2026-09-15T05:35:45Z spira: CHECK6 sp-0wqx: not landed");
    assert_eq!((a["ts"].as_str(), a["repo"].as_str(), a["text"].as_str(), a["event"].is_null()), (Some("2026-09-15T05:35:45Z"), Some("spira"), Some("CHECK6 sp-0wqx: not landed"), true));
    let b = landing_log_row("landing.log", 2, "QUEUE ROUND-LAND 1800000310 repo=sim batch=b1 head=h members=sp-a:t1");
    assert_eq!((b["event"].as_str(), b["repo"].as_str(), b["ts"].is_null()), (Some("ROUND-LAND"), Some("sim"), true));
    assert!(b["text"].as_str().unwrap().contains("members=sp-a:t1"));
    assert!(landing_log_row("landing.log", 3, "2026-09-15T05:35:45Z no repo here")["repo"].is_null());
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
    let all = read(&w, "");
    assert_eq!(rows(&all, "run_verdicts").len(), 1);
    let q: Vec<(String, String)> = rows(&all, "run_queue").iter().map(|r| (r["file"].as_str().unwrap().into(), r["key"].as_str().unwrap_or("-").into())).collect();
    assert!(q.contains(&("publish".into(), "members".into())) && q.contains(&("round-seq".into(), "-".into())), "{q:?}");
    assert!(!q.iter().any(|(f, _)| f.contains("lock")), "lock files belong to run_locks: {q:?}");
    assert_eq!(rows(&all, "run_ejected")[0]["suites"], "test-a.sh,test-b.sh");
    let locks: Vec<(&str, Option<&str>)> = rows(&all, "run_locks").iter().map(|r| (r["name"].as_str().unwrap(), r["holder"].as_str())).collect();
    assert_eq!(locks, vec![("queue/sim/lock", Some("99")), ("queue/sim/step.lock", None), ("landing.lock", None), (".gate.repo.lock", Some("4"))]);
    assert!(rows(&all, "run_locks").iter().all(|r| r["held"] == false));
    assert_eq!(rows(&all, "run_gate_log").iter().map(|r| r["rc"].as_i64().unwrap()).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(rows(&all, "run_landing_log")[0]["event"], "ROUND-LAND");
}

#[test]
fn an_absent_run_directory_reads_as_nothing() {
    let w = testkit::TempDir::new("sim-runstate-empty");
    assert!(read(&w.join("no-such-run"), "").is_empty());
}

#[test]
fn a_held_flock_is_seen_held_and_a_released_one_is_not() {
    use std::os::unix::fs::MetadataExt;
    if std::process::Command::new("flock").arg("--version").output().is_err() {
        eprintln!("SKIPPED: no flock binary");
        return;
    }
    let w = testkit::TempDir::new("sim-runstate-held");
    put(&w, "landing.lock", "");
    let lock = w.join("landing.lock");
    use std::os::unix::process::CommandExt;
    let mut holder = std::process::Command::new("flock").arg("-x").arg(&lock).args(["sleep", "30"]).process_group(0).spawn().unwrap();
    let ino = std::fs::metadata(&lock).unwrap().ino();
    let mut seen = false;
    for _ in 0..100 {
        seen = flocked_inodes(&std::fs::read_to_string("/proc/locks").unwrap()).contains(&ino);
        if seen {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let held = read(&w, &std::fs::read_to_string("/proc/locks").unwrap());
    let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{}", holder.id())).status();
    let _ = holder.wait();
    assert!(seen, "the flock never showed in /proc/locks");
    assert_eq!(rows(&held, "run_locks")[0]["held"], true);
    let mut free = false;
    for _ in 0..100 {
        free = rows(&read(&w, &std::fs::read_to_string("/proc/locks").unwrap()), "run_locks")[0]["held"] == false;
        if free {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(free, "the flock stayed held after its holder was killed");
}

#[test]
fn a_row_with_a_field_its_table_lacks_is_refused() {
    let t = table("run_ejected").unwrap();
    assert!(normalise(t, &json!({"bead": "sp-a", "suite": "x"})).unwrap_err().contains("unknown field"));
    assert_eq!(normalise(t, &json!({"bead": "sp-a"})).unwrap()["suites"], Value::Null);
    assert_eq!(kv("a=1\nnoise\nb = 2").len(), 2);
}

fn snapshot_with(seq: u64, t: &Trace, run: Vec<(&'static str, Value)>) {
    t.snapshot(seq, &Snapshot { run, ..Default::default() }).unwrap();
}

#[test]
fn state_families_repeat_per_snapshot_and_log_lines_appear_once() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = testkit::TempDir::new("sim-runstate-trace");
    let t = Trace::create(&w).unwrap();
    let verdict = ("run_verdicts", verdict_row("k", "branch=spira/sp-a\n"));
    let line = ("run_landing_log", landing_log_row("landing.log", 1, "QUEUE ROUND-LAND 1 repo=sim batch=b members=sp-a:t"));
    snapshot_with(1, &t, vec![verdict.clone()]);
    snapshot_with(2, &t, vec![verdict.clone(), line.clone()]);
    snapshot_with(3, &t, vec![verdict, line]);
    let db = t.load(&w).unwrap();
    let count = |sql: &str| -> String { std::process::Command::new(spira_sim::trace::duckdb_bin()).arg(&db).arg("-csv").arg("-noheader").arg("-c").arg(sql).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap() };
    assert_eq!(count("SELECT count(*) FROM run_verdicts"), "3");
    assert_eq!(count("SELECT seq FROM run_landing_log"), "2");
}

#[test]
fn the_landing_invariant_needs_a_certificate_at_the_same_snapshot_and_the_lock_invariant_a_held_flock() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let land = ("run_landing_log", landing_log_row("landing.log", 1, "QUEUE ROUND-LAND 1 repo=sim batch=b members=sp-a:t"));
    let cert = ("run_verdicts", verdict_row("k", "branch=spira/sp-a\n"));
    let lock = |held: bool| ("run_locks", json!({"name": "landing.lock", "holder": null, "held": held}));
    let hits = |run: Vec<(&'static str, Value)>| {
        let w = testkit::TempDir::new("sim-runstate-inv");
        let t = Trace::create(&w).unwrap();
        snapshot_with(1, &t, run);
        let db = t.load(&w).unwrap();
        violations(&db, &[]).unwrap().into_iter().map(|v| v.view).collect::<Vec<_>>()
    };
    assert_eq!(hits(vec![land.clone()]), vec!["inv_round_land_has_certificate"]);
    assert!(hits(vec![land, cert]).is_empty());
    assert_eq!(hits(vec![lock(true)]), vec!["inv_no_lock_held_between_events"]);
    assert!(hits(vec![lock(false)]).is_empty());
}

#[test]
fn every_table_is_declared_once() {
    let mut names: Vec<_> = TABLES.iter().map(|t| t.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), TABLES.len());
}
