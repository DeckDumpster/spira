use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// bd: answers each read with the read's argv and how many reads it has answered; while `held`,
/// a read waits for it to be let go.
#[derive(Clone, Default)]
struct Bd {
    reads: Arc<AtomicU64>,
    held: Arc<(Mutex<bool>, std::sync::Condvar)>,
    fail: Arc<Mutex<bool>>,
}

impl Bd {
    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Content for Bd {
    fn read(&self, args: &[String]) -> Result<String, String> {
        let (m, cv) = &*self.held;
        let held = m.lock().unwrap();
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        cv.notify_all();
        drop(cv.wait_while(held, |held| *held).unwrap());
        if *self.fail.lock().unwrap() {
            return Err("bd: no answer".into());
        }
        Ok(format!("{} #{n}", args.join(" ")))
    }
    fn write(&self, _: &[&str]) -> Result<String, String> {
        Ok(String::new())
    }
    fn write_stdin(&self, _: &[String], _: Option<&str>) -> Result<String, String> {
        Ok(String::new())
    }
    fn delete(&self, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn ask_label(&self) -> Result<String, String> {
        Ok("ask".into())
    }
    fn registry(&self) -> spira_config::repos::Registry {
        LiveContent.registry()
    }
}

fn door(bd: &Bd, watermark: &Arc<AtomicU64>) -> Cached<Bd> {
    let w = Arc::clone(watermark);
    Cached::new(bd.clone(), Box::new(move || Some(w.load(Ordering::SeqCst))))
}

fn argv(s: &str) -> Vec<String> {
    s.split(' ').map(str::to_string).collect()
}

#[test]
fn a_read_is_answered_from_memory_while_the_planning_watermark_stands() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(7)));
    let c = door(&bd, &w);
    let list = argv("list --all --json");
    assert_eq!(c.read(&list).unwrap(), "list --all --json #1");
    assert_eq!(c.read(&list).unwrap(), "list --all --json #1", "the same answer, from memory");
    assert_eq!(bd.reads(), 1);
    assert_eq!(c.read(&argv("show sp-a --json")).unwrap(), "show sp-a --json #2", "another argv is its own read");
}

#[test]
fn a_read_after_a_stamped_write_reflects_it() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(7)));
    let c = door(&bd, &w);
    let list = argv("list --json");
    c.read(&list).unwrap();
    w.store(8, Ordering::SeqCst);
    assert_eq!(c.read(&list).unwrap(), "list --json #2", "a moved watermark reads bd again");
    assert_eq!(c.read(&list).unwrap(), "list --json #2");
}

#[test]
fn a_write_through_the_door_forgets_every_answer_even_when_its_stamp_failed() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(7)));
    let c = door(&bd, &w);
    let list = argv("list --json");
    c.read(&list).unwrap();
    c.write_stdin(&argv("note sp-a --stdin"), Some("x")).unwrap();
    assert_eq!(c.read(&list).unwrap(), "list --json #2", "the watermark never moved, and the write is read");
    c.write(&["label", "add", "sp-a", "x"]).unwrap();
    assert_eq!(c.read(&list).unwrap(), "list --json #3");
}

#[test]
fn concurrent_reads_of_one_argv_share_one_bd_call() {
    let bd = Bd::default();
    *bd.held.0.lock().unwrap() = true;
    let w = Arc::new(AtomicU64::new(1));
    let c = Arc::new(door(&bd, &w));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let c = Arc::clone(&c);
            std::thread::spawn(move || c.read(&argv("list --all --json")).unwrap())
        })
        .collect();
    // Whichever reader reached bd first holds it; the rest find its read in flight or its answer.
    let (m, cv) = &*bd.held;
    drop(cv.wait_while(m.lock().unwrap(), |_| bd.reads() == 0).unwrap());
    *m.lock().unwrap() = false;
    cv.notify_all();
    let answers: Vec<String> = readers.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(answers.iter().all(|a| a == "list --all --json #1"), "{answers:?}");
    assert_eq!(bd.reads(), 1);
}

#[test]
fn with_no_watermark_every_read_goes_to_bd() {
    let bd = Bd::default();
    let c = Cached::new(bd.clone(), Box::new(|| None));
    c.read(&argv("list")).unwrap();
    c.read(&argv("list")).unwrap();
    assert_eq!(bd.reads(), 2);
}

#[test]
fn a_failed_read_is_not_kept() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(1)));
    let c = door(&bd, &w);
    *bd.fail.lock().unwrap() = true;
    assert!(c.read(&argv("list")).is_err());
    *bd.fail.lock().unwrap() = false;
    assert_eq!(c.read(&argv("list")).unwrap(), "list #2");
}

#[test]
fn an_answer_past_its_age_bound_is_read_again() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(1)));
    let mut c = door(&bd, &w);
    c.fresh_max = Duration::ZERO;
    c.read(&argv("list")).unwrap();
    assert_eq!(c.read(&argv("list")).unwrap(), "list #2");
}

#[test]
fn a_moved_watermark_refreshes_the_hot_reads_ahead_of_their_next_caller() {
    let (bd, w) = (Bd::default(), Arc::new(AtomicU64::new(1)));
    let c = door(&bd, &w);
    let (hot, cold) = (argv("list --all --json"), argv("show sp-a --json"));
    c.read(&hot).unwrap();
    c.read(&hot).unwrap();
    c.read(&cold).unwrap();
    w.store(2, Ordering::SeqCst);
    c.refresh_hot();
    assert_eq!(bd.reads(), 3, "only the read asked for twice was refreshed");
    assert_eq!(c.read(&hot).unwrap(), "list --all --json #3", "its next caller finds it fresh");
    assert_eq!(bd.reads(), 3);
}

/// A hit on a production-size answer (`content list --all --json` over 13k beads, about 8 MB):
/// a measurement, not an assertion. `cargo test --release -p spira-lc -- --ignored --nocapture
/// content_hit`.
#[test]
#[ignore]
fn content_hit_latency_on_a_production_size_answer() {
    #[derive(Clone)]
    struct Big(String);
    impl Content for Big {
        fn read(&self, _: &[String]) -> Result<String, String> {
            Ok(self.0.clone())
        }
        fn write(&self, _: &[&str]) -> Result<String, String> {
            Ok(String::new())
        }
        fn write_stdin(&self, _: &[String], _: Option<&str>) -> Result<String, String> {
            Ok(String::new())
        }
        fn delete(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn ask_label(&self) -> Result<String, String> {
            Ok("ask".into())
        }
        fn registry(&self) -> spira_config::repos::Registry {
            LiveContent.registry()
        }
    }
    let row = r#"{"id":"sp-b00000","title":"a bead of about six hundred bytes of title, labels and dependencies","status":"closed","priority":2,"issue_type":"task","labels":["repo:spira","plan","spira"],"dependencies":[{"issue_id":"sp-b00000","depends_on_id":"sp-a","type":"parent-child"}],"created_at":"2026-11-01T00:00:00Z","updated_at":"2026-11-01T00:00:00Z","description":"the body of the bead, which a list carries in part, padded to look like one of production's beads rather than a counter"}"#;
    let answer = format!("[{}]", vec![row; 13_000].join(","));
    let w = Arc::new(AtomicU64::new(1));
    let watermark = Arc::clone(&w);
    let c = Cached::new(Big(answer.clone()), Box::new(move || Some(watermark.load(Ordering::SeqCst))));
    let list = argv("list --all --limit 0 --json");
    c.read(&list).unwrap();
    let mut t: Vec<Duration> = (0..101)
        .map(|_| {
            let at = std::time::Instant::now();
            assert_eq!(c.read(&list).unwrap().len(), answer.len());
            at.elapsed()
        })
        .collect();
    t.sort();
    println!("content hit on a {} KiB answer: p50 {:?} p90 {:?}", answer.len() / 1024, t[50], t[90]);
}
