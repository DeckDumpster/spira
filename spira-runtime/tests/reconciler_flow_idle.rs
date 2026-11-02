//! A flow pass whose verdicts did not change leaves the journal alone and launches no program.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{EffectSpec, Engine, Executor, Journal, MemJournal, Outcome};
use spira_runtime::watchers::reconciler_flow::{ReconcilerFlow, MAIL, OBSERVE, STATE, STATUS};
use spira_runtime::Effects;

#[path = "common/flow.rs"]
mod flow;
use flow::*;

/// Counts each effect the runtime asks for before running it.
struct Counting {
    inner: Effects,
    asked: Mutex<BTreeMap<String, u64>>,
}

impl EffectRunner for Counting {
    fn run(&self, key: &str, spec: &EffectSpec) -> Result<String, String> {
        *self.asked.lock().unwrap().entry(spec.name.clone()).or_default() += 1;
        self.inner.run(key, spec)
    }
}

struct Rig {
    flow: Flow,
    engine: Engine,
    journal: Arc<MemJournal>,
    clock: VirtualClock,
    fx: Counting,
    seq: Arc<AtomicU64>,
}

impl Rig {
    fn build() -> Rig {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let flow = Flow::build("idle", Scenario::Quiet, now);
        let seq = Arc::new(AtomicU64::new(LC_SEQ));
        let sock = flow.serve_lc(seq.clone());
        let exporter = flow.counting_exporter();
        let inner = Effects { world: flow.world(&sock, &exporter), concierge_mail: Some(Arc::new(spira_runtime::concierge_mail(mail_env(flow.dir.path())))), ..Effects::default() };
        let clock = VirtualClock::new(now * 1000);
        let journal = MemJournal::new();
        let mut engine = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![handler().registration()], 0).unwrap();
        let (id, stream, kind) = ReconcilerFlow::boot();
        engine.submit(id, stream, kind).unwrap();
        Rig { flow, engine, journal, clock, fx: Counting { inner, asked: Mutex::new(BTreeMap::new()) }, seq }
    }

    fn pass(&mut self, n: u64) {
        self.flow.sentinel_pass(&format!("p{n}"), 30, self.flow.now + n * INTERVAL_SECS - 60);
        self.engine.run_until_idle().unwrap();
        loop {
            let pending = self.engine.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match self.fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => panic!("effect {} failed: {d}", p.spec.name),
                };
                self.engine.complete(&p.key, out).unwrap();
            }
            self.engine.run_until_idle().unwrap();
        }
        self.clock.advance(INTERVAL_SECS * 1000);
    }

    fn journaled(&self) -> usize {
        self.journal.read_from(0, 100_000).unwrap().len()
    }

    fn states(&self) -> usize {
        self.journal.read_from(0, 100_000).unwrap().iter().filter(|(_, e)| e.kind.tag == STATE).count()
    }

    fn asked(&self, effect: &str) -> u64 {
        self.fx.asked.lock().unwrap().get(effect).copied().unwrap_or(0)
    }
}

/// A PATH of shims, one per program the oracle pass launched, each recording its launch.
fn counting_path(flow: &Flow) -> std::path::PathBuf {
    let dir = flow.p("shims");
    std::fs::create_dir_all(&dir).unwrap();
    for program in ["timeout", "date", "uname", "duckdb", "tsd-write", "tsd-lifecycle-export", "bd", "dolt", "spira-lc", "mail"] {
        testkit::write_exe(dir.join(program), &format!("#!/bin/sh\necho \"{program} $*\" >> {}\n", flow.p("launches.log").display()));
    }
    dir
}

#[test]
fn ten_quiet_passes_journal_nothing_after_the_first_state_and_launch_no_program() {
    let mut rig = Rig::build();
    let shims = counting_path(&rig.flow);
    let _path = testkit::env(&[("PATH", Some(shims.to_str().unwrap()))]);
    rig.pass(0);
    assert_eq!(rig.states(), 1, "the first pass records what every invariant read as");
    let settled = rig.journaled();
    for n in 1..10 {
        rig.pass(n);
    }
    assert_eq!(rig.asked(OBSERVE), 10, "the simulation ran ten passes");
    assert_eq!(rig.asked(STATUS), 10);
    assert_eq!(rig.asked(MAIL), 0);
    assert_eq!((rig.states(), rig.journaled()), (1, settled), "nine unchanged passes added no event");
    assert_eq!(rig.flow.exports(), 0, "the exporter was not launched");
    assert_eq!(std::fs::read_to_string(rig.flow.p("launches.log")).unwrap_or_default(), "", "no program was launched by name");
    std::process::Command::new("date").status().unwrap();
    assert_eq!(std::fs::read_to_string(rig.flow.p("launches.log")).unwrap(), "date \n", "positive control: a launch by name is counted");
}

#[test]
fn the_exporter_is_launched_once_the_lifecycle_log_is_ahead_of_it() {
    // The environment lock, held unchanged: the quiet test swaps PATH for its shims, and a launch
    // of `timeout` here while it does would run a shim instead of the exporter.
    let _env = testkit::env(&[]);
    let mut rig = Rig::build();
    rig.pass(0);
    assert_eq!(rig.flow.exports(), 0);
    rig.seq.fetch_add(1, Ordering::SeqCst);
    rig.pass(1);
    assert_eq!(rig.flow.exports(), 1, "the counting exporter can see a launch when there is one");
}
