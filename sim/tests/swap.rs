//! The swap test: the fixture is the trace the simulator's own loop produced for this scenario
//! and seed, recorded before the loop was replaced by the runtime scheduler.

use sim::{EventKind, Outcome, Sim};

const FIXTURE: &str = include_str!("fixtures/swap-seed-42.trace");
const SEED: u64 = 42;

fn scenario(seed: u64) -> Sim {
    let mut sim = Sim::new(seed);
    for (n, staged) in [("a", false), ("b", true), ("c", false), ("d", true)] {
        sim.add_actor(
            n,
            staged,
            Box::new(move |world, rng| Outcome {
                writes: vec![(format!("k{}", rng.next_u64() % 3), format!("{n}{}", world.len()))],
                duration: rng.range(1, 60),
            }),
        );
        for i in 0..6 {
            let at = sim.rng.range(0, 25) + i * 30;
            sim.schedule_due(at, n);
        }
        sim.schedule_due(10, n);
    }
    sim.add_scenario(
        "probe",
        Box::new(|sim| {
            let seen = sim.world.len() as u64;
            sim.schedule_due(sim.now + seen, "a");
            if sim.now < 120 {
                sim.schedule_step(sim.now + 40, "probe");
            }
            sim.schedule(sim.now + 3, EventKind::Fault);
        }),
    );
    sim.schedule_step(5, "probe");
    sim.schedule_step(5, "probe");
    sim
}

pub fn render(sim: &Sim) -> String {
    sim.trace.iter().map(|t| format!("{} {:?} {}\n", t.time, t.kind, t.coalesced)).collect()
}

#[test]
fn the_trace_after_the_swap_is_the_trace_before_it() {
    let mut sim = scenario(SEED);
    sim.run();
    let got = render(&sim);
    if std::env::var_os("SIM_BLESS_SWAP").is_some() {
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/swap-seed-42.trace"), &got).unwrap();
    }
    assert_eq!(got, FIXTURE);
    assert!(got.lines().count() > 50, "scenario too small to mean anything");
    assert!(got.contains("true"), "scenario must exercise coalescing");
}
