//! The control store's off-box copy end to end on real files: the shipper's signed PUTs to an S3
//! endpoint through a recording fake curl (no process is started), the outbox, a
//! restore from the uploaded objects and the drill's fold-and-compare.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::ports::{Proc, ProcOutput, ProcSpec, VirtualClock};
use serde_json::json;
use sha2::{Digest, Sha256};
use state_store::conformance as suite;
use state_store::gc::Controller;
use state_store::{Backend, Contract, Object, Request, Txn, Write};
use state_store_sqlite::durability::drill::{self, Exclusions, Mismatch};
use state_store_sqlite::durability::put::{Dest, Put, S3Creds, Uploader};
use state_store_sqlite::durability::restore;
use state_store_sqlite::durability::revlog::{self, REVLOG, SNAPSHOT};
use state_store_sqlite::durability::ship::{ShipConfig, Shipper};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::TempDir;

fn open(path: &Path) -> SqliteBackend {
    SqliteBackend::open(path, Options::default()).unwrap()
}

/// A copy being checked (a snapshot, a restore, a backup): opened without the jump, so its
/// revisions read exactly as written (sp-r3yb7x).
fn verify(path: &Path) -> SqliteBackend {
    SqliteBackend::open(path, Options { jump_on_open: false, day_receipts: false, ..Options::default() }).unwrap()
}

fn req(id: &str) -> Request {
    Request::new(id, "durability-test", "test.write")
}

fn put(c: &Contract<SqliteBackend>, key: &str, v: Object, id: &str) -> u64 {
    let r = c.txn(&Txn::new().put(key, v), &req(id)).unwrap();
    assert!(r.succeeded, "{key}: {r:?}");
    r.rev
}

fn keys_at(b: &SqliteBackend, at: u64) -> Vec<(String, Vec<u8>)> {
    b.range("", None, usize::MAX, at)
        .unwrap()
        .into_iter()
        .map(|(k, v)| (k, v.value))
        .collect()
}

fn ship_cfg(dir: &Path, snapshot_every_ms: u64) -> ShipConfig {
    ShipConfig {
        dir: dir.join("lc-ship"),
        max_mains: 1_000,
        snapshot_every_ms,
    }
}

fn file_put(dest: &Path) -> Arc<dyn Put> {
    Arc::new(Uploader {
        dest: Dest::File(dest.to_path_buf()),
        creds: None,
        proc: Arc::new(Curl::default()),
        deadline: Duration::from_secs(10),
    })
}

/// One curl run the uploader asked for: its argv, its stdin (the `-K -` config) and the bytes of
/// the file it was told to upload, read at the call.
type Run = (Vec<String>, String, Vec<u8>);

/// A fake curl: records every run, exits 22 (an HTTP refusal under `--fail-with-body`) on the
/// next run while `refuse_next` is set, and 0 otherwise. No process is started.
#[derive(Clone, Default)]
struct Curl {
    runs: Arc<Mutex<Vec<Run>>>,
    refuse_next: Arc<Mutex<bool>>,
}

impl Proc for Curl {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        assert_eq!(spec.prog, "curl");
        let file = spec.args.iter().position(|a| a == "-T").map(|i| spec.args[i + 1].clone()).unwrap();
        let body = std::fs::read(&file).unwrap();
        self.runs.lock().unwrap().push((spec.args.clone(), String::from_utf8(spec.stdin.clone()).unwrap(), body));
        let code = if std::mem::take(&mut *self.refuse_next.lock().unwrap()) { 22 } else { 0 };
        Ok(ProcOutput { code: Some(code), stdout: Vec::new(), stderr: b"The requested URL returned error: 500".to_vec() })
    }
}

#[test_id(t5txm05)]
#[test]
fn the_shipper_puts_every_revision_to_a_put_only_endpoint_and_retries_a_refusal_under_the_same_name(
) {
    let dir = TempDir::new("ship-s3");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let curl = Curl::default();
    let uploader = Uploader {
        dest: Dest::parse("s3://bucket/spira/lc/").unwrap(),
        creds: Some(S3Creds {
            access_key: "AKTEST".into(),
            secret_key: "secret\"x".into(),
            session_token: Some("tok".into()),
            region: "us-test-1".into(),
            endpoint: Some("http://endpoint.test".into()),
            part_bytes: 256 << 20,
        }),
        proc: Arc::new(curl.clone()),
        deadline: Duration::from_secs(10),
    };
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), u64::MAX),
        Arc::new(uploader),
        Arc::new(clock.clone()),
    )
    .unwrap();
    put(&c, "a", Object::new(json!(1)), "1");
    let first = s.tick().unwrap();
    assert_eq!(first.uploaded.len(), 2, "{first:?}");
    put(&c, "b", Object::new(json!(2)), "2");
    put(&c, "a", Object::new(json!(3)), "3");
    *curl.refuse_next.lock().unwrap() = true;
    let refused = s.tick().unwrap();
    assert!(
        refused
            .failed
            .as_deref()
            .is_some_and(|f| f.contains("PUT spira/lc/revlog/")),
        "{refused:?}"
    );
    assert!(refused.uploaded.is_empty());
    clock.advance(4_000);
    let held = s.tick().unwrap();
    assert_eq!(held.uploaded.len(), 1, "{held:?}");

    let runs = curl.runs.lock().unwrap().clone();
    assert!(
        runs.iter().all(|(a, ..)| a.windows(2).any(|w| w == ["-X", "PUT"])),
        "only PUTs: {:?}",
        runs.iter().map(|r| &r.0).collect::<Vec<_>>()
    );
    let urls: Vec<&str> = runs.iter().map(|(a, ..)| a.last().unwrap().as_str()).collect();
    let retried = urls[2];
    assert_eq!(urls[3], retried, "the retry is the refused object, under its own name");
    assert!(retried.starts_with("http://endpoint.test/bucket/spira/lc/revlog/"), "{retried}");
    assert!(
        urls.iter()
            .any(|u| u.starts_with("http://endpoint.test/bucket/spira/lc/snapshot/")),
        "{urls:?}"
    );
    for (args, config, body) in &runs {
        let sum: String = Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(args.contains(&format!("x-amz-content-sha256: {sum}")), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["--aws-sigv4", "aws:amz:us-test-1:s3"]), "{args:?}");
        assert!(config.contains("AKTEST:secret\\\"x"), "the credentials go on stdin: {config}");
        assert!(config.contains("x-amz-security-token: tok"), "{config}");
        assert!(
            !args.iter().any(|a| a.contains("AKTEST") || a.contains("secret") || a.contains("tok")),
            "no credential on argv: {args:?}"
        );
    }
    let mut mains = BTreeSet::new();
    for (args, _, body) in &runs {
        if args.last().unwrap().contains("/revlog/") {
            mains.extend(
                revlog::decode(body)
                    .unwrap()
                    .entries
                    .iter()
                    .map(|e| e.rev.main),
            );
        }
    }
    let head = c.backend().head().unwrap();
    assert_eq!(
        mains,
        (1..=head).collect(),
        "every revision reached the endpoint"
    );
    assert_eq!(held.shipped_through, head);
}

/// Ships a snapshot at `s` and two runs after it to `file://`; returns the live contract.
fn shipped_store(dir: &Path, clock: &VirtualClock) -> (Contract<SqliteBackend>, Shipper) {
    let c = suite::open(open(&dir.join("live.db")), clock);
    put(
        &c,
        "lifecycle/sp-1",
        Object::new(json!({"state": "READY"})),
        "s1",
    );
    let t = c
        .txn(
            &Txn::new().put(
                "telemetry/sections/base",
                Object::new(json!(0)).with_ttl(600),
            ),
            &req("s2"),
        )
        .unwrap();
    assert!(t.succeeded);
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir, u64::MAX),
        file_put(&dir.join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    assert!(s.tick().unwrap().snapshot.is_some());
    put(
        &c,
        "lifecycle/sp-1",
        Object::new(json!({"state": "WORKING"})),
        "r1",
    );
    put(
        &c,
        "lifecycle/sp-2",
        Object::new(json!({"state": "READY"})),
        "r2",
    );
    for (i, k) in [
        "telemetry/sections/a",
        "telemetry/sections/b",
        "telemetry/reconciler/pass",
    ]
    .iter()
    .enumerate()
    {
        let t = c
            .txn(
                &Txn::new().put(k, Object::new(json!(i)).with_ttl(600)),
                &req(&format!("t{i}")),
            )
            .unwrap();
        assert!(t.succeeded);
    }
    put(
        &c,
        "intents/claim/sp-2",
        Object::new(json!({"by": "d1"})).with_ttl(30),
        "i1",
    );
    put(
        &c,
        "intents/claim/sp-1",
        Object::new(json!({"by": "d2"})).with_ttl(3_600),
        "i2",
    );
    assert!(s.tick().unwrap().failed.is_none());
    clock.advance(31_000);
    let swept = Controller::new(&c).tick(c.now_ms()).unwrap();
    assert_eq!(swept.expired, ["intents/claim/sp-2"]);
    let fence = c.lock("lifecycle/sp-1", 60_000, &req("l1")).unwrap();
    c.release(&fence, &req("l2")).unwrap();
    put(
        &c,
        "lifecycle/sp-2",
        Object::new(json!({"state": "WORKING"})),
        "r3",
    );
    let t = s.tick().unwrap();
    assert!(
        t.failed.is_none() && t.shipped_through == c.backend().head().unwrap(),
        "{t:?}"
    );
    (c, s)
}

#[test_id(tze8fc6)]
#[test]
fn the_drill_passes_on_the_uploaded_objects_and_names_its_exclusions() {
    let dir = TempDir::new("drill-pass");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    assert_eq!(std::fs::read_dir(s.sent(REVLOG)).unwrap().count(), 2);
    let (restored, rep) =
        drill::drill(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT).unwrap();
    assert!(rep.passed(), "{}", drill::render(&rep));
    assert_eq!(restored.through, c.backend().head().unwrap());
    assert_eq!(
        rep.ttl_swept,
        BTreeSet::from(["intents/claim/sp-2".to_string()])
    );
    let telemetry: Vec<&str> = rep.telemetry_excluded.iter().map(String::as_str).collect();
    assert_eq!(
        telemetry,
        [
            "telemetry/reconciler/pass",
            "telemetry/sections/a",
            "telemetry/sections/b",
            "telemetry/sections/base"
        ]
    );
    let out = drill::render(&rep);
    assert!(
        out.starts_with("drill ok:")
            && out.contains("ttl_swept intents/claim/sp-2")
            && out.contains("skew guard: after a restore"),
        "{out}"
    );
    let restored_db = verify(&dir.path().join("scratch/control.db"));
    let head = c.backend().head().unwrap();
    assert_eq!(
        keys_at(&restored_db, head),
        keys_at(c.backend(), head),
        "the restore is the live store at the shipped head"
    );

    let (_, red) =
        drill::drill(s.dir(), &dir.path().join("scratch-red"), Exclusions::NONE).unwrap();
    let named: BTreeSet<String> = red.mismatches.iter().map(ToString::to_string).collect();
    assert!(
        named.contains("intents/claim/sp-2: the fold holds it and the restored copy does not"),
        "{named:?}"
    );
    assert!(
        named.contains(
            "telemetry/sections/a: the restored copy holds it and no audited write produced it"
        ),
        "{named:?}"
    );
}

/// Overwrites the newest version of `key` in the store file with `value`, keeping its revision:
/// in `latest`, the only copy of it.
fn tamper(path: &Path, key: &str, value: &[u8]) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let n = conn
        .prepare("UPDATE latest SET value = ?1 WHERE key = ?2")
        .unwrap()
        .execute(rusqlite::params![value, key.as_bytes()])
        .unwrap();
    assert_eq!(n, 1, "{key}");
}

fn restored_copy(dir: &Path, s: &Shipper) -> (PathBuf, PathBuf, Vec<revlog::Run>) {
    let (_, snap) = drill::newest_snapshot(s.dir()).unwrap();
    let out = dir.join("restored.db");
    let r = restore::restore(&snap, &s.sent(REVLOG), &out).unwrap();
    let base = dir.join("base.db");
    std::fs::copy(&snap, &base).unwrap();
    (out, base, r.runs)
}

fn flipped(b: &SqliteBackend, key: &str) -> Vec<u8> {
    let mut v = b.get(key, b.head().unwrap()).unwrap().unwrap().value;
    let last = v.len() - 2;
    v[last] ^= 1;
    v
}

#[test_id(t2nj9ko)]
#[test]
fn a_flipped_byte_in_a_restored_value_fails_the_drill_naming_the_key() {
    let dir = TempDir::new("drill-flip");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    for key in ["lifecycle/sp-1", "intents/claim/sp-1"] {
        let sub = dir.path().join(key.replace('/', "-"));
        std::fs::create_dir_all(&sub).unwrap();
        let (out, base, runs) = restored_copy(&sub, &s);
        tamper(&out, key, &flipped(c.backend(), key));
        let rep =
            drill::fold_compare(&verify(&base), &runs, &verify(&out), Exclusions::CONTRACT).unwrap();
        assert_eq!(
            rep.mismatches,
            [Mismatch::Value { key: key.into() }],
            "{}",
            drill::render(&rep)
        );
    }
}

/// A clock that moves one millisecond per reading, so a read costs what it touches: the fixture
/// whose one full-keyspace read passes `max_read_ms` while a short page does not.
struct Ticking(std::sync::atomic::AtomicU64);

impl runtime::ports::Clock for Ticking {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }
}

fn capped(path: &Path) -> SqliteBackend {
    let opts = Options {
        max_read_ms: 4,
        clock: Arc::new(Ticking(Default::default())),
        jump_on_open: false,
        ..Options::default()
    };
    SqliteBackend::open(path, opts).unwrap()
}

#[test_id(thsfqvi)]
#[test]
fn the_drill_gives_its_verdict_when_one_read_of_the_keyspace_would_pass_max_read_ms() {
    let dir = TempDir::new("drill-capped");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    let (out, base, runs) = restored_copy(dir.path(), &s);
    {
        let b = capped(&base);
        assert!(
            b.range("", None, usize::MAX, b.head().unwrap()).is_err(),
            "the fixture's single full read must exceed the cap"
        );
    }
    let paged = drill::fold_compare(&capped(&base), &runs, &capped(&out), Exclusions::CONTRACT).unwrap();
    assert!(paged.passed(), "{}", drill::render(&paged));
    let whole =
        drill::fold_compare(&verify(&base), &runs, &verify(&out), Exclusions::CONTRACT).unwrap();
    assert!(whole.compared > 3, "{}", drill::render(&whole));
    assert_eq!(paged, whole);

    let key = "intents/claim/sp-1";
    tamper(&out, key, &flipped(c.backend(), key));
    let rep = drill::fold_compare(&capped(&base), &runs, &capped(&out), Exclusions::CONTRACT).unwrap();
    assert_eq!(rep.mismatches, [Mismatch::Value { key: key.into() }], "{}", drill::render(&rep));
}

#[test_id(tptyh1v)]
#[test]
fn a_live_key_no_audited_write_produced_fails_the_drill_under_any_other_prefix() {
    let dir = TempDir::new("drill-unfolded");
    let clock = VirtualClock::new(1_000);
    let (c, mut s) = shipped_store(dir.path(), &clock);
    let b = c.backend();
    let head = b.head().unwrap();
    let raw = Write::Put {
        key: "lifecycle/sp-ghost".into(),
        value: Object::new(json!(1)).encode(),
        create_rev: state_store::Rev::new(head + 1, 0),
        prev: None,
    };
    b.commit(head, &[raw]).unwrap();
    assert!(s.tick().unwrap().failed.is_none());
    let (_, rep) =
        drill::drill(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT).unwrap();
    assert_eq!(
        rep.mismatches,
        [Mismatch::Unfolded {
            key: "lifecycle/sp-ghost".into()
        }],
        "{}",
        drill::render(&rep)
    );
}

#[test_id(tta4daa)]
#[test]
fn a_restore_with_one_run_missing_refuses_naming_the_gap() {
    let dir = TempDir::new("restore-gap");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    let runs: Vec<PathBuf> = std::fs::read_dir(s.sent(REVLOG))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let mut names: Vec<String> = runs
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let (from, to) = revlog::parse_run_name(&names[0]).unwrap();
    std::fs::remove_file(s.sent(REVLOG).join(&names[0])).unwrap();
    let (_, snap) = drill::newest_snapshot(s.dir()).unwrap();
    let out = dir.path().join("out.db");
    let err = restore::restore(&snap, &s.sent(REVLOG), &out).unwrap_err();
    assert_eq!(
        err,
        format!(
            "gap: revisions {from}..={to} are in no run (the next run is {})",
            names[1]
        )
    );
    assert!(!out.exists(), "a refused restore leaves no file");
    assert!(c.backend().head().unwrap() > to);
}

#[test_id(trixa1h)]
#[test]
fn a_snapshot_of_a_store_larger_than_a_tick_copies_finishes_in_one_tick_and_holds_its_revision() {
    let dir = TempDir::new("ship-onepass");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..400 {
        put(
            &c,
            &format!("b/{i:03}"),
            Object::new(json!(big)),
            &format!("seed{i}"),
        );
    }
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), u64::MAX),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    let rev = s
        .tick()
        .unwrap()
        .snapshot
        .expect("the first tick finishes the snapshot");
    put(&c, "after", Object::new(json!(1)), "after");
    s.tick().unwrap();
    let snap = s.sent(SNAPSHOT).join(revlog::snapshot_name(rev));
    let copy = dir.path().join("copy.db");
    std::fs::copy(&snap, &copy).unwrap();
    let copy = verify(&copy);
    assert_eq!(
        copy.head().unwrap(),
        rev,
        "the snapshot is tagged with the revision it holds"
    );
    assert_eq!(keys_at(&copy, rev), keys_at(c.backend(), rev));
}

#[test_id(tbv8sav)]
#[test]
fn an_audit_record_that_does_not_describe_its_txn_fails_the_drill_naming_it() {
    let dir = TempDir::new("drill-event");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    let head = c.backend().head().unwrap();
    let key = state_store::events_key(head);
    for (name, value) in [
        ("empty", Object::new(json!({"subs": []})).encode()),
        ("garbled", b"not json".to_vec()),
    ] {
        let sub = dir.path().join(name);
        std::fs::create_dir_all(&sub).unwrap();
        let (out, base, runs) = restored_copy(&sub, &s);
        tamper(&out, &key, &value);
        let rep =
            drill::fold_compare(&verify(&base), &runs, &verify(&out), Exclusions::CONTRACT).unwrap();
        assert!(
            matches!(rep.mismatches.as_slice(), [Mismatch::Event { main, .. }] if *main == head),
            "{name}: {}",
            drill::render(&rep)
        );
        assert!(
            drill::render(&rep).contains(&format!("mismatch {key}: ")),
            "{}",
            drill::render(&rep)
        );
    }
}

#[test_id(tp89iom)]
#[test]
fn local_copies_keep_two_snapshots_and_the_runs_after_the_older_and_still_drill() {
    let dir = TempDir::new("ship-prune");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), 0),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    let mut snaps = Vec::new();
    for i in 0..4 {
        put(
            &c,
            &format!("lifecycle/sp-{i}"),
            Object::new(json!(i)),
            &format!("p{i}"),
        );
        snaps.push(s.tick().unwrap().snapshot.unwrap());
    }
    let kept: Vec<u64> = std::fs::read_dir(s.sent(SNAPSHOT))
        .unwrap()
        .map(|e| revlog::parse_snapshot_name(&e.unwrap().file_name().to_string_lossy()).unwrap())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(kept, snaps[2..]);
    let runs: Vec<(u64, u64)> = std::fs::read_dir(s.sent(REVLOG))
        .unwrap()
        .map(|e| revlog::parse_run_name(&e.unwrap().file_name().to_string_lossy()).unwrap())
        .collect();
    assert!(
        runs.iter().all(|(_, to)| *to > snaps[2]),
        "a run no kept snapshot needs was kept: {runs:?}"
    );
    assert!(
        std::fs::read_dir(dir.path().join("off").join(REVLOG))
            .unwrap()
            .count()
            == 4,
        "the destination's copies are never pruned"
    );
    let (_, rep) =
        drill::drill(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT).unwrap();
    assert!(rep.passed(), "{}", drill::render(&rep));
}

#[test_id(tmj22xu)]
#[test]
fn a_snapshot_that_changed_after_it_was_staged_is_refused_by_the_drill() {
    let dir = TempDir::new("drill-snapshot-sum");
    let clock = VirtualClock::new(1_000);
    let (_c, s) = shipped_store(dir.path(), &clock);
    let (rev, snap) = drill::newest_snapshot(s.dir()).unwrap();
    let mut bytes = std::fs::read(&snap).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 1;
    std::fs::write(&snap, bytes).unwrap();
    let err = drill::drill(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT).unwrap_err();
    assert!(
        err.starts_with(&format!(
            "snapshot {} changed since it was staged",
            revlog::snapshot_name(rev)
        )),
        "{err}"
    );
}

#[test_id(t8ge0ea)]
#[test]
fn an_idle_store_takes_no_second_snapshot_at_the_same_revision_and_another_history_is_refused() {
    let dir = TempDir::new("ship-idle");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    put(&c, "k", Object::new(json!(1)), "1");
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), 0),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    assert!(s.tick().unwrap().snapshot.is_some());
    clock.advance(1_000);
    let idle = s.tick().unwrap();
    assert_eq!((idle.snapshot, idle.uploaded.len()), (None, 0), "{idle:?}");
    assert_eq!(
        std::fs::read_dir(dir.path().join("off").join(SNAPSHOT))
            .unwrap()
            .count(),
        1
    );
    put(&c, "k", Object::new(json!(2)), "2");
    assert!(s.tick().unwrap().snapshot.is_some());

    let other = open(&dir.path().join("restored.db"));
    let err = Shipper::open(
        other,
        ship_cfg(dir.path(), 0),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .err()
    .unwrap();
    assert!(
        err.contains("the store's head 0 is behind revision 2") && err.contains("move it aside"),
        "{err}"
    );
}

#[test_id(t89remx)]
#[test]
fn a_ttl_d_key_deleted_by_an_audited_write_but_still_restored_missing_is_a_mismatch_not_ttl_swept()
{
    let dir = TempDir::new("drill-ttl-narrow");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    let (out, base, runs) = restored_copy(dir.path(), &s);
    let b = verify(&out);
    let head = b.head().unwrap();
    let prev = b.get("intents/claim/sp-1", head).unwrap().unwrap().mod_rev;
    b.commit(
        head,
        &[Write::Delete {
            key: "intents/claim/sp-1".into(),
            prev,
        }],
    )
    .unwrap();
    drop(b);
    let restored = verify(&out);
    let rep = drill::fold_compare(&verify(&base), &runs, &restored, Exclusions::CONTRACT).unwrap();
    assert!(
        rep.mismatches.contains(&Mismatch::Missing {
            key: "intents/claim/sp-1".into()
        }),
        "{}",
        drill::render(&rep)
    );
    assert!(
        rep.ttl_swept.contains("intents/claim/sp-2"),
        "{}",
        drill::render(&rep)
    );
    drop(c);
}

/// The store's live keys and values at its head.
fn live(b: &SqliteBackend) -> Vec<(String, Vec<u8>)> {
    keys_at(b, b.head().unwrap())
}

#[test_id(t87ydes)]
#[test]
fn commits_made_then_the_store_dropped_without_a_stop_hook_ship_every_revision_with_no_snapshot_out_of_cadence(
) {
    let dir = TempDir::new("ship-outbox-restart");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    let off = dir.path().join("off");
    let mut first = {
        let c = suite::open(open(&path), &clock);
        let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
        put(&c, "a", Object::new(json!(1)), "1");
        let t = s.tick().unwrap();
        assert!(t.snapshot.is_some(), "the first tick takes the snapshot a restore starts from");
        put(&c, "a", Object::new(json!(2)), "2");
        put(&c, "b", Object::new(json!(3)), "3");
        put(&c, "a", Object::new(json!(4)), "4");
        c.backend().head().unwrap()
    };
    assert!(first > 3);
    let c = suite::open(open(&path), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
    let t = s.tick().unwrap();
    assert_eq!(t.snapshot, None, "no snapshot out of cadence: {t:?}");
    let head = c.backend().head().unwrap();
    assert!(head > first, "the open jumped the head");
    assert_eq!(t.shipped_through, head, "{t:?}");
    first = c.backend().head().unwrap();
    put(&c, "c", Object::new(json!(5)), "5");
    s.tick().unwrap();
    let (out, _, _) = restored_copy(dir.path(), &s);
    let restored = verify(&out);
    assert_eq!(restored.head().unwrap(), c.backend().head().unwrap());
    assert!(restored.head().unwrap() > first);
    assert_eq!(live(&restored), live(c.backend()));
}

#[test_id(tadgnh6)]
#[test]
fn shipped_entries_leave_the_outbox_and_unshipped_ones_stay() {
    let dir = TempDir::new("ship-outbox-release");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    let c = suite::open(open(&path), &clock);
    let curl = Curl::default();
    let put_to: Arc<dyn Put> = Arc::new(Uploader {
        dest: Dest::parse("s3://bucket/spira/lc/").unwrap(),
        creds: Some(S3Creds {
            access_key: "AKTEST".into(),
            secret_key: "secret".into(),
            session_token: None,
            region: "us-test-1".into(),
            endpoint: Some("http://endpoint.test".into()),
            part_bytes: 256 << 20,
        }),
        proc: Arc::new(curl.clone()),
        deadline: Duration::from_secs(10),
    });
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), put_to, Arc::new(clock.clone())).unwrap();
    let b = c.backend().clone();
    let r1 = put(&c, "a", Object::new(json!(1)), "1");
    let r2 = put(&c, "a", Object::new(json!(2)), "2");
    assert_eq!(b.outbox_mains().unwrap(), [r1, r2]);
    let t = s.tick().unwrap();
    assert_eq!(t.shipped_through, r2);
    assert!(b.outbox_mains().unwrap().is_empty(), "everything shipped is gone");
    let r3 = put(&c, "a", Object::new(json!(3)), "3");
    let r4 = put(&c, "a", Object::new(json!(4)), "4");
    *curl.refuse_next.lock().unwrap() = true;
    let t = s.tick().unwrap();
    assert!(t.failed.is_some() && t.shipped_through == r2, "{t:?}");
    assert_eq!(b.outbox_mains().unwrap(), [r3, r4], "the unshipped entries remain");
    let t = s.tick().unwrap();
    assert_eq!(t.shipped_through, r4);
    assert!(b.outbox_mains().unwrap().is_empty());
}

#[test_id(trg9a01)]
#[test]
fn an_outbox_entry_above_the_main_head_at_open_is_removed() {
    use state_store::{LogEntry, Rev};
    let dir = TempDir::new("outbox-above-head");
    let path = dir.path().join("live.db");
    let clock = VirtualClock::new(1_000);
    let head = {
        let c = suite::open(open(&path), &clock);
        put(&c, "a", Object::new(json!(1)), "1");
        c.backend().head().unwrap()
    };
    let mut o = state_store_sqlite::outbox::Outbox::open(&state_store_sqlite::outbox_dir(&path), head, 1 << 20).unwrap();
    let ghost = LogEntry { key: "ghost".into(), rev: Rev::new(head + 1, 0), value: Some(b"{}".to_vec()), create_rev: Some(Rev::new(head + 1, 0)), prev: None };
    o.append(head + 1, &[ghost]).unwrap();
    assert_eq!(o.mains().unwrap().last(), Some(&(head + 1)));
    drop(o);
    let b = open(&path);
    assert_eq!(b.outbox_mains().unwrap().last(), Some(&head), "the commit that never happened is gone");
}

#[test_id(tts44ah)]
#[test]
fn a_shipper_whose_outbox_does_not_reach_back_takes_a_snapshot_at_once_and_resumes_after_it() {
    let dir = TempDir::new("ship-outbox-gap");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    let off = dir.path().join("off");
    {
        let c = suite::open(open(&path), &clock);
        let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
        put(&c, "a", Object::new(json!(1)), "1");
        s.tick().unwrap();
        put(&c, "a", Object::new(json!(2)), "2");
        put(&c, "b", Object::new(json!(3)), "3");
    }
    std::fs::remove_dir_all(state_store_sqlite::outbox_dir(&path)).unwrap();
    let c = suite::open(open(&path), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
    let t = s.tick().unwrap();
    let head = c.backend().head().unwrap();
    assert_eq!(t.snapshot, Some(head), "the snapshot taken out of cadence: {t:?}");
    assert_eq!(t.staged, None);
    put(&c, "c", Object::new(json!(4)), "4");
    let t = s.tick().unwrap();
    assert_eq!(t.staged.map(|(from, _)| from), Some(head + 1), "{t:?}");
    let (out, _, _) = restored_copy(dir.path(), &s);
    assert_eq!(live(&verify(&out)), live(c.backend()));
}

// ---- sp-r3yb7x: synchronous=NORMAL and the recorded jump on open --------------------------

fn reopened(path: &Path) -> SqliteBackend {
    open(path)
}

#[test_id(ty5dhio)]
#[test]
fn reopening_a_store_jumps_its_head_records_the_jump_and_commits_in_normal_mode() {
    let dir = TempDir::new("jump-reopen");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    let first = {
        let c = suite::open(open(&path), &clock);
        assert_eq!(c.backend().synchronous().unwrap(), 1, "NORMAL");
        put(&c, "lifecycle/sp-1", Object::new(json!({"state": "READY"})), "j1")
    };
    let b = reopened(&path);
    let jumped = first + state_store_sqlite::JUMP;
    assert_eq!(b.head().unwrap(), jumped);
    assert_eq!(b.jumps(0, jumped).unwrap(), vec![(first + 1, jumped)]);
    let c = suite::open(b, &clock);
    assert_eq!(put(&c, "lifecycle/sp-2", Object::new(json!({"state": "READY"})), "j2"), jumped + 1, "the next commit follows the jump");
    drop(c);
    let quiet = SqliteBackend::open(&path, Options { jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(quiet.head().unwrap(), jumped + 1, "a verifying open leaves the revisions as they are");
}

#[test_id(tp80nvj)]
#[test]
fn a_jump_at_the_head_with_nothing_after_it_is_shipped() {
    let dir = TempDir::new("jump-ship-head");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    {
        let (c, mut s) = shipped_store(dir.path(), &clock);
        s.tick().unwrap();
        drop((c, s));
    }
    let b = reopened(&path);
    let head = b.head().unwrap();
    let mut s = Shipper::open(b, ship_cfg(dir.path(), u64::MAX), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    s.tick().unwrap();
    assert_eq!(s.shipped_through().unwrap(), head, "the jump ships, so the lag clears and compaction can follow");
}

#[test_id(tqfhvfn)]
#[test]
fn a_restore_across_a_jump_reproduces_the_head_the_contents_and_the_jump() {
    let dir = TempDir::new("jump-restore");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    {
        let (c, mut s) = shipped_store(dir.path(), &clock);
        s.tick().unwrap();
        drop((c, s));
    }
    let c = suite::open(reopened(&path), &clock);
    put(&c, "lifecycle/sp-9", Object::new(json!({"state": "LANDED"})), "after-jump");
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    s.tick().unwrap();
    let live = c.backend().clone();
    let head = live.head().unwrap();
    assert_eq!(s.shipped_through().unwrap(), head);
    let (out, _base, _runs) = restored_copy(dir.path(), &s);
    let r = SqliteBackend::open(&out, Options { jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(r.head().unwrap(), head);
    assert_eq!(keys_at(&r, head), keys_at(&live, head));
    assert_eq!(r.jumps(0, head).unwrap(), live.jumps(0, head).unwrap());
}
