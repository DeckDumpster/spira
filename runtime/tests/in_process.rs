//! The conformance suite on the in-process executor with the in-memory journal.

use runtime::conformance::*;
use runtime::journal::MemJournal;

fn rig() -> impl Rig {
    InProcessRig(|| MemJournal::new() as std::sync::Arc<dyn runtime::Journal>)
}


mod conformance {
    use super::*;
    use runtime::conformance as super_;
    fn rig() -> impl Rig {
        super::rig()
    }
    macro_rules! s {
        ($($name:ident),*) => { $( #[test] fn $name() { super_::$name(&rig()); } )* };
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
}

mod ids {
    use runtime::heartbeat::Heartbeat;
    use runtime::ports::VirtualClock;
    use runtime::*;
    use std::sync::Arc;

    #[test]
    fn forty_heartbeats_keep_every_id_within_the_column_and_keep_firing() {
        let journal = MemJournal::new();
        let clock = VirtualClock::new(1_000);
        let regs = vec![Heartbeat { interval_ms: 60_000, ping_deadline_ms: 1_000 }.registration()];
        let mut e = Engine::open(journal.clone(), Arc::new(clock.clone()), regs, 0).unwrap();
        let (id, stream, kind) = Heartbeat::boot();
        e.submit(id, stream, kind).unwrap();
        for _ in 0..40 {
            e.run_until_idle().unwrap();
            clock.advance(60_000);
        }
        let events = journal.read_from(0, 1000).unwrap();
        let beats = events.iter().filter(|(_, ev)| ev.kind.tag == "heartbeat").count();
        assert!(beats >= 40, "the beats keep firing: {beats}");
        assert!(events.iter().all(|(_, ev)| ev.id.0.chars().count() <= EVENT_ID_MAX && ev.cause.as_ref().is_none_or(|c| c.0.chars().count() <= EVENT_ID_MAX)));
        assert!(journal.pending_schedules().unwrap().iter().all(|r| r.id.chars().count() <= EVENT_ID_MAX));
    }

    #[test]
    fn an_id_that_fits_is_unchanged_and_a_longer_one_is_fixed_width_and_stable() {
        assert_eq!(derive_id("sched/heartbeat/boot/heartbeat/0".into()), "sched/heartbeat/boot/heartbeat/0");
        let exact = "a".repeat(EVENT_ID_MAX);
        assert_eq!(derive_id(exact.clone()), exact);
        let long = "é".repeat(300);
        let d = derive_id(long.clone());
        assert_eq!(d.chars().count(), EVENT_ID_MAX - 1, "never the width the store truncates to");
        assert_eq!(d, derive_id(long));
        assert_ne!(d, derive_id("é".repeat(301)), "the hash covers the whole id");
    }

    struct Echo;
    impl Handler for Echo {
        fn interests(&self) -> &[EventKindTag] {
            &["echo"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![Command::Emit(EventKind::new("echoed", ""))]
        }
    }

    #[test]
    fn a_refused_command_quarantines_its_stream_and_a_fault_is_still_fatal() {
        let run = |refuse: bool| {
            let inner = MemJournal::new();
            let faulty = FaultyJournal::new(inner.clone());
            let sink = MemSink::new();
            let mut e = Engine::open(faulty.clone(), Arc::new(VirtualClock::new(1)), vec![Registration::new("echo", Echo)], 0).unwrap().with_sink(sink.clone());
            e.submit("e1", "s", EventKind::new("echo", "")).unwrap();
            if refuse { faulty.refuse_next("append") } else { faulty.fail_next("append") }
            (e.run_until_idle(), inner, sink)
        };
        let (r, inner, sink) = run(true);
        r.unwrap();
        assert!(inner.poisoned(&StreamKey("s".into())).unwrap().is_some(), "the stream is quarantined");
        assert_eq!(inner.cursor().unwrap(), 1, "the cursor advanced past the refused event");
        assert!(sink.records().iter().any(|r| r.get("outcome") == Some(&Value::from("refused"))), "the dispatch wide event says refused");
        let (r, inner, _) = run(false);
        assert!(r.is_err() && inner.poisoned(&StreamKey("s".into())).unwrap().is_none(), "a fault propagates and quarantines nothing");
    }
}

mod cursor {
    use runtime::ports::VirtualClock;
    use runtime::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Count(Arc<AtomicUsize>);
    impl Handler for Count {
        fn interests(&self) -> &[EventKindTag] {
            &["ping"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            self.0.fetch_add(1, Ordering::SeqCst);
            vec![]
        }
    }

    #[test]
    fn a_cursor_write_lost_to_a_crash_replays_without_running_a_handler_twice() {
        let mem = MemJournal::new();
        let faulty = FaultyJournal::new(mem.clone());
        let runs = Arc::new(AtomicUsize::new(0));
        let open = |j: Arc<dyn Journal>| Engine::open(j, Arc::new(VirtualClock::new(1)), vec![Registration::new("count", Count(runs.clone()))], 0).unwrap();
        let mut e = open(faulty.clone());
        e.submit("p1", "s", EventKind::new("ping", "")).unwrap();
        faulty.fail_next("set_cursor");
        assert!(e.run_until_idle().is_err());
        assert_eq!(mem.cursor().unwrap(), 0);
        let mut again = open(mem.clone());
        again.run_until_idle().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(mem.cursor().unwrap(), 1);
    }
}
