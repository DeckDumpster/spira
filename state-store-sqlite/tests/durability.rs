//! The control store's off-box copy end to end on real files: the shipper's signed PUTs to an S3
//! endpoint, a recording HTTP listener on loopback (no process is started), the outbox, a
//! restore from the uploaded objects and the drill's fold-and-compare.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::ports::{RealFs, VirtualClock};
use serde_json::json;
use sha2::{Digest, Sha256};
use state_store::conformance as suite;
use state_store::gc::Controller;
use state_store::{Backend, Contract, Object, Request, Txn, Write};
use state_store_sqlite::durability::drill::{self, Exclusions, Mismatch};
use state_store_sqlite::durability::put::{Dest, Put, S3Creds, Uploader};
use state_store_sqlite::durability::restore;
use state_store_sqlite::durability::revlog::{self, REVLOG, SNAPSHOT};
use state_store_sqlite::durability::ship::{self, ShipConfig, Shipper};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::TempDir;

fn open(path: &Path) -> SqliteBackend {
    SqliteBackend::open(path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap()
}

/// A copy being checked (a snapshot, a restore, a backup): opened without the jump, so its
/// revisions read exactly as written (sp-r3yb7x).
fn verify(path: &Path) -> SqliteBackend {
    SqliteBackend::open(path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, day_receipts: false, ..Options::default() }).unwrap()
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

/// A tick as the served shipper runs it: the scheduled snapshot's step, then the tick.
fn tick(s: &mut Shipper) -> ship::Tick {
    let snap = s.snapshotter().step().unwrap();
    let mut t = s.tick().unwrap();
    t.snapshot = snap.or(t.snapshot);
    t
}

fn ship_cfg(dir: &Path, snapshot_every_ms: u64) -> ShipConfig {
    ShipConfig {
        dir: dir.join("lc-ship"),
        max_mains: 1_000,
        snapshot_every_ms,
        stage_every_ms: 0,
    }
}

fn file_put(dest: &Path) -> Arc<dyn Put> {
    Arc::new(Uploader {
        dest: Dest::File(dest.to_path_buf()),
        creds: None,
        deadline: Duration::from_secs(10),
    })
}

/// One request the listener received: method, target, lower-cased headers and body.
#[derive(Clone)]
struct Seen {
    method: String,
    target: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Vec<u8>,
}

/// A recording HTTP listener on loopback standing in for S3: answers each request with the next
/// status of its script (200 once the script is spent) and records it. No process is started.
#[derive(Clone, Default)]
struct S3Listener {
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<std::collections::VecDeque<u16>>>,
    port: u16,
}

impl S3Listener {
    fn start() -> S3Listener {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let me = S3Listener { port: listener.local_addr().unwrap().port(), ..S3Listener::default() };
        let shared = me.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let shared = shared.clone();
                std::thread::spawn(move || shared.serve(conn.unwrap()));
            }
        });
        me
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn refuse_next(&self, status: u16) {
        self.script.lock().unwrap().push_back(status);
    }

    fn serve(&self, conn: std::net::TcpStream) {
        use std::io::{BufRead, Read, Write};
        let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
        let mut w = conn;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.split_whitespace();
            let (method, target) = (parts.next().unwrap().to_string(), parts.next().unwrap().to_string());
            let mut headers = std::collections::BTreeMap::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let Some((k, v)) = h.trim_end().split_once(':') else { break };
                headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
            }
            let mut body = vec![0u8; headers.get("content-length").map_or(0, |n| n.parse().unwrap())];
            r.read_exact(&mut body).unwrap();
            self.seen.lock().unwrap().push(Seen { method, target, headers, body });
            let status = self.script.lock().unwrap().pop_front().unwrap_or(200);
            let text = if status < 300 { String::new() } else { "<Error><Code>Refused</Code></Error>".to_string() };
            let reply = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nETag: \"e\"\r\n\r\n{text}", text.len());
            if w.write_all(reply.as_bytes()).is_err() {
                return;
            }
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn s3_creds(endpoint: String, secret: &str, token: Option<&str>) -> S3Creds {
    S3Creds { access_key: "AKTEST".into(), secret_key: secret.into(), session_token: token.map(String::from), region: "us-test-1".into(), endpoint: Some(endpoint), part_bytes: 256 << 20 }
}

fn assert_signed_like_s3_wants(req: &Seen) {
    let auth = &req.headers["authorization"];
    assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKTEST/"), "{auth}");
    let day = &req.headers["x-amz-date"][..8];
    assert!(auth.contains(&format!("Credential=AKTEST/{day}/us-test-1/s3/aws4_request")), "{auth}");
    let sum: String = Sha256::digest(&req.body).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(req.headers["x-amz-content-sha256"], sum);
    assert_eq!(req.headers["x-amz-server-side-encryption"], "AES256");
    assert!(auth.contains("x-amz-server-side-encryption"), "signed: {auth}");
}

#[test_id(t5txm05)]
#[test]
fn the_shipper_puts_every_revision_to_a_put_only_endpoint_and_retries_a_refusal_under_the_same_name(
) {
    let dir = TempDir::new("ship-s3");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let s3 = S3Listener::start();
    let uploader = Uploader {
        dest: Dest::parse("s3://bucket/spira/lc/").unwrap(),
        creds: Some(s3_creds(s3.endpoint(), "secret\"x", Some("tok"))),
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
    let first = tick(&mut s);
    assert_eq!(first.uploaded.len(), 2, "{first:?}");
    put(&c, "b", Object::new(json!(2)), "2");
    put(&c, "a", Object::new(json!(3)), "3");
    s3.refuse_next(500);
    let refused = tick(&mut s);
    assert!(
        refused
            .failed
            .as_deref()
            .is_some_and(|f| f.contains("PUT spira/lc/revlog/")),
        "{refused:?}"
    );
    assert!(refused.uploaded.is_empty());
    clock.advance(4_000);
    let held = tick(&mut s);
    assert_eq!(held.uploaded.len(), 1, "{held:?}");

    let runs = s3.requests();
    assert!(runs.iter().all(|r| r.method == "PUT"), "only PUTs");
    let urls: Vec<&str> = runs.iter().map(|r| r.target.as_str()).collect();
    let retried = urls[2];
    assert_eq!(urls[3], retried, "the retry is the refused object, under its own name");
    assert!(retried.starts_with("/bucket/spira/lc/revlog/"), "{retried}"); // not a path: an HTTP request target
    assert!(urls.iter().any(|u| u.starts_with("/bucket/spira/lc/snapshot/")), "{urls:?}"); // not a path: an HTTP request target
    for r in &runs {
        assert_signed_like_s3_wants(r);
        assert_eq!(r.headers["x-amz-security-token"], "tok");
        assert!(r.headers["authorization"].contains("x-amz-security-token"), "signed");
        assert!(!r.headers["authorization"].contains("secret"));
    }
    let mut mains = BTreeSet::new();
    for r in &runs {
        if r.target.contains("/revlog/") && r.headers.contains_key("x-amz-content-sha256") {
            mains.extend(revlog::decode(&r.body).unwrap().entries.iter().map(|e| e.rev.main));
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
    assert!(tick(&mut s).snapshot.is_some());
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
    assert!(tick(&mut s).failed.is_none());
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
    let t = tick(&mut s);
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

/// A clock that moves `0` ms per reading: a host so slow that every read looks long.
struct Stalled(u64, std::sync::atomic::AtomicU64);

impl runtime::ports::Clock for Stalled {
    fn now_ms(&self) -> u64 {
        self.1.fetch_add(self.0, std::sync::atomic::Ordering::SeqCst)
    }
}

fn stalled_offline(by: u64) -> impl Fn() -> Options {
    let clock: Arc<dyn runtime::ports::Clock> = Arc::new(Stalled(by, Default::default()));
    move || Options { clock: clock.clone(), ..Options::offline() }
}

#[test_id(tf0s9xp)]
#[test]
fn the_offline_read_limit_leaves_the_live_one_alone() {
    assert_eq!(Options::default().max_read_ms, 100);
    assert_eq!(Options::offline().max_read_ms, state_store_sqlite::OFFLINE_MAX_READ_MS);
    const { assert!(state_store_sqlite::OFFLINE_MAX_READ_MS < i32::MAX as u64) };
}

#[test_id(tk7p3dq)]
#[test]
fn a_drill_on_a_host_that_stalls_every_reading_still_gives_its_verdict() {
    let dir = TempDir::new("drill-stalled");
    let clock = VirtualClock::new(1_000);
    let (_c, s) = shipped_store(dir.path(), &clock);
    let (_, plain) = drill::drill(s.dir(), &dir.path().join("plain"), Exclusions::CONTRACT).unwrap();
    let (_, rep) = drill::drill_with(
        s.dir(),
        &dir.path().join("stalled"),
        Exclusions::CONTRACT,
        &stalled_offline(150),
    )
    .unwrap();
    assert!(rep.passed(), "{}", drill::render(&rep));
    assert_eq!(rep.compared, plain.compared);

    let (rev, snap) = drill::newest_snapshot(s.dir()).unwrap();
    let mut bytes = std::fs::read(&snap).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 1;
    std::fs::write(&snap, bytes).unwrap();
    let err = drill::drill_with(
        s.dir(),
        &dir.path().join("stalled2"),
        Exclusions::CONTRACT,
        &stalled_offline(150),
    )
    .unwrap_err();
    assert!(
        err.starts_with(&format!("checksum: snapshot {} changed", revlog::snapshot_name(rev))),
        "{err}"
    );
}

#[test_id(tm3cq8n)]
#[test]
fn a_stalled_drill_still_reports_a_tampered_copy_and_a_missing_run() {
    let dir = TempDir::new("drill-stalled-bad");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    let key = "intents/claim/sp-1";
    let flipped = flipped(c.backend(), key);
    let hook = stalled_offline(150);
    let (_, rep) = drill::drill_with(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT, &hook).unwrap();
    assert!(rep.passed());
    let (_, base, runs) = restored_copy(dir.path(), &s);
    let out = dir.path().join("scratch").join("control.db");
    tamper(&out, key, &flipped);
    let rep = drill::fold_compare(&verify(&base), &runs, &verify(&out), Exclusions::CONTRACT).unwrap();
    assert_eq!(rep.mismatches, [Mismatch::Value { key: key.into() }]);

    let mut names: Vec<_> = std::fs::read_dir(s.sent(REVLOG)).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    std::fs::remove_file(s.sent(REVLOG).join(&names[0])).unwrap();
    let err = drill::drill_with(s.dir(), &dir.path().join("gap"), Exclusions::CONTRACT, &hook).unwrap_err();
    assert!(err.starts_with("restore: gap: revisions "), "{err}");
    assert!(err.contains("; steps: checksum "), "{err}");
}

#[test_id(tr5v2hw)]
#[test]
fn a_point_read_that_cannot_finish_fails_the_drill_naming_the_fold_and_the_read() {
    let dir = TempDir::new("drill-point-read");
    let clock = VirtualClock::new(1_000);
    let (_c, s) = shipped_store(dir.path(), &clock);
    let slow = VirtualClock::new(0);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook = {
        let (slow, calls) = (slow.clone(), calls.clone());
        move || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let probe_clock = slow.clone();
            Options {
                clock: Arc::new(slow.clone()),
                read_probe: (n == 3).then(|| -> Arc<dyn Fn() + Send + Sync> {
                    Arc::new(move || probe_clock.advance(state_store_sqlite::OFFLINE_MAX_READ_MS + 1))
                }),
                ..Options::offline()
            }
        }
    };
    let err = drill::drill_with(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT, &hook).unwrap_err();
    assert!(err.starts_with("fold: read of events/"), "{err}");
    assert!(err.contains("steps: checksum ") && err.contains(", fold "), "{err}");
}

#[test_id(tz8n1ub)]
#[test]
fn a_scan_page_that_cannot_finish_at_one_key_names_the_page() {
    let dir = TempDir::new("drill-page");
    let clock = VirtualClock::new(1_000);
    let (_c, s) = shipped_store(dir.path(), &clock);
    let slow = VirtualClock::new(0);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook = {
        let (slow, calls) = (slow.clone(), calls.clone());
        move || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let probe_clock = slow.clone();
            Options {
                clock: Arc::new(slow.clone()),
                read_probe: (n == 2).then(|| -> Arc<dyn Fn() + Send + Sync> {
                    Arc::new(move || probe_clock.advance(state_store_sqlite::OFFLINE_MAX_READ_MS + 1))
                }),
                ..Options::offline()
            }
        }
    };
    let err = drill::drill_with(s.dir(), &dir.path().join("scratch"), Exclusions::CONTRACT, &hook).unwrap_err();
    assert!(err.starts_with("snapshot scan: page of 1 after"), "{err}");
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
    assert!(tick(&mut s).failed.is_none());
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
    let rev = tick(&mut s)
        .snapshot
        .expect("the first tick finishes the snapshot");
    put(&c, "after", Object::new(json!(1)), "after");
    tick(&mut s);
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

#[test_id(tz1h3ex)]
#[test]
fn a_queued_snapshot_of_five_hundred_requests_commits_in_a_bounded_number_of_transactions() {
    let dir = TempDir::new("ship-batched");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    for i in 0..500 {
        put(&c, &format!("b/{i:03}"), Object::new(json!(i)), &format!("seed{i}"));
    }
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), u64::MAX),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    let rev = tick(&mut s).snapshot.expect("the first tick finishes the snapshot");
    let snap = s.sent(SNAPSHOT).join(revlog::snapshot_name(rev));
    let header = std::fs::read(&snap).unwrap();
    assert_eq!((header[18], header[19]), (1, 1), "the copy stays in rollback mode");
    let counter = u32::from_be_bytes(header[24..28].try_into().unwrap());
    assert!(counter <= 20, "{counter} transactions for 500 requests");
    let copy = dir.path().join("copy.db");
    std::fs::copy(&snap, &copy).unwrap();
    assert_eq!(keys_at(&verify(&copy), rev), keys_at(c.backend(), rev));
}

#[test_id(tpxjqfj)]
#[test]
fn sha256_of_a_file_longer_than_the_read_buffer_equals_the_digest_of_its_bytes() {
    let dir = TempDir::new("ship-sha");
    let bytes: Vec<u8> = (0..3_500_000u32).map(|i| (i % 251) as u8).collect();
    let path = dir.path().join("big");
    std::fs::write(&path, &bytes).unwrap();
    let want: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(ship::sha256_file(&RealFs, &path).unwrap(), want);
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
        snaps.push(tick(&mut s).snapshot.unwrap());
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
            "checksum: snapshot {} changed since it was staged",
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
    assert!(tick(&mut s).snapshot.is_some());
    clock.advance(1_000);
    let idle = tick(&mut s);
    assert_eq!((idle.snapshot, idle.uploaded.len()), (None, 0), "{idle:?}");
    assert_eq!(
        std::fs::read_dir(dir.path().join("off").join(SNAPSHOT))
            .unwrap()
            .count(),
        1
    );
    put(&c, "k", Object::new(json!(2)), "2");
    assert!(tick(&mut s).snapshot.is_some());

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
        let t = tick(&mut s);
        assert!(t.snapshot.is_some(), "the first tick takes the snapshot a restore starts from");
        put(&c, "a", Object::new(json!(2)), "2");
        put(&c, "b", Object::new(json!(3)), "3");
        put(&c, "a", Object::new(json!(4)), "4");
        c.backend().head().unwrap()
    };
    assert!(first > 3);
    let c = suite::open(open(&path), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
    let t = tick(&mut s);
    assert_eq!(t.snapshot, None, "no snapshot out of cadence: {t:?}");
    let head = c.backend().head().unwrap();
    assert!(head > first, "the open jumped the head");
    assert_eq!(t.shipped_through, head, "{t:?}");
    first = c.backend().head().unwrap();
    put(&c, "c", Object::new(json!(5)), "5");
    tick(&mut s);
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
    let s3 = S3Listener::start();
    let put_to: Arc<dyn Put> = Arc::new(Uploader {
        dest: Dest::parse("s3://bucket/spira/lc/").unwrap(),
        creds: Some(s3_creds(s3.endpoint(), "secret", Some("tok"))),
        deadline: Duration::from_secs(10),
    });
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), put_to, Arc::new(clock.clone())).unwrap();
    let b = c.backend().clone();
    let r1 = put(&c, "a", Object::new(json!(1)), "1");
    let r2 = put(&c, "a", Object::new(json!(2)), "2");
    assert_eq!(b.outbox_mains().unwrap(), [r1, r2]);
    let t = tick(&mut s);
    assert_eq!(t.shipped_through, r2);
    assert!(b.outbox_mains().unwrap().is_empty(), "everything shipped is gone");
    let r3 = put(&c, "a", Object::new(json!(3)), "3");
    let r4 = put(&c, "a", Object::new(json!(4)), "4");
    s3.refuse_next(500);
    let t = tick(&mut s);
    assert!(t.failed.is_some() && t.shipped_through == r2, "{t:?}");
    assert_eq!(b.outbox_mains().unwrap(), [r3, r4], "the unshipped entries remain");
    let t = tick(&mut s);
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
        let mut s = Shipper::open(c.backend().clone(), paced_cfg(dir.path(), 15_000), file_put(&off), Arc::new(clock.clone())).unwrap();
        put(&c, "a", Object::new(json!(1)), "1");
        tick(&mut s);
        put(&c, "a", Object::new(json!(2)), "2");
        put(&c, "b", Object::new(json!(3)), "3");
    }
    std::fs::remove_dir_all(state_store_sqlite::outbox_dir(&path)).unwrap();
    let c = suite::open(open(&path), &clock);
    let mut s = Shipper::open(c.backend().clone(), paced_cfg(dir.path(), 15_000), file_put(&off), Arc::new(clock.clone())).unwrap();
    let t = s.tick().unwrap();
    let head = c.backend().head().unwrap();
    assert_eq!(t.snapshot, Some(head), "the snapshot taken out of cadence: {t:?}");
    assert_eq!(t.staged, None);
    assert_eq!(std::fs::read_to_string(s.dir().join("staged")).unwrap(), head.to_string(), "the forced snapshot records staged");
    put(&c, "c", Object::new(json!(4)), "4");
    let t = tick(&mut s);
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
        tick(&mut s);
        drop((c, s));
    }
    let b = reopened(&path);
    let head = b.head().unwrap();
    let mut s = Shipper::open(b, ship_cfg(dir.path(), u64::MAX), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
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
        tick(&mut s);
        drop((c, s));
    }
    let c = suite::open(reopened(&path), &clock);
    put(&c, "lifecycle/sp-9", Object::new(json!({"state": "LANDED"})), "after-jump");
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    let live = c.backend().clone();
    let head = live.head().unwrap();
    assert_eq!(s.shipped_through().unwrap(), head);
    let (out, _base, _runs) = restored_copy(dir.path(), &s);
    let r = SqliteBackend::open(&out, Options { jump_on_open: false, ..Options::default() }).unwrap();
    assert_eq!(r.head().unwrap(), head);
    assert_eq!(keys_at(&r, head), keys_at(&live, head));
    assert_eq!(r.jumps(0, head).unwrap(), live.jumps(0, head).unwrap());
}

// ---- sp-x73bzk: the scheduled snapshot runs beside the tick ---------------------------------

/// Holds every snapshot copy until released; `entered` receives one message per copy begun.
fn hold_copies(snap: &ship::Snapshotter) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    snap.on_copy(move || {
        entered_tx.send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv();
    });
    (entered, release)
}

const BOUND: Duration = Duration::from_secs(60);

#[test_id(t3m9aon)]
#[test]
fn a_tick_stages_runs_and_reports_lag_while_a_scheduled_snapshot_is_held_in_its_copy() {
    let dir = TempDir::new("ship-beside-copy");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    put(&c, "a", Object::new(json!(1)), "1");
    let mut s = Shipper::open_staging(c.backend().clone(), ship_cfg(dir.path(), 0), Arc::new(clock.clone())).unwrap();
    let snap = s.snapshotter();
    let (entered, release) = hold_copies(&snap);
    let stepper = std::thread::spawn({
        let snap = snap.clone();
        move || snap.step().unwrap()
    });
    entered.recv_timeout(BOUND).expect("the copy began");

    put(&c, "b", Object::new(json!(2)), "2");
    let (done_tx, done) = std::sync::mpsc::channel();
    let ticker = std::thread::spawn(move || {
        let t = s.tick().unwrap();
        done_tx.send(()).unwrap();
        (s, t)
    });
    done.recv_timeout(BOUND).expect("the tick returned while the copy was held");
    release.send(()).unwrap();
    let snap_rev = stepper.join().unwrap().expect("the snapshot landed");
    let (s, t) = ticker.join().unwrap();

    assert!(t.staged.is_some(), "{t:?}");
    assert!(t.oldest_unshipped_ms.is_some(), "{t:?}");
    assert!(!ship::names(&RealFs, &s.dir().join("queue").join(REVLOG)).unwrap().is_empty(), "the run waits in queue/");
    let drained = ship::drain(&RealFs, s.dir(), file_put(&dir.path().join("off")).as_ref()).unwrap();
    assert!(drained.failed.is_none(), "{drained:?}");
    let (out, _, _) = restored_copy(dir.path(), &s);
    assert!(snap_rev <= c.backend().head().unwrap());
    assert_eq!(live(&verify(&out)), live(c.backend()));
}

#[test_id(twikg97)]
#[test]
fn a_due_snapshot_is_taken_once_per_cadence_and_one_at_an_unchanged_revision_is_dropped() {
    let dir = TempDir::new("ship-cadence");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    put(&c, "a", Object::new(json!(1)), "1");
    let s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), 10_000), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    let snap = s.snapshotter();
    let first = snap.step().unwrap();
    assert!(first.is_some());
    assert_eq!(snap.step().unwrap(), None, "not due again within the cadence");
    clock.advance(10_000);
    assert_eq!(snap.step().unwrap(), None, "due, but the revision is unchanged: dropped");
    put(&c, "b", Object::new(json!(2)), "2");
    clock.advance(10_000);
    assert!(snap.step().unwrap().is_some_and(|r| Some(r) > first));
}

#[test_id(t0ebl94)]
#[test]
fn a_forced_snapshot_during_a_scheduled_one_waits_and_then_stages_runs_after_its_revision() {
    let dir = TempDir::new("ship-forced-waits");
    let clock = VirtualClock::new(1_000);
    let path = dir.path().join("live.db");
    let off = dir.path().join("off");
    {
        let c = suite::open(open(&path), &clock);
        let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&off), Arc::new(clock.clone())).unwrap();
        put(&c, "a", Object::new(json!(1)), "1");
        tick(&mut s);
        put(&c, "a", Object::new(json!(2)), "2");
    }
    std::fs::remove_dir_all(state_store_sqlite::outbox_dir(&path)).unwrap();
    let c = suite::open(open(&path), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), 0), file_put(&off), Arc::new(clock.clone())).unwrap();
    let snap = s.snapshotter();
    let (entered, release) = hold_copies(&snap);
    let stepper = std::thread::spawn({
        let snap = snap.clone();
        move || snap.step().unwrap()
    });
    entered.recv_timeout(BOUND).expect("the scheduled copy began");
    let (done_tx, done) = std::sync::mpsc::channel();
    let ticker = std::thread::spawn(move || {
        let t = s.tick().unwrap();
        done_tx.send(()).unwrap();
        (s, t)
    });
    assert!(entered.recv_timeout(Duration::from_millis(300)).is_err(), "the forced copy does not begin while the scheduled one is held");
    release.send(()).unwrap();
    let rev = stepper.join().unwrap().expect("the scheduled snapshot landed");
    entered.recv_timeout(BOUND).expect("the forced copy began after the scheduled one finished");
    release.send(()).unwrap();
    done.recv_timeout(BOUND).expect("the tick finished after the release");
    let (mut s, t) = ticker.join().unwrap();
    assert_eq!(t.snapshot, None, "the forced snapshot was at the scheduled one's revision: dropped");
    put(&c, "c", Object::new(json!(3)), "3");
    let t = s.tick().unwrap();
    assert_eq!(t.staged.map(|(from, _)| from), Some(rev + 1), "{t:?}");
}

fn sent_revlog_made_a_file(s: &Shipper) {
    let sent = s.sent(REVLOG);
    std::fs::create_dir_all(sent.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&sent);
    std::fs::write(&sent, "not a directory").unwrap();
}

#[test_id(t8n3gkp)]
#[test]
fn an_idle_tick_does_not_list_sent_revlog() {
    let dir = TempDir::new("ship-idle-no-relist");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    put(&c, "a", Object::new(json!(1)), "1");
    let first = tick(&mut s);
    sent_revlog_made_a_file(&s);
    for _ in 0..3 {
        let idle = tick(&mut s);
        assert_eq!(idle.shipped_through, first.shipped_through);
        assert_eq!(idle.staged, None);
    }
}

#[test_id(tf6cdtf)]
#[test]
fn a_staging_tick_after_a_commit_does_not_list_sent_revlog() {
    let dir = TempDir::new("ship-staging-no-relist");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open_staging(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), Arc::new(clock.clone())).unwrap();
    put(&c, "a", Object::new(json!(1)), "1");
    let before = tick(&mut s);
    sent_revlog_made_a_file(&s);
    let r = put(&c, "a", Object::new(json!(2)), "2");
    let after = tick(&mut s);
    assert_eq!(after.staged.map(|(_, to)| to), Some(r));
    assert_eq!(after.shipped_through, before.shipped_through);
}

#[test_id(twjgcp6)]
#[test]
fn a_run_in_sent_whose_shipped_mark_was_lost_is_counted_shipped_by_an_open_and_a_reopened_shipper() {
    let dir = TempDir::new("ship-lost-mark");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let cfg = || ship_cfg(dir.path(), u64::MAX);
    let mut s = Shipper::open_staging(c.backend().clone(), cfg(), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    let r = put(&c, "a", Object::new(json!(1)), "1");
    let t = tick(&mut s);
    assert_eq!(t.staged.map(|(_, to)| to), Some(r));
    assert!(t.shipped_through < r);
    let queued = s.dir().join("queue").join(REVLOG);
    let sent = s.sent(REVLOG);
    std::fs::create_dir_all(&sent).unwrap();
    for n in ship::names(&RealFs, &queued).unwrap() {
        std::fs::rename(queued.join(&n), sent.join(&n)).unwrap();
    }
    let t = tick(&mut s);
    assert_eq!(t.shipped_through, r, "{t:?}");
    let reopened = Shipper::open_staging(c.backend().clone(), cfg(), Arc::new(clock.clone())).unwrap();
    assert_eq!(reopened.shipped_through().unwrap(), r);
}

#[test_id(tm33sam)]
#[test]
fn prune_sweeps_sent_revlog_only_when_its_point_moves() {
    let dir = TempDir::new("ship-sweep-once");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open(c.backend().clone(), ship_cfg(dir.path(), 0), file_put(&dir.path().join("off")), Arc::new(clock.clone())).unwrap();
    for i in 0..3 {
        put(&c, &format!("k{i}"), Object::new(json!(i)), &format!("p{i}"));
        tick(&mut s);
    }
    tick(&mut s);
    sent_revlog_made_a_file(&s);
    let idle = s.tick().unwrap();
    assert_eq!(idle.staged, None);
}

#[test_id(tnq605c)]
#[test]
fn a_relist_of_sent_revlog_happens_once_per_staged_value() {
    let dir = TempDir::new("ship-relist-once");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut lost = Shipper::open_staging(c.backend().clone(), ship_cfg(dir.path(), u64::MAX), Arc::new(clock.clone())).unwrap();
    put(&c, "late", Object::new(json!(9)), "late");
    let t = lost.tick().unwrap();
    assert!(t.staged.is_some());
    let queued = lost.dir().join("queue").join(REVLOG);
    for n in ship::names(&RealFs, &queued).unwrap() {
        std::fs::remove_file(queued.join(n)).unwrap();
    }
    sent_revlog_made_a_file(&lost);
    assert!(lost.tick().is_err(), "the first tick at a staged value lists sent/revlog once");
    std::fs::remove_file(lost.sent(REVLOG)).unwrap();
    std::fs::create_dir_all(lost.sent(REVLOG)).unwrap();
    lost.tick().unwrap();
    sent_revlog_made_a_file(&lost);
    lost.tick().unwrap();
}

fn paced_cfg(dir: &Path, stage_every_ms: u64) -> ShipConfig {
    ShipConfig { stage_every_ms, ..ship_cfg(dir, u64::MAX) }
}

#[test_id(toeszp2)]
#[test]
fn thirty_commits_two_seconds_apart_stage_at_most_six_runs_and_restore_whole() {
    let dir = TempDir::new("ship-paced");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open_staging(c.backend().clone(), paced_cfg(dir.path(), 15_000), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    for i in 0..30 {
        put(&c, &format!("k{i}"), Object::new(json!(i)), &format!("p{i}"));
        tick(&mut s);
        clock.advance(2_000);
    }
    let runs = ship::names(&RealFs, &s.dir().join("queue").join(REVLOG)).unwrap();
    assert!(runs.len() <= 6, "{runs:?}");
    clock.advance(15_000);
    assert!(tick(&mut s).staged.is_some(), "the tail the pace held back");
    let drained = ship::drain(&RealFs, s.dir(), file_put(&dir.path().join("off")).as_ref()).unwrap();
    assert!(drained.failed.is_none(), "{drained:?}");
    tick(&mut s);
    let (out, _, _) = restored_copy(dir.path(), &s);
    assert_eq!(live(&verify(&out)), live(c.backend()));
}

#[test_id(tcopd80)]
#[test]
fn staging_a_run_leaves_the_staged_mark_alone() {
    let dir = TempDir::new("ship-staged-untouched");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let mut s = Shipper::open_staging(c.backend().clone(), paced_cfg(dir.path(), 0), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    let mark = s.dir().join("staged");
    ship::write_durably(&RealFs, &mark, b"0").unwrap();
    let stat = || (std::fs::read(&mark).ok(), std::fs::metadata(&mark).and_then(|m| m.modified()).ok());
    let before = stat();
    put(&c, "a", Object::new(json!(1)), "1");
    assert!(tick(&mut s).staged.is_some());
    assert_eq!(before, stat());
}

#[test_id(tkj4f4l)]
#[test]
fn a_reopen_stages_from_the_revision_after_a_run_in_queue_and_after_one_only_in_sent() {
    let dir = TempDir::new("ship-reopen-floor");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let cfg = || paced_cfg(dir.path(), 0);
    let mut s = Shipper::open_staging(c.backend().clone(), cfg(), Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    let r = put(&c, "a", Object::new(json!(1)), "1");
    assert_eq!(tick(&mut s).staged.map(|(_, to)| to), Some(r));
    let mut s = Shipper::open_staging(c.backend().clone(), cfg(), Arc::new(clock.clone())).unwrap();
    let r2 = put(&c, "b", Object::new(json!(2)), "2");
    assert_eq!(tick(&mut s).staged, Some((r + 1, r2)), "a run in queue/");
    let queued = s.dir().join("queue").join(REVLOG);
    std::fs::create_dir_all(s.sent(REVLOG)).unwrap();
    for n in ship::names(&RealFs, &queued).unwrap() {
        std::fs::rename(queued.join(&n), s.sent(REVLOG).join(&n)).unwrap();
    }
    let mut s = Shipper::open_staging(c.backend().clone(), cfg(), Arc::new(clock.clone())).unwrap();
    let r3 = put(&c, "c", Object::new(json!(3)), "3");
    assert_eq!(tick(&mut s).staged, Some((r2 + 1, r3)), "a run only in sent/");
}

#[test_id(tpz83rl)]
#[test]
fn a_paced_shipper_stages_a_full_run_s_successor_without_waiting() {
    let dir = TempDir::new("ship-paced-full");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let cfg = ShipConfig { max_mains: 2, ..paced_cfg(dir.path(), 60_000) };
    let mut s = Shipper::open_staging(c.backend().clone(), cfg, Arc::new(clock.clone())).unwrap();
    tick(&mut s);
    for i in 0..5 {
        put(&c, &format!("k{i}"), Object::new(json!(i)), &format!("p{i}"));
    }
    assert!(tick(&mut s).staged.is_some());
    assert!(tick(&mut s).staged.is_some(), "the first run was full");
    assert!(tick(&mut s).staged.is_some(), "the second run was full");
    assert_eq!(tick(&mut s).staged, None, "the last run was short, and the pace holds");
}

struct Accepts;

impl Put for Accepts {
    fn put(&self, _file: &Path, _name: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Every commit in `revs` is in a staged or sent run, or still in the outbox.
fn unaccounted(fs: &runtime::ports::SimFs, ship_dir: &Path, c: &Contract<SqliteBackend>, revs: &[u64]) -> Vec<u64> {
    let mut runs = Vec::new();
    for dir in [ship_dir.join("queue").join(REVLOG), ship_dir.join("sent").join(REVLOG)] {
        runs.extend(ship::names(fs, &dir).unwrap().iter().filter_map(|n| revlog::parse_run_name(n)));
    }
    revs.iter()
        .copied()
        .filter(|r| !runs.iter().any(|(from, to)| (from..=to).contains(&r)))
        .filter(|r| !c.backend().outbox_log(r - 1, 1).is_ok_and(|e| e.first().is_some_and(|e| e.rev.main == *r)))
        .collect()
}

#[test_id(tbszon5)]
#[test]
fn a_shipper_on_the_sim_disk_crashed_at_each_call_in_turn_then_reopened_loses_no_commit() {
    let ship_path = Path::new("lc-ship");
    let mut crashed_runs = 0;
    for k in 1.. {
        let dir = TempDir::new("ship-crash-sim");
        let clock = VirtualClock::new(1_000);
        let c = suite::open(open(&dir.path().join("live.db")), &clock);
        let fs = runtime::ports::SimFs::new(7, Default::default(), clock.clone());
        let cfg = ShipConfig { dir: ship_path.to_path_buf(), max_mains: 1_000, snapshot_every_ms: u64::MAX, stage_every_ms: 0 };
        fs.crash_after(k);
        let mut revs = Vec::new();
        let finished = (|| -> Result<(), String> {
            let mut s = Shipper::open_on(fs.clone(), c.backend().clone(), cfg.clone(), Arc::new(Accepts), Arc::new(clock.clone()))?;
            for i in 0..3 {
                revs.push(put(&c, &format!("k{i}"), Object::new(json!(i)), &format!("p{i}")));
                s.tick()?;
            }
            Ok(())
        })()
        .is_ok();
        fs.revive();
        let mut s = Shipper::open_on(fs.clone(), c.backend().clone(), cfg, Arc::new(Accepts), Arc::new(clock.clone())).unwrap();
        s.tick().unwrap();
        s.tick().unwrap();
        assert_eq!(unaccounted(&fs, ship_path, &c, &revs), Vec::<u64>::new(), "crash at call {k}: commits in neither a run nor the outbox");
        if finished {
            break;
        }
        crashed_runs += 1;
    }
    assert!(crashed_runs > 10, "the workload made more calls than that: {crashed_runs}");
}
