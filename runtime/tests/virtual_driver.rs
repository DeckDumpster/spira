//! The virtual-time driver and sources: scripted submissions land at their virtual times,
//! effects are answered synchronously, timers are jumped to, a reply hears acceptance, and a
//! source that reads the real world is refused under virtual time.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use runtime::conformance::trace;
use runtime::host::{drive, Driver, EffectRunner, ServeOpts, Submit, VirtualDriver};
use runtime::journal::{FaultyJournal, Journal};
use runtime::offload::Pool;
use runtime::pools::PoolCaps;
use runtime::ports::{Clock, VirtualClock, WallClock};
use runtime::sources::{ScriptedSource, Source, Sources};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration, EFFECT_SUCCEEDED};
use tokio::sync::mpsc;

const DAY: u64 = 86_400_000;

/// `ping` answers `pong@now`; `work` asks for an effect; `tick` re-arms itself a day on.
struct Loop;
impl Handler for Loop {
    fn interests(&self) -> &[EventKindTag] {
        &["ping", "work", "tick", EFFECT_SUCCEEDED]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "ping" => vec![Command::Emit(EventKind::new("pong", &format!("{}@{}", ev.kind.body, ctx.now())))],
            "work" => vec![Command::Effect(EffectSpec { name: "job".into(), input: ev.kind.body.clone(), deadline_ms: 5_000 })],
            "tick" => vec![Command::Emit(EventKind::new("ticked", &ctx.now().to_string())), Command::Schedule { at: ev.at + DAY, kind: EventKind::new("tick", "") }],
            _ => vec![Command::Emit(EventKind::new("worked", &ev.kind.body))],
        }
    }
}

#[derive(Default)]
struct Jobs(AtomicUsize);
impl EffectRunner for Jobs {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(spec.input.clone())
    }
}

fn engine(j: Arc<dyn Journal>, clock: &VirtualClock, seed: u64) -> Engine {
    Engine::open(j, Arc::new(clock.clone()), vec![Registration::new("loop", Loop)], seed).unwrap()
}

fn sub(id: &str, tag: &str, body: &str) -> Submit {
    Submit::new(id, "s", EventKind::new(tag, body))
}

fn bodies(j: &dyn Journal, tag: &str) -> Vec<(String, u64)> {
    trace(j).into_iter().filter(|t| t.2 == tag).map(|t| (t.3, t.4)).collect()
}

fn scripted(clock: &VirtualClock, script: Vec<(u64, Submit)>) -> Sources {
    let mut s = Sources::default();
    s.register(Box::new(ScriptedSource::new("script", Arc::new(clock.clone()), script))).unwrap();
    s
}

/// The journal's trace and how many effects the runner ran.
type Run = (Vec<(String, String, String, String, u64)>, usize);

fn run(seed: u64) -> Run {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let jobs = Arc::new(Jobs::default());
    let script = vec![(300, sub("c", "ping", "c")), (50, sub("a", "ping", "a")), (100, sub("w", "work", "w")), (100, sub("b", "ping", "b")), (0, sub("t", "tick", ""))];
    let mut v = VirtualDriver::new(engine(j.clone(), &clock, seed), clock.clone(), jobs.clone()).with_sources(scripted(&clock, script)).unwrap();
    v.run_until(3 * DAY).unwrap();
    assert_eq!(clock.now_ms(), 3 * DAY);
    (trace(&*j), jobs.0.load(Ordering::SeqCst))
}

#[test]
fn scripted_submissions_land_at_their_virtual_times_and_effects_run_synchronously() {
    let (t, jobs) = run(7);
    let pongs: Vec<String> = t.iter().filter(|e| e.2 == "pong").map(|e| e.3.clone()).collect();
    assert_eq!(pongs, ["a@50", "b@100", "c@300"]);
    assert_eq!(jobs, 1, "the effect ran once, through the runner");
    let worked: Vec<u64> = t.iter().filter(|e| e.2 == "worked").map(|e| e.4).collect();
    assert_eq!(worked, [100], "answered at the instant it was asked: no virtual time passes for an effect");
    let ticked: Vec<String> = t.iter().filter(|e| e.2 == "ticked").map(|e| e.3.clone()).collect();
    assert_eq!(ticked, ["0", &DAY.to_string(), &(2 * DAY).to_string(), &(3 * DAY).to_string()], "the clock jumps from timer to timer across days");
}

#[test]
fn same_seed_same_trace() {
    assert_eq!(run(11), run(11));
}

#[test]
fn a_reply_hears_acceptance_and_refusal() {
    let (mem, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let faulty = FaultyJournal::new(mem.clone());
    let (ok, mut ok_rx) = Submit::with_reply("ok", "s", EventKind::new("ping", "x"));
    let (no, mut no_rx) = Submit::with_reply("no", "s", EventKind::new("ping", "y"));
    let mut v = VirtualDriver::new(engine(faulty.clone(), &clock, 1), clock.clone(), Arc::new(Jobs::default())).with_sources(scripted(&clock, vec![(10, ok), (20, no)])).unwrap();
    v.step(15).unwrap();
    assert_eq!(ok_rx.try_recv(), Ok(Ok(())), "accepted once journaled");
    assert!(no_rx.try_recv().is_err(), "not yet offered");
    faulty.refuse_next("append");
    v.run_until(30).unwrap();
    let refused = no_rx.try_recv().expect("answered");
    assert!(refused.is_err(), "a refused append is answered as a refusal: {refused:?}");
    assert_eq!(bodies(&*mem, "ping").len(), 1);

    let (late, mut late_rx) = Submit::with_reply("late", "s", EventKind::new("ping", "z"));
    let v = VirtualDriver::new(engine(MemJournal::new(), &clock, 1), clock.clone(), Arc::new(Jobs::default())).with_sources(scripted(&clock, vec![(DAY, late)])).unwrap();
    drop(v);
    assert!(late_rx.try_recv().is_err(), "a host that stopped first drops the reply: the sender reads it as not accepted");
}

struct Bus;
impl Source for Bus {
    fn name(&self) -> &str {
        "bus"
    }
    fn run(self: Box<Self>, _: mpsc::Sender<Submit>) {}
}

#[test]
fn a_source_that_reads_the_world_is_refused_under_virtual_time_and_names_are_unique() {
    let clock = VirtualClock::new(0);
    let mut s = scripted(&clock, vec![]);
    assert!(s.register(Box::new(ScriptedSource::new("script", Arc::new(clock.clone()), vec![]))).unwrap_err().is_refused());
    s.register(Box::new(Bus)).unwrap();
    assert_eq!(s.names(), ["script", "bus"]);
    let err = VirtualDriver::new(engine(MemJournal::new(), &clock, 1), clock.clone(), Arc::new(Jobs::default())).with_sources(s).err().expect("refused");
    assert!(err.0.contains("bus") && !err.0.contains("script"), "{err}");
}

#[test]
fn a_scripted_source_on_its_own_thread_sends_when_its_clock_arrives() {
    let clock = VirtualClock::new(0);
    let (tx, mut rx) = mpsc::channel(4);
    let handles = scripted(&clock, vec![(5, sub("a", "ping", "")), (9, sub("b", "ping", ""))]).start(tx).unwrap();
    std::thread::sleep(Duration::from_millis(60)); // clock-safe: proves nothing is sent before the clock arrives
    assert!(rx.try_recv().is_err());
    clock.advance_to(5);
    assert_eq!(rx.blocking_recv().unwrap().id, "a");
    clock.advance_to(9);
    assert_eq!(rx.blocking_recv().unwrap().id, "b");
    handles.into_iter().for_each(|h| h.join().unwrap());
    assert!(rx.blocking_recv().is_none(), "the source ends and drops its sender");
}

#[tokio::test]
async fn the_wall_driver_runs_sources_beside_submissions_and_answers_the_reply() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let wall: Arc<dyn Clock> = Arc::new(WallClock);
    let e = Engine::open(j.clone(), wall.clone(), vec![Registration::new("loop", Loop)], 1).unwrap();
    let (s, reply) = Submit::with_reply("from-source", "s", EventKind::new("work", "w"));
    let mut sources = Sources::default();
    sources.register(Box::new(ScriptedSource::new("script", wall.clone(), vec![(0, s)]))).unwrap();
    let (tx, rx) = mpsc::channel(4);
    tx.send(sub("from-channel", "ping", "p")).await.unwrap();
    drop(tx);
    let opts = ServeOpts { pool: Some(Pool::new(2, 8)), ..ServeOpts::default() };
    tokio::time::timeout(Duration::from_secs(20), drive(Driver::Wall(wall), e, Arc::new(Jobs::default()), PoolCaps::default(), rx, sources, opts)).await.expect("drains").unwrap();
    assert_eq!(reply.await, Ok(Ok(())));
    assert_eq!(bodies(&*j, "worked").len(), 1);
    assert_eq!(bodies(&*j, "pong").len(), 1);
}

#[tokio::test]
async fn the_virtual_driver_takes_queued_submissions_and_stops_at_its_horizon() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let (tx, rx) = mpsc::channel(4);
    tx.send(sub("t", "tick", "")).await.unwrap();
    drive(Driver::Virtual { clock: clock.clone(), until_ms: 2 * DAY }, engine(j.clone(), &clock, 1), Arc::new(Jobs::default()), 1, rx, Sources::default(), ServeOpts::default()).await.unwrap();
    assert_eq!(bodies(&*j, "ticked").len(), 3);
    assert_eq!(clock.now_ms(), 2 * DAY);
}
