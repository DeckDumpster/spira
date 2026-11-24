//! The ops pane's loop over a scripted terminal and clock, on a planted document: an idle pane
//! starts no thread and writes no file, its ages move with the clock alone, and a replaced
//! document shows on the next repaint. One test, so it owns its process and its thread set.

#[macro_use]
extern crate spira_test_id;
use cockpit_ops::lcpane::{self, Clock, Terminal};
use cockpit_ops::lcview::{document_path, Meta, Row, Snapshot};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const START: i64 = 1_800_000_000;
const SECS: u64 = 15;
const IDLE_REPAINTS: usize = 40;

struct Time(Arc<AtomicI64>);

impl Clock for Time {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

enum Step {
    Idle(usize),
    Do(Box<dyn FnOnce()>),
    Key(char),
}

struct Scripted {
    clock: Arc<AtomicI64>,
    steps: VecDeque<Step>,
    drawn: Vec<(i64, String)>,
}

impl Terminal for Scripted {
    fn size(&mut self) -> (usize, usize) {
        (120, 40)
    }

    fn poll(&mut self, wait: Duration) -> io::Result<Option<Event>> {
        self.clock.fetch_add(wait.as_secs() as i64, Ordering::SeqCst);
        match self.steps.pop_front() {
            None => Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)))),
            Some(Step::Idle(n)) => {
                if n > 1 {
                    self.steps.push_front(Step::Idle(n - 1));
                }
                Ok(None)
            }
            Some(Step::Do(f)) => {
                f();
                Ok(None)
            }
            Some(Step::Key(c)) => Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))),
        }
    }

    fn draw(&mut self, lines: &[String]) -> io::Result<()> {
        self.drawn.push((self.clock.load(Ordering::SeqCst), lines.join("\n")));
        Ok(())
    }
}

fn document(id: &str, since: i64) -> Snapshot {
    let row = Row { id: id.into(), state: "WORKING".into(), holder: Some("1@1".into()), since, updated_at: since, repo: "spira".into(), ..Default::default() };
    Snapshot { now: START, good_at: START, rows: vec![row], meta: [(id.to_string(), Meta { title: format!("title of {id}"), priority: Some(1), ..Default::default() })].into(), world: "plane work: RUNNING".into(), ..Default::default() }
}

fn plant(path: &Path, doc: &Snapshot) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(doc).unwrap()).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    std::fs::File::open(path).unwrap().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(START as u64)).unwrap();
}

fn threads() -> BTreeSet<String> {
    std::fs::read_dir("/proc/self/task").unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
}

fn files(root: &Path) -> BTreeMap<String, (u64, std::time::SystemTime, u64)> {
    use std::os::unix::fs::MetadataExt;
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.insert(e.path().display().to_string(), (m.ino(), m.modified().unwrap(), m.len()));
            }
        }
    }
    out
}

#[test_id(tk7l4z7)]
#[test]
fn an_idle_pane_starts_no_thread_and_writes_nothing_while_its_ages_follow_the_clock_and_a_new_document_shows() {
    let dir = testkit::TempDir::new("pane-idle");
    let (run, runtime) = (dir.join("run"), dir.join("runtime"));
    std::fs::create_dir_all(&run).unwrap();
    let view_dir = spira_config::cockpit_view_dir(&runtime);
    let doc = document_path(&view_dir);
    plant(&doc, &document("sp-old", START - 120));
    let clock = Arc::new(AtomicI64::new(START));
    let script = |steps: Vec<Step>| Scripted { clock: clock.clone(), steps: steps.into(), drawn: vec![] };
    let pane = |term: &mut Scripted| lcpane::run(term, &Time(clock.clone()), &view_dir, Some(run.join("lcview/ui.json")), SECS).unwrap();

    let (threads_before, run_before, runtime_before) = (threads(), files(&run), files(&runtime));
    let mut idle = script(vec![Step::Idle(SECS as usize * IDLE_REPAINTS), Step::Key('q')]);
    pane(&mut idle);
    assert_eq!(threads(), threads_before, "the loop started a thread");
    assert_eq!(files(&run), run_before, "a file in the run directory changed");
    assert_eq!(files(&runtime), runtime_before, "a file in the runtime directory changed in inode, mtime or size");

    let repaints: Vec<&(i64, String)> = idle.drawn.iter().filter(|(_, f)| f.contains("sp-old")).collect();
    assert!(repaints.len() >= IDLE_REPAINTS, "one repaint every {SECS}s: {}", repaints.len());
    let (first, last) = (repaints[0], repaints[repaints.len() - 1]);
    assert!(first.1.contains("2m") && !first.1.contains("12m"), "{}", first.1);
    assert!(last.0 - first.0 >= 600 && last.1.contains("12m"), "an age moves with the clock alone, drawn at {}: {}", last.0, last.1);

    let replaced = {
        let doc = doc.clone();
        Box::new(move || plant(&doc, &document("sp-new", START - 30))) as Box<dyn FnOnce()>
    };
    let mut swapped = script(vec![Step::Idle(20), Step::Do(replaced), Step::Idle(2), Step::Key('q')]);
    pane(&mut swapped);
    let shown_at = swapped.drawn.iter().position(|(_, f)| f.contains("sp-new")).expect("a replaced document shows");
    assert!(swapped.drawn[shown_at - 1].1.contains("sp-old") && swapped.drawn.len() - shown_at <= 2, "on the next repaint, not a refresh interval later: {}", swapped.drawn.len() - shown_at);
}
