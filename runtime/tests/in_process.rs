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
        out_of_order_effect_outcomes,
        effect_deadline_times_out,
        handler_panic_quarantines_its_stream_only,
        schedule_rows_survive_restart_and_fire_once,
        observe_shadows_writes_runs_probes_and_never_replays_them,
        halt_holds_writes_runs_probes_and_resume_releases_once_in_order,
        a_handlers_writes_are_seen_before_its_streams_next_event,
        an_outcome_is_offered_only_to_the_task_that_asked
    );
}

mod ids {
    use runtime::ports::VirtualClock;
    use runtime::*;
    use std::sync::Arc;

    struct Beat;
    impl Handler for Beat {
        fn interests(&self) -> &[EventKindTag] {
            &["heartbeat"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            vec![Command::Schedule { at: ev.at + 60_000, kind: EventKind::new("heartbeat", "") }]
        }
    }

    #[test]
    fn forty_heartbeats_keep_every_id_within_the_column_and_keep_firing() {
        let journal = MemJournal::new();
        let clock = VirtualClock::new(1_000);
        let regs = vec![Registration::new("heartbeat", Beat)];
        let mut e = Engine::open(journal.clone(), Arc::new(clock.clone()), regs, 0).unwrap();
        e.submit("heartbeat/boot", "runtime.heartbeat", EventKind::new("heartbeat", "0")).unwrap();
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

mod wide {
    use runtime::ports::VirtualClock;
    use runtime::*;
    use std::sync::Arc;

    /// Hears every `q`; does something only when its body says so.
    struct Maybe;
    impl Handler for Maybe {
        fn interests(&self) -> &[EventKindTag] {
            &["q"]
        }
        fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            match ev.kind.body.as_str() {
                "field" => {
                    ctx.event().set("maybe.saw", "field");
                    vec![]
                }
                "emit" => vec![Command::Emit(EventKind::new("did", ""))],
                _ => vec![],
            }
        }
    }

    #[test]
    fn a_call_that_does_nothing_leaves_no_wide_event_and_one_that_does_leaves_its_fields() {
        let sink = MemSink::new();
        let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(1)), vec![Registration::new("maybe", Maybe)], 0).unwrap().with_sink(sink.clone());
        for (id, body) in [("q1", ""), ("q2", "field"), ("q3", "emit"), ("q4", "")] {
            e.submit(id, "s", EventKind::new("q", body)).unwrap();
        }
        e.run_until_idle().unwrap();
        let dispatched: Vec<_> = sink.records().into_iter().filter(|r| r.get("unit") == Some(&Value::from("dispatch"))).collect();
        let ids: Vec<_> = dispatched.iter().map(|r| r.get("event.id").cloned()).collect();
        assert_eq!(ids, vec![Some(Value::from("q2")), Some(Value::from("q3"))], "only the calls that set a field or returned a command: {dispatched:?}");
        assert_eq!(dispatched[0].get("maybe.saw"), Some(&Value::from("field")), "the handler's field is on its record");
        assert_eq!(dispatched[1].get("commands.emit"), Some(&Value::from(1usize)));
        assert!(dispatched.iter().all(|r| r.get("process.rss_kb").is_some() && r.get("handler") == Some(&Value::from("maybe"))));
    }
}
