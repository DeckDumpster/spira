//! The SQLite backend's one end-to-end suite: every conformance scenario on a fresh file, and
//! the backend's own primitives.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::{Clock, VirtualClock};
use serde_json::json;
use state_store::conformance::{self as suite, Rig, SCENARIOS};
use state_store::{Backend, Contract, Error, Object, Request, Rev, Txn, Write};
use state_store_sqlite::{Options, SqliteBackend, DRAIN_PER_COMMIT};
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
        SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).expect("open")
    }
    /// One opener per file, so a second handle shares the first's connections; a real
    /// close and reopen is its own test below.
    fn reopen(&self, b: &SqliteBackend) -> SqliteBackend {
        b.clone()
    }
}

macro_rules! scenarios {
    ($( #[test_id($id:ident)] $name:ident ),* $(,)?) => {
        $( #[test_id($id)] #[test] fn $name() { suite::$name(&Sqlite::new()); } )*
        const COVERED: &[&str] = &[$(stringify!($name)),*];
    };
}

scenarios!(
    #[test_id(tv8lizc)] cas_conflict_on_a_stale_mod_rev,
    #[test_id(tdzzxyu)] a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal,
    #[test_id(t4weasp)] one_main_per_txn_with_main_sub_per_key,
    #[test_id(tdufkjx)] the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening,
    #[test_id(twkmk0v)] create_with_an_equal_value_succeeds_and_a_different_value_conflicts,
    #[test_id(tpfgxxf)] list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages,
    #[test_id(t7q157v)] watch_resumes_from_a_revision,
    #[test_id(tlw1c9s)] a_watch_on_a_quiet_prefix_returns_progress_rev,
    #[test_id(ttxv85u)] closing_the_watches_answers_a_held_watch_and_every_later_one_with_progress,
    #[test_id(t2utpkd)] closing_the_watches_still_delivers_events_already_committed,
    #[test_id(tzvuojn)] watch_returns_too_old_after_compaction,
    #[test_id(th8xogq)] a_value_overwritten_or_deleted_inside_the_ring_reads_as_of_a_revision_between_the_writes,
    #[test_id(txodxgp)] get_many_answers_what_get_answers_per_key_at_the_head_and_below_it,
    #[test_id(t3r67uj)] a_lock_expires_on_the_virtual_clock,
    #[test_id(t90ml94)] a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    #[test_id(tgctsbf)] reopen_extends_every_lock,
    #[test_id(tnw7a5t)] a_lock_ttl_below_the_minimum_is_refused,
    #[test_id(tp2ajh1)] a_finalizer_blocks_delete_until_the_write_clearing_it_removes_the_key,
    #[test_id(tjg3ou4)] an_owner_reference_orphan_is_collected,
    #[test_id(t3wcipb)] an_owner_deleted_with_more_dependents_than_a_pass_is_drained_over_passes,
    #[test_id(toexjdy)] a_put_naming_an_owner_the_batch_writes_or_one_that_lives_queues_no_marker,
    #[test_id(typnmkc)] an_owner_recreated_before_collection_keeps_its_dependents,
    #[test_id(tgndzh1)] owned_by_names_a_keys_dependents_until_they_drop_the_reference,
    #[test_id(tgjw3a7)] a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    #[test_id(trqeas4)] retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    #[test_id(to4kdjp)] the_gc_never_expires_a_receipt,
    #[test_id(t5syc95)] a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    #[test_id(t8hbe1c)] a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    #[test_id(t2m1yrw)] a_put_under_intents_alone_writes_one_events_key,
    #[test_id(tmczog4)] a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    #[test_id(t82syw1)] ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    #[test_id(t9bxsqg)] a_rewrite_restarts_a_keys_ttl,
    #[test_id(ttzkmk4)] a_reopen_renews_no_lifetime,
    #[test_id(t8zh4mk)] a_gc_expiry_writes_no_events_key,
    #[test_id(ty2zkdb)] a_retained_prefix_put_may_carry_a_ttl_and_expires_only_with_one,
    #[test_id(t9lokqr)] a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes,
    #[test_id(t636zsk)] txn_ops_at_the_limit_pass_and_one_over_is_refused,
    #[test_id(txra1dn)] txn_compares_at_the_limit_pass_and_one_over_is_refused,
    #[test_id(t1143tq)] a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused,
    #[test_id(tau1pjo)] a_key_twice_in_one_txn_is_refused,
    #[test_id(t6tg85d)] caller_writes_to_the_contracts_own_prefixes_are_refused,
    #[test_id(t1sa7z1)] a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all,
    #[test_id(teiulle)] a_txn_acquiring_a_lock_held_by_another_holder_fails_whole,
    #[test_id(txzd8wt)] lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken,
    #[test_id(tdvkft7)] get_and_list_return_a_locks_expiry_and_renew_moves_it_forward,
    #[test_id(thpv0k5)] a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry,
    #[test_id(t4fw2hl)] a_lock_renew_op_keeps_the_token_restarts_the_ttl_and_runs_only_while_held,
    #[test_id(t0ko33c)] a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    #[test_id(tpi7j20)] a_kind_since_range_reads_the_keys_of_that_kind_after_that_time_at_the_head_and_below_it,
    #[test_id(tdw6bp9)] a_seq_range_reads_the_keys_above_a_seq_in_seq_order_at_the_head_and_below_it,
    #[test_id(tohf4df)] gc_range_skips_the_history_prefix,
    #[test_id(t8c6rne)] committed_at_is_the_wall_time_of_the_first_audited_commit_at_or_after_a_main,
);

/// Scenarios with no unit test here: each runs over 100 ms (sp-bym7j8) and stays in the suite
/// for the live run alone.
const OVER_BUDGET: &[&str] = &["request_bytes_at_the_limit_pass_and_one_over_is_refused"];

#[test_id(tl61bap)]
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
    SqliteBackend::open(path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap()
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
#[test_id(tpvodlc)]
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
/// below it is `TooOld`, a watch from it sees only the jump, and the head is served.
#[test_id(tyvu37n)]
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
    assert_eq!(c.backend().range("t/", None, 10, head).unwrap().len(), 1);
}

/// A ring that has turned past a revision answers `TooOld` for the read and the watch.
#[test_id(t3wuq2d)]
#[test]
fn a_revision_the_ring_has_turned_past_is_too_old_to_read_or_watch() {
    let dir = TempDir::new("sqlite-ring-turns");
    let clock = VirtualClock::new(1_000);
    let b = SqliteBackend::open(
        &dir.path().join("s.db"),
        Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, history_bytes: 4_000, ..Options::default() },
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

#[test_id(ttwrueg)]
#[test]
fn journal_mode_and_synchronous_read_back_as_wal_and_normal() {
    let dir = TempDir::new("sqlite-pragmas");
    let b = open(&dir.path().join("s.db"));
    // NORMAL, per Ryan 2026-11-02 (sp-3uvzc6, sp-r3yb7x): the jump on open covers what a crash loses.
    assert_eq!(b.pragmas().unwrap(), ("wal".to_string(), 1));
}

#[test_id(t426ubx)]
#[test]
fn a_second_open_of_the_same_file_is_refused() {
    let dir = TempDir::new("sqlite-exclusive");
    let path = dir.path().join("s.db");
    let first = open(&path);
    match SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }) {
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

#[test_id(tz64bz9)]
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

#[test_id(t0l71bl)]
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

#[test_id(t9f3fxk)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
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

#[test_id(tpfswhj)]
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
    let copy = SqliteBackend::open(&dest, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(copy.head().unwrap(), rev);
    assert_eq!(keys_at(&copy, "", rev), keys_at(&live, "", rev));
}

#[test_id(tdv19aj)]
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

#[test_id(tmz0y2k)]
#[test]
fn a_snapshot_copy_does_not_carry_the_pages_deleted_rows_left_free_in_the_live_file() {
    let dir = TempDir::new("sqlite-snapshot-vacuum");
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, day_receipts: false, ..Options::default() }).unwrap();
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

#[test_id(tmrteqf)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(copy.head().unwrap(), rev);
}

const DAY_MS: u64 = 86_400_000;
/// 2024-10-04T00:00:00Z, the first of the days these tests name.
const D0: u64 = 20_000 * DAY_MS;

/// Reads in day-file tests are never ended by time: the read clock is separate from `wall` and never advances.
fn day_opts(wall: &VirtualClock) -> Options {
    Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, wall: Arc::new(wall.clone()), clock: Arc::new(VirtualClock::new(0)), ..Options::default() }
}

fn receipt_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("s.receipts-") && n.ends_with(".db"))
        .collect();
    v.sort();
    v
}

fn day_file(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("s.receipts-{date}.db"))
}

fn count(path: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let n = conn.prepare(sql).unwrap().query_row([], |r| r.get(0)).unwrap();
    n
}

fn receipt(id: &str, value: &[u8], main: u64) -> Write {
    put_write(&state_store::request_key(id), value, main, None)
}

#[test_id(t02wxyg)]
#[test]
fn a_receipt_lands_in_the_day_file_of_its_commit_and_answers_a_retry_through_the_next_day() {
    let dir = TempDir::new("sqlite-receipt-day");
    let wall = VirtualClock::new(D0 + 3_600_000);
    let path = dir.path().join("s.db");
    let c = contract(SqliteBackend::open(&path, day_opts(&wall)).unwrap(), &wall);
    let t = Txn::new().put("a", Object::new(json!(1)));
    let first = c.txn(&t, &req("once")).unwrap();
    let file = day_file(dir.path(), "2024-10-04");
    assert_eq!(count(&file, "SELECT count(*) FROM rows WHERE CAST(key AS TEXT) = 'requests/once'"), 1);
    assert_eq!(count(&path, "SELECT count(*) FROM latest WHERE CAST(key AS TEXT) LIKE 'requests/%'"), 0);
    assert!(c.txn(&t, &req("once")).unwrap().already_applied);
    wall.advance_to(D0 + 2 * DAY_MS - 60_000);
    let again = c.txn(&t, &req("once")).unwrap();
    assert_eq!((again.rev, again.already_applied), (first.rev, true));
}

#[test_id(tzpnvj1)]
#[test]
fn a_failure_between_the_side_commit_and_the_main_commit_leaves_no_row_above_the_head() {
    let dir = TempDir::new("sqlite-side-fail");
    let wall = VirtualClock::new(D0);
    let fail = Arc::new(AtomicBool::new(true));
    let probe = {
        let fail = fail.clone();
        Arc::new(move || {
            if fail.swap(false, Ordering::SeqCst) {
                Err(Error::Backend("injected".into()))
            } else {
                Ok(())
            }
        }) as Arc<dyn Fn() -> state_store::Result<()> + Send + Sync>
    };
    let b = SqliteBackend::open(
        &dir.path().join("s.db"),
        Options { commit_probe: Some(probe), ..day_opts(&wall) },
    )
    .unwrap();
    let head = b.head().unwrap();
    assert!(b.commit(head, &[receipt("x", b"first", head + 1)]).is_err());
    assert_eq!(b.head().unwrap(), head);
    let file = day_file(dir.path(), "2024-10-04");
    assert_eq!(count(&file, &format!("SELECT count(*) FROM rows WHERE main > {head}")), 0);
    b.commit(head, &[receipt("x", b"second", head + 1)]).unwrap();
    let got = b.get(&state_store::request_key("x"), head + 1).unwrap().unwrap();
    assert_eq!(got.value, b"second");
}

#[test_id(tvdn366)]
#[test]
fn a_side_row_above_the_head_is_gone_when_the_store_reopens() {
    let dir = TempDir::new("sqlite-side-reopen");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    let head = b.commit(0, &[receipt("a", b"1", 1)]).unwrap();
    drop(b);
    let file = day_file(dir.path(), "2024-10-04");
    rusqlite::Connection::open(&file)
        .unwrap()
        .prepare("INSERT INTO rows (key, main, sub, value, create_main, create_sub) VALUES (?1, ?2, 0, X'00', ?2, 0)")
        .unwrap()
        .execute(rusqlite::params![b"requests/torn".as_slice(), head + 1])
        .unwrap();
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    assert_eq!(count(&file, "SELECT count(*) FROM rows"), 1, "trimmed before the first read");
    let at = b.head().unwrap();
    assert!(b.get("requests/torn", at).unwrap().is_none());
    b.commit(at, &[receipt("torn", b"real", at + 1)]).unwrap();
    assert_eq!(b.get("requests/torn", at + 1).unwrap().unwrap().value, b"real");
}

fn open_fds_naming(prefix: &str) -> Vec<String> {
    std::fs::read_dir(format!("{}/fd", system_paths::PROC_SELF))
        .unwrap()
        .filter_map(|e| std::fs::read_link(e.unwrap().path()).ok())
        .map(|p| p.display().to_string())
        .filter(|p| p.contains(prefix))
        .collect()
}

#[test_id(tkenwus)]
#[test]
fn a_retired_day_file_waits_for_the_read_in_flight_and_then_leaves_nothing_behind() {
    let dir = TempDir::new("sqlite-retire-wait");
    let wall = VirtualClock::new(D0);
    let armed = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let probe = {
        let (armed, barrier) = (armed.clone(), barrier.clone());
        Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                barrier.wait();
                barrier.wait();
            }
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let b = SqliteBackend::open(
        &dir.path().join("s.db"),
        Options { read_probe: Some(probe), ..day_opts(&wall) },
    )
    .unwrap();
    let head = b.commit(0, &[receipt("old", b"v", 1)]).unwrap();
    let file = day_file(dir.path(), "2024-10-04");
    armed.store(true, Ordering::SeqCst);
    let reader = {
        let b = b.clone();
        std::thread::spawn(move || b.get("requests/old", head))
    };
    barrier.wait();
    wall.advance_to(D0 + 2 * DAY_MS);
    b.commit(head, &[receipt("new", b"v", head + 1)]).unwrap();
    assert!(file.exists(), "a read is still on the retired file");
    barrier.wait();
    let got = reader.join().unwrap().unwrap().unwrap();
    assert_eq!(got.value, b"v");
    for suffix in ["", "-wal", "-shm"] {
        let gone = PathBuf::from(format!("{}{suffix}", file.display()));
        assert!(!gone.exists(), "{} remains", gone.display());
    }
    let own = format!("{}/s.receipts-2024-10-04", dir.path().display());
    assert_eq!(open_fds_naming(&own), Vec::<String>::new());
}

#[test_id(t73xvu7)]
#[test]
fn thirty_days_of_commits_never_leave_more_than_two_day_files_and_today_is_one() {
    let dir = TempDir::new("sqlite-thirty-days");
    let wall = VirtualClock::new(D0);
    let b = SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap();
    for i in 0..120u64 {
        wall.advance_to(D0 + i * 6 * 3_600_000);
        let head = b.head().unwrap();
        b.commit(head, &[receipt(&format!("r{i}"), b"v", head + 1)]).unwrap();
        let files = receipt_files(dir.path());
        assert!(files.len() <= 2, "{files:?} after commit {i}");
        let today = D0 + i * 6 * 3_600_000;
        let day = today / DAY_MS - 20_000;
        let (month, dom) = if day < 28 { (10, 4 + day) } else { (11, day - 27) };
        let name = format!("s.receipts-2024-{month:02}-{dom:02}.db");
        assert!(files.contains(&name), "{name} missing from {files:?}");
    }
}

#[test_id(try91ig)]
#[test]
fn main_file_receipts_drain_five_hundred_a_commit_once_they_are_a_day_old() {
    let dir = TempDir::new("sqlite-drain");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    drop(b);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        for table in ["latest", "versions"] {
            let sql = format!(
                "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 1199)
                 INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                 SELECT CAST(printf('requests/old%05d', i) AS BLOB), 1, 0, X'7B7D', 1, 0, 1 FROM n"
            );
            conn.prepare(&sql).unwrap().execute([]).unwrap();
        }
    }
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    let left = |table: &str| count(&path, &format!("SELECT count(*) FROM {table} WHERE CAST(key AS TEXT) LIKE 'requests/%'"));
    let live = |b: &SqliteBackend, i: u64| {
        let head = b.head().unwrap();
        b.commit(head, &[put_write(&format!("live/{i}"), b"1", head + 1, None)]).unwrap();
    };
    wall.advance_to(D0 + DAY_MS - 1);
    live(&b, 1);
    live(&b, 2);
    assert_eq!(left("latest"), 1200);
    wall.advance_to(D0 + DAY_MS);
    let mut seen = Vec::new();
    for i in 3..6 {
        live(&b, i);
        seen.push(left("latest"));
    }
    assert_eq!(seen, [700, 200, 0]);
    assert_eq!(left("versions"), 0);
    assert_eq!(count(&path, "SELECT count(*) FROM latest WHERE CAST(key AS TEXT) LIKE 'live/%'"), 6);
}

fn family_files(dir: &Path, family: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(&format!("s.{family}-")) && n.ends_with(".db"))
        .collect();
    v.sort();
    v
}

fn failing_once() -> (Arc<AtomicBool>, state_store_sqlite::CommitProbe) {
    let fail = Arc::new(AtomicBool::new(false));
    let probe = {
        let fail = fail.clone();
        Arc::new(move || {
            if fail.swap(false, Ordering::SeqCst) {
                Err(Error::Backend("injected".into()))
            } else {
                Ok(())
            }
        }) as state_store_sqlite::CommitProbe
    };
    (fail, probe)
}

#[test_id(tbbvtx4)]
#[test]
fn an_event_lands_in_the_day_file_of_its_commit_and_not_in_the_main_file() {
    let dir = TempDir::new("sqlite-event-day");
    let wall = VirtualClock::new(D0 + 3_600_000);
    let path = dir.path().join("s.db");
    let c = contract(SqliteBackend::open(&path, day_opts(&wall)).unwrap(), &wall);
    let rev = put(&c, "a", json!(1), "w1");
    let file = dir.path().join("s.events-2024-10-04.db");
    let key = state_store::events_key(rev);
    assert_eq!(count(&file, &format!("SELECT count(*) FROM rows WHERE CAST(key AS TEXT) = '{key}'")), 1);
    assert_eq!(count(&path, "SELECT count(*) FROM latest WHERE CAST(key AS TEXT) LIKE 'events/%'"), 0);
    assert!(c.backend().get(&key, rev).unwrap().is_some());
}

#[test_id(tmf7arr)]
#[test]
fn thirty_days_of_audited_commits_keep_fifteen_event_files_and_an_event_reads_back_for_fourteen_days() {
    let dir = TempDir::new("sqlite-event-thirty");
    let wall = VirtualClock::new(D0);
    let c = contract(SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap(), &wall);
    let mut revs = Vec::new();
    for i in 0..120u64 {
        wall.advance_to(D0 + i * 6 * 3_600_000);
        revs.push((i / 4, put(&c, "a", json!(i), &format!("w{i}"))));
        let files = family_files(dir.path(), "events");
        assert!(files.len() <= 15, "{files:?} after commit {i}");
    }
    let head = c.backend().head().unwrap();
    for (day, rev) in revs {
        let read = c.backend().get(&state_store::events_key(rev), head).unwrap();
        assert_eq!(read.is_some(), 29 - day <= 14, "event of day {day}");
    }
}

#[test_id(tgi8ujd)]
#[test]
fn a_failed_main_commit_leaves_neither_an_event_nor_a_receipt_above_the_head_in_process_or_after_a_reopen() {
    let dir = TempDir::new("sqlite-event-fail");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let (fail, probe) = failing_once();
    let b = SqliteBackend::open(&path, Options { commit_probe: Some(probe), ..day_opts(&wall) }).unwrap();
    let c = contract(b.clone(), &wall);
    put(&c, "a", json!(1), "first");
    let head = b.head().unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(c.txn(&Txn::new().put("b", Object::new(json!(2))), &req("second")).is_err());
    assert_eq!(b.head().unwrap(), head);
    let (events, receipts) = (dir.path().join("s.events-2024-10-04.db"), day_file(dir.path(), "2024-10-04"));
    for file in [&events, &receipts] {
        assert_eq!(count(file, &format!("SELECT count(*) FROM rows WHERE main > {head}")), 0, "{}", file.display());
    }
    drop(c);
    drop(b);
    for (file, key) in [(&events, "events/torn"), (&receipts, "requests/torn")] {
        rusqlite::Connection::open(file)
            .unwrap()
            .prepare("INSERT INTO rows (key, main, sub, value, create_main, create_sub) VALUES (?1, ?2, 0, X'00', ?2, 0)")
            .unwrap()
            .execute(rusqlite::params![key.as_bytes(), head + 1])
            .unwrap();
    }
    let _b = open_day(&path, &wall);
    for file in [&events, &receipts] {
        assert_eq!(count(file, &format!("SELECT count(*) FROM rows WHERE main > {head}")), 0, "{}", file.display());
    }
}

fn open_day(path: &Path, wall: &VirtualClock) -> SqliteBackend {
    SqliteBackend::open(path, day_opts(wall)).unwrap()
}

#[test_id(tt7z0i6)]
#[test]
fn a_retired_events_file_waits_for_the_range_in_flight_and_then_leaves_nothing_behind() {
    let dir = TempDir::new("sqlite-event-retire-wait");
    let wall = VirtualClock::new(D0);
    let armed = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let probe = {
        let (armed, barrier) = (armed.clone(), barrier.clone());
        Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                barrier.wait();
                barrier.wait();
            }
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let b = SqliteBackend::open(&dir.path().join("s.db"), Options { read_probe: Some(probe), ..day_opts(&wall) }).unwrap();
    let ev = |main: u64| put_write(&state_store::events_key(main), b"{}", main, None);
    b.commit(0, &[ev(1)]).unwrap();
    let file = dir.path().join("s.events-2024-10-04.db");
    assert!(file.exists());
    armed.store(true, Ordering::SeqCst);
    let reader = {
        let b = b.clone();
        std::thread::spawn(move || b.range(state_store::EVENTS_PREFIX, None, usize::MAX, 1))
    };
    barrier.wait();
    wall.advance_to(D0 + 15 * DAY_MS);
    b.commit(1, &[ev(2)]).unwrap();
    assert!(file.exists(), "a read is still on the retired file");
    barrier.wait();
    let rows = reader.join().unwrap().unwrap();
    assert_eq!(rows.len(), 1, "every row the range began with");
    for suffix in ["", "-wal", "-shm"] {
        let gone = PathBuf::from(format!("{}{suffix}", file.display()));
        assert!(!gone.exists(), "{} remains", gone.display());
    }
    let own = format!("{}/s.events-2024-10-04", dir.path().display());
    assert_eq!(open_fds_naming(&own), Vec::<String>::new());
}

fn seed_events(path: &Path, rows: &[(u64, Option<u64>)]) {
    let mut conn = rusqlite::Connection::open(path).unwrap();
    let conn = conn.transaction().unwrap();
    for (i, at) in rows {
        let value = match at {
            Some(at) => format!("{{\"data\":{{\"at\":{at}}}}}"),
            None => "{\"data\":{}}".to_string(),
        };
        for table in ["latest", "versions"] {
            conn.prepare(&format!(
                "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                 VALUES (CAST(?1 AS BLOB), 1, 0, ?2, 1, 0, 1)"
            ))
            .unwrap()
            .execute(rusqlite::params![format!("events/{i:020}"), value.as_bytes()])
            .unwrap();
        }
    }
    conn.commit().unwrap();
}

fn many_unstamped_then_one_stamped() -> Vec<(u64, Option<u64>)> {
    let mut rows: Vec<(u64, Option<u64>)> = (1..=20_000).map(|i| (i, None)).collect();
    rows.push((20_001, Some(D0 + 29 * DAY_MS)));
    rows
}

fn events_left(path: &Path) -> i64 {
    count(path, "SELECT count(*) FROM latest WHERE CAST(key AS TEXT) LIKE 'events/%'")
}

fn seeded_store(dir: &TempDir, wall: &VirtualClock, rows: &[(u64, Option<u64>)]) -> (PathBuf, SqliteBackend) {
    let path = dir.path().join("s.db");
    let b = open_day(&path, wall);
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    drop(b);
    seed_events(&path, rows);
    let b = open_day(&path, wall);
    (path, b)
}

fn live_commit(b: &SqliteBackend, i: u64) {
    let head = b.head().unwrap();
    b.commit(head, &[put_write(&format!("live/{i}"), b"1", head + 1, None)]).unwrap();
}

#[test_id(tu7a0oy)]
#[test]
fn main_file_events_older_than_fourteen_days_drain_five_hundred_a_commit_and_the_rest_stay() {
    let dir = TempDir::new("sqlite-event-drain");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let rows: Vec<(u64, Option<u64>)> =
        (1..=1_200).map(|i| (i, Some(if i <= 1_000 { D0 } else { D0 + 29 * DAY_MS }))).collect();
    let (path, b) = seeded_store(&dir, &wall, &rows);
    let mut seen = Vec::new();
    for i in 0..5 {
        live_commit(&b, i);
        seen.push(events_left(&path));
    }
    assert_eq!(seen, [700, 200, 200, 200, 200]);
    assert_eq!(count(&path, "SELECT count(*) FROM versions"), 0, "the legacy rows drain whatever their age");
}

#[test_id(twq2pa9)]
#[test]
fn unstamped_main_file_events_are_as_old_as_the_first_stamped_one_after_them() {
    let dir = TempDir::new("sqlite-event-unstamped");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let rows = [(1, None), (2, None), (3, Some(D0)), (4, Some(D0 + 29 * DAY_MS))];
    let (path, b) = seeded_store(&dir, &wall, &rows);
    live_commit(&b, 1);
    assert_eq!(events_left(&path), 1);
    let dir = TempDir::new("sqlite-event-unstamped-young");
    let rows = [(1, None), (2, Some(D0 + 29 * DAY_MS))];
    let (path, b) = seeded_store(&dir, &wall, &rows);
    live_commit(&b, 1);
    assert_eq!(events_left(&path), 2, "nothing older than the window is stamped");
}

#[test_id(tjvrvmh)]
#[test]
fn a_reopen_over_many_unstamped_main_file_events_drains_a_bounded_page_a_commit() {
    let dir = TempDir::new("sqlite-event-unstamped-bounded");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let (path, b) = seeded_store(&dir, &wall, &many_unstamped_then_one_stamped());
    b.take_drain_rows();
    live_commit(&b, 0);
    let rows = b.take_drain_rows();
    assert!(rows > 0, "the commit drained nothing");
    assert!(rows <= (2 * DRAIN_PER_COMMIT) as u64, "the first commit examined {rows} rows");
    assert_eq!(events_left(&path), 20_001, "nothing in the window is older");
}

#[test_id(tipj3sx)]
#[test]
fn a_reopen_whose_event_drain_is_waiting_examines_no_event_rows_on_its_first_commit() {
    let dir = TempDir::new("sqlite-event-waiting-reopen");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let rows = [(1, None), (2, Some(D0 + 29 * DAY_MS))];
    let (path, b) = seeded_store(&dir, &wall, &rows);
    live_commit(&b, 1);
    assert!(b.take_drain_rows() > 0, "the first commit reached the wait");
    drop(b);
    let b = open_day(&path, &wall);
    live_commit(&b, 2);
    assert_eq!(b.take_drain_rows(), 0, "a waiting drain survives a restart");
}

#[test_id(toowyuf)]
#[test]
fn a_scan_resumes_from_its_persisted_cursor_and_drains_once_the_stamped_event_ages_out() {
    let dir = TempDir::new("sqlite-event-scan-resume");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let (path, b) = seeded_store(&dir, &wall, &many_unstamped_then_one_stamped());
    let steps = |b: &SqliteBackend, first: u64| {
        let mut n = 0;
        loop {
            live_commit(b, first + n);
            n += 1;
            if b.take_drain_rows() == 0 || n > 100 {
                return n;
            }
        }
    };
    for i in 0..10 {
        live_commit(&b, i);
    }
    drop(b);
    let b = open_day(&path, &wall);
    let resumed = steps(&b, 100);
    let cold = {
        let dir = TempDir::new("sqlite-event-scan-cold");
        let (_, c) = seeded_store(&dir, &wall, &many_unstamped_then_one_stamped());
        steps(&c, 0)
    };
    assert!(resumed + 8 <= cold, "resumed after {resumed} commits, a cold scan takes {cold}");
    assert_eq!(events_left(&path), 20_001, "nothing in the window is older");
    wall.advance(30 * DAY_MS);
    for i in 0..45 {
        live_commit(&b, 300 + i);
    }
    assert_eq!(events_left(&path), 0, "the unstamped events drain once the stamped one is out of the window");
}

#[test_id(t0907pl)]
#[test]
fn a_reopen_mid_deletion_does_not_rescan_the_unstamped_events_it_already_passed() {
    let dir = TempDir::new("sqlite-event-reopen-mid-deletion");
    let wall = VirtualClock::new(D0 + 60 * DAY_MS);
    let (path, b) = seeded_store(&dir, &wall, &many_unstamped_then_one_stamped());
    let mut i = 0;
    while events_left(&path) == 20_001 {
        live_commit(&b, i);
        i += 1;
        assert!(i < 100, "the scan never reached the deletion");
    }
    let before = events_left(&path);
    b.take_drain_rows();
    drop(b);
    let b = open_day(&path, &wall);
    live_commit(&b, 1_000);
    let rows = b.take_drain_rows();
    assert!(rows <= DRAIN_PER_COMMIT as u64, "the first commit after the reopen examined {rows} rows");
    assert_eq!(events_left(&path), before - DRAIN_PER_COMMIT as i64, "the reopen did not go on deleting");
}

#[test_id(t8kd08q)]
#[test]
fn a_commit_that_drains_records_a_drain_phase() {
    let dir = TempDir::new("sqlite-drain-phase");
    let wall = VirtualClock::new(D0 + 30 * DAY_MS);
    let (_path, b) = seeded_store(&dir, &wall, &many_unstamped_then_one_stamped());
    let rec = Arc::new(state_store::phases::Recorder::default());
    state_store::phases::track(rec.clone());
    live_commit(&b, 0);
    let names: Vec<_> = rec.snapshot().phases.iter().map(|p| p.0).collect();
    assert!(names.contains(&"drain"), "phases {names:?}");
    state_store::phases::untrack();
}

#[test_id(tdov7ps)]
#[test]
fn log_over_a_mixed_batch_sequence_is_the_same_on_the_memory_and_the_sqlite_backends() {
    let dir = TempDir::new("sqlite-log-same");
    let wall = VirtualClock::new(D0);
    let s = SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap();
    let m = state_store::MemBackend::new();
    let batches = [
        vec![put_write("a", b"1", 1, None), receipt("r1", b"x", 1), put_write("b", b"1", 1, None)],
        vec![put_write("a", b"2", 2, Some(Rev::new(1, 0)))],
        vec![receipt("r2", b"y", 3), Write::Delete { key: "b".into(), prev: Rev::new(1, 2) }],
        vec![receipt("r3", b"z", 4), receipt("r4", b"z", 4)],
        vec![receipt("r5", b"w", 5)],
    ];
    for (i, batch) in batches.iter().enumerate() {
        wall.advance_to(D0 + (i as u64 / 2) * DAY_MS);
        s.commit(i as u64, batch).unwrap();
        m.commit(i as u64, batch).unwrap();
    }
    assert_eq!(receipt_files(dir.path()).len(), 2, "the log spans two day files");
    assert_eq!(s.log(0, 10).unwrap(), m.log(0, 10).unwrap());
    assert_eq!(s.log(2, 2).unwrap(), m.log(2, 2).unwrap());
}

#[test_id(tzm7l66)]
#[test]
fn a_receipt_is_written_once_and_a_failed_cleanup_refuses_every_commit_until_a_reopen_trims_it() {
    let dir = TempDir::new("sqlite-receipt-refusals");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let file = day_file(dir.path(), "2024-10-04");
    let sabotage = {
        let file = file.clone();
        Arc::new(move || {
            let conn = rusqlite::Connection::open(&file).unwrap();
            conn.prepare("CREATE TRIGGER stuck BEFORE DELETE ON rows BEGIN SELECT RAISE(ABORT, 'stuck'); END")
                .unwrap()
                .execute([])
                .unwrap();
            Err(Error::Backend("injected".into()))
        }) as Arc<dyn Fn() -> state_store::Result<()> + Send + Sync>
    };
    let b = SqliteBackend::open(&path, Options { commit_probe: Some(sabotage), ..day_opts(&wall) }).unwrap();
    let rewrite = put_write("requests/a", b"2", 1, Some(Rev::new(1, 0)));
    assert!(b.commit(0, &[rewrite]).is_err());
    assert_eq!(b.head().unwrap(), 0, "the refusal wrote nothing");
    assert!(!file.exists());
    let e = b.commit(0, &[receipt("a", b"1", 1)]).unwrap_err().to_string();
    assert!(e.contains("could not take back"), "{e}");
    let refused = b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap_err().to_string();
    assert!(refused.contains(&*file.display().to_string()), "{refused}");
    drop(b);
    rusqlite::Connection::open(&file).unwrap().prepare("DROP TRIGGER stuck").unwrap().execute([]).unwrap();
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    assert_eq!(count(&file, &format!("SELECT count(*) FROM rows WHERE main > {}", 0)), 0);
    let head = b.head().unwrap();
    b.commit(head, &[put_write("live/a", b"1", head + 1, None)]).unwrap();
}

#[test_id(tmzpx0b)]
#[test]
fn deleting_a_receipt_removes_it_from_a_day_file_or_the_main_file_and_the_log_names_what_it_was() {
    let dir = TempDir::new("sqlite-receipt-delete");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[receipt("filed", b"f", 1)]).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .prepare(
            "INSERT INTO latest (key, main, sub, value, create_main, create_sub, version)
             VALUES (CAST('requests/legacy' AS BLOB), 1, 0, X'6C', 1, 0, 1)",
        )
        .unwrap()
        .execute([])
        .unwrap();
    b.commit(1, &[Write::Delete { key: "requests/filed".into(), prev: Rev::new(1, 0) }]).unwrap();
    b.commit(2, &[Write::Delete { key: "requests/legacy".into(), prev: Rev::new(1, 0) }]).unwrap();
    assert!(b.get("requests/filed", 3).unwrap().is_none());
    assert!(b.get("requests/legacy", 3).unwrap().is_none());
    assert_eq!(count(&day_file(dir.path(), "2024-10-04"), "SELECT count(*) FROM rows"), 0);
    assert_eq!(b.get("requests/filed", 1).unwrap().unwrap().value, b"f", "as of the revision before the delete");
    let log = b.log(1, 10).unwrap();
    assert_eq!(log.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["requests/filed", "requests/legacy"]);
    assert!(log.iter().all(|e| e.value.is_none()));
}

#[test_id(tfq5h8b)]
#[test]
fn a_store_opened_without_day_receipts_keeps_them_in_its_one_file_and_never_drains_them() {
    let dir = TempDir::new("sqlite-receipt-main");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let opts = Options { day_receipts: false, ..day_opts(&wall) };
    let b = SqliteBackend::open(&path, opts).unwrap();
    b.commit(0, &[receipt("a", b"1", 1)]).unwrap();
    wall.advance_to(D0 + 3 * DAY_MS);
    b.commit(1, &[put_write("live/a", b"1", 2, None)]).unwrap();
    assert!(receipt_files(dir.path()).is_empty());
    assert_eq!(count(&path, "SELECT count(*) FROM latest WHERE CAST(key AS TEXT) LIKE 'requests/%'"), 1);
    assert_eq!(b.get("requests/a", 2).unwrap().unwrap().value, b"1");
}

#[test_id(taw968l)]
#[test]
fn a_range_lays_receipts_among_the_main_files_keys_at_the_head_and_below_it() {
    let dir = TempDir::new("sqlite-receipt-range");
    let wall = VirtualClock::new(D0);
    let b = SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap();
    b.commit(0, &[put_write("a", b"1", 1, None), receipt("r1", b"x", 1), put_write("z", b"1", 1, None)]).unwrap();
    wall.advance_to(D0 + DAY_MS);
    b.commit(1, &[receipt("r2", b"y", 2), put_write("a", b"2", 2, Some(Rev::new(1, 0)))]).unwrap();
    let keys = |at: u64| -> Vec<String> { b.range("", None, 100, at).unwrap().into_iter().map(|(k, _)| k).collect() };
    assert_eq!(keys(2), ["a", "requests/r1", "requests/r2", "z"]);
    assert_eq!(keys(1), ["a", "requests/r1", "z"]);
    let page = b.range("requests/", Some("requests/r1"), 1, 2).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].0, "requests/r2");
    let outside: Vec<String> =
        b.range_outside(&["a"], None, 100, 2).unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(outside, ["requests/r1", "requests/r2", "z"]);
    assert!(b.gc_range(None, 100, 2).unwrap().iter().all(|(k, _)| !k.starts_with("requests/")));
}

#[test_id(t7esvcz)]
#[test]
fn a_snapshot_copy_carries_the_receipts_up_to_its_revision_in_one_file() {
    let dir = TempDir::new("sqlite-receipt-snapshot");
    let wall = VirtualClock::new(D0);
    let b = SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap();
    b.commit(0, &[receipt("r1", b"x", 1)]).unwrap();
    let dest = dir.path().join("snap.db");
    let rev = b.snapshot_to(&dest).unwrap();
    b.commit(1, &[receipt("r2", b"y", 2)]).unwrap();
    let copy = SqliteBackend::open(&dest, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(copy.get("requests/r1", rev).unwrap().unwrap().value, b"x");
    assert!(copy.get("requests/r2", rev).unwrap().is_none());
    assert_eq!(receipt_files(dir.path()).len(), 1, "the copy made no day file of its own");
    assert!(copy.range_in_state("requests/", "none", None, 10, rev).unwrap().is_empty());
}

#[test_id(ty7mgyj)]
#[test]
fn a_snapshot_copy_of_five_hundred_day_file_rows_commits_in_a_bounded_number_of_transactions() {
    let dir = TempDir::new("sqlite-snapshot-batched");
    let wall = VirtualClock::new(D0);
    let c = contract(SqliteBackend::open(&dir.path().join("s.db"), day_opts(&wall)).unwrap(), &wall);
    for i in 0..500 {
        let r = c.txn(&Txn::new().put(&format!("k{i}"), Object::new(json!(i))), &req(&format!("r{i}"))).unwrap();
        assert!(r.succeeded);
    }
    let rows: i64 = day_file_rows(dir.path());
    assert!(rows >= 500, "{rows}");
    let dest = dir.path().join("snap.db");
    let rev = c.backend().snapshot_to(&dest).unwrap();
    let header = std::fs::read(&dest).unwrap();
    assert_eq!((header[18], header[19]), (1, 1), "the copy stays in rollback mode");
    let counter = u32::from_be_bytes(header[24..28].try_into().unwrap());
    assert!(counter <= 20, "{counter} transactions for {rows} rows");
    let copy = SqliteBackend::open(&dest, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() }).unwrap();
    for i in [0, 250, 499] {
        assert!(copy.get(&state_store::request_key(&format!("r{i}")), rev).unwrap().is_some(), "r{i}");
    }
}

fn day_file_rows(dir: &Path) -> i64 {
    receipt_files(dir).iter().map(|n| count(&dir.join(n), "SELECT count(*) FROM rows")).sum()
}

#[test_id(trdx3d6)]
#[test]
fn a_day_file_behind_the_mark_is_retired_when_the_store_opens() {
    let dir = TempDir::new("sqlite-receipt-open-retire");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[receipt("old", b"x", 1)]).unwrap();
    drop(b);
    wall.advance_to(D0 + 2 * DAY_MS);
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    assert!(receipt_files(dir.path()).is_empty());
    assert!(b.get("requests/old", b.head().unwrap()).unwrap().is_none());
}

#[test_id(t6mbnhh)]
#[test]
fn a_store_opened_without_day_receipts_starts_no_drain_clock() {
    let dir = TempDir::new("sqlite-receipt-clock");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    drop(SqliteBackend::open(&path, Options { day_receipts: false, ..day_opts(&wall) }).unwrap());
    assert_eq!(count(&path, "SELECT count(*) FROM meta WHERE name = 'receipts_from_at'"), 0);
    drop(SqliteBackend::open(&path, day_opts(&wall)).unwrap());
    assert_eq!(count(&path, "SELECT count(*) FROM meta WHERE name = 'receipts_from_at'"), 1);
}

#[test_id(tssggju)]
#[test]
fn a_failed_main_commit_leaves_a_receipt_the_batch_deleted() {
    let dir = TempDir::new("sqlite-receipt-undeleted");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let fail = Arc::new(AtomicBool::new(false));
    let probe = {
        let fail = Arc::clone(&fail);
        Arc::new(move || if fail.load(Ordering::SeqCst) { Err(Error::Backend("injected".into())) } else { Ok(()) })
            as Arc<dyn Fn() -> state_store::Result<()> + Send + Sync>
    };
    let b = SqliteBackend::open(&path, Options { commit_probe: Some(probe), ..day_opts(&wall) }).unwrap();
    b.commit(0, &[receipt("a", b"1", 1)]).unwrap();
    fail.store(true, Ordering::SeqCst);
    let del = Write::Delete { key: state_store::request_key("a"), prev: Rev::new(1, 0) };
    assert!(b.commit(1, &[del]).is_err());
    assert_eq!(b.get(&state_store::request_key("a"), 1).unwrap().unwrap().value, b"1");
    assert_eq!(count(&day_file(dir.path(), "2024-10-04"), "SELECT count(*) FROM rows"), 1);
}

#[test_id(t430082)]
#[test]
fn an_open_whose_trim_of_rows_above_the_head_cannot_complete_fails() {
    let dir = TempDir::new("sqlite-receipt-open-trim");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    drop({
        let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
        b.commit(0, &[receipt("a", b"1", 1)]).unwrap();
        b
    });
    let file = day_file(dir.path(), "2024-10-04");
    let conn = rusqlite::Connection::open(&file).unwrap();
    conn.prepare("INSERT INTO rows VALUES (X'61', 9, 0, X'62', 9, 0)").unwrap().execute([]).unwrap();
    conn.prepare("CREATE TRIGGER stuck BEFORE DELETE ON rows BEGIN SELECT RAISE(ABORT, 'stuck'); END").unwrap().execute([]).unwrap();
    drop(conn);
    assert!(SqliteBackend::open(&path, day_opts(&wall)).is_err());
}

mod relayout {
    use super::*;
    use rusqlite::{params, Connection};
    use state_store::{events_key, request_key};
    use runtime::ports::RealFs;
    use state_store_sqlite::durability::ship::sha256_file;
    use state_store_sqlite::relayout::{relayout, relayout_probed};

    const HOUR: u64 = 3_600_000;
    const HEAD: u64 = 5_000;
    const WALL: u64 = D0 + 25 * DAY_MS + 12 * HOUR;
    const EVENT_HOURS: u64 = 20 * 24;

    /// The layout before the history ring: the base schema and migrations 1 and 2, frozen.
    const OLD_LAYOUT: &[&str] = &[
        "CREATE TABLE meta (name TEXT PRIMARY KEY, value INTEGER NOT NULL)",
        "CREATE TABLE versions (key BLOB NOT NULL, main INTEGER NOT NULL, sub INTEGER NOT NULL, value BLOB,
            create_main INTEGER, create_sub INTEGER, version INTEGER NOT NULL, prev_main INTEGER, prev_sub INTEGER,
            PRIMARY KEY (key, main))",
        "CREATE INDEX versions_by_rev ON versions (main, sub)",
        "CREATE TABLE jumps (from_main INTEGER PRIMARY KEY, to_main INTEGER NOT NULL)",
        "INSERT INTO meta (name, value) VALUES ('head', 0), ('compacted', 0)",
        "CREATE TABLE latest (key BLOB PRIMARY KEY, main INTEGER NOT NULL, sub INTEGER NOT NULL, value BLOB NOT NULL,
            create_main INTEGER, create_sub INTEGER, version INTEGER NOT NULL,
            state TEXT GENERATED ALWAYS AS (CASE WHEN json_valid(CAST(value AS TEXT))
                AND json_type(CAST(value AS TEXT), '$.data.state') = 'text'
                THEN json_extract(CAST(value AS TEXT), '$.data.state') END) VIRTUAL) WITHOUT ROWID",
        "CREATE INDEX latest_by_state ON latest (state, key) WHERE state IS NOT NULL",
    ];

    type Row = (Vec<u8>, i64, i64, Vec<u8>, i64, i64, i64);

    fn obj(data: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({ "metadata": {}, "data": data })).unwrap()
    }

    fn event_main(hour: u64) -> u64 {
        1_000 + hour
    }

    fn event_at(hour: u64) -> u64 {
        WALL - (EVENT_HOURS - 1 - hour) * HOUR
    }

    /// 200 live keys, an event an hour for twenty days (hour `EVENT_HOURS - 1` is at the wall),
    /// one unstamped event, and a receipt an hour for forty-eight, each naming its own event.
    fn fixture(path: &Path) -> Vec<Row> {
        let conn = Connection::open(path).unwrap();
        for sql in OLD_LAYOUT {
            conn.prepare(sql).unwrap().execute([]).unwrap();
        }
        let mut rows: Vec<Row> = Vec::new();
        for i in 0..200i64 {
            let state = ["Queued", "Batched"][(i % 2) as usize];
            rows.push((format!("deliveries/sp-{i:04}").into_bytes(), 10 + i, i % 3, obj(json!({ "state": state, "n": i })), 5 + i / 2, i % 5, 1 + i % 4));
        }
        rows.push((events_key(900).into_bytes(), 900, 0, obj(json!({ "refused": false })), 900, 0, 1));
        for h in 0..EVENT_HOURS {
            let m = event_main(h) as i64;
            rows.push((events_key(m as u64).into_bytes(), m, 0, obj(json!({ "at": event_at(h) })), m, 0, 1));
        }
        for h in 0..48u64 {
            let m = event_main(EVENT_HOURS - 1 - h) as i64;
            rows.push((request_key(&format!("r{h:02}")).into_bytes(), m, 0, obj(json!({ "main": m, "succeeded": true })), m, 0, 1));
        }
        for (k, main, sub, v, cm, cs, ver) in &rows {
            conn.prepare("INSERT INTO latest (key, main, sub, value, create_main, create_sub, version) VALUES (?1,?2,?3,?4,?5,?6,?7)")
                .unwrap()
                .execute(params![k, main, sub, v, cm, cs, ver])
                .unwrap();
        }
        conn.prepare("UPDATE meta SET value = ?1 WHERE name = 'head'").unwrap().execute([HEAD as i64]).unwrap();
        conn.prepare("INSERT INTO jumps VALUES (3000, 4000)").unwrap().execute([]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
        drop(conn);
        rows
    }

    fn is_audit(k: &[u8]) -> bool {
        k.starts_with(b"events/") || k.starts_with(b"requests/")
    }

    fn files_beside(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    fn day_rows(dir: &Path, family: &str, date: &str) -> Vec<String> {
        let file = dir.join(format!("new.{family}-{date}.db"));
        let conn = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut s = conn.prepare("SELECT CAST(key AS TEXT) FROM rows ORDER BY key").unwrap();
        s.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    fn date(ms: u64) -> String {
        state_store_sqlite::dated::date_of(state_store_sqlite::dated::day_of(ms))
    }

    #[test_id(tujduj9)]
    #[test]
    fn relayout_builds_the_live_keys_exactly_and_files_the_young_audit_rows_by_their_own_day() {
        let dir = TempDir::new("relayout");
        let (old, new) = (dir.path().join("old.db"), dir.path().join("new.db"));
        let rows = fixture(&old);
        let sum = sha256_file(&RealFs, &old).unwrap();

        let report = relayout(&old, &new, &VirtualClock::new(WALL)).unwrap();
        assert_eq!(sha256_file(&RealFs, &old).unwrap(), sum, "the old file is never written");
        assert_eq!(count(&new, "PRAGMA auto_vacuum"), 2, "the --to file is incremental");
        assert_eq!((report.live.read, report.live.copied), (200, 200));

        let b = SqliteBackend::open(&new, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, wall: Arc::new(VirtualClock::new(WALL)), ..Options::default() }).unwrap();
        assert_eq!(b.head().unwrap(), HEAD);
        assert_eq!(b.jumps(0, HEAD).unwrap(), vec![(3000, 4000)]);
        let live: Vec<_> = b.range("", None, usize::MAX, HEAD).unwrap().into_iter().filter(|(k, _)| !is_audit(k.as_bytes())).collect();
        let want: Vec<&Row> = rows.iter().filter(|r| !is_audit(&r.0)).collect();
        assert_eq!(live.len(), 200);
        for ((k, v), r) in live.iter().zip(&want) {
            assert_eq!(k.as_bytes(), r.0.as_slice());
            assert_eq!((v.mod_rev, v.create_rev, &v.value), (Rev::new(r.1 as u64, r.2 as u32), Rev::new(r.4 as u64, r.5 as u32), &r.3), "{k}");
        }

        let window = WALL - 14 * DAY_MS;
        for h in 0..EVENT_HOURS {
            let got = b.get(&events_key(event_main(h)), HEAD).unwrap();
            assert_eq!(got.is_some(), event_at(h) >= window, "event of hour {h}");
            if let Some(v) = got {
                assert!(day_rows(dir.path(), "events", &date(event_at(h))).contains(&events_key(event_main(h))));
                assert_eq!(v.value, obj(json!({ "at": event_at(h) })));
            }
        }
        assert!(b.get(&events_key(900), HEAD).unwrap().is_none(), "an unstamped event is older than any stamped one");
        for h in 0..48u64 {
            let at = event_at(EVENT_HOURS - 1 - h);
            let got = b.get(&request_key(&format!("r{h:02}")), HEAD).unwrap();
            assert_eq!(got.is_some(), at > WALL - 24 * HOUR, "receipt {h} hours old");
            if got.is_some() {
                assert!(day_rows(dir.path(), "receipts", &date(at)).contains(&request_key(&format!("r{h:02}"))));
            }
        }
        assert_eq!(report.events.copied, 14 * 24 + 1);
        assert_eq!(report.receipts.copied, 24);
        assert_eq!(report.events.read, EVENT_HOURS + 1);
    }

    #[test_id(tupd8pa)]
    #[test]
    fn relayout_refuses_and_leaves_nothing_when_the_old_store_is_open_or_the_target_exists() {
        let dir = TempDir::new("relayout-refuse");
        let (old, new) = (dir.path().join("old.db"), dir.path().join("new.db"));
        fixture(&old);
        let before = files_beside(dir.path());
        let clock = VirtualClock::new(WALL);

        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.path().join("old.db.lock")).unwrap();
        held.try_lock().unwrap();
        let e = relayout(&old, &new, &clock).unwrap_err().to_string();
        assert!(e.contains("open") && e.contains("stop lc-serve"), "{e}");
        drop(held);
        std::fs::remove_file(dir.path().join("old.db.lock")).unwrap();
        assert_eq!(files_beside(dir.path()), before);

        std::fs::write(dir.path().join("new.events-2024-10-04.db"), b"x").unwrap();
        assert!(relayout(&old, &new, &clock).is_err(), "a day file of the target's stem is in the way");
        std::fs::remove_file(dir.path().join("new.events-2024-10-04.db")).unwrap();
        std::fs::write(&new, b"x").unwrap();
        assert!(relayout(&old, &new, &clock).is_err(), "the target exists");
        assert_eq!(std::fs::read(&new).unwrap(), b"x");
        std::fs::remove_file(&new).unwrap();
        assert!(files_beside(dir.path()).iter().all(|f| !f.starts_with("new")), "{:?}", files_beside(dir.path()));
    }

    #[test_id(tdqsh0y)]
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn relayout_folds_a_crash_image_wal_in_and_the_wal_only_rows_reach_the_new_store() {
        let dir = TempDir::new("relayout-wal");
        let (live, old, new) = (dir.path().join("live.db"), dir.path().join("old.db"), dir.path().join("new.db"));
        fixture(&live);
        let conn = Connection::open(&live).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        conn.prepare("UPDATE latest SET value = CAST('{\"state\":\"Batched\",\"wal\":1}' AS BLOB) WHERE key = CAST('deliveries/sp-0007' AS BLOB)")
            .unwrap().execute([]).unwrap();
        std::fs::copy(&live, &old).unwrap();
        std::fs::copy(format!("{}-wal", live.display()), format!("{}-wal", old.display())).unwrap();
        assert!(std::fs::metadata(format!("{}-wal", old.display())).unwrap().len() > 0);
        let report = relayout(&old, &new, &VirtualClock::new(WALL)).unwrap();
        drop(conn);
        assert!(report.wal_bytes_before > 0);
        let to = Connection::open(&new).unwrap();
        let value: Vec<u8> = to.query_row("SELECT value FROM latest WHERE key = CAST('deliveries/sp-0007' AS BLOB)", [], |r| r.get(0)).unwrap();
        assert!(String::from_utf8_lossy(&value).contains("\"wal\""), "{value:?}");
    }

    #[test_id(tgfn57t)]
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn relayout_refuses_naming_the_wal_when_a_reader_keeps_it_from_folding_and_leaves_nothing() {
        let dir = TempDir::new("relayout-wal-held");
        let (old, new) = (dir.path().join("old.db"), dir.path().join("new.db"));
        fixture(&old);
        let conn = Connection::open(&old).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("BEGIN; SELECT count(*) FROM latest;").unwrap();
        let other = Connection::open(&old).unwrap();
        other.prepare("DELETE FROM latest WHERE key = CAST('deliveries/sp-0001' AS BLOB)").unwrap().execute([]).unwrap();
        let e = relayout(&old, &new, &VirtualClock::new(WALL)).unwrap_err().to_string();
        assert!(e.contains(&format!("{}-wal", old.display())), "{e}");
        assert!(files_beside(dir.path()).iter().all(|f| !f.starts_with("new")), "{:?}", files_beside(dir.path()));
        drop(other);
        drop(conn);
    }

    #[test_id(toneacm)]
    #[test]
    fn relayout_refuses_an_old_file_newer_than_this_binary() {
        let dir = TempDir::new("relayout-newer");
        let (old, new) = (dir.path().join("old.db"), dir.path().join("new.db"));
        fixture(&old);
        let conn = Connection::open(&old).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let e = relayout(&old, &new, &VirtualClock::new(WALL)).unwrap_err().to_string();
        assert!(e.contains("99"), "{e}");
        assert!(!new.exists());
    }

    #[test_id(tkcazyz)]
    #[test]
    fn certification_refuses_a_copy_that_differs_and_removes_every_new_file() {
        let dir = TempDir::new("relayout-certify");
        let (old, new) = (dir.path().join("old.db"), dir.path().join("new.db"));
        fixture(&old);
        let alter = |to: &Path| {
            let conn = Connection::open(to).unwrap();
            conn.prepare("UPDATE latest SET value = X'7b7d' WHERE key = CAST('deliveries/sp-0007' AS BLOB)").unwrap().execute([]).unwrap();
        };
        let e = relayout_probed(&old, &new, &VirtualClock::new(WALL), &alter).unwrap_err().to_string();
        assert!(e.contains("deliveries/sp-0007"), "{e}");
        assert!(files_beside(dir.path()).iter().all(|f| !f.starts_with("new")), "{:?}", files_beside(dir.path()));

        let alter_event = |to: &Path| {
            let file = to.with_file_name(format!("new.events-{}.db", date(WALL)));
            let conn = Connection::open(file).unwrap();
            conn.prepare("UPDATE rows SET value = X'7b7d' WHERE key = (SELECT max(key) FROM rows)").unwrap().execute([]).unwrap();
        };
        assert!(relayout_probed(&old, &new, &VirtualClock::new(WALL), &alter_event).is_err());
        assert!(files_beside(dir.path()).iter().all(|f| !f.starts_with("new")));

        let alter_revision = |to: &Path| {
            let file = to.with_file_name(format!("new.events-{}.db", date(WALL)));
            let conn = Connection::open(file).unwrap();
            conn.prepare("UPDATE rows SET sub = sub + 1 WHERE key = (SELECT max(key) FROM rows)").unwrap().execute([]).unwrap();
        };
        let e = relayout_probed(&old, &new, &VirtualClock::new(WALL), &alter_revision).unwrap_err().to_string();
        assert!(e.contains("certification failed"), "{e}");
        assert!(files_beside(dir.path()).iter().all(|f| !f.starts_with("new")));
    }

    #[test_id(tv71zon)]
    #[test]
    fn relayout_builds_into_a_target_directory_that_does_not_exist_yet() {
        let dir = TempDir::new("relayout-parent");
        let old = dir.path().join("old.db");
        fixture(&old);
        let new = dir.path().join("trial").join("new").join("control.db");
        relayout(&old, &new, &VirtualClock::new(WALL)).unwrap();
        assert!(new.is_file());
    }
}

const BUSY_RETRIES: u32 = 10_000;

/// A commit by a writer racing a checkpoint pass: the pass's WAL restart briefly holds the write
/// lock and the backend's writer never waits on it, so a commit may surface busy; it is retried
/// by count, never by time.
fn put_next(b: &SqliteBackend) {
    let head = b.head().unwrap();
    let prev = (head > 0).then(|| Rev::new(head, 0));
    for _ in 0..BUSY_RETRIES {
        match b.commit(head, &[put_write("k", &[7; 4096], head + 1, prev)]) {
            Err(e) if e.to_string().contains("database is locked") => std::thread::yield_now(),
            r => {
                r.unwrap();
                return;
            }
        }
    }
    panic!("commit still busy after {BUSY_RETRIES} attempts");
}

#[test_id(tnomt7f)]
#[test]
fn no_commit_runs_a_wal_checkpoint() {
    let dir = TempDir::new("sqlite-no-commit-checkpoint");
    let path = dir.path().join("s.db");
    let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, wal_line_bytes: u64::MAX, ..Options::default() };
    let b = SqliteBackend::open(&path, opts).unwrap();
    let mut moved = Vec::new();
    for i in 0..state_store_sqlite::JUMP + 10 {
        let before = state_store_sqlite::bench::wal_backfill(&path).unwrap();
        put_next(&b);
        if state_store_sqlite::bench::wal_backfill(&path).unwrap() != before {
            moved.push(i);
        }
    }
    assert!(moved.is_empty(), "commits {moved:?} moved the WAL backfill mark");
}

#[test_id(tbxg0mt)]
#[test]
fn a_checkpoint_completes_after_checkpoint_every_commits() {
    let dir = TempDir::new("sqlite-background-checkpoint");
    let path = dir.path().join("s.db");
    let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, wal_line_bytes: u64::MAX, ..Options::default() };
    let b = SqliteBackend::open(&path, opts).unwrap();
    for _ in 0..state_store_sqlite::CHECKPOINT_EVERY {
        put_next(&b);
    }
    assert!(state_store_sqlite::bench::await_checkpoint(&b, 0, CHECKPOINT_CAP).is_some(), "no checkpoint completed");
    assert_eq!(std::fs::metadata(format!("{}-wal", path.display())).unwrap().len(), 0);
}

fn seed_versions(path: &Path, rows: i64, prefixes: &[&str]) {
    let conn = rusqlite::Connection::open(path).unwrap();
    for (p, prefix) in prefixes.iter().enumerate() {
        let sql = format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < {})
             INSERT INTO versions (key, main, sub, value, create_main, create_sub, version)
             SELECT CAST(printf('{prefix}old%06d', i) AS BLOB), 1, 0, zeroblob(2000), 1, 0, {p} FROM n",
            rows / prefixes.len() as i64 - 1
        );
        conn.prepare(&sql).unwrap().execute([]).unwrap();
    }
}

fn commit_live(b: &SqliteBackend, i: u64) {
    let head = b.head().unwrap();
    b.commit(head, &[put_write(&format!("live/{i}"), b"1", head + 1, None)]).unwrap();
}

#[test_id(tn6whuo)]
#[test]
fn legacy_version_rows_drain_five_hundred_a_commit_and_the_live_rows_stay() {
    let dir = TempDir::new("sqlite-drain-versions");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    drop(b);
    seed_versions(&path, 1200, &["readiness/", "lifecycle/"]);
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    let latest = count(&path, "SELECT count(*) FROM latest");
    let mut seen = Vec::new();
    for i in 1..4 {
        commit_live(&b, i);
        seen.push(count(&path, "SELECT count(*) FROM versions"));
    }
    assert_eq!(seen, [700, 200, 0]);
    assert_eq!(count(&path, "SELECT count(*) FROM latest"), latest + 3);
    let head = b.head().unwrap();
    let got = b.get("live/a", head).unwrap();
    assert!(got.is_some());
}

#[test_id(t0hnvs7)]
#[test]
fn a_store_opened_without_day_receipts_never_drains_its_version_rows() {
    let dir = TempDir::new("sqlite-keep-versions");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    drop(b);
    seed_versions(&path, 1200, &["readiness/", "lifecycle/"]);
    let b = SqliteBackend::open(&path, Options { day_receipts: false, ..day_opts(&wall) }).unwrap();
    for i in 1..4 {
        commit_live(&b, i);
    }
    assert_eq!(count(&path, "SELECT count(*) FROM versions"), 1200);
    assert!(count(&path, "PRAGMA freelist_count") == 0 && count(&path, "PRAGMA auto_vacuum") == 2);
}

#[test_id(ttgpump)]
#[test]
fn a_new_store_is_incremental_and_gives_back_at_most_the_step_a_commit() {
    let dir = TempDir::new("sqlite-incremental");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    drop(b);
    assert_eq!(count(&path, "PRAGMA auto_vacuum"), 2);
    let step = state_store_sqlite::VACUUM_PAGES_PER_COMMIT;
    seed_versions(&path, 1500, &["readiness/"]);
    rusqlite::Connection::open(&path)
        .unwrap()
        .prepare("UPDATE versions SET value = zeroblob(8000)")
        .unwrap()
        .execute([])
        .unwrap();
    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    let mut pages = count(&path, "PRAGMA page_count");
    assert!(count(&path, "SELECT count(*) FROM versions") == 1500);
    let (mut max_fall, mut max_free, mut falls) = (0, 0, 0);
    for i in 1..30 {
        commit_live(&b, i);
        let now = count(&path, "PRAGMA page_count");
        max_fall = max_fall.max(pages - now);
        falls += i64::from(now < pages);
        pages = now;
        let fl = count(&path, "PRAGMA freelist_count");
        max_free = max_free.max(fl);
    }
    assert_eq!(count(&path, "SELECT count(*) FROM versions"), 0);
    assert!(falls > 3, "the file shrank in {falls} commits");
    // a released range may also drop one pointer-map page, which is not a freelist page
    assert!(
        (step..=step + 1).contains(&max_fall),
        "the busiest commit gave back {max_fall} pages"
    );
    assert!(
        max_free > step,
        "the drain freed no more than one step at a time, so the cap was never the limit"
    );
    assert_eq!(count(&path, "PRAGMA freelist_count"), 0);
}

#[test_id(tcvastm)]
#[test]
fn an_existing_none_mode_store_keeps_its_mode_and_still_drains() {
    let dir = TempDir::new("sqlite-none-mode");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    let none = path;
    {
        let conn = rusqlite::Connection::open(&none).unwrap();
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        for sql in [
            "CREATE TABLE meta (name TEXT PRIMARY KEY, value INTEGER NOT NULL)",
            "CREATE TABLE versions (key BLOB NOT NULL, main INTEGER NOT NULL, sub INTEGER NOT NULL, value BLOB,
                create_main INTEGER, create_sub INTEGER, version INTEGER NOT NULL, prev_main INTEGER, prev_sub INTEGER,
                PRIMARY KEY (key, main))",
            "CREATE INDEX versions_by_rev ON versions (main, sub)",
            "CREATE TABLE jumps (from_main INTEGER PRIMARY KEY, to_main INTEGER NOT NULL)",
            "INSERT INTO meta (name, value) VALUES ('head', 0), ('compacted', 0)",
        ] {
            conn.prepare(sql).unwrap().execute([]).unwrap();
        }
    }
    seed_versions(&none, 1200, &["readiness/"]);
    let b = SqliteBackend::open(&none, day_opts(&wall)).unwrap();
    for i in 1..4 {
        commit_live(&b, i);
    }
    assert_eq!(count(&none, "PRAGMA auto_vacuum"), 0);
    assert_eq!(count(&none, "SELECT count(*) FROM versions"), 0);
}

fn none_mode_store(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "auto_vacuum", 0).unwrap();
    for sql in [
        "CREATE TABLE meta (name TEXT PRIMARY KEY, value INTEGER NOT NULL)",
        "CREATE TABLE versions (key BLOB NOT NULL, main INTEGER NOT NULL, sub INTEGER NOT NULL, value BLOB,
            create_main INTEGER, create_sub INTEGER, version INTEGER NOT NULL, prev_main INTEGER, prev_sub INTEGER,
            PRIMARY KEY (key, main))",
        "CREATE INDEX versions_by_rev ON versions (main, sub)",
        "CREATE TABLE jumps (from_main INTEGER PRIMARY KEY, to_main INTEGER NOT NULL)",
        "INSERT INTO meta (name, value) VALUES ('head', 0), ('compacted', 0)",
    ] {
        conn.prepare(sql).unwrap().execute([]).unwrap();
    }
}

fn none_mode_store_with_free_pages(path: &Path) {
    none_mode_store(path);
    seed_versions(path, 1200, &["readiness/"]);
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.prepare("DELETE FROM versions").unwrap().execute([]).unwrap();
    assert!(count(path, "PRAGMA freelist_count") > 0);
}

#[test_id(tp9b9q3)]
#[test]
fn compact_converts_a_none_mode_store_so_the_commit_reclaim_returns_pages() {
    let dir = TempDir::new("sqlite-compact");
    let wall = VirtualClock::new(D0);
    let path = dir.path().join("s.db");
    none_mode_store_with_free_pages(&path);
    let report = state_store_sqlite::compact::compact(&path).unwrap();
    assert_eq!(report.auto_vacuum, 2);
    assert!(report.free_bytes_before > 0 && report.free_bytes_after == 0);
    assert!(report.bytes_after < report.bytes_before);
    assert_eq!(count(&path, "PRAGMA auto_vacuum"), 2);
    assert_eq!(count(&path, "PRAGMA freelist_count"), 0);

    let b = SqliteBackend::open(&path, day_opts(&wall)).unwrap();
    let mut mains = Vec::new();
    for i in 0..40 {
        let head = b.head().unwrap();
        b.commit(head, &[put_write(&format!("live/{i}"), &[7u8; 3000], head + 1, None)]).unwrap();
        mains.push(head + 1);
    }
    let before = count(&path, "PRAGMA page_count");
    for (i, main) in mains.into_iter().enumerate() {
        let head = b.head().unwrap();
        b.commit(head, &[Write::Delete { key: format!("live/{i}"), prev: Rev::new(main, 0) }]).unwrap();
    }
    assert!(count(&path, "PRAGMA page_count") < before);
}

#[test_id(tveocgj)]
#[test]
fn compact_refuses_while_the_store_is_open_and_writes_nothing() {
    let dir = TempDir::new("sqlite-compact-open");
    let path = dir.path().join("s.db");
    none_mode_store_with_free_pages(&path);
    let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
    let sum = std::fs::read(&path).unwrap();
    let err = state_store_sqlite::compact::compact(&path).unwrap_err().to_string();
    assert!(err.contains("the store is open") && err.contains("stop lc-serve first"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), sum);
    drop(b);
}

fn wal_conn(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn
}

#[test_id(tef0b8g)]
#[test]
#[allow(clippy::disallowed_methods)]
fn compact_folds_a_crash_image_wal_into_the_main_file_before_it_rewrites() {
    let dir = TempDir::new("sqlite-compact-wal");
    let live = dir.path().join("live.db");
    none_mode_store_with_free_pages(&live);
    let conn = wal_conn(&live);
    conn.execute_batch("CREATE TABLE crash_rows(n INTEGER); BEGIN; INSERT INTO crash_rows VALUES (1),(2),(3); COMMIT;")
        .unwrap();
    let image = dir.path().join("image.db");
    std::fs::copy(&live, &image).unwrap();
    std::fs::copy(format!("{}-wal", live.display()), format!("{}-wal", image.display())).unwrap();
    assert!(std::fs::metadata(format!("{}-wal", image.display())).unwrap().len() > 0);
    let report = state_store_sqlite::compact::compact(&image).unwrap();
    drop(conn);
    assert!(report.wal_bytes_before > 0);
    assert_eq!(report.auto_vacuum, 2);
    assert_eq!(count(&image, "SELECT count(*) FROM crash_rows"), 3);
}

#[test_id(t9kve0v)]
#[test]
#[allow(clippy::disallowed_methods)]
fn compact_refuses_naming_the_wal_when_a_reader_keeps_it_from_folding_and_writes_nothing() {
    let dir = TempDir::new("sqlite-compact-wal-held");
    let path = dir.path().join("s.db");
    none_mode_store_with_free_pages(&path);
    let conn = wal_conn(&path);
    conn.execute_batch("CREATE TABLE held(n INTEGER); INSERT INTO held VALUES (1);").unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    conn.execute_batch("BEGIN; SELECT count(*) FROM held;").unwrap();
    let other = rusqlite::Connection::open(&path).unwrap();
    other.execute_batch("INSERT INTO held VALUES (2);").unwrap();
    let sum = std::fs::read(&path).unwrap();
    let err = state_store_sqlite::compact::compact(&path).unwrap_err().to_string();
    assert!(err.contains(&format!("{}-wal", path.display())), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), sum);
    drop(other);
    drop(conn);
}

#[test_id(t0f175i)]
#[test]
fn a_snapshot_of_a_none_mode_store_is_incremental_and_so_is_its_restore() {
    let dir = TempDir::new("sqlite-snapshot-incremental");
    let path = dir.path().join("s.db");
    none_mode_store_with_free_pages(&path);
    let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, day_receipts: false, ..Options::default() }).unwrap();
    b.commit(0, &[put_write("live/a", b"1", 1, None)]).unwrap();
    assert_eq!(count(&path, "PRAGMA auto_vacuum"), 0);
    let snap = dir.path().join("snap.db");
    b.snapshot_to(&snap).unwrap();
    assert_eq!(count(&snap, "PRAGMA auto_vacuum"), 2);
    assert_eq!(count(&snap, "PRAGMA freelist_count"), 0);
    assert!(!dir.path().join("snap.db-wal").exists());
    let revlog = dir.path().join("revlog");
    std::fs::create_dir(&revlog).unwrap();
    let out = dir.path().join("restored.db");
    state_store_sqlite::durability::restore::restore(&snap, &revlog, &out).unwrap();
    assert_eq!(count(&out, "PRAGMA auto_vacuum"), 2);
}

fn mx_frame(path: &Path) -> u32 {
    use std::os::unix::fs::FileExt;
    let shm = std::fs::File::open(format!("{}-shm", path.display())).unwrap();
    let mut b = [0u8; 4];
    shm.read_exact_at(&mut b, 16).unwrap();
    u32::from_ne_bytes(b)
}

const BOUND_EVERY: u64 = 8;
/// A hang guard on waiting for a checkpoint, sized for a loaded round; never the verdict.
const CHECKPOINT_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// 400 commits of 4 KiB values under a reader that never stops; returns the open handle. The
/// background thread never takes a pass: the workload runs each interval's checkpoint itself
/// when `checkpoint`, so host load cannot decide when it happens.
fn bounded_wal_workload(path: &Path, checkpoint: bool) -> SqliteBackend {
    let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, readers: 4, ..Options::default() };
    let b = SqliteBackend::open(path, opts).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (b, stop) = (b.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let head = b.head().unwrap();
                let _ = b.range("", None, 10, head);
            }
        })
    };
    for interval in 0..400 / BOUND_EVERY {
        for _ in 0..BOUND_EVERY {
            put_next(&b);
        }
        if checkpoint && interval + 1 < 400 / BOUND_EVERY {
            while !state_store_sqlite::bench::checkpoint_pass(&b).unwrap() {}
        }
    }
    stop.store(true, Ordering::SeqCst);
    reader.join().unwrap();
    b
}

fn frames_per_commit() -> u32 {
    let dir = TempDir::new("sqlite-frames-per-commit");
    let p = dir.path().join("f.db");
    let b = SqliteBackend::open(&p, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, ..Options::default() }).unwrap();
    let before = mx_frame(&p);
    for _ in 0..10 {
        put_next(&b);
    }
    (mx_frame(&p) - before).div_ceil(10)
}

#[test_id(th7zetr)]
#[test]
fn the_wal_stays_within_three_checkpoint_intervals_while_reads_never_stop() {
    let dir = TempDir::new("sqlite-bounded-wal");
    let path = dir.path().join("s.db");
    let b = bounded_wal_workload(&path, true);
    let bound = frames_per_commit() * 3 * BOUND_EVERY as u32;
    let mx = mx_frame(&path);
    assert!(mx <= bound, "mxFrame {mx} exceeds {bound}");
    drop(b);
}

#[test_id(taaic6o)]
#[test]
fn a_checkpoint_pass_on_an_idle_store_restarts_the_wal() {
    let dir = TempDir::new("sqlite-checkpoint-pass-idle");
    let path = dir.path().join("s.db");
    let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, ..Options::default() }).unwrap();
    for _ in 0..4 {
        put_next(&b);
    }
    assert!(state_store_sqlite::bench::checkpoint_pass(&b).unwrap());
    assert_eq!(std::fs::metadata(format!("{}-wal", path.display())).unwrap().len(), 0);
}

#[test_id(tjpiwcb)]
#[test]
#[should_panic(expected = "exceeds")]
fn the_wal_bound_fails_when_no_checkpoint_pass_runs() {
    let dir = TempDir::new("sqlite-bounded-wal-unchecked");
    let path = dir.path().join("s.db");
    let b = bounded_wal_workload(&path, false);
    let bound = frames_per_commit() * 3 * BOUND_EVERY as u32;
    let mx = mx_frame(&path);
    assert!(mx <= bound, "mxFrame {mx} exceeds {bound}");
    drop(b);
}

const CHILD_ENV: &str = "SQLITE_TEST_CHILD";

/// Distinct 4 KiB keys, `n` of them in batches, so the main file grows to about `n` * 4 KiB.
fn fill_main_file(b: &SqliteBackend, n: u64) {
    for batch in 0..n / 50 {
        let head = b.head().unwrap();
        let writes: Vec<Write> =
            (0..50).map(|i| put_write(&format!("bulk/{batch:06}/{i:02}"), &[3; 4096], head + 1, None)).collect();
        b.commit(head, &writes).unwrap();
    }
}

/// The work a re-executed copy of this binary does for [`run_child`]: `leave:<path>` is an
/// unclean stop (a backlog of the bounded-WAL workload, handle leaked, exit with no drop);
/// `fail:<path>` commits under a file-size limit the main file is already past.
#[test_id(t2p98h7)]
#[test]
fn child_work_when_asked() {
    let Ok(spec) = std::env::var(CHILD_ENV) else { return };
    let (mode, path) = spec.split_once(':').unwrap();
    let path = Path::new(path);
    match mode {
        "leave" => {
            let b = bounded_wal_workload(path, true);
            std::mem::forget(b);
            std::process::exit(0);
        }
        "fail" => {
            let b = SqliteBackend::open(path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: 4, ..Options::default() }).unwrap();
            for _ in 0..8 {
                put_next(&b);
            }
            // clock-safe: lets the checkpointer's 1 s retries run, to count the lines they write
            std::thread::sleep(std::time::Duration::from_millis(3500));
            std::process::exit(0);
        }
        other => panic!("{other}"),
    }
}

fn run_child(spec: String, file_limit_blocks: Option<u64>) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut c = match file_limit_blocks {
        None => std::process::Command::new(exe),
        Some(n) => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c")
                .arg(format!("trap '' XFSZ; ulimit -f {n}; exec \"$0\" \"$@\""))
                .arg(exe);
            c
        }
    };
    let out = c
        .args(["--exact", "child_work_when_asked::t2p98h7", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, spec)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

#[test_id(t8l9936)]
#[test]
fn a_checkpoint_failure_is_logged_once_with_sqlites_error() {
    let dir = TempDir::new("sqlite-checkpoint-failure");
    let path = dir.path().join("s.db");
    {
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        fill_main_file(&b, 2_400);
        b.checkpoint().unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() > 8 << 20);
    let out = run_child(format!("fail:{}", path.display()), Some(1_000));
    let err = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = err.lines().filter(|l| l.starts_with("sqlite-checkpoint:")).collect();
    assert_eq!(lines.len(), 1, "{err}");
    assert!(lines[0].contains(&path.display().to_string()), "{err}");
}

#[test_id(ts1tdbi)]
#[test]
fn a_large_store_left_by_an_unclean_stop_has_only_a_bounded_wal_to_recover() {
    let dir = TempDir::new("sqlite-bounded-recovery");
    let path = dir.path().join("s.db");
    {
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        fill_main_file(&b, 17_000);
        b.checkpoint().unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() >= 64 << 20);
    run_child(format!("leave:{}", path.display()), None);
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    let bound = u64::from(frames_per_commit()) * 3 * BOUND_EVERY;
    let frames = std::fs::metadata(&wal).unwrap().len() / 4120;
    assert!(frames <= bound, "the WAL the next open recovers holds {frames} frames, over {bound}");
    SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, outbox: false, ..Options::default() }).unwrap();
}

#[test_id(tj9drpq)]
#[test]
fn a_commit_is_not_held_by_a_wal_restart_that_is_waiting_on_a_reader() {
    use std::sync::mpsc::channel;
    let dir = TempDir::new("sqlite-restart-no-hold");
    let (held_tx, held_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let armed = Arc::new(AtomicBool::new(false));
    let probe = {
        let armed = armed.clone();
        let held_tx = Mutex::new(held_tx);
        let release_rx = Mutex::new(release_rx);
        Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                held_tx.lock().unwrap().send(()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
            }
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let opts = Options {
        max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
        readers: 1,
        read_probe: Some(probe),
        ..Options::default()
    };
    let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
    put_next(&b);

    armed.store(true, Ordering::SeqCst);
    let reader = {
        let b = b.clone();
        std::thread::spawn(move || {
            let head = b.head().unwrap();
            let _ = b.range("", None, 10, head);
        })
    };
    held_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the reader never opened");

    let pass = {
        let b = b.clone();
        std::thread::spawn(move || state_store_sqlite::bench::checkpoint_pass(&b))
    };
    let (done_tx, done_rx) = channel::<()>();
    let committer = {
        let b = b.clone();
        std::thread::spawn(move || {
            for _ in 0..50 {
                put_next(&b);
            }
            let _ = done_tx.send(());
        })
    };
    let returned = done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .is_ok();
    release_tx.send(()).unwrap();
    reader.join().unwrap();
    pass.join().unwrap().unwrap();
    committer.join().unwrap();
    assert!(returned, "commits were held until the reader was released: the restart holds the writer while it waits");
}

/// A one-reader store whose first read, once armed, holds its transaction open until released.
/// Every later read reports the `-wal` size it sees on `probed`.
const HELD_LINE_FRAMES: u32 = 64;

struct HeldRead {
    b: SqliteBackend,
    wal: String,
    path: PathBuf,
    release: std::sync::mpsc::Sender<()>,
    probed: std::sync::mpsc::Receiver<u64>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl HeldRead {
    fn open(dir: &TempDir, line_bytes: u64) -> HeldRead {
        use std::sync::mpsc::channel;
        let path = dir.path().join("s.db");
        let wal = format!("{}-wal", path.display());
        let (held_tx, held_rx) = channel::<()>();
        let (release, release_rx) = channel::<()>();
        let (probed_tx, probed) = channel::<u64>();
        let armed = Arc::new(AtomicBool::new(false));
        let probe = {
            let (armed, wal) = (armed.clone(), wal.clone());
            let (held_tx, release_rx, probed_tx) = (Mutex::new(held_tx), Mutex::new(release_rx), Mutex::new(probed_tx));
            Arc::new(move || {
                if armed.swap(false, Ordering::SeqCst) {
                    held_tx.lock().unwrap().send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv();
                } else {
                    let len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
                    let _ = probed_tx.lock().unwrap().send(len);
                }
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let opts = Options {
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
            readers: 1,
            checkpoint_every: u64::MAX,
            wal_line_bytes: line_bytes,
            read_probe: Some(probe),
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        put_next(&b);
        armed.store(true, Ordering::SeqCst);
        let reader = {
            let b = b.clone();
            std::thread::spawn(move || {
                let head = b.head().unwrap();
                let _ = b.range("", None, 10, head);
            })
        };
        held_rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the reader never opened");
        HeldRead { b, wal, path, release, probed, reader: Some(reader) }
    }

    fn grow_wal_past(&self, frames: u32) {
        while mx_frame(&self.path) <= frames {
            put_next(&self.b);
        }
    }

    fn start_pass(&self) -> std::thread::JoinHandle<Result<bool, String>> {
        let b = self.b.clone();
        std::thread::spawn(move || state_store_sqlite::bench::checkpoint_pass(&b))
    }

    fn release_reader(&mut self) {
        self.release.send(()).unwrap();
        self.reader.take().unwrap().join().unwrap();
    }
}

#[test_id(t0bbulp)]
#[test]
fn a_restart_past_the_wal_line_waits_out_a_read_in_flight() {
    let dir = TempDir::new("sqlite-restart-past-cap");
    let mut h = HeldRead::open(&dir, u64::from(HELD_LINE_FRAMES) * 4096);
    h.grow_wal_past(HELD_LINE_FRAMES);
    let pass = h.start_pass();
    std::thread::sleep(std::time::Duration::from_millis(200)); // clock-safe: only orders the release after GATE_HOLD; the verdict holds however late it comes
    h.release_reader();
    assert!(pass.join().unwrap().unwrap(), "the restart gave up on a read in flight past the line");
    assert_eq!(std::fs::metadata(&h.wal).unwrap().len(), 0);
}

#[test_id(tdqny6p)]
#[test]
fn a_read_queued_on_a_reader_does_not_start_while_a_restart_holds_the_gate() {
    let dir = TempDir::new("sqlite-restart-queued-read");
    let mut h = HeldRead::open(&dir, u64::from(HELD_LINE_FRAMES) * 4096);
    h.grow_wal_past(HELD_LINE_FRAMES);
    let queued = {
        let b = h.b.clone();
        std::thread::spawn(move || {
            let head = b.head().unwrap();
            let _ = b.range("", None, 10, head);
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(100)); // clock-safe: only orders the queued caller before the restart
    let pass = h.start_pass();
    std::thread::sleep(std::time::Duration::from_millis(100)); // clock-safe: only orders the restart's gate before the release
    h.release_reader();
    assert!(pass.join().unwrap().unwrap());
    queued.join().unwrap();
    let seen = h.probed.recv().unwrap();
    assert_eq!(seen, 0, "the queued read opened its transaction before the restart finished: the WAL held {seen} B");
}

#[test_id(tpy2lrz)]
#[test]
fn a_restart_below_the_wal_line_still_gives_up_while_a_read_is_held() {
    let dir = TempDir::new("sqlite-restart-below-cap");
    let mut h = HeldRead::open(&dir, u64::from(HELD_LINE_FRAMES) * 4096);
    assert!(mx_frame(&h.path) <= HELD_LINE_FRAMES);
    let pass = h.start_pass();
    assert!(!pass.join().unwrap().unwrap(), "the restart finished while a read was still held");
    h.release_reader();
}

#[test_id(ty5gke5)]
#[test]
fn the_background_checkpointer_starts_a_pass_when_the_wal_reaches_the_line_whatever_the_commit_count() {
    const LINE: u64 = 1 << 20;
    let dir = TempDir::new("sqlite-wal-line-trigger");
    let path = dir.path().join("s.db");
    let wal = format!("{}-wal", path.display());
    let opts = Options { checkpoint_every: u64::MAX, wal_line_bytes: LINE, readers: 4, ..Options::default() };
    let b = SqliteBackend::open(&path, opts).unwrap();
    let commit_bytes = u64::from(frames_per_commit()) * (4096 + 24);
    let wal_len = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    let mut written = 0u64;
    let mut largest = 0u64;
    while written < 4 << 20 {
        let before = b.checkpoints_completed();
        put_next(&b);
        written += commit_bytes;
        if wal_len() >= LINE {
            let seen = wal_len();
            assert!(
                state_store_sqlite::bench::await_checkpoint(&b, before, CHECKPOINT_CAP).is_some(),
                "the WAL is {seen} B, at the line {LINE} B, and no pass ran"
            );
        }
        largest = largest.max(wal_len());
    }
    assert!(largest <= LINE + 2 * commit_bytes, "the WAL grew to {largest} B, past the line {LINE} B plus two commits");
    assert!(b.checkpoints_completed() >= 3, "only {} passes ran over 4 MiB at a 1 MiB line", b.checkpoints_completed());
}

#[test_id(t0n2lh3)]
#[test]
fn a_wal_past_half_its_line_is_reset_before_it_reaches_the_line() {
    const LINE: u64 = 1 << 20;
    let dir = TempDir::new("sqlite-wal-half-line-trigger");
    let path = dir.path().join("s.db");
    let wal = format!("{}-wal", path.display());
    let opts = Options { checkpoint_every: u64::MAX, wal_line_bytes: LINE, max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() };
    let b = SqliteBackend::open(&path, opts).unwrap();
    let wal_len = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    let before = b.checkpoints_completed();
    let mut largest = 0u64;
    while largest < LINE / 2 && b.checkpoints_completed() == before {
        put_next(&b);
        largest = largest.max(wal_len());
    }
    assert!(
        state_store_sqlite::bench::await_checkpoint(&b, before, CHECKPOINT_CAP).is_some(),
        "the WAL reached {largest} B, past half the line {LINE} B, and no pass ran"
    );
    largest = largest.max(wal_len());
    assert!(largest < LINE, "the WAL grew to {largest} B, up to the line {LINE} B");
}

/// A [`VirtualClock`] that notes when the thread named `pass` first reads it: the restart reads
/// it as it begins, so the flag says the restart has taken its start time.
#[derive(Clone)]
struct PassClock {
    time: VirtualClock,
    pass_began: Arc<AtomicBool>,
}

impl Clock for PassClock {
    fn now_ms(&self) -> u64 {
        let now = self.time.now_ms();
        if std::thread::current().name() == Some("pass") {
            self.pass_began.store(true, Ordering::SeqCst);
        }
        now
    }
}

#[test_id(t63juu6)]
#[test]
fn the_wal_stays_under_nine_mib_while_two_readers_never_pause_and_one_is_held_open() {
    const LINE: u64 = 8 << 20;
    const MAX_READ_MS: u64 = 100;
    let dir = TempDir::new("sqlite-wal-line-held");
    let path = dir.path().join("s.db");
    let wal = format!("{}-wal", path.display());
    let clock = VirtualClock::new(0);
    let pass_began = Arc::new(AtomicBool::new(false));
    let test_clock = PassClock { time: clock.clone(), pass_began: pass_began.clone() };
    let (armed_tx, armed_rx) = std::sync::mpsc::channel::<u64>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (ended_tx, ended_rx) = std::sync::mpsc::channel::<()>();
    let probe = {
        let (clock, armed_tx, release_rx) = (clock.clone(), Mutex::new(armed_tx), Mutex::new(release_rx));
        Arc::new(move || {
            if std::thread::current().name() == Some("held") {
                armed_tx.lock().unwrap().send(clock.now_ms()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
            }
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let opts = Options {
        max_read_ms: MAX_READ_MS,
        readers: 4,
        clock: Arc::new(test_clock),
        checkpoint_every: u64::MAX,
        wal_line_bytes: LINE,
        read_probe: Some(probe),
        ..Options::default()
    };
    let b = SqliteBackend::open(&path, opts).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut readers: Vec<_> = (0..2)
        .map(|_| {
            let (b, stop) = (b.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let head = b.head().unwrap();
                    let _ = b.range("", None, 10, head);
                }
            })
        })
        .collect();
    readers.push({
        let (b, stop) = (b.clone(), stop.clone());
        std::thread::Builder::new()
            .name("held".into())
            .spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let head = b.head().unwrap();
                    let _ = b.range("", None, 10, head);
                    let _ = ended_tx.send(());
                }
            })
            .unwrap()
    });
    let page = 4096u64;
    let mut held_deadline = armed_rx.recv().unwrap() + MAX_READ_MS;
    let release_held = |held_deadline: &mut u64| {
        release_tx.send(()).unwrap();
        ended_rx.recv().unwrap();
        *held_deadline = armed_rx.recv().unwrap() + MAX_READ_MS;
    };
    let mut largest = 0u64;
    for commit in 1..=2_000u32 {
        put_next(&b);
        largest = largest.max(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0));
        if commit % 50 != 0 {
            continue;
        }
        clock.advance(1);
        if clock.now_ms() > held_deadline {
            release_held(&mut held_deadline);
            clock.advance(1);
        }
        let past_line = u64::from(mx_frame(&path)) * page > LINE;
        let b2 = b.clone();
        pass_began.store(false, Ordering::SeqCst);
        let pass = std::thread::Builder::new()
            .name("pass".into())
            .spawn(move || state_store_sqlite::bench::checkpoint_pass(&b2))
            .unwrap();
        while !pass_began.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        if past_line {
            clock.advance_to(held_deadline + 1);
            release_tx.send(()).unwrap();
            ended_rx.recv().unwrap();
            assert!(pass.join().unwrap().unwrap(), "the restart past the line gave up with the held read ended");
            held_deadline = armed_rx.recv().unwrap() + MAX_READ_MS;
        } else {
            clock.advance(50);
            pass.join().unwrap().unwrap();
        }
        largest = largest.max(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0));
    }
    stop.store(true, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    for r in readers {
        r.join().unwrap();
    }
    assert!(largest <= 9 * 1_048_576, "the WAL grew to {largest} B, past {} B", 9 * 1_048_576);
    assert!(largest > LINE / 2, "the workload never built a WAL worth bounding: {largest} B");
}

type Seen = Arc<Mutex<Vec<state_store::Watched>>>;

fn watching_probe(
    watcher: &Arc<std::sync::OnceLock<Contract<SqliteBackend>>>,
    from: &Arc<std::sync::atomic::AtomicU64>,
    seen: &Seen,
    fail: bool,
) -> Arc<dyn Fn() -> state_store::Result<()> + Send + Sync> {
    let (watcher, from, seen) = (watcher.clone(), from.clone(), seen.clone());
    Arc::new(move || {
        let Some(w) = watcher.get() else { return Ok(()) };
        let n = from.load(Ordering::SeqCst);
        let first = w.watch("w/", n, 0).unwrap();
        let again = match &first {
            state_store::Watched::Progress { progress_rev } => w.watch("w/", *progress_rev, 0).unwrap(),
            other => other.clone(),
        };
        seen.lock().unwrap().extend([first, again]);
        if fail { Err(Error::Backend("injected".into())) } else { Ok(()) }
    })
}

#[test_id(tg9n2cv)]
#[test]
fn a_watch_inside_a_commit_is_progress_at_the_revision_before_it_and_the_event_arrives_after() {
    let dir = TempDir::new("sqlite-watch-in-commit");
    let wall = VirtualClock::new(D0);
    let watcher = Arc::new(std::sync::OnceLock::new());
    let from = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let probe = watching_probe(&watcher, &from, &seen, false);
    let b = SqliteBackend::open(&dir.path().join("s.db"), Options { commit_probe: Some(probe), ..day_opts(&wall) }).unwrap();
    let c = contract(b.clone(), &wall);
    let prior = put(&c, "w/a", json!(1), "1");
    watcher.set(contract(b, &wall)).ok().unwrap();
    from.store(prior, Ordering::SeqCst);
    let n = put(&c, "w/b", json!(2), "2");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![state_store::Watched::Progress { progress_rev: prior }; 2],
        "inside commit {n} a watch from {prior} sees nothing and its re-watch is accepted"
    );
    let state_store::Watched::Events { events, through } = c.watch("w/", prior, 0).unwrap() else { panic!("no event at {n}") };
    assert_eq!((events.len(), through), (1, n));
    assert_eq!(c.watch("w/", n, 0).unwrap(), state_store::Watched::Progress { progress_rev: n });
}

#[test_id(tk4w8pe)]
#[test]
fn a_watch_delivers_nothing_for_a_commit_the_probe_fails() {
    let dir = TempDir::new("sqlite-watch-failed-commit");
    let wall = VirtualClock::new(D0);
    let watcher = Arc::new(std::sync::OnceLock::new());
    let from = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let probe = watching_probe(&watcher, &from, &seen, true);
    let b = SqliteBackend::open(&dir.path().join("s.db"), Options { commit_probe: Some(probe), ..day_opts(&wall) }).unwrap();
    let c = contract(b.clone(), &wall);
    let head = c.head().unwrap();
    watcher.set(contract(b, &wall)).ok().unwrap();
    from.store(head, Ordering::SeqCst);
    assert!(c.txn(&Txn::new().put("w/a", Object::new(json!(1))), &req("1")).is_err());
    assert_eq!(*seen.lock().unwrap(), vec![state_store::Watched::Progress { progress_rev: head }; 2]);
    assert_eq!(c.watch("w/", head, 0).unwrap(), state_store::Watched::Progress { progress_rev: head });
}
