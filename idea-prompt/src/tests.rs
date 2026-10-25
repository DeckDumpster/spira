use super::*;
use std::cell::{Cell, RefCell};

struct Fake {
    now: Cell<u64>,
    hour: Cell<u32>,
    busy: Cell<bool>,
    ideas: Vec<(&'static str, &'static str)>,
    sent: RefCell<Vec<(String, String, String)>>,
}

impl Fake {
    fn new() -> Self {
        Fake { now: Cell::new(1_000_000), hour: Cell::new(12), busy: Cell::new(false), ideas: vec![("sp-b", "newer"), ("sp-a", "older")], sent: RefCell::new(Vec::new()) }
    }
}

impl Ports for Fake {
    fn now(&self) -> u64 {
        self.now.get()
    }
    fn local_hour(&self, _: u64) -> Result<u32, String> {
        Ok(self.hour.get())
    }
    fn busy(&self) -> Result<bool, String> {
        Ok(self.busy.get())
    }
    fn ideas(&self) -> Result<Vec<Idea>, String> {
        Ok(self.ideas.iter().map(|(i, t)| Idea { id: i.to_string(), title: t.to_string() }).collect())
    }
    fn send(&self, s: &str, d: &str, b: &str) -> Result<(), String> {
        self.sent.borrow_mut().push((s.into(), d.into(), b.into()));
        Ok(())
    }
}

fn cfg() -> Config {
    Config { idle_secs: 1800, wake_from: 7, wake_until: 22, min_gap_secs: 86400 }
}

fn run(f: &Fake, st: &mut State) -> Outcome {
    pass(f, &cfg(), st).unwrap()
}

#[test]
fn thirty_idle_minutes_in_waking_hours_prompt_exactly_once() {
    let f = Fake::new();
    let mut st = State::default();
    assert_eq!(run(&f, &mut st), Outcome::NotIdleLongEnough);
    f.now.set(f.now.get() + 1799);
    assert_eq!(run(&f, &mut st), Outcome::NotIdleLongEnough);
    f.now.set(f.now.get() + 1);
    assert_eq!(run(&f, &mut st), Outcome::Prompted(2));
    f.now.set(f.now.get() + 3600);
    assert_eq!(run(&f, &mut st), Outcome::AlreadyPrompted);
    let sent = f.sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, "not now");
    assert!(sent[0].2.contains("1. sp-b — newer\n2. sp-a — older\n"), "{}", sent[0].2);
}

#[test]
fn busy_work_suppresses_and_resets_the_idle_clock() {
    let f = Fake::new();
    let mut st = State { last_busy: f.now.get() - 7200, ..State::default() };
    f.busy.set(true);
    assert_eq!(run(&f, &mut st), Outcome::Busy);
    f.busy.set(false);
    f.now.set(f.now.get() + 600);
    assert_eq!(run(&f, &mut st), Outcome::NotIdleLongEnough);
    assert!(f.sent.borrow().is_empty());
}

#[test]
fn an_unanswered_prompt_waits_for_busy_then_idle_and_a_day() {
    let f = Fake::new();
    let mut st = State { last_busy: f.now.get() - 3600, ..State::default() };
    assert_eq!(run(&f, &mut st), Outcome::Prompted(2));
    f.busy.set(true);
    f.now.set(f.now.get() + 60);
    run(&f, &mut st);
    f.busy.set(false);
    f.now.set(f.now.get() + 3600);
    assert_eq!(run(&f, &mut st), Outcome::TooSoon, "busy-then-idle inside a day is still one prompt a day");
    f.now.set(f.now.get() + 86400);
    assert_eq!(run(&f, &mut st), Outcome::Prompted(2));
    assert_eq!(f.sent.borrow().len(), 2);
}

#[test]
fn outside_waking_hours_nothing_is_sent() {
    let f = Fake::new();
    let mut st = State { last_busy: f.now.get() - 7200, ..State::default() };
    for h in [0, 6, 22, 23] {
        f.hour.set(h);
        assert_eq!(run(&f, &mut st), Outcome::OutsideWakingHours, "hour {h}");
    }
    f.hour.set(7);
    assert_eq!(run(&f, &mut st), Outcome::Prompted(2), "positive control: the same state prompts at 07:00");
}

#[test]
fn no_ideas_sends_nothing() {
    let mut f = Fake::new();
    f.ideas.clear();
    let mut st = State { last_busy: f.now.get() - 7200, ..State::default() };
    assert_eq!(run(&f, &mut st), Outcome::NoIdeas);
    assert!(f.sent.borrow().is_empty());
}

#[test]
fn state_round_trips() {
    let s = State { last_busy: 5, last_prompt: 9, prompted: true };
    assert_eq!(parse_state(&render_state(&s)), s);
    assert_eq!(parse_state("garbage"), State::default());
}
