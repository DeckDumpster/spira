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
        a_duplicate_submit_dispatches_once,
        a_failed_commit_is_sent_again_and_the_turn_lands_once,
        a_restart_never_relaunches_an_unresolved_effect_and_delivers_timed_out,
        determinism_under_one_seed,
        a_fired_schedule_row_is_not_fired_by_the_next_process,
        out_of_order_effect_outcomes,
        effect_deadline_times_out,
        handler_panic_quarantines_its_stream_only,
        schedule_rows_survive_restart_and_fire_once,
        observe_shadows_writes_and_runs_probes,
        halt_holds_writes_runs_probes_and_resume_releases_once_in_order,
        a_handlers_writes_are_seen_before_its_streams_next_event,
        an_outcome_is_offered_only_to_the_task_that_asked,
        a_halt_holds_work_and_lets_signals_through
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
        let events = journal.log();
        let beats = events.iter().filter(|ev| ev.kind.tag == "heartbeat").count();
        assert!(beats >= 40, "the beats keep firing: {beats}");
        assert!(events.iter().all(|ev| ev.id.0.chars().count() <= EVENT_ID_MAX && ev.cause.as_ref().is_none_or(|c| c.0.chars().count() <= EVENT_ID_MAX)));
        assert!(journal.boot().unwrap().schedules.iter().all(|r| r.id.chars().count() <= EVENT_ID_MAX));
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

    struct Ticker;
    impl Handler for Ticker {
        fn interests(&self) -> &[EventKindTag] {
            &["tick"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            vec![Command::Timer { at: ev.at + 30_000, kind: EventKind::new("tick", "") }]
        }
    }

    #[test]
    fn a_plain_ticker_has_its_next_tick_armed_after_the_boot_tick_and_every_later_one() {
        let clock = VirtualClock::new(1_000);
        let regs = vec![Registration::new("ticker", Ticker)];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 0).unwrap();
        e.start("ticker", EventKind::new("tick", ""));
        let mut last = 0;
        for _ in 0..4 {
            e.run_until_idle().unwrap();
            let due = e.status().tasks[0].next_due_at.expect("the next tick stays armed");
            assert!(due > last, "each tick arms a later one, not the same one again");
            last = due;
            clock.advance(30_000);
        }
    }

    #[test]
    fn a_tick_on_a_stream_with_a_standing_poison_row_is_parked_for_the_recheck_not_dropped() {
        let clock = VirtualClock::new(1_000);
        let journal = MemJournal::new();
        let regs = vec![Registration::new("ticker", Ticker)];
        let mut e = Engine::open(journal.clone(), Arc::new(clock.clone()), regs, 0).unwrap();
        e.start("ticker", EventKind::new("tick", ""));
        e.run_until_idle().unwrap();
        let before = e.status().tasks[0].next_due_at.expect("armed");
        journal.poison(&Poison { stream: StreamKey("ticker".into()), event: EventId("x".into()), handler: "ticker".into(), message: "planted".into() }).unwrap();
        clock.advance(30_000);
        e.run_until_idle().unwrap();
        let after = e.status().tasks[0].next_due_at.expect("a poisoned stream's tick is parked, not dropped");
        assert!(after > before, "the parked tick is later than the one it replaces");
    }

    #[test]
    fn a_tick_on_a_stream_whose_standing_poison_came_from_a_refused_commit_runs_and_one_a_handler_wrote_stays_parked() {
        let run = |handler: &str| {
            let clock = VirtualClock::new(1_000);
            let journal = MemJournal::new();
            journal.poison(&Poison { stream: StreamKey("ticker".into()), event: EventId("x".into()), handler: handler.into(), message: "planted".into() }).unwrap();
            let mut e = Engine::open(journal, Arc::new(clock.clone()), vec![Registration::new("ticker", Ticker)], 0).unwrap();
            e.start("ticker", EventKind::new("tick", ""));
            e.run_until_idle().unwrap();
            e.status().tasks[0].last_fired_at.is_some()
        };
        assert!(run("scheduler"), "a commit refusal is a function of the release that wrote it: a new process tries again");
        assert!(!run("ticker"), "a handler's quarantine outlives a restart");
    }

    struct Twice;
    impl Handler for Twice {
        fn interests(&self) -> &[EventKindTag] {
            &["go"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![Command::Effect(EffectSpec::keyed("w", "same", "", 1_000)), Command::Effect(EffectSpec::keyed("w", "same", "", 1_000))]
        }
    }

    #[test]
    fn the_status_counts_the_commands_a_fire_returned_and_the_ones_the_engine_admitted() {
        let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(1)), vec![Registration::new("twice", Twice)], 0).unwrap();
        e.submit("g1", "s", EventKind::new("go", "")).unwrap();
        e.run_until_idle().unwrap();
        assert_eq!(e.status().tasks[0].last_commands, Some(FireCommands { returned: 2, admitted: 1 }), "the second request is refused while the first is in flight");
    }

    struct Probes;
    impl Handler for Probes {
        fn interests(&self) -> &[EventKindTag] {
            &["go"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![Command::Probe(EffectSpec::probe("look", "", 1_000))]
        }
    }

    #[test]
    fn a_probe_asked_again_while_the_one_before_is_unanswered_is_admitted_again() {
        let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(1)), vec![Registration::new("probes", Probes)], 0).unwrap();
        for n in 0..3 {
            e.submit(&format!("g{n}"), "s", EventKind::new("go", "")).unwrap();
            e.run_until_idle().unwrap();
            assert_eq!(e.status().tasks[0].last_commands, Some(FireCommands { returned: 1, admitted: 1 }), "ask {n}");
            assert_eq!(e.status().tasks[0].in_flight.len(), n + 1, "every ask is in flight beside the ones before it");
        }
    }

    #[test]
    fn a_fire_whose_effects_are_held_by_a_halt_counts_as_returned_and_not_admitted() {
        let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(1)), vec![Registration::new("twice", Twice)], 0).unwrap();
        e.halt_at_boot("test").unwrap();
        e.submit("g1", "s", EventKind::new("go", "")).unwrap();
        e.run_until_idle().unwrap();
        assert_eq!(e.status().tasks[0].last_commands, Some(FireCommands { returned: 2, admitted: 0 }));
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
            if refuse { faulty.refuse_next("commit") } else { faulty.fail_next("commit") }
            (e.run_until_idle(), inner, sink)
        };
        let (r, inner, sink) = run(true);
        r.unwrap();
        assert!(inner.poisoned(&StreamKey("s".into())).unwrap().is_some(), "the stream is quarantined");
        assert!(sink.records().iter().any(|r| r.get("unit") == Some(&Value::from("quarantine"))), "the quarantine wide event says so");
        let (r, inner, _) = run(false);
        assert!(r.is_err() && inner.poisoned(&StreamKey("s".into())).unwrap().is_none(), "a fault propagates and quarantines nothing");
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
