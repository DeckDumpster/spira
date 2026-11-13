//! The SQLite backend's one end-to-end suite: every conformance scenario on a fresh file, and
//! the backend's own primitives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::VirtualClock;
use serde_json::json;
use state_store::conformance::{self as suite, Rig, SCENARIOS};
use state_store::{Backend, Contract, Error, Object, Request, Rev, Txn, Write};
use state_store_sqlite::{Options, SqliteBackend};
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
    a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused,
    a_key_twice_in_one_txn_is_refused,
    caller_writes_to_the_contracts_own_prefixes_are_refused,
    a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all,
    a_txn_acquiring_a_lock_held_by_another_holder_fails_whole,
    lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken,
    get_and_list_return_a_locks_expiry_and_renew_moves_it_forward,
    a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry,
    a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    gc_range_and_open_skip_the_history_prefix,
);

/// Scenarios with no unit test here: each runs over 100 ms (sp-bym7j8) and stays in the suite
/// for the live run alone.
const OVER_BUDGET: &[&str] = &["request_bytes_at_the_limit_pass_and_one_over_is_refused"];

#[test]
fn every_scenario_of_the_suite_is_its_own_test_here() {
    let want: Vec<&str> = SCENARIOS.iter().copied().filter(|s| !OVER_BUDGET.contains(s)).collect();
    assert_eq!(COVERED, want.as_slice());
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

/// Puts at 1 and 2, then the delete at 3 that compaction drops: no row is left at the head, so
/// a counter derived from the keys or a rowid would read back 1.
#[test]
fn after_deleting_the_highest_revision_keys_and_reopening_the_next_main_is_greater() {
    let dir = TempDir::new("sqlite-reopen");
    let path = dir.path().join("s.db");
    let b = open(&path);
    b.commit(0, &[put_write("n/a", b"{}", 1, None)]).unwrap();
    b.commit(1, &[put_write("n/b", b"{}", 2, None)]).unwrap();
    let del = Write::Delete {
        key: "n/b".into(),
        prev: Rev::new(2, 0),
    };
    let last = b.commit(2, &[del]).unwrap();
    b.compact(last).unwrap();
    let left: Vec<(String, u64)> = b
        .range("", None, 10, last)
        .unwrap()
        .into_iter()
        .map(|(k, v)| (k, v.mod_rev.main))
        .collect();
    assert_eq!(left, [("n/a".to_string(), 1)], "no row is left at the head");
    drop(b);
    let b = open(&path);
    let head = b.head().unwrap();
    assert!(head >= last, "the counter is read back from its meta row, never from the keys: head {head}, last {last}");
    let clock = VirtualClock::new(1_000);
    let c = contract(b, &clock);
    let next = put(&c, "n/c", json!(3), "after-reopen");
    assert!(next > last, "a reopened store never reissues a revision: next {next}, last {last}");
    assert_eq!(next, head + 1, "the next txn follows the head the reopen read back");
}

#[test]
fn journal_mode_and_synchronous_read_back_as_wal_and_normal() {
    let dir = TempDir::new("sqlite-pragmas");
    let b = open(&dir.path().join("s.db"));
    // NORMAL, per Ryan 2026-11-02 (sp-3uvzc6, sp-r3yb7x): the jump on open covers what a crash loses.
    assert_eq!(b.pragmas().unwrap(), ("wal".to_string(), 1));
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
        jump_on_open: true,
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

/// Reads its first `free` times unchanged, then jumps an hour: the reads after that are the
/// progress handler's, inside a statement.
struct JumpAfter {
    calls: AtomicUsize,
    free: AtomicUsize,
}

impl runtime::ports::Clock for JumpAfter {
    fn now_ms(&self) -> u64 {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.free.load(Ordering::SeqCst) {
            0
        } else {
            3_600_000
        }
    }
}

/// Every row a deleted key, so the statement scans thousands of rows and returns none: only the
/// progress handler can see the deadline pass. The read is below the head, where `versions` is
/// read: at the head, `latest` holds no deleted key to scan.
#[test]
fn a_single_long_statement_is_ended_past_max_read_ms_by_the_progress_handler() {
    let dir = TempDir::new("sqlite-progress");
    let clock = Arc::new(JumpAfter {
        calls: AtomicUsize::new(0),
        free: AtomicUsize::new(usize::MAX),
    });
    let opts = Options {
        max_read_ms: 100,
        readers: 1,
        clock: clock.clone(),
        read_probe: None,
        jump_on_open: true,
    };
    let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
    let keys: Vec<String> = (0..5_000).map(|i| format!("t/{i:05}")).collect();
    let puts: Vec<Write> = keys.iter().map(|k| put_write(k, b"{}", 1, None)).collect();
    b.commit(0, &puts).unwrap();
    let dels: Vec<Write> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| Write::Delete {
            key: k.clone(),
            prev: Rev::new(1, i as u32),
        })
        .collect();
    b.commit(1, &dels).unwrap();
    b.commit(2, &[put_write("u/after", b"{}", 3, None)]).unwrap();
    assert!(b.range("t/", None, 10, 2).unwrap().is_empty());

    clock.calls.store(0, Ordering::SeqCst);
    clock.free.store(2, Ordering::SeqCst);
    match b.range("t/", None, 10, 2) {
        Err(Error::Backend(m)) => assert!(m.contains("max_read_ms"), "{m}"),
        other => panic!("the long statement was not ended: {other:?}"),
    }
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
fn a_snapshot_copy_matches_the_live_keys_at_its_revision_and_refuses_an_existing_file() {
    let dir = TempDir::new("sqlite-backup");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..60 {
        put(&c, &format!("b/{i:03}"), json!(big), &format!("seed{i}"));
    }
    let dest = dir.path().join("copy.db");
    let rev = c.backend().snapshot_to(&dest).unwrap();
    put(&c, "after/backup", json!(1), "after");
    assert!(
        c.backend().snapshot_to(&dest).is_err(),
        "a copy never overwrites"
    );
    let live = c.backend().clone();
    // A copy being verified keeps its revisions exactly: no jump on this open.
    let copy = SqliteBackend::open(
        &dest,
        Options {
            jump_on_open: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(
        copy.head().unwrap(),
        rev,
        "the copy is tagged with the revision it captured"
    );
    assert_eq!(keys_at(&copy, "", rev), keys_at(&live, "", rev));
}

#[test]
fn a_snapshot_copy_taken_while_another_thread_commits_holds_one_revision() {
    let dir = TempDir::new("sqlite-backup-live");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..400 {
        put(&c, &format!("b/{i:03}"), json!(big), &format!("seed{i}"));
    }
    let dest = dir.path().join("copy.db");
    let stop = std::sync::atomic::AtomicBool::new(false);
    let rev = std::thread::scope(|sc| {
        let writer = sc.spawn(|| {
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                n += 1;
                put(&c, &format!("live/{n:05}"), json!(n), &format!("live{n}"));
            }
        });
        let rev = c.backend().snapshot_to(&dest).unwrap();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        rev
    });
    let live = c.backend().clone();
    let copy = SqliteBackend::open(&dest, Options { jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(copy.head().unwrap(), rev);
    assert_eq!(keys_at(&copy, "", rev), keys_at(&live, "", rev));
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

#[test]
fn a_snapshot_copy_does_not_carry_the_pages_deleted_rows_left_free_in_the_live_file() {
    let dir = TempDir::new("sqlite-snapshot-vacuum");
    let path = dir.path().join("s.db");
    let b = open(&path);
    let clock = VirtualClock::new(0);
    let c = contract(b, &clock);
    for i in 0..200 {
        put(&c, &format!("k/{i}"), json!("x".repeat(16_384)), &format!("p{i}"));
    }
    for i in 0..200 {
        c.delete(&format!("k/{i}"), None, &req(&format!("d{i}"))).unwrap();
    }
    c.backend().clear_compaction_floor().unwrap();
    c.compact(c.head().unwrap()).unwrap();
    let dest = dir.path().join("snap.db");
    c.backend().snapshot_to(&dest).unwrap();
    let live = std::fs::metadata(&path).unwrap().len();
    let snap = std::fs::metadata(&dest).unwrap().len();
    assert!(snap * 4 < live, "snapshot {snap} bytes beside a live file of {live}");
}

#[test]
fn a_snapshot_is_compacted_straight_to_its_destination_in_rollback_mode() {
    let dir = TempDir::new("sqlite-snapshot-direct");
    let clock = VirtualClock::new(1_000);
    let c = contract(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..60 {
        put(&c, &format!("b/{i:03}"), json!(big), &format!("seed{i}"));
    }
    let dest = dir.path().join("copy.db");
    let rev = c.backend().snapshot_to(&dest).unwrap();
    // Header bytes 18 and 19 are 2 only for a file a WAL travels with: the copy-then-VACUUM
    // path wrote a WAL of the whole store beside it.
    let head = std::fs::read(&dest).unwrap();
    assert_eq!((head[18], head[19]), (1, 1), "the snapshot is not a WAL-mode file");
    assert!(!dir.path().join("copy.db-wal").exists());
    let copy = SqliteBackend::open(
        &dest,
        Options {
            jump_on_open: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(copy.head().unwrap(), rev);
}
