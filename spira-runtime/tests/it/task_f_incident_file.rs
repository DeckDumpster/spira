//! `incident.file` on the engine in virtual time: nothing ticks it but a change under
//! `incidents/`, a re-pin, or the time a row's cause moves on its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use runtime::changes::ScriptedChanges;
use runtime::ports::VirtualClock;
use runtime::taskrig::effects_runnable;
use runtime::{Engine, Executor, MemJournal, Outcome, StoreChanged, Wake, STORE_STREAM};
use spira_runtime::tasks::f::incident_file::*;
use spira_runtime::tasks::{self, f, Reads};
use spira_store::incident::{Claim, Row, State};

const MODULE: &str = include_str!("../../src/tasks/f/incident_file.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];
const HOUR_MS: u64 = 3_600_000;

struct Rig {
    engine: Engine,
    clock: VirtualClock,
    changes: Arc<ScriptedChanges>,
    looks: usize,
    answered: std::collections::BTreeSet<String>,
}

impl Rig {
    /// The task as the service hosts it, booted, its first look answered with `first`.
    fn booted(first: Snapshot) -> Rig {
        let row = tasks::all().find(|t| t.name == NAME).expect("the row is in tasks::all()");
        let reg = IncidentFile { config: BTreeMap::new() }.registration().with_wake(Wake { reads: row.reads.prefixes().to_vec(), min_gap_ms: CADENCE_MS, polls: false });
        let clock = VirtualClock::new(0);
        let changes = Arc::new(ScriptedChanges::default());
        changes.set(1);
        let mut engine = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![reg], 1).unwrap().with_sweep(60_000, changes.clone());
        let (_, stream, kind) = (row.boot)();
        engine.start(stream, kind);
        let mut rig = Rig { engine, clock, changes, looks: 0, answered: Default::default() };
        assert_eq!(rig.settle(&first), 1, "the boot tick looks once");
        rig
    }

    /// Runs what is due now, answering each new look with `snap`; how many looks that made.
    fn settle(&mut self, snap: &Snapshot) -> usize {
        let before = self.looks;
        loop {
            self.engine.run_until_idle().unwrap();
            let Some(p) = self.engine.pending_effects().into_iter().find(|p| p.spec.name == OBSERVE && !self.answered.contains(&format!("{:?}", p.key))) else { break };
            self.answered.insert(format!("{:?}", p.key));
            self.looks += 1;
            assert!(self.engine.complete(&p.key, Outcome::Succeeded(serde_json::to_string(snap).unwrap())).unwrap());
        }
        self.looks - before
    }

    /// Moves the clock to `at`, stopping at every timer on the way; the looks made by then.
    fn advance_to(&mut self, at: u64, snap: &Snapshot) -> usize {
        let mut looks = 0;
        loop {
            let now = self.engine.now_ms();
            let next = self.engine.next_timer_at().filter(|t| *t <= at).unwrap_or(at);
            self.clock.advance(next.saturating_sub(now));
            looks += self.settle(snap);
            if next >= at {
                return looks;
            }
        }
    }

    fn changed(&mut self, id: &str, key: &str) {
        let rev = 2;
        self.changes.set(rev);
        self.engine.submit(id, STORE_STREAM, StoreChanged { rev, keys: vec![key.into()], reload: false }.kind()).unwrap();
    }

    fn runs_asked(&self) -> usize {
        self.engine.pending_effects().iter().filter(|p| p.spec.name == RUN).count()
    }
}

fn waiting(state: State, retry_at: Option<i64>) -> Snapshot {
    let claim = (state == State::Filing).then(|| Claim { by: "r".into(), until: 600, events: 1 });
    Snapshot { rows: vec![Waiting { row: Row { state, retry_at, claim, lanes: Some("plan".into()), events: 1, ..Row::default() }, lanes_now: Some("plan".into()) }] }
}

#[test_id(tk41b9f)]
#[test]
fn the_task_reads_only_the_incident_rows_and_the_drain_is_gone() {
    let row = tasks::all().find(|t| t.name == NAME).expect("the row is in tasks::all()");
    assert_eq!((row.stream(), row.reads), (STREAM, Reads::Only(&["incidents/"])));
    assert!(tasks::watched_prefixes().contains(&"incidents/"));
    assert!(!tasks::all().chain(tasks::pending()).any(|t| t.name == "incident.drain"));
}

#[test_id(tragl6k)]
#[test]
fn an_idle_hour_makes_no_tick() {
    let idle = Snapshot::default();
    let mut rig = Rig::booted(idle.clone());
    assert_eq!(rig.advance_to(HOUR_MS, &idle), 0);
}

#[test_id(tqxyjzh)]
#[test]
fn a_change_under_incidents_makes_one_tick_and_any_other_none() {
    let idle = Snapshot::default();
    let mut rig = Rig::booted(idle.clone());
    rig.clock.advance(5_000);
    rig.engine.submit("other", STORE_STREAM, StoreChanged { rev: 1, keys: vec!["lifecycle/sp-a".into()], reload: false }.kind()).unwrap();
    assert_eq!(rig.settle(&idle), 0);
    rig.changed("raised", "incidents/incident:a");
    assert_eq!(rig.settle(&idle), 1);
}

#[test_id(tbkw432)]
#[test]
fn an_unread_row_is_looked_at_again_at_its_retry_time_and_not_before() {
    let unread = waiting(State::Unread, Some(120));
    let mut rig = Rig::booted(unread.clone());
    assert_eq!(rig.advance_to(119_999, &unread), 0);
    assert_eq!(rig.runs_asked(), 0);
    assert_eq!(rig.advance_to(120_000, &unread), 1);
    assert_eq!(rig.runs_asked(), 1);
}

#[test_id(tmmzqo1)]
#[test]
fn a_pending_row_asks_for_one_pass_and_a_row_waiting_on_its_cause_for_none() {
    assert_eq!(Rig::booted(waiting(State::Pending, None)).runs_asked(), 1);
    assert_eq!(Rig::booted(waiting(State::Refused, None)).runs_asked(), 0);
    assert_eq!(decide(&waiting(State::Filing, None), 599_000), Decision::Wait(Some(600_000)));
    assert_eq!(decide(&waiting(State::Filing, None), 600_000), Decision::File);
    assert_eq!(decide(&waiting(State::Refused, None), HOUR_MS), Decision::Wait(None));
}

#[test_id(t4bgzl6)]
#[test]
fn another_look_follows_a_pass_only_when_it_moved_a_row_or_the_store_refused_it() {
    assert_eq!(after_pass(&Passed { claimed: 1, ..Passed::default() }, 10), Some(10 + CADENCE_MS));
    assert_eq!(after_pass(&Passed { failed: 1, ..Passed::default() }, 10), Some(60_010));
    assert_eq!(after_pass(&Passed::default(), 10), None);
}

#[test_id(tynvz57)]
#[test]
fn a_repository_map_told_unchanged_or_a_re_pinned_batch_wakes_the_task_to_look_again() {
    use spira_runtime::repin::Repin;
    use spira_runtime::status::Control;
    let idle = Snapshot::default();
    let mut rig = Rig::booted(idle.clone());
    let batch = Arc::new(std::sync::Mutex::new(None::<String>));
    let base = spira_runtime::fixture::lookup("7");
    let held = base(intake_batch_key()).unwrap();
    let lookup = Arc::new(spira_runtime::fixture::lookup("7"));
    let file = batch.clone();
    let fresh = Arc::new(move |keys: &[String]| Ok(keys.iter().map(|k| (k.clone(), file.lock().unwrap().clone().filter(|_| k == intake_batch_key()).unwrap_or_else(|| base(k).unwrap()))).collect()));
    let settings = spira_runtime::Settings::from_lookup(&spira_runtime::fixture::lookup("7")).unwrap();
    let repin = Arc::new(Repin::new(settings, fresh, lookup));
    let submitted = Arc::new(std::sync::Mutex::new(0));
    let count = submitted.clone();
    let submit: spira_runtime::status::SubmitFn = Arc::new(move |_: &str, _: &str, _: runtime::EventKind| {
        *count.lock().unwrap() += 1;
        Ok(())
    });
    let control = Control::new(Arc::default(), submit, Arc::new(rig.clock.clone())).with_repin(repin.clone());
    let told = |key: &str| control.respond(&["scheduler", "repin", key].map(String::from)).unwrap();

    repin.apply(&mut rig.engine);
    assert_eq!(rig.settle(&idle), 0, "nothing told, nothing woken");

    rig.clock.advance(5_000);
    assert!(told(MAP_KEY).contains("unchanged"));
    assert_eq!(*submitted.lock().unwrap(), 1, "the host is given a turn to act on it");
    repin.apply(&mut rig.engine);
    assert_eq!(rig.settle(&idle), 1);

    rig.clock.advance(5_000);
    *batch.lock().unwrap() = Some(format!("{held}1"));
    assert!(told(intake_batch_key()).contains("pending"));
    repin.apply(&mut rig.engine);
    assert_eq!(rig.settle(&idle), 1);
}

fn intake_batch_key() -> &'static str {
    incident::intake::BATCH_KEY
}

#[test_id(tjz5nxp)]
#[test]
fn every_effect_has_a_real_runner_and_a_sim_double() {
    let row = f::TASKS.iter().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS);
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&["incident.file.bogus"], &real, &sim).is_err());
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tozhlmi)]
#[test]
fn the_module_launches_no_process() {
    let launches = |src: &str| LAUNCHERS.iter().any(|l| src.contains(l));
    assert!(launches("let c = Command::new(tool);"), "the matcher finds a planted launcher");
    assert!(!launches(MODULE));
}

#[test_id(tez4pv3)]
#[test]
fn a_look_the_store_did_not_answer_is_tried_again_after_a_minute() {
    let mut rig = Rig::booted(Snapshot::default());
    rig.clock.advance(5_000);
    rig.changed("raised", "incidents/incident:a");
    rig.engine.run_until_idle().unwrap();
    let look = rig.engine.pending_effects().into_iter().find(|p| p.spec.name == OBSERVE && !rig.answered.contains(&format!("{:?}", p.key))).expect("a look");
    rig.answered.insert(format!("{:?}", look.key));
    assert!(rig.engine.complete(&look.key, Outcome::Failed("the views are stale".into())).unwrap());
    rig.engine.run_until_idle().unwrap();
    assert_eq!(rig.advance_to(64_999, &Snapshot::default()), 0);
    assert_eq!(rig.advance_to(65_000, &Snapshot::default()), 1);
}
