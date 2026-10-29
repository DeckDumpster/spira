//! An idle runtime asks the store in proportion to the events it handles, not to how often it wakes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::event::*;
use runtime::journal::{Journal, Offset, Poison, ScheduleRow};
use runtime::ports::{Clock, VirtualClock};
use runtime::{Engine, Executor, MemJournal, Outcome, Result};
use spira_runtime::watchers::{inbox_keeper::InboxKeeper, round_duty::RoundDuty};

struct Counting {
    inner: Arc<MemJournal>,
    calls: Mutex<BTreeMap<&'static str, u64>>,
}

impl Counting {
    fn hit(&self, verb: &'static str) {
        *self.calls.lock().unwrap().entry(verb).or_default() += 1;
    }
    fn total(&self) -> u64 {
        self.calls.lock().unwrap().values().sum()
    }
}

impl Journal for Counting {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.hit("append");
        self.inner.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.hit("read_from");
        self.inner.read_from(from, limit)
    }
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.hit("applied");
        self.inner.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.hit("mark_applied");
        self.inner.mark_applied(handler, id)
    }
    fn cursor(&self) -> Result<Offset> {
        self.hit("cursor");
        self.inner.cursor()
    }
    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.hit("set_cursor");
        self.inner.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.hit("schedule");
        self.inner.schedule(row)
    }
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.hit("pending_schedules");
        self.inner.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.hit("mark_fired");
        self.inner.mark_fired(row_id)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.hit("poison");
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.hit("poisoned");
        self.inner.poisoned(stream)
    }
}

const PER_EVENT: u64 = 5;

#[test]
fn ten_idle_minutes_at_production_intervals_cost_a_few_store_calls_per_event() {
    let mem = MemJournal::new();
    let counting = Arc::new(Counting { inner: mem.clone(), calls: Mutex::new(BTreeMap::new()) });
    let clock = VirtualClock::new(1_800_000_000_000);
    let start = clock.now_ms();
    let regs = vec![
        runtime::heartbeat::Heartbeat { interval_ms: 60_000, ping_deadline_ms: 5_000 }.registration(),
        RoundDuty { interval_ms: 15_000, min: 1, max_open: 1, remind_secs: 600, idle_cut_secs: 900 }.registration(),
        InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: 3600 }.registration(),
    ];
    let mut e = Engine::open(counting.clone(), Arc::new(clock.clone()), regs, 0).unwrap();
    for (id, stream, kind) in [runtime::heartbeat::Heartbeat::boot(), RoundDuty::boot(), InboxKeeper::boot()] {
        e.submit(id, stream, kind).unwrap();
    }
    let mut wakes = 0;
    while clock.now_ms() < start + 600_000 {
        e.run_until_idle().unwrap();
        for p in e.pending_effects() {
            e.complete(&p.key, Outcome::Succeeded(String::new())).unwrap();
        }
        e.run_until_idle().unwrap();
        let Some(next) = e.next_timer_at() else { break };
        clock.advance_to(next.max(clock.now_ms() + 1));
        wakes += 1;
    }
    let events = mem.read_from(0, 100_000).unwrap().len() as u64;
    let calls = counting.calls.lock().unwrap().clone();
    let total = counting.total();
    assert!(events > 40 && wakes > 40, "the simulation did not run: {events} events, {wakes} wakes");
    assert!(total <= PER_EVENT * events, "{total} store calls for {events} events over {wakes} wakes ({}/event): {calls:?}", total / events);
}
