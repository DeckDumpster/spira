//! The control store's off-box copy end to end on real files: the shipper against a test S3
//! endpoint through the real curl, the compaction floor, a restore from the uploaded objects
//! and the drill's fold-and-compare.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::ports::{SysProc, VirtualClock};
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
        snapshot_pages: 64,
    }
}

fn file_put(dest: &Path) -> Arc<dyn Put> {
    Arc::new(Uploader {
        dest: Dest::File(dest.to_path_buf()),
        creds: None,
        proc: Arc::new(SysProc),
        deadline: Duration::from_secs(10),
    })
}

/// One request the endpoint saw: method, path, body and lower-cased headers.
type Seen = (String, String, Vec<u8>, Vec<(String, String)>);

/// A test S3 endpoint: records every request, answers 403 to every method but PUT, and 500 to
/// the next PUT while `refuse_next` is set.
#[derive(Clone, Default)]
struct Endpoint {
    seen: Arc<Mutex<Vec<Seen>>>,
    refuse_next: Arc<Mutex<bool>>,
}

impl Endpoint {
    fn serve(&self) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let me = self.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                me.answer(s);
            }
        });
        format!("http://{addr}")
    }

    fn answer(&self, s: TcpStream) {
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let mut parts = line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            r.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let header = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone());
        let mut w = s;
        if header("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
            w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
        }
        let len: usize = header("content-length")
            .and_then(|l| l.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        let code = if method != "PUT" {
            403
        } else if std::mem::take(&mut *self.refuse_next.lock().unwrap()) {
            500
        } else {
            200
        };
        self.seen
            .lock()
            .unwrap()
            .push((method, path, body, headers));
        write!(
            w,
            "HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    }
}

#[test]
fn the_shipper_puts_every_revision_to_a_put_only_endpoint_and_retries_a_refusal_under_the_same_name(
) {
    let dir = TempDir::new("ship-s3");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let ep = Endpoint::default();
    let url = ep.serve();
    let uploader = Uploader {
        dest: Dest::parse("s3://bucket/spira/lc/").unwrap(),
        creds: Some(S3Creds {
            access_key: "AKTEST".into(),
            secret_key: "secret\"x".into(),
            session_token: Some("tok".into()),
            region: "us-test-1".into(),
            endpoint: Some(url),
        }),
        proc: Arc::new(SysProc),
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
    *ep.refuse_next.lock().unwrap() = true;
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

    let seen = ep.seen.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(m, ..)| m == "PUT"),
        "only PUTs: {:?}",
        seen.iter().map(|r| &r.0).collect::<Vec<_>>()
    );
    let paths: Vec<&str> = seen.iter().map(|(_, p, ..)| p.as_str()).collect();
    let retried = paths[2];
    assert_eq!(
        paths[3], retried,
        "the retry is the refused object, under its own name"
    );
    assert!(retried.starts_with("/bucket/spira/lc/revlog/"), "{retried}");
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with("/bucket/spira/lc/snapshot/")),
        "{paths:?}"
    );
    for (_, _, body, headers) in &seen {
        let h = |k: &str| {
            headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
        };
        let sum: String = Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(h("x-amz-content-sha256"), Some(sum.as_str()));
        assert!(
            h("authorization")
                .is_some_and(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=AKTEST/")),
            "{headers:?}"
        );
        assert_eq!(h("x-amz-security-token"), Some("tok"));
    }
    let mut mains = BTreeSet::new();
    for (_, p, body, _) in &seen[..] {
        if p.contains("/revlog/") {
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

#[test]
fn compaction_past_the_last_shipped_revision_stops_at_it() {
    let dir = TempDir::new("ship-floor");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let blocked = dir.path().join("not-a-dir");
    std::fs::write(&blocked, "").unwrap();
    let mut s = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), u64::MAX),
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    put(&c, "k", Object::new(json!(1)), "1");
    let shipped = s.tick().unwrap().shipped_through;
    assert_eq!(shipped, c.backend().head().unwrap());

    let mut stuck = Shipper::open(
        c.backend().clone(),
        ship_cfg(dir.path(), u64::MAX),
        file_put(&blocked),
        Arc::new(clock.clone()),
    )
    .unwrap();
    put(&c, "k", Object::new(json!(2)), "2");
    put(&c, "k", Object::new(json!(3)), "3");
    let t = stuck.tick().unwrap();
    assert!(t.failed.is_some() && t.shipped_through == shipped, "{t:?}");
    clock.advance(9_000);
    assert_eq!(stuck.tick().unwrap().oldest_unshipped_ms, Some(9_000));

    let b = c.backend();
    b.compact(b.head().unwrap()).unwrap();
    assert_eq!(
        b.compacted().unwrap(),
        shipped,
        "compaction passed the last shipped revision"
    );
    assert!(
        b.log(shipped, 10)
            .unwrap()
            .iter()
            .any(|e| e.rev.main == b.head().unwrap()),
        "the unshipped revisions are still in the log"
    );
}

/// Ships a snapshot at `s` and two runs after it to `file://`; returns the live contract.
fn shipped_store(dir: &Path, clock: &VirtualClock) -> (Contract<SqliteBackend>, Shipper) {
    let c = suite::open(open(&dir.join("live.db")), clock);
    put(
        &c,
        "beads/sp-1",
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
        "beads/sp-1",
        Object::new(json!({"state": "WORKING"})),
        "r1",
    );
    put(
        &c,
        "beads/sp-2",
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
        "beads/sp-2",
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
    let restored_db = open(&dir.path().join("scratch/control.db"));
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

/// Overwrites the newest version of `key` in the store file with `value`, keeping its revision.
fn tamper(path: &Path, key: &str, value: &[u8]) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let n = conn
        .prepare("UPDATE versions SET value = ?1 WHERE key = ?2 AND main = (SELECT max(main) FROM versions WHERE key = ?2)")
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

#[test]
fn a_flipped_byte_in_a_restored_value_fails_the_drill_naming_the_key() {
    let dir = TempDir::new("drill-flip");
    let clock = VirtualClock::new(1_000);
    let (c, s) = shipped_store(dir.path(), &clock);
    for key in ["beads/sp-1", "intents/claim/sp-1"] {
        let sub = dir.path().join(key.replace('/', "-"));
        std::fs::create_dir_all(&sub).unwrap();
        let (out, base, runs) = restored_copy(&sub, &s);
        tamper(&out, key, &flipped(c.backend(), key));
        let rep =
            drill::fold_compare(&open(&base), &runs, &open(&out), Exclusions::CONTRACT).unwrap();
        assert_eq!(
            rep.mismatches,
            [Mismatch::Value { key: key.into() }],
            "{}",
            drill::render(&rep)
        );
    }
}

#[test]
fn a_live_key_no_audited_write_produced_fails_the_drill_under_any_other_prefix() {
    let dir = TempDir::new("drill-unfolded");
    let clock = VirtualClock::new(1_000);
    let (c, mut s) = shipped_store(dir.path(), &clock);
    let b = c.backend();
    let head = b.head().unwrap();
    let raw = Write::Put {
        key: "beads/sp-ghost".into(),
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
            key: "beads/sp-ghost".into()
        }],
        "{}",
        drill::render(&rep)
    );
}

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

#[test]
fn a_snapshot_stepped_while_commits_run_opens_with_the_live_keys_at_its_revision() {
    let dir = TempDir::new("ship-stepped");
    let clock = VirtualClock::new(1_000);
    let c = suite::open(open(&dir.path().join("live.db")), &clock);
    let big = "x".repeat(2_000);
    for i in 0..60 {
        put(
            &c,
            &format!("b/{i:03}"),
            Object::new(json!(big)),
            &format!("seed{i}"),
        );
    }
    let mut cfg = ship_cfg(dir.path(), u64::MAX);
    cfg.snapshot_pages = 4;
    let mut s = Shipper::open(
        c.backend().clone(),
        cfg,
        file_put(&dir.path().join("off")),
        Arc::new(clock.clone()),
    )
    .unwrap();
    let mut ticks = 0;
    let rev = loop {
        let t = s.tick().unwrap();
        if let Some(rev) = t.snapshot {
            break rev;
        }
        ticks += 1;
        put(
            &c,
            &format!("b/{ticks:03}"),
            Object::new(json!(ticks)),
            &format!("during{ticks}"),
        );
    };
    assert!(ticks > 1, "the snapshot finished in one tick");
    put(&c, "after", Object::new(json!(1)), "after");
    s.tick().unwrap();
    let snap = s.sent(SNAPSHOT).join(revlog::snapshot_name(rev));
    let copy = dir.path().join("copy.db");
    std::fs::copy(&snap, &copy).unwrap();
    let copy = open(&copy);
    assert_eq!(
        copy.head().unwrap(),
        rev,
        "the snapshot is tagged with the revision it holds"
    );
    assert_eq!(keys_at(&copy, rev), keys_at(c.backend(), rev));
}
