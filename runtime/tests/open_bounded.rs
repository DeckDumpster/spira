//! `Engine::open` reads what dispatch needs, not the journal: the rows it pulls from the store
//! are the same however long the journal is.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::journal::{Journal, MemJournal, Offset, Poison, ScheduleRow};
use runtime::ports::VirtualClock;
use runtime::*;

struct Counting {
    inner: Arc<MemJournal>,
    rows: AtomicUsize,
}

impl Counting {
    fn new(inner: Arc<MemJournal>) -> Arc<Self> {
        Arc::new(Counting { inner, rows: AtomicUsize::new(0) })
    }
    fn took(&self, n: usize) {
        self.rows.fetch_add(n, Ordering::SeqCst);
    }
}

impl Journal for Counting {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.inner.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        let r = self.inner.read_from(from, limit)?;
        self.took(r.len());
        Ok(r)
    }
    fn read_stream(&self, stream: &StreamKey, below: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        let r = self.inner.read_stream(stream, below, limit)?;
        self.took(r.len());
        Ok(r)
    }
    fn end(&self) -> Result<Offset> {
        self.inner.end()
    }
    fn unresolved_effects(&self) -> Result<Vec<(Offset, Event)>> {
        let r = self.inner.unresolved_effects()?;
        self.took(r.len());
        Ok(r)
    }
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.inner.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.inner.mark_applied(handler, id)
    }
    fn cursor(&self) -> Result<Offset> {
        self.inner.cursor()
    }
    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.inner.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.inner.schedule(row)
    }
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.inner.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.inner.mark_fired(row_id)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
}

struct SeesPast(Arc<Mutex<Vec<usize>>>);

impl Handler for SeesPast {
    fn interests(&self) -> &[EventKindTag] {
        &["note"]
    }
    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        self.0.lock().unwrap().push(view.events(&ev.stream).len());
        vec![Command::Emit(EventKind::new("seen", "x"))]
    }
}

fn note(i: usize, stream: &str) -> Event {
    Event { id: EventId(format!("n{i}")), stream: StreamKey(stream.into()), kind: EventKind::new("note", ""), at: 1, cause: None, correlation: CorrelationId("c".into()) }
}

fn seeded(rows: usize) -> Arc<MemJournal> {
    let j = MemJournal::new();
    for i in 0..rows {
        j.append(&note(i, if i % 2 == 0 { "even" } else { "odd" })).unwrap();
    }
    j.set_cursor(rows as u64).unwrap();
    j
}

#[test]
fn open_reads_the_backlog_past_the_cursor_and_not_the_history_before_it() {
    let j = seeded(10_000);
    j.append(&note(10_000, "even")).unwrap();
    let counting = Counting::new(j);
    Engine::open(counting.clone(), Arc::new(VirtualClock::new(0)), vec![], 0).unwrap();
    assert_eq!(counting.rows.load(Ordering::SeqCst), 0, "the journal end is one call, not a page of the backlog");
}

#[test]
fn the_first_dispatch_of_a_long_stream_loads_a_window_not_its_past() {
    let rows = 4 * WINDOW;
    let j = seeded(rows);
    j.append(&note(rows, "even")).unwrap();
    let counting = Counting::new(j);
    let seen = Arc::new(Mutex::new(vec![]));
    let regs = vec![Registration::new("past", SeesPast(seen.clone()))];
    let mut e = Engine::open(counting.clone(), Arc::new(VirtualClock::new(0)), regs, 0).unwrap();
    e.run_until_idle().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![WINDOW + 1]);
    assert!(counting.rows.load(Ordering::SeqCst) <= WINDOW + 4, "read {} rows", counting.rows.load(Ordering::SeqCst));
}

#[test]
fn trimming_a_busy_stream_never_lowers_what_the_heartbeat_counts() {
    use runtime::heartbeat::{Heartbeat, HEARTBEAT};
    let hb = Heartbeat { interval_ms: 1000, ping_deadline_ms: 500 };
    let boot = Event { id: EventId("b".into()), stream: StreamKey("hb".into()), kind: EventKind::new(HEARTBEAT, &(2 * WINDOW - 1).to_string()), at: 1, cause: None, correlation: CorrelationId("c".into()) };
    let mut p = Projection::default();
    let mut last = 0;
    for i in 0..5 * WINDOW {
        p.push(&note(i, "busy"));
        assert!(p.total() > last);
        last = p.total();
    }
    p.push(&boot);
    let cmds = hb.handle(&Ctx::new(1, 1), &p, &boot);
    assert!(cmds.iter().any(|c| matches!(c, Command::Probe(_))), "foreign events past the trim still advance the heartbeat");
}
