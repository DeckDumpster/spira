use std::sync::{Arc, Mutex};

use lifecycle::bead::{apply, BeadEvent, BeadEventKind, BeadState, DispositionStatus, HoldKind, LifecycleRow};
use lifecycle::reason::{GateRedReason, HoldCause};
use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::claim_count::*;
use spira_runtime::watchers::INBOX_APPEND;


fn registration() -> runtime::Registration {
    ClaimCount { interval_ms: 60_000, every: 4 }.registration()
}

fn step(r: &LifecycleRow, actor: &str, kind: BeadEventKind) -> LifecycleRow {
    let out = apply(r, &BeadEvent { expect: r.state, version: r.version, kind, actor: actor.into(), at: None });
    assert!(out.applied, "{:?}", out.refusal);
    let mut row = out.row;
    row.version += 1;
    row
}

fn claim(r: &LifecycleRow) -> LifecycleRow {
    step(r, "dwarf-a", BeadEventKind::Claim { holder: "dwarf-a".into(), lease_until: 1, stack: Default::default(), stack_depth: 0, stack_max_depth: 4, persona: Some("builder".into()), directed: None })
}

fn red(r: &LifecycleRow, by: &str, reason: GateRedReason) -> LifecycleRow {
    let tip = format!("{:040x}", r.builder_claims);
    let submitted = step(r, "dwarf-a", BeadEventKind::Submit { tip: tip.clone() });
    step(&submitted, by, BeadEventKind::GateRed { tip, reason, step: None, message: None })
}

/// Ends the builder session that `claim_no` opened, the way the walk's four kinds of exit do.
fn end_session(r: &LifecycleRow, claim_no: u32) -> LifecycleRow {
    match claim_no % 4 {
        1 => red(r, "sift", GateRedReason::NoRebase),
        2 => {
            let held = step(r, "dwarf-a", BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some("q".into()) });
            let released = step(&held, "dwarf-a", BeadEventKind::Release);
            step(&released, "operator", BeadEventKind::Reply { message_id: "m".into() })
        }
        3 => red(r, "gate", GateRedReason::SuitesFailed),
        _ => {
            let lapsed = step(r, "sentinel", BeadEventKind::Disposition { status: DispositionStatus::Lapsed, note: "n".into() });
            step(&lapsed, "sentinel", BeadEventKind::HolderDead)
        }
    }
}

fn claims_of(r: &LifecycleRow) -> Claims {
    Claims { id: r.bead_id.clone(), n: r.builder_claims, ends: r.session_ends.iter().map(|e| serde_json::to_value(e).unwrap()).collect() }
}

type Live = Arc<Mutex<Vec<LifecycleRow>>>;

fn rig(live: &Live) -> (TaskRig, Arc<Mutex<Vec<String>>>) {
    let (rows, lines) = (live.clone(), Arc::new(Mutex::new(vec![])));
    let sink = lines.clone();
    let rig = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let live: Vec<Claims> = rows.lock().unwrap().iter().filter(|r| !r.state.is_terminal() && r.builder_claims >= 1).map(claims_of).collect();
            Ok(serde_json::to_string(&Snapshot { rows: live }).unwrap())
        })
        .double(INBOX_APPEND, move |l| {
            sink.lock().unwrap().push(l.to_string());
            Ok(l.into())
        });
    (rig, lines)
}

fn tick(rig: &mut TaskRig) {
    rig.advance(60_000).unwrap();
}

fn start(rig: &mut TaskRig) {
    let (_, stream, boot) = ClaimCount::boot();
    rig.tick(stream, boot).unwrap();
}

#[test_id(tjpe2g4)]
#[test]
fn an_unlanded_item_is_paged_at_its_fourth_and_eighth_builder_claim_with_how_each_session_ended() {
    let a = LifecycleRow::filed("sp-a");
    let live: Live = Arc::new(Mutex::new(vec![a.clone(), LifecycleRow::filed("sp-landed")]));
    let (mut rig, lines) = rig(&live);
    start(&mut rig);
    let seen = |n: usize| assert_eq!(lines.lock().unwrap().len(), n, "{:?}", lines.lock().unwrap());

    let (mut row, mut landed) = (a, LifecycleRow::filed("sp-landed"));
    for n in 1..=8u32 {
        row = claim(&row);
        live.lock().unwrap()[0] = row.clone();
        if n <= 4 {
            landed = claim(&landed);
            if n < 4 {
                landed = end_session(&landed, n);
            } else {
                landed.state = BeadState::Landed;
            }
            live.lock().unwrap()[1] = landed.clone();
        }
        tick(&mut rig);
        match n {
            4 => {
                seen(1);
                let line = lines.lock().unwrap()[0].clone();
                assert!(line.contains("[watch:claim-count]") && line.contains("sp-a") && line.contains("4 times"), "{line}");
                for end in ["#1 Submit by dwarf-a -> GateRed:no-rebase by sift", "#2 Release+ask by dwarf-a -> -", "#3 Submit by dwarf-a -> GateRed:suites-failed by gate"] {
                    assert!(line.contains(end), "{end} not in {line}");
                }
            }
            8 => {
                seen(2);
                let line = lines.lock().unwrap()[1].clone();
                assert!(line.contains("sp-a") && line.contains("8 times"), "{line}");
                assert!(line.contains("#4 HolderDead/lapsed by sentinel -> -"), "{line}");
                assert_eq!(line.matches(" -> ").count(), 7, "{line}");
                assert!(!line.contains("newest 8 kept"), "{line}");
            }
            5..=7 => seen(1),
            _ => seen(0),
        }
        if n < 8 {
            row = end_session(&row, n);
            live.lock().unwrap()[0] = row.clone();
        }
    }
    assert!(!lines.lock().unwrap().iter().any(|l| l.contains("sp-landed")), "a landed item is never paged");
}

#[test_id(tcn0zbi)]
#[test]
fn a_fourth_claim_taken_between_ticks_is_paged_at_the_next_tick_once() {
    let live: Live = Arc::new(Mutex::new(vec![]));
    let (mut rig, lines) = rig(&live);
    start(&mut rig);
    let mut row = LifecycleRow::filed("sp-late");
    for n in 1..=4 {
        row = claim(&row);
        if n < 4 {
            row = end_session(&row, n);
        }
    }
    live.lock().unwrap().push(row);
    assert!(lines.lock().unwrap().is_empty(), "nothing is said before a tick");
    tick(&mut rig);
    tick(&mut rig);
    let said = lines.lock().unwrap().clone();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("sp-late") && said[0].contains("4 times"), "{said:?}");
}

fn two_items_one_paged() -> TaskRig {
    let mut a = LifecycleRow::filed("sp-paged");
    let mut b = LifecycleRow::filed("sp-near");
    for n in 1..=3 {
        a = claim(&a);
        b = claim(&b);
        if n < 3 {
            a = end_session(&a, n);
            b = end_session(&b, n);
        }
    }
    let live: Live = Arc::new(Mutex::new(vec![]));
    let (mut rig, _) = rig(&live);
    start(&mut rig);
    live.lock().unwrap().extend([a.clone(), b]);
    tick(&mut rig);
    live.lock().unwrap()[0] = claim(&end_session(&a, 3));
    tick(&mut rig);
    rig
}

#[test_id(t8jjj89)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let read = kit::assert_kept_as_rows(&two_items_one_paged(), NAME);
    assert!(read.iter().any(|k| k == "paged/sp-paged"), "{read:?}");
}

#[test_id(tjxuu7z)]
#[test]
fn a_boot_neither_pages_a_paged_item_again_nor_misses_a_crossing() {
    kit::assert_boot_matches_an_unstopped_run(two_items_one_paged, registration, STREAM, EventKind::new(TICK, ""));
}
