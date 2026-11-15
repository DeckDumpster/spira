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
    a_value_overwritten_or_deleted_inside_the_ring_reads_as_of_a_revision_between_the_writes,
    a_lock_expires_on_the_virtual_clock,
    a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    reopen_extends_every_lock,
    a_lock_ttl_below_the_minimum_is_refused,
    a_finalizer_blocks_delete_until_the_write_clearing_it_removes_the_key,
    an_owner_reference_orphan_is_collected,
    an_owner_deleted_with_more_dependents_than_a_pass_is_drained_over_passes,
    a_put_naming_an_owner_the_batch_writes_or_one_that_lives_queues_no_marker,
    an_owner_recreated_before_collection_keeps_its_dependents,
    owned_by_names_a_keys_dependents_until_they_drop_the_reference,
    a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    request_keys_expire_at_24h_of_virtual_time,
    a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    a_put_under_intents_alone_writes_one_events_key,
    a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    a_rewrite_restarts_a_keys_ttl,
    a_reopen_renews_no_lifetime,
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
    a_lock_renew_op_keeps_the_token_restarts_the_ttl_and_runs_only_while_held,
    a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    gc_range_skips_the_history_prefix,
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

/// Puts at 1 and 2, then the delete at 3: no row is left at the head, so a counter derived from
/// the keys or a rowid would read back 1.
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

/// The ring is empty after a restart and its floor is the head the file held at close: a read
/// below it is `TooOld`, a watch from it sees only the jump, and a read model built over the
/// reopened store serves at the head.
#[test]
fn straight_after_a_restart_history_below_the_close_head_is_too_old_and_the_head_is_served() {
    let dir = TempDir::new("sqlite-restart-history");
    let path = dir.path().join("s.db");
    let clock = VirtualClock::new(1_000);
    let (r1, close) = {
        let c = contract(open(&path), &clock);
        let r1 = put(&c, "t/a", json!(1), "1");
        put(&c, "t/a", json!(2), "2");
        (r1, c.head().unwrap())
    };
    let c = contract(open(&path), &clock);
    let head = c.head().unwrap();
    assert!(head > close, "the open jumped the head");
    assert_eq!(c.get_at("t/a", r1), Err(Error::TooOld { compacted: close }));
    assert_eq!(c.watch("t/", r1, 0), Err(Error::TooOld { compacted: close }));
    assert_eq!(
        c.watch("t/", close, 0).unwrap(),
        state_store::Watched::Progress { progress_rev: head }
    );
    assert_eq!(c.get_at("t/a", head).unwrap().unwrap().value.data, json!(2));
    drop(c);
    let modeled = state_store::Modeled::open(open(&path), &[], 1_024).unwrap();
    let head = modeled.head().unwrap();
    assert_eq!(modeled.range("t/", None, 10, head).unwrap().len(), 1);
}

/// A ring that has turned past a revision answers `TooOld` for the read and the watch.
#[test]
fn a_revision_the_ring_has_turned_past_is_too_old_to_read_or_watch() {
    let dir = TempDir::new("sqlite-ring-turns");
    let clock = VirtualClock::new(1_000);
    let b = SqliteBackend::open(
        &dir.path().join("s.db"),
        Options { history_bytes: 4_000, ..Options::default() },
    )
    .unwrap();
    let c = contract(b, &clock);
    let r1 = put(&c, "t/a", json!(0), "0");
    assert!(c.get_at("t/a", r1).is_ok());
    for i in 1..60 {
        put(&c, "t/a", json!(i), &i.to_string());
    }
    let floor = c.backend().compacted().unwrap();
    assert!(floor > r1);
    assert_eq!(c.get_at("t/a", r1), Err(Error::TooOld { compacted: floor }));
    assert_eq!(c.watch("t/", r1, 0), Err(Error::TooOld { compacted: floor }));
    assert!(c.get_at("t/a", floor).is_ok());
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
        ..Options::default()
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
fn gc_range_skips_events_and_history_and_pages_across_them() {
    let dir = TempDir::new("sqlite-gc-range");
    let b = open(&dir.path().join("s.db"));
    let keys = [
        "a",
        "events/00000000000000000001",
        "events/00000000000000000002",
        "f",
        "g",
        "history/sp-a/0/r1",
        "history/sp-a/1/r2",
        "i",
    ];
    let batch: Vec<Write> = keys.iter().map(|k| put_write(k, b"{}", 1, None)).collect();
    b.commit(0, &batch).unwrap();
    let mut got = Vec::new();
    let mut after: Option<String> = None;
    while let Some((k, _)) = b.gc_range(after.as_deref(), 1, 1).unwrap().pop() {
        after = Some(k.clone());
        got.push(k);
    }
    assert_eq!(got, ["a", "f", "g", "i"]);
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
