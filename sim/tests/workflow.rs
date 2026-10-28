use runtime::{Command, Ctx, EffectSpec, Event, EventKind as K, Handler, Projection, Registration, StreamKey, EFFECT_SUCCEEDED};
use sim::{Outcome, Sim};

struct Heartbeat;

impl Handler for Heartbeat {
    fn interests(&self) -> &[&'static str] {
        &["beat"]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let n = view.count(&ev.stream, "beat");
        let mut out = vec![Command::Emit(K::new("beaten", &ctx.now().to_string()))];
        if n < 5 {
            out.push(Command::Schedule { at: ctx.now() + 60_000, kind: K::new("beat", "") });
        }
        out
    }
}

struct Build;

impl Handler for Build {
    fn interests(&self) -> &[&'static str] {
        &["build", EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "build" => vec![Command::Effect(EffectSpec { name: "compile".into(), input: String::new(), deadline_ms: 10_000 })],
            _ => vec![Command::Emit(K::new("built", ""))],
        }
    }
}

#[test]
fn a_handler_workflow_runs_on_virtual_time_with_no_actor_launched() {
    let mut sim = Sim::new(9);
    sim.add_workflow(Registration::new("heartbeat", Heartbeat));
    sim.submit(0, "hb-0", "hb", "beat", "");
    sim.run();
    let view = sim.workflow_projection().unwrap();
    let key = StreamKey("hb".into());
    assert_eq!(view.count(&key, "beat"), 5);
    let times: Vec<_> = view.events(&key).iter().filter(|e| e.kind.tag == "beaten").map(|e| e.kind.body.clone()).collect();
    assert_eq!(times, ["0", "60000", "120000", "180000", "240000"]);
    assert!(sim.trace.iter().all(|t| !matches!(t.kind, sim::EventKind::Due(_))), "nothing may be launched");
}

#[test]
fn an_effect_launches_the_process_actor_of_its_name_and_its_outcome_returns_as_an_event() {
    let mut sim = Sim::new(3);
    sim.add_actor("compile", false, Box::new(|_, _| Outcome { writes: vec![("out".into(), "ok".into())], duration: 400 }));
    sim.add_workflow(Registration::new("build", Build));
    sim.submit(100, "b-1", "b", "build", "");
    sim.run();
    let view = sim.workflow_projection().unwrap();
    let key = StreamKey("b".into());
    assert_eq!(view.count(&key, "built"), 1);
    assert_eq!(sim.read("out"), Some("ok"));
    let done = view.events(&key).iter().find(|e| e.kind.tag == EFFECT_SUCCEEDED).unwrap();
    assert_eq!(done.at, 500);
}

#[test]
fn the_same_seed_gives_the_same_workflow_trace() {
    let run = |seed| {
        let mut sim = Sim::new(seed);
        sim.add_actor("compile", false, Box::new(|_, rng| Outcome { writes: vec![], duration: rng.range(1, 900) }));
        sim.add_workflow(Registration::new("build", Build));
        for i in 0..4 {
            sim.submit(i * 10, &format!("b-{i}"), &format!("s{i}"), "build", "");
        }
        sim.run();
        sim.trace_signature()
    };
    assert_eq!(run(5), run(5));
}
