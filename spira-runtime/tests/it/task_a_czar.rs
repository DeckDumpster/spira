//! Acceptance T1–T6 for `czar`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/czar.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide` and `advance` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven).

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: czar,
    registration: Czar::new(policy(Stage::Shadow)).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: 30_000,
    test_ids: [ #[test_id(t53b4ab)] #[test_id(tiq40zc)] #[test_id(tbu57i3)] #[test_id(tjtsx3i)] #[test_id(tnrwnsk)] ],
}

const NOW: u64 = 1_793_000_000_000;

/// Test thresholds, none of them a shipped value; every class at `stage`.
fn policy(stage: Stage) -> Policy {
    Policy {
        stages: CLASSES.iter().chain(&["starved"]).map(|c| (c.to_string(), stage)).collect(),
        loop_stall_secs: 900,
        ci_queued_max_secs: 1_800,
        ci_red_max_secs: 3_600,
        base_ci_unreadable_grace_secs: 600,
        reconciler_grace_secs: 600,
        queue_round_min_n: 2,
        queue_round_stall_secs: 1_800,
    }
}

/// The contract's idle fixture: every key satisfied, no open batch, a fresh landing pass.
fn idle() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![]),
        landing_task: Read::Observed(LandingTask { last_fired_at_ms: Some(NOW - 60_000), last_outcome: Some("quiet".into()), ..LandingTask::default() }),
        open_batch: [("spira".to_string(), Read::Observed(None))].into(),
        certified: [("spira".to_string(), Read::Observed(vec![]))].into(),
        open_rounds: [("spira".to_string(), Read::Observed(0))].into(),
        batcher_held: Default::default(),
        ci: [("spira".to_string(), Read::Observed(Ci::default()))].into(),
        base_ci: [("spira".to_string(), Read::Observed(BaseCi { conclusion: Some("success".into()), head_sha: "b1".into(), ..BaseCi::default() }))].into(),
        reconciler_last_pass_ms: Read::Observed(Some(NOW - 30_000)),
        now_ms: NOW,
        gh_budget: None,
        mark: Mark::default(),
    }
}

/// `no suites identified; leaving batch open` with the batch still open: a deadlock.
fn busy() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![LandingEvent { kind: "verdict.deadlock".into(), repo: "spira".into(), at_ms: NOW - 10_000 }]),
        open_batch: [("spira".to_string(), Read::Observed(Some(Batch { pr: Some(41), branch: "spira/batch-1".into(), members: vec!["sp-a".into(), "sp-b".into()] })))].into(),
        ..idle()
    }
}

#[test_id(to9ay33)]
#[test]
fn t3_every_rung_of_the_ladder_is_taken() {
    for (from, to) in LADDER {
        assert_eq!(advance("deadlock", *from, *to), Ok(*to), "{from:?} → {to:?}");
    }
}

#[test_id(tx13oy0)]
#[test]
fn t3_inferred_without_a_gap_is_refused_naming_the_key() {
    let refused = advance("ci-red:spira", Phase::Satisfied, Phase::Inferred).expect_err("inferred needs a gap first");
    assert!(refused.contains("ci-red:spira"), "{refused}");
}

#[test_id(tmb16zx)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(30_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `cp:312-320`: in shadow a deadlock is a `CZAR-WOULD` line, never an incident.
#[test_id(t39hxdm)]
#[test]
fn t5_a_shadow_deadlock_is_a_would_line_and_files_nothing() {
    let (carry, acts) = decide(&policy(Stage::Shadow), &Carry::default(), &busy());
    assert_eq!(carry.keys.get("deadlock").map(|k| k.phase), Some(Phase::Gap));
    assert!(acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("CZAR-WOULD")), "{acts:?}");
    assert!(acts.iter().all(|a| a.effect != FILE_INCIDENT), "{acts:?}");
}

/// `cp:332-343`: in act a deadlock files one incident keyed `incident:queue-<ref>`.
#[test_id(txbknif)]
#[test]
fn t5_an_act_deadlock_files_one_incident_by_its_ref() {
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &busy());
    let filed: Vec<&Act> = acts.iter().filter(|a| a.effect == FILE_INCIDENT).collect();
    assert_eq!(filed.len(), 1, "{acts:?}");
    assert!(filed[0].key.starts_with("incident:queue-"), "{:?}", filed[0]);
}

/// `cp:369-395`: a red base files P0 against the red repo, keyed `incident:base-red:<repo>:<suites>`.
#[test_id(tbge2hl)]
#[test]
fn t5_a_red_base_files_base_red_against_the_repo() {
    let red = Snapshot { base_ci: [("spira".to_string(), Read::Observed(BaseCi { conclusion: Some("failure".into()), head_sha: "b2".into(), run_url: Some("u".into()), red_suites: vec!["lint".into()], ..BaseCi::default() }))].into(), ..idle() };
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &red);
    assert!(acts.iter().any(|a| a.effect == FILE_BASE_RED && a.key == "incident:base-red:spira:lint"), "{acts:?}");
}

/// sp-pu7v6: an unreadable field is never satisfied.
#[test_id(t8iccjd)]
#[test]
fn t5_an_unobservable_ci_is_not_satisfied() {
    let blind = Snapshot { ci: [("spira".to_string(), Read::Unobservable("forge timed out".into()))].into(), ..idle() };
    let (carry, _) = decide(&policy(Stage::Shadow), &Carry::default(), &blind);
    assert_ne!(carry.keys.get("ci-stalled:spira").map(|k| k.phase), Some(Phase::Satisfied));
}

/// law-lifecycle-state-is-not-text (sp-mijstw): the certified pool counts only unheld rows, and a
/// hold written as the JSON-encoded string spira-lc may emit is still a hold.
#[test_id(t27h598)]
#[test]
fn t7_a_hold_in_string_form_keeps_its_row_out_of_the_certified_pool() {
    let list = r#"[{"bead_id":"sp-a","state":"CERTIFIED","holds":[]},{"bead_id":"sp-b","state":"CERTIFIED","holds":"[\"poison\"]"},{"bead_id":"sp-c","state":"CERTIFIED","holds":["ask"]},{"bead_id":"sp-d","state":"CERTIFIED","holds":"[]"}]"#;
    assert_eq!(unheld_ids(list), Ok(vec!["sp-a".to_string(), "sp-d".to_string()]));
    assert!(unheld_ids("cannot tell: deadline").is_err());
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(30_000).unwrap();
    r
}

#[test_id(tapo3mb)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&at_work(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("keys/deadlock")), "{rows:?}");
    assert!(rows.iter().any(|k| k == "cursor_ms"), "{rows:?}");
}

#[test_id(tgxe5sh)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

/// A spent GitHub budget is one page with its reset and the top spender, not one per repository.
#[test_id(thh57vh)]
#[test]
fn a_spent_budget_files_one_page_naming_the_reset_and_the_top_spender() {
    let blind = |repo: &str| (repo.to_string(), Read::Unobservable("forge exited 1".into()));
    let spent = Snapshot {
        base_ci: [blind("spira"), blind("other")].into(),
        gh_budget: Some(SpentBudget { reset: NOW / 1000 + 600, top_spender: Some(("forge:pr-state".into(), 900)) }),
        ..idle()
    };
    let p = Policy { base_ci_unreadable_grace_secs: 0, ..policy(Stage::Act) };
    let (_, acts) = decide(&p, &Carry::default(), &spent);
    let filed: Vec<&Act> = acts.iter().filter(|a| a.effect == FILE_BASE_RED).collect();
    assert_eq!(filed.len(), 1, "{acts:?}");
    let text = format!("{:?}", filed[0]);
    assert!(text.contains("forge:pr-state") && text.contains(&spira_runtime::api_budget::clock_of(NOW / 1000 + 600)), "{text}");
}

const LOG_PASS: &str = "2026-11-20T10:00:00Z landing: pass complete";
const LOG_SORT: &str = "queue_sort_rows: ranking failed";

fn stamped(secs_into_day: u32, text: &str) -> String {
    format!("2026-11-21T{:02}:{:02}:{:02}Z {text}", secs_into_day / 3600, secs_into_day / 60 % 60, secs_into_day % 60)
}

/// One observe over `run`, fed through `decide` as the scheduler does; returns the next carry and what it saw.
fn pass(run: &std::path::Path, carry: &Carry) -> (Carry, Snapshot) {
    let e = spira_runtime::Effects { world: spira_runtime::world::World { run_dir: run.to_path_buf(), ..Default::default() }, ..Default::default() };
    let input = serde_json::to_string(&ObserveIn { carry: carry.clone(), pinned: Default::default() }).unwrap();
    let snap: Snapshot = serde_json::from_str(&observe(&e, &input).unwrap()).unwrap();
    let snap = Snapshot { open_batch: idle().open_batch, certified: idle().certified, ..snap };
    (decide(&policy(Stage::Shadow), carry, &snap).0, snap)
}

fn events(s: &Snapshot) -> Vec<LandingEvent> {
    match &s.landing_events {
        Read::Observed(e) => e.clone(),
        other => panic!("{other:?}"),
    }
}

#[test_id(tvmfuis)]
#[test]
fn an_unchanged_log_is_not_read_again_even_when_a_line_changed_in_place() {
    let dir = testkit::TempDir::new("czar-log-unchanged");
    let log = dir.path().join("landing.log");
    let old = stamped(100, LOG_SORT);
    std::fs::write(&log, format!("{old}\n{LOG_PASS}\n")).unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    assert_eq!(events(&first).len(), 1);
    let newer = stamped(200, LOG_SORT);
    assert_eq!(newer.len(), old.len());
    let f = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
    std::os::unix::fs::FileExt::write_all_at(&f, newer.as_bytes(), 0).unwrap();
    let (_, second) = pass(dir.path(), &carry);
    assert!(events(&second).is_empty(), "{second:?}");
}

#[test_id(t261hfa)]
#[test]
fn an_appended_event_is_reported_once_and_an_unterminated_one_only_when_completed() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-append");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, format!("{LOG_PASS}\n")).unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    let line = stamped(300, LOG_SORT);
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    f.write_all(line.as_bytes()).unwrap();
    let (carry, partial) = pass(dir.path(), &carry);
    assert!(events(&partial).is_empty(), "{partial:?}");
    f.write_all(b"\n").unwrap();
    let (carry, done) = pass(dir.path(), &carry);
    assert_eq!(events(&done).len(), 1, "{done:?}");
    let (_, again) = pass(dir.path(), &carry);
    assert!(events(&again).is_empty(), "{again:?}");
}

#[test_id(t8p276x)]
#[test]
fn a_truncated_or_replaced_log_is_read_again_from_the_start() {
    let dir = testkit::TempDir::new("czar-log-rotate");
    let log = dir.path().join("landing.log");
    let filler = "x".repeat(200);
    std::fs::write(&log, format!("{}\n{filler}\n{filler}\n", stamped(100, LOG_SORT))).unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    std::fs::write(&log, format!("{}\n", stamped(200, LOG_SORT))).unwrap();
    let (carry, truncated) = pass(dir.path(), &carry);
    assert_eq!(events(&truncated).len(), 1, "{truncated:?}");
    let next = dir.path().join("landing.log.new");
    std::fs::write(&next, format!("{}\n", stamped(300, LOG_SORT))).unwrap();
    assert!(std::fs::metadata(&next).unwrap().len() <= std::fs::metadata(&log).unwrap().len(), "only the inode may tell");
    std::fs::rename(&next, &log).unwrap();
    let (_, replaced) = pass(dir.path(), &carry);
    let seen = events(&replaced);
    assert_eq!(seen.len(), 1, "{replaced:?}");
    assert!(seen[0].at_ms > events(&truncated)[0].at_ms);
}

#[test_id(tnidode)]
#[test]
fn a_missing_log_is_no_events_and_an_empty_mark() {
    let dir = testkit::TempDir::new("czar-log-missing");
    let had = Carry { mark: Mark { dev: 1, ino: 2, offset: 9 }, ..Carry::default() };
    let (carry, snap) = pass(dir.path(), &had);
    assert!(events(&snap).is_empty());
    assert_eq!(carry.mark, Mark::default());
}

#[test_id(txh570l)]
#[test]
fn an_unreadable_log_leaves_the_mark_and_reads_unobservable() {
    let dir = testkit::TempDir::new("czar-log-unreadable");
    std::fs::create_dir(dir.path().join("landing.log")).unwrap();
    let had = Carry { mark: Mark { dev: 1, ino: 2, offset: 9 }, ..Carry::default() };
    let (carry, snap) = pass(dir.path(), &had);
    assert!(matches!(snap.landing_events, Read::Unobservable(_)), "{snap:?}");
    assert_eq!(carry.mark, had.mark);
}

#[test_id(tl8zvx8)]
#[test]
fn the_mark_is_kept_as_a_row_and_joins_back() {
    let mark = Mark { dev: 3, ino: 4, offset: 5 };
    let body = serde_json::to_string(&Carry { mark: mark.clone(), ..Carry::default() }).unwrap();
    let rows = runtime::rows::split(&body, ROW_PARTS);
    assert!(rows.contains_key("mark"), "{rows:?}");
    let back: Carry = serde_json::from_str(&runtime::rows::join(&rows, ROW_PARTS)).unwrap();
    assert_eq!(back.mark, mark);
}

#[test_id(tjl6w67)]
#[test]
fn an_event_before_rotation_is_reported_once_and_one_in_the_new_live_file_in_the_same_pass() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-rotated");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, format!("{LOG_PASS}\n")).unwrap();
    let (carry, first) = pass(dir.path(), &Carry::default());
    assert!(events(&first).is_empty());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(format!("{}\n", stamped(100, LOG_SORT)).as_bytes())
        .unwrap();
    std::fs::rename(&log, dir.path().join("landing.log.1")).unwrap();
    std::fs::write(&log, format!("{}\n", stamped(200, LOG_SORT))).unwrap();
    let (carry, second) = pass(dir.path(), &carry);
    let at: Vec<u64> = events(&second).iter().map(|e| e.at_ms).collect();
    assert_eq!(at.len(), 2, "{second:?}");
    assert!(at[0] < at[1], "{at:?}");
    let (_, third) = pass(dir.path(), &carry);
    assert!(events(&third).is_empty(), "{third:?}");
}

#[test_id(tj8b01j)]
#[test]
fn a_rotation_before_the_new_live_file_exists_loses_nothing() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-rotated-gap");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, format!("{LOG_PASS}\n")).unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(format!("{}\n", stamped(100, LOG_SORT)).as_bytes())
        .unwrap();
    std::fs::rename(&log, dir.path().join("landing.log.1")).unwrap();
    let (carry, second) = pass(dir.path(), &carry);
    assert_eq!(events(&second).len(), 1, "{second:?}");
    std::fs::write(&log, format!("{}\n", stamped(200, LOG_SORT))).unwrap();
    let (_, third) = pass(dir.path(), &carry);
    assert_eq!(events(&third).len(), 1, "{third:?}");
}

#[test_id(ty6iwfc)]
#[test]
fn a_mark_in_a_file_rotated_twice_is_followed_through_every_newer_file() {
    use std::io::Write;
    let dir = testkit::TempDir::new("czar-log-rotated-twice");
    let log = dir.path().join("landing.log");
    std::fs::write(&log, format!("{LOG_PASS}\n")).unwrap();
    let (carry, _) = pass(dir.path(), &Carry::default());
    std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{}\n", stamped(100, LOG_SORT)).as_bytes()).unwrap();
    std::fs::rename(&log, dir.path().join("landing.log.1")).unwrap();
    std::fs::write(&log, format!("{}\n", stamped(200, LOG_SORT))).unwrap();
    std::fs::rename(dir.path().join("landing.log.1"), dir.path().join("landing.log.2")).unwrap();
    std::fs::rename(&log, dir.path().join("landing.log.1")).unwrap();
    std::fs::write(&log, format!("{}\n", stamped(300, LOG_SORT))).unwrap();
    let (_, second) = pass(dir.path(), &carry);
    assert_eq!(events(&second).len(), 3, "{second:?}");
}

mod landing_task {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use runtime::engine::{Engine, Executor, Outcome};
    use runtime::event::*;
    use runtime::journal::MemJournal;
    use runtime::ports::{Clock, VirtualClock};
    use spira_runtime::tasks::a::landing::{self, Landing};

    use super::*;

    const HOUR_MS: u64 = 3_600_000;

    /// The real `Landing`, which a switch can stop hearing its events: a scheduler that no longer fires it.
    struct Switchable(Landing, Arc<AtomicBool>);
    impl Handler for Switchable {
        fn interests(&self) -> &[EventKindTag] {
            if self.1.load(Ordering::SeqCst) { self.0.interests() } else { &[] }
        }
        fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
            self.0.handle(ctx, view, ev)
        }
    }

    struct World {
        engine: Engine,
        clock: VirtualClock,
        firing: Arc<AtomicBool>,
        landing_answers: Result<String, String>,
        inferences: usize,
    }

    impl World {
        fn new() -> World {
            let clock = VirtualClock::new(0);
            let firing = Arc::new(AtomicBool::new(true));
            let regs = vec![
                Registration::new(landing::NAME, Switchable(Landing::new(BTreeMap::new()), firing.clone())).with_mode(Mode::Act),
                Czar::new(policy(Stage::Act)).registration().with_mode(Mode::Act),
            ];
            let mut engine = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap();
            let (_, stream, kind) = Landing::boot();
            engine.start(stream, kind);
            let (_, stream, kind) = Czar::boot();
            engine.start(stream, kind);
            World { engine, clock, firing, landing_answers: Ok(serde_json::to_string(&spira_runtime::tasks::a::landing::Snapshot::default()).unwrap()), inferences: 0 }
        }

        fn czar_reads(&self) -> String {
            let doc = spira_runtime::status::document(&self.engine.status()).to_string();
            serde_json::to_string(&Snapshot { landing_task: landing_task(&doc), now_ms: self.clock.now_ms(), ..idle() }).unwrap()
        }

        fn answer_everything_pending(&mut self) {
            loop {
                self.engine.run_until_idle().unwrap();
                let pending = self.engine.pending_effects();
                if pending.is_empty() {
                    return;
                }
                for p in pending {
                    let outcome = match p.spec.name.as_str() {
                        landing::OBSERVE => match &self.landing_answers {
                            Ok(body) => Outcome::Succeeded(body.clone()),
                            Err(why) => Outcome::Failed(why.clone()),
                        },
                        OBSERVE => Outcome::Succeeded(self.czar_reads()),
                        FILE_INCIDENT => {
                            self.inferences += p.spec.key.contains("loop-stalled") as usize;
                            Outcome::Succeeded("{}".into())
                        }
                        _ => Outcome::Succeeded("{}".into()),
                    };
                    self.engine.complete(&p.key, outcome).unwrap();
                }
            }
        }

        fn run_for(&mut self, ms: u64) {
            let target = self.clock.now_ms() + ms;
            self.answer_everything_pending();
            while let Some(at) = self.engine.next_timer_at().filter(|&t| t <= target) {
                self.clock.advance_to(at);
                self.answer_everything_pending();
            }
            self.clock.advance_to(target);
            self.answer_everything_pending();
        }
    }

    #[test_id(tmf9mek)]
    #[test]
    fn an_idle_hour_of_quiet_landing_fires_is_not_a_stall() {
        let mut w = World::new();
        w.run_for(2 * HOUR_MS);
        assert_eq!(w.inferences, 0);
        let doc = spira_runtime::status::document(&w.engine.status()).to_string();
        assert!(matches!(landing_task(&doc), Read::Observed(LandingTask { last_outcome: Some(ref o), .. }) if o == "quiet"), "{doc}");
    }

    #[test_id(tp67fsx)]
    #[test]
    fn a_landing_task_that_stops_firing_past_the_bound_files_one_inference() {
        let mut w = World::new();
        w.run_for(HOUR_MS / 2);
        w.firing.store(false, Ordering::SeqCst);
        w.run_for(2 * HOUR_MS);
        assert_eq!(w.inferences, 1);
    }

    #[test_id(th3ru0v)]
    #[test]
    fn a_landing_task_whose_observe_fails_files_one_inference() {
        let mut w = World::new();
        w.landing_answers = Err("store unreachable".into());
        w.run_for(2 * HOUR_MS);
        assert_eq!(w.inferences, 1);
    }

    #[test_id(tslclhb)]
    #[test]
    fn a_halted_scheduler_whose_landing_is_unanswered_files_nothing() {
        let mut w = World::new();
        w.run_for(HOUR_MS / 2);
        w.engine.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "")).unwrap();
        w.firing.store(false, Ordering::SeqCst);
        w.run_for(2 * HOUR_MS);
        assert_eq!(w.inferences, 0);
    }

    #[test_id(tyyjbe9)]
    #[test]
    fn a_halted_scheduler_is_deliberate_where_the_same_stale_row_unhalted_is_a_gap() {
        let stale = |halted| Snapshot { landing_task: Read::Observed(LandingTask { halted, last_fired_at_ms: Some(NOW - 2_000_000), ..LandingTask::default() }), ..idle() };
        let files = |snap: &Snapshot| decide(&policy(Stage::Act), &Carry::default(), snap).1.iter().any(|a| a.effect == FILE_INCIDENT && a.key.contains("loop-stalled"));
        assert!(files(&stale(false)), "the control must fire");
        assert!(!files(&stale(true)));
        let no_row = Snapshot { landing_task: Read::Unobservable("no landing row".into()), ..idle() };
        assert!(!files(&no_row), "an unreadable row is not a stall");
    }
}

#[test_id(t96itjb)]
#[test]
fn an_observe_with_no_pinned_scheduler_socket_cannot_see_the_landing_task() {
    let dir = testkit::TempDir::new("czar-no-socket");
    let (_, snap) = pass(dir.path(), &Carry::default());
    assert!(matches!(snap.landing_task, Read::Unobservable(_)), "{snap:?}");
}
