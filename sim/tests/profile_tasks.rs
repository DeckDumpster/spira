//! Diagnostic: how many journal events each stream writes over a short virtual window.
mod common;
use std::sync::Arc;
use sim::Sim;
use spira_runtime::{registrations, tasks};
use std::sync::Mutex;
use runtime::{Command, Ctx, Event, EventKindTag, Handler, Projection, Registration};

static SPENT: Mutex<std::collections::BTreeMap<String, (u128, u64)>> = Mutex::new(std::collections::BTreeMap::new());

struct Timed(String, Box<dyn Handler>);
impl Handler for Timed {
    fn interests(&self) -> &[EventKindTag] {
        self.1.interests()
    }
    fn handle(&self, c: &Ctx, v: &Projection, e: &Event) -> Vec<Command> {
        let t = std::time::Instant::now();
        let out = self.1.handle(c, v, e);
        let mut m = SPENT.lock().unwrap();
        let x = m.entry(self.0.clone()).or_default();
        x.0 += t.elapsed().as_micros();
        x.1 += 1;
        out
    }
}

#[test]
#[ignore]
fn profile() {
    let t0 = std::time::Instant::now();
    for _ in 0..10_000 {
        std::hint::black_box(runtime::wide::rss_kb());
    }
    println!("rss_kb x10000: {:?}", t0.elapsed());
    let hours: u64 = std::env::var("H").ok().and_then(|h| h.parse().ok()).unwrap_or(1);
    let mut sim = Sim::new(3);
    for r in registrations(&common::settings()) {
        let Registration { name, handler, mode } = r;
        sim.add_workflow(Registration { name: name.clone(), handler: Box::new(Timed(name, handler)), mode });
    }
    sim.set_effects(Arc::new(sim::doubles::service()));
    for (_, stream, kind) in tasks::boots() {
        sim.boot_workflow(0, stream, &kind.tag, &kind.body);
    }
    let t = std::time::Instant::now();
    sim.run_until(hours * 3_600_000);
    let trace = sim.workflow_trace();
    let mut by: std::collections::BTreeMap<(String, String), u64> = Default::default();
    for e in &trace {
        *by.entry((e.1.clone(), e.2.clone())).or_default() += 1;
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    for x in v.iter().take(25) {
        println!("{:>7} {} {}", x.1, x.0 .0, x.0 .1);
    }
    let mut sp: Vec<_> = SPENT.lock().unwrap().clone().into_iter().collect();
    sp.sort_by_key(|x| std::cmp::Reverse(x.1 .0));
    let total: u128 = sp.iter().map(|x| x.1 .0).sum();
    println!("handler total {total}us");
    for x in sp.iter().take(12) {
        println!("{:>10}us {:>7} calls {}", x.1 .0, x.1 .1, x.0);
    }
    println!("events {} effects {} wall {:?}", trace.len(), sim.effects_run(), t.elapsed());
}
