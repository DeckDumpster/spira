//! T1–T6 for `watch.ami_stale` (contract `wiki/projects/spira/designs/tasks/watch-ami-stale.md`),
//! a once-then-clears alarm. The behaviour tests are red until the task is ported: the stub's
//! `decide` panics, the rig quarantines the stream, and `assert_healthy` turns that into the
//! failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_ami_stale::*;
use spira_runtime::watchers::INBOX_APPEND;

const STALE: &str = "round AMI ami-0abc is stale: recipe 1234abcd is not the one it was built from";

fn config() -> Config {
    Config { spill: "ec2".into(), inbox: "/q/inbox".into() }
}

fn seen(verdict: Verdict) -> Snapshot {
    Snapshot { spill: "ec2".into(), verdict }
}

fn stale(line: &str) -> Snapshot {
    seen(Verdict::Stale { line: line.into() })
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchAmiStale::boot();
    let mut r = TaskRig::new(seed, WatchAmiStale::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn said(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_five_minute_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_spill_off_and_a_fresh_image_journal_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(Snapshot { spill: String::new(), verdict: Verdict::Fresh })));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
    let mut r = rig(1, Arc::new(Mutex::new(seen(Verdict::Fresh))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

/// The latch: said once, said again only for a different line, cleared on falling back to fresh.
#[test]
fn t3_a_stale_line_is_said_once_a_new_line_again_and_fresh_clears_the_latch() {
    let w = Arc::new(Mutex::new(stale(STALE)));
    let mut r = rig(1, w.clone());
    assert_eq!((said(&r).len(), carry(&r).said.as_deref()), (1, Some(STALE)), "quiet → said(L)");
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(said(&r).len(), 1, "said(L) → said(L) records nothing");
    *w.lock().unwrap() = stale("round AMI ami-0def is stale: recipe 5678 differs");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(said(&r).len(), 2, "said(L) → said(L')");
    *w.lock().unwrap() = seen(Verdict::Fresh);
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(carry(&r).said, None, "said(L) → quiet on fresh");
    *w.lock().unwrap() = stale(STALE);
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(said(&r).len(), 3, "after clearing, the first line is said again");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_ami_stale.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.ami_stale.bogus") && !sim.contains(&"watch.ami_stale.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `spira/round-ami-stale.sh:285,296,299`, exactly.
#[test]
fn t5_the_lines_are_the_ones_round_ami_stale_wrote() {
    let r = rig(1, Arc::new(Mutex::new(stale(STALE))));
    assert_eq!(said(&r), [format!("1970-01-01T00:00:00Z [watch:round-ami-stale] {STALE}; EC2 rounds refuse the image until the Round AMI workflow builds one")]);
    let r = rig(1, Arc::new(Mutex::new(seen(Verdict::Unreadable { rc: 2, out: "ExpiredToken".into() }))));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:round-ami-stale] round AMI check could not read the images: 'round-vm ami-check' exited 2: ExpiredToken; EC2 rounds refuse the image until the Round AMI workflow builds one"]);
}

fn said_once() -> TaskRig {
    rig(1, Arc::new(Mutex::new(stale(STALE))))
}

fn registration() -> runtime::event::Registration {
    WatchAmiStale::new(config()).registration()
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&said_once(), NAME);
    assert_eq!(rows, ["said"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(said_once, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
