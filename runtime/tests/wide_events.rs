//! Wide events: one record per dispatch and per effect, open keys, no silent path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use runtime::ports::VirtualClock;
use runtime::*;

struct Annotator(Arc<AtomicUsize>);
impl Handler for Annotator {
    fn interests(&self) -> &[EventKindTag] {
        &["note", "boom", "work", EFFECT_SUCCEEDED]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ctx.event().set("zz.never.seen.anywhere.else", "planted");
        ctx.event().set("bead.id", "sp-test");
        match ev.kind.tag.as_str() {
            "boom" => panic!("handler exploded"),
            "work" => vec![Command::Effect(EffectSpec { name: "work".into(), input: String::new(), deadline_ms: 500 })],
            "note" => vec![Command::Emit(EventKind::new("noted", ""))],
            _ => vec![],
        }
    }
}

fn boot(sink: Arc<MemSink>, calls: Arc<AtomicUsize>) -> (Engine, Arc<dyn Journal>) {
    let journal: Arc<dyn Journal> = MemJournal::new();
    let clock = VirtualClock::new(1_000);
    let engine = Engine::open(journal.clone(), Arc::new(clock), vec![Registration::new("annotator", Annotator(calls))], 7)
        .unwrap()
        .with_sink(sink)
        .with_baseline(vec![("runtime.version".into(), "9.9.9".into()), ("config.hash".into(), "abc123".into())]);
    (engine, journal)
}

fn units(sink: &MemSink, unit: &str) -> Vec<Fields> {
    sink.records().into_iter().filter(|r| r.get("unit") == Some(&Value::Str(unit.into()))).collect()
}

#[test]
fn one_dispatch_is_one_record_carrying_the_baseline_and_a_key_only_the_handler_knows() {
    let (sink, calls) = (MemSink::new(), Arc::new(AtomicUsize::new(0)));
    let (mut e, _) = boot(sink.clone(), calls);
    e.submit("e1", "s1", EventKind::new("note", "")).unwrap();
    e.run_until_idle().unwrap();
    let d = units(&sink, "dispatch");
    assert_eq!(d.len(), 1, "{:?}", sink.records());
    let r = &d[0];
    for k in ["event.id", "event.stream", "event.kind", "correlation.id", "handler", "duration_us", "outcome", "journal.offset", "journal.lag", "process.rss_kb", "process.uptime_ms", "runtime.version", "config.hash", "commands.emit", "commands.effect", "commands.schedule"] {
        assert!(r.contains_key(k), "missing {k}: {r:?}");
    }
    assert_eq!(r.get("outcome"), Some(&Value::Str("ok".into())));
    assert_eq!(r.get("commands.emit"), Some(&Value::Int(1)));
    assert_eq!(r.get("zz.never.seen.anywhere.else"), Some(&Value::Str("planted".into())));
    assert_eq!(r.get("bead.id"), Some(&Value::Str("sp-test".into())));
}

#[test]
fn an_effect_is_one_record_from_request_to_outcome() {
    let (sink, calls) = (MemSink::new(), Arc::new(AtomicUsize::new(0)));
    let (mut e, _) = boot(sink.clone(), calls);
    e.submit("e1", "s1", EventKind::new("work", "")).unwrap();
    e.run_until_idle().unwrap();
    let key = e.pending_effects()[0].key.clone();
    e.note_attempt(&key);
    e.complete(&key, Outcome::Succeeded("done".into())).unwrap();
    let f = units(&sink, "effect");
    assert_eq!(f.len(), 1, "{:?}", sink.records());
    let r = &f[0];
    assert_eq!(r.get("effect.kind"), Some(&Value::Str("work".into())));
    assert_eq!(r.get("effect.idempotency_key"), Some(&Value::Str(key)));
    assert_eq!(r.get("effect.outcome"), Some(&Value::Str("succeeded".into())));
    assert_eq!(r.get("effect.attempt"), Some(&Value::Int(1)));
    assert!(r.contains_key("effect.deadline_at") && r.contains_key("duration_ms"), "{r:?}");
}

#[test]
fn every_handler_call_emits_exactly_one_dispatch_record_on_every_path() {
    let (sink, calls) = (MemSink::new(), Arc::new(AtomicUsize::new(0)));
    let (mut e, _) = boot(sink.clone(), calls.clone());
    e.submit("e1", "s1", EventKind::new("note", "")).unwrap();
    e.submit("e2", "s2", EventKind::new("boom", "")).unwrap();
    e.submit("e3", "s3", EventKind::new("work", "")).unwrap();
    e.run_until_idle().unwrap();
    let d = units(&sink, "dispatch");
    assert_eq!(d.len(), calls.load(Ordering::SeqCst), "a dispatch path emitted no record");
    let outcomes: Vec<_> = d.iter().filter_map(|r| r.get("outcome")).collect();
    assert!(outcomes.contains(&&Value::Str("panic".into())) && outcomes.contains(&&Value::Str("ok".into())), "{outcomes:?}");
    assert_eq!(units(&sink, "quarantine").len(), 1, "the panic quarantines its stream");
}

#[test]
fn a_key_outside_the_dotted_lowercase_form_is_recorded_as_rejected() {
    struct Bad;
    impl Handler for Bad {
        fn interests(&self) -> &[EventKindTag] {
            &["x"]
        }
        fn handle(&self, ctx: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            ctx.event().set("Not A Key", 1i64);
            vec![]
        }
    }
    let sink = MemSink::new();
    let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(0)), vec![Registration::new("bad", Bad)], 1).unwrap().with_sink(sink.clone());
    e.submit("e1", "s", EventKind::new("x", "")).unwrap();
    e.run_until_idle().unwrap();
    let r = &units(&sink, "dispatch")[0];
    assert_eq!(r.get(wide::REJECTED_KEYS), Some(&Value::Strs(vec!["Not A Key".into()])));
}
