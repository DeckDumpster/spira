use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::conformance::Interpose;
use crate::MemBackend;

/// Counts the reads that reach it, so a test can tell memory from the inner backend.
#[derive(Clone, Default)]
struct Counted {
    inner: MemBackend,
    reads: Arc<AtomicU64>,
}

impl Counted {
    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Backend for Counted {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key, at)
    }
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.range(prefix, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> Result<()> {
        self.inner.compact(rev)
    }
}

const SKIP: &[&str] = &["events/", "history/"];

fn put(key: &str, value: &str) -> Write {
    Write::Put {
        key: key.into(),
        value: value.as_bytes().to_vec(),
        create_rev: Rev::new(0, 0),
        prev: None,
    }
}

fn stated(key: &str, state: &str) -> Write {
    put(key, &format!(r#"{{"data":{{"state":"{state}"}}}}"#))
}

fn value(v: Option<Version>) -> Option<String> {
    v.map(|v| String::from_utf8(v.value).unwrap())
}

#[test]
fn a_read_after_a_commit_reflects_it_from_memory() {
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    let before = inner.reads();
    let rev = m.commit(0, &[put("lifecycle/a", "1")]).unwrap();
    assert_eq!(
        value(m.get("lifecycle/a", rev).unwrap()).as_deref(),
        Some("1")
    );
    let rev = m
        .commit(rev, &[put("lifecycle/a", "2"), put("lifecycle/b", "3")])
        .unwrap();
    assert_eq!(
        value(m.get("lifecycle/a", rev).unwrap()).as_deref(),
        Some("2")
    );
    let keys: Vec<String> = m
        .range("lifecycle/", None, 10, rev)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, ["lifecycle/a", "lifecycle/b"]);
    assert_eq!(inner.reads(), before, "no read reached the inner backend");
    assert_eq!(
        m.get("lifecycle/a", rev).unwrap(),
        inner.inner.get("lifecycle/a", rev).unwrap(),
        "the version the inner backend holds"
    );
}

#[test]
fn an_older_revision_inside_the_window_is_read_from_memory_and_one_below_it_from_the_inner_backend()
{
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 2).unwrap();
    let mut rev = 0;
    for n in 1..=5 {
        rev = m.commit(rev, &[put("k", &n.to_string())]).unwrap();
    }
    let before = inner.reads();
    assert_eq!(value(m.get("k", 3).unwrap()).as_deref(), Some("3"));
    assert_eq!(inner.reads(), before);
    assert_eq!(value(m.get("k", 1).unwrap()).as_deref(), Some("1"));
    assert_eq!(
        inner.reads(),
        before + 1,
        "a revision below the floor is the inner backend's to answer"
    );
}

#[test]
fn a_skipped_prefix_is_always_the_inner_backends() {
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    let rev = m
        .commit(0, &[put("events/1", "e"), put("lifecycle/a", "1")])
        .unwrap();
    let before = inner.reads();
    assert_eq!(value(m.get("events/1", rev).unwrap()).as_deref(), Some("e"));
    assert_eq!(
        m.range("", None, 10, rev).unwrap().len(),
        2,
        "a range that covers a skipped prefix reads it"
    );
    assert_eq!(inner.reads(), before + 2);
    assert_eq!(m.size().0, 1, "only the unskipped key is held");
}

#[test]
fn a_state_filtered_range_reads_the_states_held_in_memory() {
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    let rev = m
        .commit(
            0,
            &[
                stated("lifecycle/a", "READY"),
                stated("lifecycle/b", "WORKING"),
                stated("lifecycle/c", "READY"),
            ],
        )
        .unwrap();
    let before = inner.reads();
    let keys: Vec<String> = m
        .range_in_state("lifecycle/", "READY", None, 10, rev)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, ["lifecycle/a", "lifecycle/c"]);
    assert_eq!(inner.reads(), before);
}

#[test]
fn the_copy_loads_what_the_store_held_when_it_opened() {
    let inner = Counted::default();
    inner
        .inner
        .commit(0, &[put("lifecycle/a", "1"), put("events/1", "e")])
        .unwrap();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    let before = inner.reads();
    assert_eq!(
        value(m.get("lifecycle/a", 1).unwrap()).as_deref(),
        Some("1")
    );
    assert_eq!(inner.reads(), before);
    assert_eq!(m.revision(), 1);
}

#[test]
fn a_commit_whose_reply_was_lost_is_read_back_into_the_copy() {
    let m = Modeled::open(Interpose::new(MemBackend::new()), SKIP, 16).unwrap();
    m.inner().lose_next_commit_reply();
    assert!(m.commit(0, &[put("k", "1")]).is_err());
    assert_eq!(
        m.revision(),
        1,
        "the commit landed, and the copy learnt it from the log"
    );
    assert_eq!(value(m.get("k", 1).unwrap()).as_deref(), Some("1"));
}

#[test]
fn a_commit_made_around_the_copy_is_read_from_the_inner_backend_until_the_copy_catches_up() {
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    inner.inner.commit(0, &[put("k", "1")]).unwrap();
    assert_eq!(m.revision(), 0);
    assert_eq!(
        value(m.get("k", 1).unwrap()).as_deref(),
        Some("1"),
        "the head the copy lacks is read through"
    );
    let rev = m.commit(1, &[put("j", "2")]).unwrap();
    assert_eq!(m.revision(), rev);
    let before = inner.reads();
    assert_eq!(value(m.get("k", rev).unwrap()).as_deref(), Some("1"));
    assert_eq!(value(m.get("j", rev).unwrap()).as_deref(), Some("2"));
    assert_eq!(inner.reads(), before);
}

#[test]
fn a_deleted_key_is_forgotten_once_no_read_can_see_it() {
    let m = Modeled::open(MemBackend::new(), SKIP, 1).unwrap();
    let rev = m.commit(0, &[put("k", "1")]).unwrap();
    let rev = m
        .commit(
            rev,
            &[Write::Delete {
                key: "k".into(),
                prev: Rev::new(1, 0),
            }],
        )
        .unwrap();
    assert_eq!(m.get("k", rev).unwrap(), None);
    assert_eq!(value(m.get("k", rev - 1).unwrap()).as_deref(), Some("1"));
    let rev = m.commit(rev, &[put("j", "1")]).unwrap();
    let rev = m.commit(rev, &[put("j", "2")]).unwrap();
    assert_eq!(m.size().0, 1, "only j is held");
    assert_eq!(m.get("k", rev).unwrap(), None);
}

#[test]
fn a_read_below_the_compaction_point_is_too_old() {
    let m = Modeled::open(MemBackend::new(), SKIP, 16).unwrap();
    let mut rev = 0;
    for n in 1..=3 {
        rev = m.commit(rev, &[put("k", &n.to_string())]).unwrap();
    }
    m.compact(2).unwrap();
    assert_eq!(m.get("k", 1), Err(Error::TooOld { compacted: 2 }));
    assert_eq!(value(m.get("k", 2).unwrap()).as_deref(), Some("2"));
}

#[test]
fn a_wait_for_a_revision_returns_once_the_copy_holds_it() {
    let m = Arc::new(Modeled::open(MemBackend::new(), SKIP, 16).unwrap());
    assert!(m.wait_for(0, Duration::ZERO));
    assert!(
        !m.wait_for(1, Duration::from_millis(20)),
        "no commit, no revision 1"
    );
    let writer = Arc::clone(&m);
    let t = std::thread::spawn(move || writer.commit(0, &[put("k", "1")]).unwrap());
    // clock-safe: a cap on the wait for the commit on the other thread
    assert!(
        m.wait_for(1, Duration::from_secs(5)),
        "the commit on the other thread reached the copy"
    );
    assert_eq!(t.join().unwrap(), 1);
}

#[test]
fn a_tracked_request_is_stamped_with_its_commit_or_the_oldest_revision_it_read() {
    let m = Modeled::open(MemBackend::new(), SKIP, 16).unwrap();
    track(false);
    assert_eq!(
        stamp(),
        None,
        "a request that touched no store carries no revision"
    );
    let rev = m.commit(0, &[put("k", "1")]).unwrap();
    let rev = m.commit(rev, &[put("k", "2")]).unwrap();
    track(false);
    m.get("k", rev).unwrap();
    m.get("k", rev - 1).unwrap();
    assert_eq!(
        stamp(),
        Some(rev - 1),
        "the answer is at least as fresh as its oldest read"
    );
    track(false);
    m.get("k", rev).unwrap();
    let wrote = m.commit(rev, &[put("k", "3")]).unwrap();
    assert_eq!(stamp(), Some(wrote), "a write is at or below the stamp");
    assert_eq!(stamp(), None, "the stamp ends the recording");
}

#[test]
fn a_read_through_request_reads_the_inner_backend() {
    let inner = Counted::default();
    let m = Modeled::open(inner.clone(), SKIP, 16).unwrap();
    let rev = m.commit(0, &[put("k", "1")]).unwrap();
    let before = inner.reads();
    track(true);
    assert_eq!(value(m.get("k", rev).unwrap()).as_deref(), Some("1"));
    m.range("", None, 10, rev).unwrap();
    assert_eq!(stamp(), Some(rev));
    assert_eq!(inner.reads(), before + 2);
}

#[test]
fn a_read_does_not_wait_for_a_commit_in_flight() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    /// A commit that holds the backend until it is let go, as SQLite's writer holds a commit
    /// through its fsync; the backend's reads wait for it.
    struct Slow {
        inner: MemBackend,
        gate: std::sync::Mutex<()>,
        stall: AtomicBool,
        entered: std::sync::Mutex<mpsc::Sender<()>>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
        in_flight: AtomicBool,
    }
    impl Backend for Slow {
        fn head(&self) -> Result<u64> {
            self.inner.head()
        }
        fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
            let _g = self.gate.lock().unwrap();
            self.inner.get(key, at)
        }
        fn range(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> Result<Vec<(String, Version)>> {
            let _g = self.gate.lock().unwrap();
            self.inner.range(prefix, after, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
            let _g = self.gate.lock().unwrap();
            if self.stall.swap(false, Ordering::SeqCst) {
                self.in_flight.store(true, Ordering::SeqCst);
                self.entered.lock().unwrap().send(()).unwrap();
                // clock-safe: a cap on the wait for the test to let the commit go
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
            }
            let out = self.inner.commit(expect, batch);
            self.in_flight.store(false, Ordering::SeqCst);
            out
        }
        fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
            self.inner.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> Result<()> {
            self.inner.compact(rev)
        }
    }
    let (entered, on_entry) = mpsc::channel();
    let (let_go, release) = mpsc::channel();
    let slow = Slow {
        inner: MemBackend::new(),
        gate: std::sync::Mutex::new(()),
        stall: AtomicBool::new(false),
        entered: std::sync::Mutex::new(entered),
        release: std::sync::Mutex::new(release),
        in_flight: AtomicBool::new(false),
    };
    let m = Arc::new(Modeled::open(slow, SKIP, 16).unwrap());
    let rev = m.commit(0, &[put("k", "1")]).unwrap();
    m.inner().stall.store(true, Ordering::SeqCst);
    let writer = Arc::clone(&m);
    let t = std::thread::spawn(move || writer.commit(rev, &[put("k", "2")]).unwrap());
    on_entry.recv().unwrap();
    assert_eq!(value(m.get("k", rev).unwrap()).as_deref(), Some("1"));
    assert!(
        m.inner().in_flight.load(Ordering::SeqCst),
        "the read was answered only once the commit let the backend go"
    );
    let_go.send(()).unwrap();
    assert_eq!(t.join().unwrap(), rev + 1);
    assert_eq!(value(m.get("k", rev + 1).unwrap()).as_deref(), Some("2"));
}
