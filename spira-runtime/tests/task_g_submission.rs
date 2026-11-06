//! Source `submission` (`wiki/projects/spira/designs/tasks/source-submission.md`) on the
//! VirtualDriver: a `ScriptedSource` standing in submits `submission.accepted` and
//! `submission.refused` in their declared shapes, a stub consumer hears both, the reply channel
//! answers the submitter once the event is journaled, and the real listener is refused under
//! virtual time. Red until the port lands: the real source's `run` is `unimplemented!`.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{ScriptedSource, Source, Sources};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration};
use spira_runtime::tasks::g::submission::*;
use tokio::sync::mpsc;

/// A subscriber's stand-in (none subscribes this round): parses each event into its schema.
struct Consumer;
impl Handler for Consumer {
    fn interests(&self) -> &[EventKindTag] {
        &[ACCEPTED, REFUSED]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        let seen = match ev.kind.tag.as_str() {
            ACCEPTED => serde_json::from_str::<Accepted>(&ev.kind.body).ok().map(|a| format!("accepted {} {}->{}", a.message_id, a.from, a.to.join(","))),
            _ => serde_json::from_str::<Refused>(&ev.kind.body).ok().map(|r| format!("refused {} {}", r.peer, r.reason)),
        };
        vec![Command::Emit(EventKind::new("consumed", &format!("{}|{}@{}", ev.stream.0, seen.unwrap_or_else(|| "unparsed".into()), ctx.now())))]
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        Err(format!("no runner for effect {}", spec.name))
    }
}

fn driver(clock: &VirtualClock, j: Arc<dyn Journal>, sources: Sources) -> runtime::Result<VirtualDriver> {
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let engine = Engine::open(j, c, vec![Registration::new("submission.stub", Consumer)], 1).unwrap();
    VirtualDriver::new(engine, clock.clone(), Arc::new(NoEffects)).with_sources(sources)
}

fn scripted(clock: &VirtualClock, script: Vec<(u64, Submit)>) -> Sources {
    let mut s = Sources::default();
    s.register(Box::new(ScriptedSource::new("submission.script", Arc::new(clock.clone()), script))).unwrap();
    s
}

fn accepted() -> Accepted {
    Accepted { peer: "10.0.0.2:5555".into(), user: "ryan".into(), from: "me@example.org".into(), to: vec!["concierge@spira".into()], message_id: "<m1@x>".into(), in_reply_to: None }
}

fn refused() -> Refused {
    Refused { peer: "10.0.0.3:6666".into(), user: String::new(), from: "x@y".into(), reason: "relay".into() }
}

#[test]
fn both_declared_events_reach_a_consumer_at_their_virtual_times() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let a = accepted();
    let r = refused();
    let script = vec![(100, Submit::new(&submit_id(Some(&a.message_id), &a.peer, 100), STREAM, a.event_kind())), (200, Submit::new(&submit_id(None, &r.peer, 200), STREAM, r.event_kind()))];
    let mut v = driver(&clock, j.clone(), scripted(&clock, script)).unwrap();
    v.run_until(1_000).unwrap();
    let consumed: Vec<String> = trace(&*j).into_iter().filter(|t| t.2 == "consumed").map(|t| t.3).collect();
    assert_eq!(consumed, ["submission|accepted <m1@x> me@example.org->concierge@spira@100", "submission|refused 10.0.0.3:6666 relay@200"]);
    assert_eq!(submit_id(None, "p", 7), "submission/p+7");
}

#[test]
fn the_reply_channel_answers_the_submitter_once_the_event_is_journaled() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let a = accepted();
    let (s, mut reply) = Submit::with_reply(&submit_id(Some(&a.message_id), &a.peer, 100), STREAM, a.event_kind());
    let mut v = driver(&clock, j.clone(), scripted(&clock, vec![(100, s)])).unwrap();
    v.step(50).unwrap();
    assert!(reply.try_recv().is_err(), "not yet offered");
    v.run_until(150).unwrap();
    assert_eq!(reply.try_recv(), Ok(Ok(())), "accepted once journaled");
    assert_eq!(trace(&*j).iter().filter(|t| t.2 == ACCEPTED).count(), 1);
}

#[test]
fn the_real_source_is_refused_under_virtual_time_naming_it() {
    let clock = VirtualClock::new(0);
    let mut s = scripted(&clock, vec![]);
    s.register(Box::new(Submission { addr: "127.0.0.1".into(), port: 2587 })).unwrap();
    let err = driver(&clock, MemJournal::new(), s).err().expect("a listener is refused under virtual time");
    assert!(err.0.contains(NAME) && !err.0.contains("submission.script"), "{err}");
    assert_eq!(BACKEND, "smtp-submission");
}

#[test]
fn the_real_source_unconfigured_is_dormant_and_returns_once_its_channel_is_closed() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    Box::new(Submission { addr: String::new(), port: 0 }).run(tx);
}
