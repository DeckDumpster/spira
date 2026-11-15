//! The control store's gc scan on a real SQLite file: each page is read at the head, and a
//! scan a failed page interrupts resumes from its cursor.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::VirtualClock;
use state_store::gc::Controller;
use state_store::{Backend, Config, Contract, Ports, Rev, Write};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::TempDir;

const LIVE: usize = 600;
const DEAD_PER_LIVE: usize = 10;
const PAGE: usize = 64;
const MAX_READ_MS: u64 = 1_000;

fn finalized_value() -> Vec<u8> {
    br#"{"metadata":{"deletionTimestamp":1},"data":1}"#.to_vec()
}

fn live_key(i: usize) -> String {
    format!("lifecycle/{i:05}")
}

/// `LIVE` finalized keys, each followed in key order by `DEAD_PER_LIVE` keys that are put and
/// then deleted, so `versions` carries their tombstones.
fn fill(b: &SqliteBackend) {
    let mut puts = Vec::new();
    let mut dead = Vec::new();
    for i in 0..LIVE {
        puts.push(Write::Put { key: live_key(i), value: finalized_value(), create_rev: Rev::new(1, 0), prev: None });
        for j in 0..DEAD_PER_LIVE {
            dead.push(format!("{}-{j:02}", live_key(i)));
        }
    }
    let first = puts.len();
    puts.extend(dead.iter().map(|k| Write::Put { key: k.clone(), value: b"{}".to_vec(), create_rev: Rev::new(1, 0), prev: None }));
    for chunk in [&puts[..first], &puts[first..]] {
        let head = b.head().unwrap();
        let batch: Vec<Write> = chunk
            .iter()
            .enumerate()
            .map(|(n, w)| match w {
                Write::Put { key, value, .. } => Write::Put { key: key.clone(), value: value.clone(), create_rev: Rev::new(head + 1, n as u32), prev: None },
                other => other.clone(),
            })
            .collect();
        b.commit(head, &batch).unwrap();
    }
    let head = b.head().unwrap();
    let dels: Vec<Write> = dead
        .iter()
        .enumerate()
        .map(|(n, k)| Write::Delete { key: k.clone(), prev: Rev::new(head, (first + n) as u32 - first as u32) })
        .collect();
    b.commit(head, &dels).unwrap();
}

struct Rig {
    _dir: TempDir,
    b: SqliteBackend,
    clock: VirtualClock,
    /// Reads whose probe pushes the clock past `max_read_ms`: `(from, count)` by read number.
    ending: Arc<Mutex<(usize, usize)>>,
    reads: Arc<AtomicUsize>,
    commits_until: Arc<AtomicUsize>,
}

impl Rig {
    /// A commit lands in each of the next `n` reads: the scan's pages, and not the point reads
    /// the collection after it makes, which a commit in every read would never let settle.
    fn commit_in_next(&self, n: usize) {
        self.commits_until.store(self.reads.load(Ordering::SeqCst) + n, Ordering::SeqCst);
    }
}

fn rig() -> Rig {
    let dir = TempDir::new("sqlite-gc-scan");
    let clock = VirtualClock::new(0);
    let slot: Arc<Mutex<Option<SqliteBackend>>> = Arc::new(Mutex::new(None));
    let ending = Arc::new(Mutex::new((usize::MAX, 0)));
    let reads = Arc::new(AtomicUsize::new(0));
    let commits_until = Arc::new(AtomicUsize::new(0));
    let probe = {
        let (slot, clock, ending, reads, until) = (slot.clone(), clock.clone(), ending.clone(), reads.clone(), commits_until.clone());
        Arc::new(move || {
            let Some(b) = slot.lock().unwrap().clone() else { return };
            let n = reads.fetch_add(1, Ordering::SeqCst);
            let (from, count) = *ending.lock().unwrap();
            if n >= from && n < from + count {
                clock.advance(MAX_READ_MS + 1);
            }
            if n >= until.load(Ordering::SeqCst) {
                return;
            }
            let head = b.head().unwrap();
            let put = Write::Put { key: format!("telemetry/{n}"), value: b"{}".to_vec(), create_rev: Rev::new(head + 1, 0), prev: None };
            b.commit(head, &[put]).unwrap();
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let opts = Options { max_read_ms: MAX_READ_MS, readers: 1, clock: Arc::new(clock.clone()), read_probe: Some(probe), jump_on_open: true };
    let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
    fill(&b);
    *slot.lock().unwrap() = Some(b.clone());
    Rig { _dir: dir, b, clock, ending, reads, commits_until }
}

fn open(r: &Rig) -> Contract<SqliteBackend> {
    Contract::open(r.b.clone(), Ports::virtual_clock(&r.clock), Config { min_lock_ttl_ms: 1_000 }).unwrap()
}

fn expected() -> Vec<String> {
    (0..LIVE).map(live_key).collect()
}

#[test]
fn a_scan_over_a_store_that_commits_between_pages_reads_o_live_keys_and_finds_every_finalized_key() {
    let r = rig();
    let c = open(&r);
    let gc = Controller::new(&c).with_page(PAGE);
    r.commit_in_next(12);
    r.b.take_read_steps();
    let tick = gc.pass(c.now_ms());
    let steps = r.b.take_read_steps();
    assert!(tick.failed.is_empty(), "{:?}", tick.failed);
    let scan = tick.scan.clone().unwrap();
    assert!(scan.complete && scan.pages > 1, "{scan:?}");
    assert!(steps <= 20 * LIVE as u64 + 5_000, "{steps} VM steps for {LIVE} live keys over {} versions", LIVE * (1 + 2 * DEAD_PER_LIVE));
    let mut got = tick.finalized.clone();
    got.sort();
    assert_eq!(got, expected());
}

#[test]
fn a_scan_a_failed_page_interrupts_names_its_last_key_and_the_next_pass_resumes_from_it() {
    let r = rig();
    let c = open(&r);
    let gc = Controller::new(&c).with_page(PAGE);
    // Three pages read, then the page and each of its halvings is ended.
    *r.ending.lock().unwrap() = (r.reads.load(Ordering::SeqCst) + 3, 7);
    r.commit_in_next(12);
    let first = gc.pass(c.now_ms());
    let failure = first.failed.iter().find(|f| f.step == "scan").expect("the scan failed");
    let last = live_key(3 * PAGE - 1);
    assert!(failure.error.to_string().contains(&last), "{failure}");
    assert!(failure.error.to_string().contains("3 pages"), "{failure}");
    assert!(first.finalized.is_empty());

    *r.ending.lock().unwrap() = (usize::MAX, 0);
    r.commit_in_next(12);
    r.b.take_read_steps();
    let second = gc.pass(c.now_ms());
    let steps = r.b.take_read_steps();
    assert!(second.failed.is_empty(), "{:?}", second.failed);
    assert!(second.scan.as_ref().unwrap().complete);
    let rest = (LIVE - 3 * PAGE) as u64;
    assert!(steps <= 20 * rest + 2_000, "{steps} VM steps to read the {rest} keys after the cursor");
    let mut got: Vec<String> = first.finalized.into_iter().chain(second.finalized).collect();
    got.sort();
    assert_eq!(got, expected());
}
