//! The scheduler: due rows in time order, ties broken by a seeded draw taken at insertion, so
//! one seed and one insertion order give one firing order. Its clock is whatever `Clock` the
//! caller passes: wall in production, virtual in the simulator and the tests.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use crate::journal::ScheduleRow;
use crate::ports::{Clock, Rng, SeededRng};

pub struct Scheduler {
    rng: SeededRng,
    heap: BinaryHeap<Reverse<(u64, u64, String)>>,
    rows: std::collections::BTreeMap<String, ScheduleRow>,
    seen: BTreeSet<String>,
}

impl Scheduler {
    pub fn new(seed: u64) -> Self {
        Scheduler { rng: SeededRng::new(seed), heap: BinaryHeap::new(), rows: Default::default(), seen: Default::default() }
    }

    /// A row id already inserted is ignored: a replay re-issues the same `Schedule`.
    pub fn insert(&mut self, row: ScheduleRow) {
        let tie = self.rng.next_u64();
        self.insert_with_tie(row, tie);
    }

    /// For a caller that owns the one PRNG every decision draws from, so a tie is a draw in
    /// that stream. A duplicate id draws nothing from the scheduler's own.
    pub fn insert_with_tie(&mut self, row: ScheduleRow, tie: u64) {
        if !self.seen.insert(row.id.clone()) {
            return;
        }
        self.heap.push(Reverse((row.at, tie, row.id.clone())));
        self.rows.insert(row.id.clone(), row);
    }

    pub fn next_at(&self) -> Option<u64> {
        self.heap.peek().map(|Reverse((at, _, _))| *at)
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// The earliest row whatever the clock says: a virtual-time loop jumps to it.
    pub fn pop_next(&mut self) -> Option<ScheduleRow> {
        let Reverse((_, _, id)) = self.heap.pop()?;
        self.rows.remove(&id)
    }

    pub fn pop_due(&mut self, clock: &dyn Clock) -> Vec<ScheduleRow> {
        let now = clock.now_ms();
        let mut due = Vec::new();
        while let Some(Reverse((at, _, _))) = self.heap.peek() {
            if *at > now {
                break;
            }
            if let Some(Reverse((_, _, id))) = self.heap.pop() {
                if let Some(r) = self.rows.remove(&id) {
                    due.push(r);
                }
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CorrelationId, EventKind, StreamKey};
    use crate::ports::VirtualClock;

    fn row(id: &str, at: u64) -> ScheduleRow {
        ScheduleRow {
            id: id.into(),
            at,
            stream: StreamKey("s".into()),
            kind: EventKind::new("t", ""),
            cause: None,
            correlation: CorrelationId("c".into()),
        }
    }

    fn order(seed: u64) -> Vec<String> {
        let mut s = Scheduler::new(seed);
        for i in 0..8 {
            s.insert(row(&format!("r{i}"), 100));
        }
        s.insert(row("early", 50));
        s.pop_due(&VirtualClock::new(100)).into_iter().map(|r| r.id).collect()
    }

    #[test]
    fn earlier_first_and_ties_follow_the_seed() {
        let a = order(1);
        assert_eq!(a[0], "early");
        assert_eq!(a, order(1));
        assert!((2..40).any(|s| order(s) != a), "ties must depend on the seed");
    }

    #[test]
    fn pop_next_ignores_the_clock_and_an_explicit_tie_orders_equal_times() {
        let mut s = Scheduler::new(1);
        s.insert_with_tie(row("late", 90), 0);
        s.insert_with_tie(row("b", 10), 7);
        s.insert_with_tie(row("a", 10), 3);
        let ids: Vec<_> = std::iter::from_fn(|| s.pop_next()).map(|r| r.id).collect();
        assert_eq!(ids, ["a", "b", "late"]);
    }

    #[test]
    fn nothing_is_due_before_its_time_and_a_duplicate_id_is_ignored() {
        let mut s = Scheduler::new(1);
        s.insert(row("a", 10));
        s.insert(row("a", 10));
        assert_eq!(s.len(), 1);
        assert_eq!(s.next_at(), Some(10));
        let c = VirtualClock::new(9);
        assert!(s.pop_due(&c).is_empty());
        c.advance_to(10);
        assert_eq!(s.pop_due(&c).len(), 1);
        assert!(s.is_empty());
    }
}
