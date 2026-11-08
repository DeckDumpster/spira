//! Acceptance T1–T6 for `intake.filings`, from its contract
//! (`wiki/projects/spira/designs/tasks/intake-filings.md`, Acceptance) on `TaskRig`. The row is ported (in
//! `tasks::f::TASKS`). T6 runs two virtual days (the contract asks seven).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::intake_filings::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 10 * 60_000;
const BOGUS: &str = "intake.filings.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/intake_filings.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> IntakeFilings {
    IntakeFilings { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn filing(name: &str) -> Outboxed {
    Outboxed { name: name.into(), filing: Ok(Filing { class: "bug".into(), action: "file".into(), repo: "deck".into(), title: format!("t {name}"), body: "b".into() }) }
}

fn snap(outbox: Vec<Outboxed>, open: &[(&str, bool)]) -> Snapshot {
    Snapshot {
        hosts: vec![Host { name: "h".into(), ssh: "u@h".into(), home: "/home/u".into() }],
        outbox: [("h".to_string(), Ok(outbox))].into(),
        open: open.iter().map(|(id, o)| (id.to_string(), Ok(*o))).collect(),
    }
}

fn names(acts: &[runtime::EffectSpec]) -> Vec<&str> {
    acts.iter().map(|a| a.name.as_str()).collect()
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = IntakeFilings::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::f::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_an_empty_outbox_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(vec![], &[]));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_seen_applied_acked_is_legal_and_acking_an_unapplied_filing_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(FilingState::Seen, "ack").expect_err("a filing not applied is never acknowledged");
    assert!(refused.contains("Seen"), "{refused}");
}

#[test]
fn t3_an_apply_is_recorded_before_the_ack_and_reads_back_as_recorded() {
    let (next, acts) = decide(&Carry::default(), &snap(vec![filing("f1.json")], &[]));
    assert_eq!(names(&acts), [FILE]);
    assert!(next.applied.contains("h/f1.json"), "the apply is carried: {next:?}");
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next);
}

#[test]
fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
    let row = f::TASKS.iter().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS, "the row declares the module's effects");
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&[BOGUS], &real, &sim).is_err(), "the planted {BOGUS} must be refused");
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn t4_the_module_launches_no_process() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("{l}(\"x\")")), Some(*l), "the scan must see {l}");
    }
    assert_eq!(launcher(MODULE), None);
}

#[test]
fn t5_parity_an_applied_filing_is_never_applied_twice() {
    let carry = Carry { applied: ["h/f1.json".to_string()].into(), ..Carry::default() };
    let (_, acts) = decide(&carry, &snap(vec![filing("f1.json")], &[]));
    assert_eq!(names(&acts), [ACK], "only acknowledged");
}

#[test]
fn t5_parity_a_closed_bead_gets_a_new_bead_and_an_open_one_an_append() {
    let carry = Carry { beads: [("h/bug".to_string(), "sp-1".to_string())].into(), ..Carry::default() };
    let (_, acts) = decide(&carry, &snap(vec![filing("f2.json")], &[("sp-1", false)]));
    assert_eq!(names(&acts), [FILE]);
    assert!(acts[0].input.contains("Collected from h: f2.json"), "{}", acts[0].input);
    let (_, acts) = decide(&carry, &snap(vec![filing("f2.json")], &[("sp-1", true)]));
    assert_eq!(names(&acts), [APPEND]);
}

#[test]
/// One new filing every 200 min, so the contract's 2 per filing plus 1 per hour is at most 3 an hour.
fn t6_two_virtual_days_are_deterministic_and_within_budget() {
    let world = |call: u64| if call % 7 == 3 { Snapshot { outbox: [("h".to_string(), Err("ssh: connect timed out".into()))].into(), ..snap(vec![], &[]) } } else { snap((0..call / 20).map(|n| filing(&format!("f{n}.json"))).collect(), &[]) };
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    let filings = a.requested().iter().filter(|q| q.name == FILE || q.name == APPEND).count() as u64;
    let hours = a.now().div_ceil(3_600_000);
    assert!(a.trace().len() as u64 <= 2 * filings + hours, "{} journaled for {filings} filings in {hours} h", a.trace().len());
    assert_journal_budget(&a, 3);
}
