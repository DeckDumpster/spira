use std::sync::{Arc, Mutex};

use reconciler::{Check, Group, Mail, Observation, Reminder, ReminderPlan, Remedy, Rules};
use reconciler_engine::core::RawStatus;
use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::reconciler::*;

fn registration() -> runtime::Registration {
    Reconciler { interval_ms: 300_000, rules: Rules { now_secs: 0, grace_secs: 1800, effect_passes: 3, shadow_kinds: vec![] }, values: Default::default(), home: "/h".into() }.registration()
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

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&passes_then(escalating(), &[1000, 2900], 4700), NAME);
    for k in ["state/disk:/", "alerted/disk:/", "reminded"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
    let rows = kit::assert_kept_as_rows(&passes_then(repairing(), &[1000, 2900], 4700), NAME);
    assert!(rows.iter().any(|r| r == "pending/disk:/"), "{rows:?}");
}

#[test]
fn a_boot_escalates_a_gap_whose_grace_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(gap_inside_its_grace, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_escalate_a_gap_that_was_already_escalated() {
    kit::assert_boot_matches_an_unstopped_run(gap_already_escalated, registration, STREAM, EventKind::new(TICK, ""));
}
