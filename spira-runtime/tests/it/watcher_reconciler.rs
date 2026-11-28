use std::sync::{Arc, Mutex};

use reconciler::{Check, Group, Mail, Observation, Reminder, ReminderPlan, Remedy, Rules};
use reconciler_engine::core::RawStatus;
use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::reconciler::*;

fn registration() -> runtime::Registration {
    Reconciler { interval_ms: 300_000, rules: Rules { now_secs: 0, grace_secs: 1800, effect_passes: 3, shadow_kinds: vec![] }, values: Default::default(), home: "h".into() }.registration()
}

fn gap(key: &str, remedy: Remedy) -> Group {
    Group { name: "disk".into(), checks: vec![Check { key: key.into(), raw: RawStatus::Gap { desired: "free".into(), observed: "full".into(), since_hint: None }, remedy }] }
}

fn reminder() -> Reminder {
    Reminder { marker: "susp-1".into(), subject: "suspension due".into(), why: "expires today".into(), mail: Mail { mailbox: "concierge".into(), subject: "suspension due".into(), kind: "note".into(), default: None, class: None, bead: None, body: "b".into() } }
}

/// Passes at each of `passes` over a world with one gap under `remedy` and one due reminder (which the world stops offering once told), then a world whose clock reads `next`.
fn passes_then(remedy: Remedy, passes: &[u64], next: u64) -> TaskRig {
    let now = Arc::new(Mutex::new(0u64));
    let clock = now.clone();
    let mut r = TaskRig::new(1, registration()).unwrap()
        .double(OBSERVE, move |input| {
            let asked: ObserveIn = serde_json::from_str(input).unwrap();
            let due = if asked.reminded.contains("susp-1") { vec![] } else { vec![reminder()] };
            let reminders = ReminderPlan { live: ["susp-1".to_string()].into(), due };
            Ok(serde_json::to_string(&Observation { now_secs: *clock.lock().unwrap(), groups: vec![gap("disk:/", remedy.clone())], reminders, ..Observation::default() }).unwrap())
        })
        .double(RECORD, |i| Ok(i.into()))
        .double(REMEDY, |i| Ok(i.into()))
        .double(MAIL, |i| Ok(i.into()));
    for at in passes {
        *now.lock().unwrap() = *at;
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        r.clock.advance(1);
    }
    *now.lock().unwrap() = next;
    r
}

fn escalating() -> Remedy {
    Remedy::Escalate
}

fn repairing() -> Remedy {
    Remedy::Command { program: "fix".into(), args: vec![] }
}

fn gap_inside_its_grace() -> TaskRig {
    passes_then(escalating(), &[1000], 2900)
}

fn gap_already_escalated() -> TaskRig {
    passes_then(escalating(), &[1000, 2900], 4700)
}

#[test_id(trgm48k)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&passes_then(escalating(), &[1000, 2900], 4700), NAME);
    for k in ["state/disk:/", "alerted/disk:/", "reminded"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
    let rows = kit::assert_kept_as_rows(&passes_then(repairing(), &[1000, 2900], 4700), NAME);
    assert!(rows.iter().any(|r| r == "pending/disk:/"), "{rows:?}");
}

#[test_id(t2p2nkm)]
#[test]
fn a_boot_escalates_a_gap_whose_grace_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(gap_inside_its_grace, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(twb86fk)]
#[test]
fn a_boot_does_not_escalate_a_gap_that_was_already_escalated() {
    kit::assert_boot_matches_an_unstopped_run(gap_already_escalated, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(t504lau)]
#[test]
fn a_repinned_cap_reaches_the_observation_the_next_tick_asks_for() {
    let build = |pool: &str| {
        let boot: std::collections::BTreeMap<String, String> = [("SPIRA_MAX_DWARVES".to_string(), pool.to_string())].into();
        let base = spira_runtime::fixture::lookup("7");
        let settings = spira_runtime::Settings::from_lookup(&move |k| boot.get(k).cloned().map_or_else(|| base(k), Ok)).unwrap();
        let task = spira_runtime::tasks::all().find(|t| t.name == NAME).unwrap();
        (task.build)(&settings)
    };
    let before = build("4");
    let mut rig = TaskRig::new(1, before).unwrap();
    rig.reopen(build("3")).unwrap();
    rig.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let asked = rig.requested().iter().find(|r| r.name == OBSERVE).expect("the tick asked for an observation");
    let input: ObserveIn = serde_json::from_str(&asked.input).unwrap();
    assert_eq!(input.values["SPIRA_MAX_DWARVES"], "3");
}

#[test_id(tbdjfg6)]
#[test]
fn a_fleet_gap_past_grace_wakes_the_sentinel_call_pass_instead_of_mailing() {
    let mut rig = passes_then(Remedy::CallWanted, &[1000, 2900], 4700);
    rig.run_until_idle().unwrap();
    let journal = rig.journaled();
    let woke: Vec<_> = journal.iter().filter(|e| e.stream.0 == "task.sentinel-call" && e.kind.tag == "call.wanted").collect();
    assert_eq!(woke.len(), 1, "{journal:?}");
    assert_eq!(woke[0].kind.body, r#"{"rune":"reconciler"}"#);
    assert!(!rig.requested().iter().any(|r| r.name == MAIL && r.input.contains("last remedy tried: none attempted")));
}

#[test_id(tnevcsz)]
#[test]
fn a_tick_asks_the_observation_with_the_starts_the_sentinel_has_in_flight() {
    use spira_runtime::tasks::a::sentinel_call;
    let carry = sentinel_call::Carry { in_flight: [("spira-run-x.service".to_string(), "builder".to_string())].into(), ..Default::default() };
    let mut rig = TaskRig::new(1, registration()).unwrap();
    rig.tick(sentinel_call::STREAM, EventKind::new(sentinel_call::STATE, &serde_json::to_string(&carry).unwrap())).unwrap();
    rig.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let asked = rig.requested().iter().find(|r| r.name == OBSERVE).expect("the tick asked for an observation");
    let input: ObserveIn = serde_json::from_str(&asked.input).unwrap();
    assert_eq!(input.starting, [("spira-run-x.service".to_string(), "builder".to_string())].into());
}

#[test_id(tqqipin)]
#[test]
fn one_reconciler_observe_asks_for_the_claim_view_once_however_many_partitions_it_reads() {
    use crate::task_a_sentinel_pass::observe_store_calls::{counted, in_own_process, serve, Fake};
    in_own_process(module_path!(), |dir| {
        let fake = Arc::new(Fake::default());
        let sock = dir.join("lc.sock");
        serve(&sock, fake.clone());
        let spira = testkit::manifest_dir().join("../spira");
        let partitions = spira_config::runes::rune_partitions(&spira, None).len();
        assert!(partitions > 1, "one call per partition is only told from one per observe with several");
        let keys = ["SPIRA_RUNES", "SPIRA_STRANDED_RUNNER_MIN_AGE", "SPIRA_INSTANCE"];
        let values = reconciler::Config::KEYS.iter().chain(&keys).filter_map(|k| Some((k.to_string(), spira_config::process::cfg(k).ok()?))).collect();
        let gone = dir.join("no-such-tool").display().to_string();
        let tool = || gone.clone();
        let seams = reconciler::Seams { systemctl: tool(), tmux: tool(), git: tool(), forge: tool(), units_install: tool(), spira_config: tool(), spira_claim: tool(), strand: tool(), queue_helpers: tool(), queue: tool(), spira_lc: tool(), bead: tool(), target_reap: tool(), podman: tool(), rebuild: tool(), layout: tool(), store_unit: tool(), store_gc_unit: tool(), lint: tool(), guard: tool() };
        let setup = Reconciler { interval_ms: 300_000, rules: Rules { now_secs: 0, grace_secs: 1800, effect_passes: 3, shadow_kinds: vec![] }, values, home: spira.display().to_string() };
        let e = spira_runtime::Effects { world: spira_runtime::world::World { lc_socket: sock, run_dir: dir.join("run"), ..Default::default() }, reconciler: Some(setup), seams: Some(seams), ..Default::default() };
        let input = serde_json::to_string(&ObserveIn { at: 1_793_000_000_000, values: e.reconciler.as_ref().unwrap().values.clone(), reminded: Default::default(), starting: Default::default() }).unwrap();
        let task = spira_runtime::tasks::all().find(|t| t.name == NAME).unwrap();
        let (_, observe) = task.runners.iter().find(|(effect, _)| *effect == OBSERVE).unwrap();
        observe(&e, "k", &runtime::EffectSpec::keyed(OBSERVE, "k", input, 60_000)).expect("the observe answers");
        let asked = fake.asked.lock().unwrap().clone();
        eprintln!("observe sent {:?}", counted(&asked));
        assert_eq!(counted(&asked)["claimable"], 1, "{asked:?}");
    });
}

#[test_id(t0ck9re)]
#[test]
fn a_mail_lost_at_a_restart_is_forgotten_and_the_next_pass_sends_it_again() {
    let observed = serde_json::to_string(&Observation { now_secs: 2900, groups: vec![gap("disk:/", escalating())], ..Observation::default() }).unwrap();
    let first = serde_json::to_string(&Observation { now_secs: 1000, groups: vec![gap("disk:/", escalating())], ..Observation::default() }).unwrap();
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let mut rig = TaskRig::new(1, registration())
        .unwrap()
        .hanging(MAIL)
        .double(OBSERVE, move |_| Ok(if asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { first.clone() } else { observed.clone() }))
        .double(RECORD, |i| Ok(i.into()));
    for _ in 0..2 {
        rig.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        rig.clock.advance(1);
    }
    let alerted = |rig: &TaskRig| rig.journaled().iter().rev().find(|e| e.stream.0 == STREAM && e.kind.tag == STATE).map(|e| serde_json::from_str::<reconciler::Book>(&e.kind.body).unwrap().alerted.contains_key("disk:/"));
    let mails = |rig: &TaskRig| rig.requested().iter().filter(|q| q.name == MAIL).count();
    assert_eq!((mails(&rig), alerted(&rig)), (1, Some(true)));
    rig.reopen(registration()).unwrap();
    rig.run_until_idle().unwrap();
    assert_eq!(alerted(&rig), Some(false), "the lost mail's alert is forgotten");
    rig.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert_eq!(mails(&rig), 2, "{:?}", rig.requested());
}
