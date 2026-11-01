//! The conformance suite on the lease executor, and the lease-steal scenarios: in one process on
//! virtual time, and across two real processes with the holder killed mid-stream.

use std::sync::Arc;
use std::time::Duration;

use runtime::conformance::{trace, Rig};
use runtime::engine::Executor;
use runtime::file_journal::FileJournal;
use runtime::journal::{Journal, MemJournal};
use runtime::lease::{LeaseConfig, LeaseExecutor, LeaseRig};
use runtime::ports::VirtualClock;
use runtime::*;
use testkit::TempDir;

mod conformance {
    use super::*;
    use runtime::conformance as c;

    fn mem() -> impl Rig {
        LeaseRig::new(|| MemJournal::new() as Arc<dyn Journal>)
    }

    macro_rules! s {
        ($($name:ident),*) => { $( #[test] fn $name() { c::$name(&mem()); } )* };
    }
    s!(
        redelivery_of_any_event,
        crash_between_append_and_dispatch,
        crash_during_dispatch_appends_nothing_twice,
        crash_between_effect_request_and_outcome,
        out_of_order_effect_outcomes,
        effect_deadline_times_out,
        determinism_under_replay,
        handler_panic_quarantines_its_stream_only,
        schedule_rows_survive_restart_and_fire_once
    );

    #[test]
    fn run_all_on_a_file_journal() {
        let dirs = std::sync::Mutex::new(Vec::new());
        let rig = LeaseRig::new(|| {
            let d = TempDir::new("rt-lease");
            let j = FileJournal::open(d.path()).unwrap();
            dirs.lock().unwrap().push(d);
            Arc::new(j) as Arc<dyn Journal>
        });
        c::run_all(&rig);
    }
}

struct Stepper {
    pause: Duration,
}

impl Handler for Stepper {
    fn interests(&self) -> &[EventKindTag] {
        &["step"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        std::thread::sleep(self.pause); // clock-safe: paces a handler so the holder is killed mid-stream
        vec![Command::Emit(EventKind::new("stepped", &ev.kind.body))]
    }
}

fn step(stream: &str, i: usize) -> Event {
    Event {
        id: EventId(format!("{stream}-{i}")),
        stream: StreamKey(stream.into()),
        kind: EventKind::new("step", &i.to_string()),
        at: 0,
        cause: None,
        correlation: CorrelationId(format!("{stream}-{i}")),
    }
}

fn stepped(j: &dyn Journal, stream: &str) -> Vec<usize> {
    trace(j).into_iter().filter(|t| t.1 == stream && t.2 == "stepped").map(|t| t.3.parse().unwrap()).collect()
}

fn node(j: Arc<dyn Journal>, clock: &VirtualClock, name: &str, ttl_ms: u64) -> LeaseExecutor {
    let regs = vec![Registration::new("stepper", Stepper { pause: Duration::ZERO })];
    LeaseExecutor::open(j, Arc::new(clock.clone()), regs, 1, LeaseConfig { node: name.into(), ttl_ms }).unwrap()
}

#[test]
fn a_live_holder_keeps_its_stream_and_the_other_node_does_not_dispatch_it() {
    let (j, clock) = (MemJournal::new() as Arc<dyn Journal>, VirtualClock::new(0));
    let mut a = node(j.clone(), &clock, "a", 1000);
    a.submit("s-0", "s", EventKind::new("step", "0")).unwrap();
    let mut b = node(j.clone(), &clock, "b", 1000);
    b.run_until_idle().unwrap();
    assert!(b.owned().is_empty());
    assert!(b.submit("s-1", "s", EventKind::new("step", "1")).is_err(), "one writer per stream");
    a.run_until_idle().unwrap();
    assert_eq!(stepped(&*j, "s"), vec![0]);
}

#[test]
fn a_stolen_stream_continues_in_order_and_the_old_holder_is_fenced() {
    let (j, clock) = (MemJournal::new() as Arc<dyn Journal>, VirtualClock::new(0));
    let mut a = node(j.clone(), &clock, "a", 1000);
    for i in 0..6 {
        j.append(&step("s", i)).unwrap();
    }
    a.run_until_idle().unwrap();
    for i in 6..12 {
        j.append(&step("s", i)).unwrap();
    }
    let mut b = node(j.clone(), &clock, "b", 1000);
    b.run_until_idle().unwrap();
    assert!(b.owned().is_empty(), "a lease within its ttl is not stolen");

    clock.advance_to(1000);
    b.run_until_idle().unwrap();
    assert_eq!(b.owned(), vec![StreamKey("s".into())]);
    assert_eq!(stepped(&*j, "s"), (0..12).collect::<Vec<_>>());

    let zombie = Event { id: EventId("late".into()), kind: EventKind::new("stepped", "99"), ..step("s", 99) };
    let err = j.leases().unwrap().append_fenced(&zombie, "a", 1).unwrap_err();
    assert!(err.0.starts_with(runtime::journal::LEASE_LOST), "{err}");
    j.append(&step("s", 12)).unwrap();
    a.run_until_idle().unwrap();
    assert!(a.owned().is_empty());
    b.run_until_idle().unwrap();
    assert_eq!(stepped(&*j, "s"), (0..13).collect::<Vec<_>>(), "the stolen-from node lands nothing after the steal");
}

#[test]
fn a_released_lease_is_taken_at_the_next_epoch() {
    let (j, clock) = (MemJournal::new() as Arc<dyn Journal>, VirtualClock::new(0));
    let s = StreamKey("s".into());
    j.append(&step("s", 0)).unwrap();
    drop(node(j.clone(), &clock, "a", 1000));
    let first = j.leases().unwrap().current(&s).unwrap().unwrap();
    let mut b = node(j.clone(), &clock, "b", 1000);
    b.run_until_idle().unwrap();
    let second = j.leases().unwrap().current(&s).unwrap().unwrap();
    assert_eq!((second.holder.as_str(), second.epoch), ("b", first.epoch + 1));
}
