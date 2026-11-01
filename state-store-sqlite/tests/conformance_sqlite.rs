//! The SQLite backend's one end-to-end suite: every conformance scenario on a fresh file, the
//! backend's own primitives, and the bench binary.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::VirtualClock;
use serde_json::json;
use state_store::conformance::{self as suite, Rig, SCENARIOS};
use state_store::{Backend, Contract, Deleted, Error, Object, Request, Rev, Txn, Write};
use state_store_sqlite::{BackupStep, Options, SqliteBackend};
use testkit::TempDir;

struct Sqlite {
    dir: TempDir,
    n: AtomicUsize,
}

impl Sqlite {
    fn new() -> Self {
        Sqlite {
            dir: TempDir::new("state-store-sqlite"),
            n: AtomicUsize::new(0),
        }
    }
}

impl Rig for Sqlite {
    type B = SqliteBackend;
    fn fresh(&self) -> SqliteBackend {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        let path = self.dir.path().join(format!("store-{n}.db"));
        SqliteBackend::open(&path, Options::default()).expect("open")
    }
    /// One opener per file, so a second handle shares the first's connections; a real
    /// close and reopen is its own test below.
    fn reopen(&self, b: &SqliteBackend) -> SqliteBackend {
        b.clone()
    }
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        $( #[test] fn $name() { suite::$name(&Sqlite::new()); } )*
        const COVERED: &[&str] = &[$(stringify!($name)),*];
    };
}

scenarios!(
    cas_conflict_on_a_stale_mod_rev,
    a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal,
    one_main_per_txn_with_main_sub_per_key,
    the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening,
    create_with_an_equal_value_succeeds_and_a_different_value_conflicts,
    list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages,
    watch_resumes_from_a_revision,
    a_watch_on_a_quiet_prefix_returns_progress_rev,
    watch_returns_too_old_after_compaction,
    watch_delivers_a_txns_events_together,
    a_lock_expires_on_the_virtual_clock,
    a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    reopen_extends_every_lock,
    a_lock_ttl_below_the_minimum_is_refused,
    a_finalizer_blocks_delete_until_gc_removes_the_key_once_cleared,
    an_owner_reference_orphan_is_collected,
    a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    request_keys_expire_at_24h_of_virtual_time,
    a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    a_put_under_intents_alone_writes_one_events_key,
    a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    a_rewrite_restarts_a_keys_ttl,
    reopen_restarts_every_ttl,
    a_gc_expiry_writes_no_events_key,
    a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes,
    txn_ops_at_the_limit_pass_and_one_over_is_refused,
    txn_compares_at_the_limit_pass_and_one_over_is_refused,
    request_bytes_at_the_limit_pass_and_one_over_is_refused,
    a_key_twice_in_one_txn_is_refused,
    caller_writes_to_the_contracts_own_prefixes_are_refused,
);

#[test]
fn every_scenario_of_the_suite_is_its_own_test_here() {
    assert_eq!(COVERED, SCENARIOS);
}

fn contract(b: SqliteBackend, clock: &VirtualClock) -> Contract<SqliteBackend> {
    suite::open(b, clock)
}

fn req(id: &str) -> Request {
    Request::new(id, "sqlite-test", "test.write")
}

fn put(c: &Contract<SqliteBackend>, key: &str, v: serde_json::Value, id: &str) -> u64 {
    let r = c
        .txn(&Txn::new().put(key, Object::new(v)), &req(id))
        .unwrap();
    assert!(r.succeeded, "{key}: {r:?}");
    r.rev
}

fn open(path: &Path) -> SqliteBackend {
    SqliteBackend::open(path, Options::default()).unwrap()
}

fn keys_at(b: &SqliteBackend, prefix: &str, at: u64) -> Vec<(String, Vec<u8>)> {
    b.range(prefix, None, usize::MAX, at)
        .unwrap()
        .into_iter()
        .map(|(k, v)| (k, v.value))
        .collect()
}

#[test]
fn after_deleting_the_highest_revision_keys_and_reopening_the_next_main_is_greater() {
    let dir = TempDir::new("sqlite-reopen");
    let path = dir.path().join("s.db");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&path), &clock);
    put(&c, "n/a", json!(1), "1");
    put(&c, "n/b", json!(2), "2");
    c.delete("n/b", None, &req("3")).unwrap();
    let Deleted::Removed { rev: last } = c.delete("n/a", None, &req("4")).unwrap() else {
        panic!("not removed")
    };
    drop(c);
    let b = open(&path);
    assert_eq!(
        b.head().unwrap(),
        last,
        "the counter is read back from its meta row"
    );
    let c = contract(b, &clock);
    let next = put(&c, "n/c", json!(3), "5");
    assert!(next > last, "{next} <= {last}");
}

#[test]
fn journal_mode_and_synchronous_read_back_as_wal_and_2() {
    let dir = TempDir::new("sqlite-pragmas");
    let b = open(&dir.path().join("s.db"));
    assert_eq!(b.pragmas().unwrap(), ("wal".to_string(), 2));
}

#[test]
fn a_second_open_of_the_same_file_is_refused() {
    let dir = TempDir::new("sqlite-exclusive");
    let path = dir.path().join("s.db");
    let first = open(&path);
    match SqliteBackend::open(&path, Options::default()) {
        Err(Error::Backend(m)) => assert!(m.contains("already open"), "{m}"),
        Err(e) => panic!("refused for another reason: {e}"),
        Ok(_) => panic!("a second opener was let in"),
    }
    drop(first);
    open(&path);
}

fn put_write(key: &str, value: &[u8], main: u64, prev: Option<Rev>) -> Write {
    Write::Put {
        key: key.into(),
        value: value.to_vec(),
        create_rev: prev.map_or(Rev::new(main, 0), |_| Rev::new(1, 0)),
        prev,
    }
}

#[test]
fn a_reader_held_past_max_read_ms_is_ended_and_the_next_checkpoint_truncates_the_wal() {
    let dir = TempDir::new("sqlite-reader");
    let path = dir.path().join("s.db");
    let clock = VirtualClock::new(0);
    let slot: Arc<Mutex<Option<SqliteBackend>>> = Arc::new(Mutex::new(None));
    let armed = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(false));
    let probe = {
        let (slot, armed, blocked, clock) =
            (slot.clone(), armed.clone(), blocked.clone(), clock.clone());
        Arc::new(move || {
            if !armed.swap(false, Ordering::SeqCst) {
                return;
            }
            let b = slot.lock().unwrap().clone().unwrap();
            clock.advance(101);
            for _ in 0..20 {
                let head = b.head().unwrap();
                b.commit(
                    head,
                    &[put_write(
                        "k",
                        &[7; 4096],
                        head + 1,
                        Some(Rev::new(head, 0)),
                    )],
                )
                .unwrap();
            }
            blocked.store(b.checkpoint().unwrap().busy, Ordering::SeqCst);
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let opts = Options {
        max_read_ms: 100,
        readers: 1,
        clock: Arc::new(clock.clone()),
        read_probe: Some(probe),
    };
    let b = SqliteBackend::open(&path, opts).unwrap();
    *slot.lock().unwrap() = Some(b.clone());
    b.commit(0, &[put_write("k", b"v", 1, None)]).unwrap();

    armed.store(true, Ordering::SeqCst);
    let head = b.head().unwrap();
    match b.range("", None, 10, head) {
        Err(Error::Backend(m)) => assert!(m.contains("max_read_ms"), "{m}"),
        other => panic!("the held reader was not ended: {other:?}"),
    }
    assert!(
        blocked.load(Ordering::SeqCst),
        "the held reader did not block the checkpoint"
    );
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let ck = b.checkpoint().unwrap();
    assert!(!ck.busy, "{ck:?}");
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len(),
        0,
        "the WAL was not truncated"
    );
    *slot.lock().unwrap() = None;
}

#[test]
fn bulk_load_reads_back_every_key_at_one_revision_and_the_next_txn_is_the_one_after() {
    let dir = TempDir::new("sqlite-bulk");
    let b = open(&dir.path().join("s.db"));
    let r = 5_000;
    let items: Vec<(String, Vec<u8>)> = (0..300)
        .map(|i| (format!("lifecycle/{i:04}"), Object::new(json!(i)).encode()))
        .collect();
    b.bulk_load(r, items.clone()).unwrap();
    assert_eq!(b.head().unwrap(), r);
    let got = b.range("", None, usize::MAX, r).unwrap();
    assert_eq!(got.len(), items.len());
    for (k, v) in &got {
        assert_eq!(v.mod_rev.main, r, "{k}");
        assert_eq!(v.create_rev, v.mod_rev, "{k}");
    }
    assert_eq!(keys_at(&b, "", r), items);
    assert!(
        b.bulk_load(r + 10, vec![("x".into(), b"{}".to_vec())])
            .is_err(),
        "a bulk load into a store that holds keys"
    );
    let clock = VirtualClock::new(1_000);
    let c = contract(b, &clock);
    assert_eq!(put(&c, "lifecycle/new", json!(1), "after-import"), r + 1);
}

#[test]
fn a_backup_taken_in_steps_while_commits_continue_matches_the_live_keys_at_its_revision() {
    let dir = TempDir::new("sqlite-backup");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..60 {
        put(&c, &format!("b/{i:03}"), json!(big), &format!("seed{i}"));
    }
    let dest = dir.path().join("copy.db");
    let mut steps = 0;
    let rev = loop {
        match c.backend().backup_step(&dest, 4).unwrap() {
            BackupStep::Copying { .. } => {
                steps += 1;
                put(
                    &c,
                    &format!("b/{steps:03}"),
                    json!(steps),
                    &format!("during{steps}"),
                );
                put(
                    &c,
                    &format!("during/{steps:03}"),
                    json!(steps),
                    &format!("new{steps}"),
                );
            }
            BackupStep::Done { rev } => break rev,
        }
    };
    assert!(steps > 1, "the backup finished in one step");
    put(&c, "after/backup", json!(1), "after");
    let live = c.backend().clone();
    let copy = open(&dest);
    assert_eq!(
        copy.head().unwrap(),
        rev,
        "the copy is tagged with the revision it captured"
    );
    assert_eq!(keys_at(&copy, "", rev), keys_at(&live, "", rev));
    assert_eq!(keys_at(&copy, "during/", rev).len(), steps);
}

#[test]
fn compact_below_the_floor_works_and_compact_above_it_stops_at_the_floor() {
    let dir = TempDir::new("sqlite-compact");
    let path = dir.path().join("s.db");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&path), &clock);
    let revs: Vec<u64> = (1..=4)
        .map(|i| put(&c, "k", json!(i), &format!("{i}")))
        .collect();
    let b = c.backend().clone();
    b.set_compaction_floor(revs[2]).unwrap();

    b.compact(revs[1]).unwrap();
    assert_eq!(b.compacted().unwrap(), revs[1]);
    assert_eq!(
        b.get("k", revs[0]),
        Err(Error::TooOld { compacted: revs[1] })
    );
    let at = |rev| {
        Object::decode(&b.get("k", rev).unwrap().unwrap().value)
            .unwrap()
            .data
    };
    assert_eq!(at(revs[1]), json!(2));

    b.compact(revs[3]).unwrap();
    assert_eq!(
        b.compacted().unwrap(),
        revs[2],
        "compaction ran past the floor"
    );
    assert_eq!(
        at(revs[2]),
        json!(3),
        "the floor's revision is still readable"
    );
    assert!(b
        .log(revs[2], 10)
        .unwrap()
        .iter()
        .any(|e| e.rev.main == revs[3]));
    assert_eq!(
        b.log(revs[1], 10),
        Err(Error::TooOld { compacted: revs[2] })
    );
    drop(c);
    drop(b);
    let b = open(&path);
    b.compact(revs[3]).unwrap();
    assert_eq!(
        b.compacted().unwrap(),
        revs[2],
        "the floor did not survive a reopen"
    );
}

#[test]
fn gc_range_skips_the_audit_history_and_pages_across_it() {
    let dir = TempDir::new("sqlite-gc-range");
    let b = open(&dir.path().join("s.db"));
    let keys = [
        "a",
        "events/00000000000000000001",
        "events/00000000000000000002",
        "f",
        "g",
    ];
    let batch: Vec<Write> = keys.iter().map(|k| put_write(k, b"{}", 1, None)).collect();
    b.commit(0, &batch).unwrap();
    let mut got = Vec::new();
    let mut after: Option<String> = None;
    while let Some((k, _)) = b.gc_range(after.as_deref(), 1, 1).unwrap().pop() {
        after = Some(k.clone());
        got.push(k);
    }
    assert_eq!(got, ["a", "f", "g"]);
}

fn bench() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_state-store-bench"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    cmd
}

#[test]
fn the_bench_reports_four_quantiles_and_a_sample_count() {
    let dir = TempDir::new("sqlite-bench");
    let load = dir.path().join("load");
    std::fs::create_dir(&load).unwrap();
    let out = bench()
        .args(["--path", &dir.path().join("b.db").display().to_string()])
        .args(["--rate", "100", "--duration-secs", "2", "--json"])
        .args([
            "--load-dir",
            &load.display().to_string(),
            "--load-mb-per-s",
            "4",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for q in ["p50_ms", "p99_ms", "p999_ms", "max_ms"] {
        assert!(report[q].is_number(), "{q} missing: {report}");
    }
    assert!(report["samples"].as_u64().unwrap() > 0, "{report}");
}

#[test]
fn bench_conformance_on_a_populated_file_passes_and_leaves_no_conformance_keys() {
    let dir = TempDir::new("sqlite-live");
    let path = dir.path().join("live.db");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&path), &clock);
    for i in 0..20 {
        put(
            &c,
            &format!("lifecycle/{i}"),
            json!({"s": "READY"}),
            &format!("seed{i}"),
        );
    }
    let before = c.backend().head().unwrap();
    let real = keys_at(c.backend(), "lifecycle/", before);
    drop(c);

    let out = bench()
        .args(["conformance", "--path", &path.display().to_string()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let b = open(&path);
    let head = b.head().unwrap();
    assert!(head > before, "the run wrote nothing");
    assert!(
        keys_at(&b, "conformance/", head).is_empty(),
        "the scratch prefix was left behind"
    );
    assert_eq!(
        keys_at(&b, "lifecycle/", head),
        real,
        "the run touched the live keys"
    );
}
