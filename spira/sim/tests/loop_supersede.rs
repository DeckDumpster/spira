//! The world scenarios `supersede` and `supersede-holds`, ported to the loop world
//! (`spira_sim::loopworld`): the service's registrations on the `VirtualDriver`, every effect a
//! double (ruling D6). The task under test is `supersede-duty` (`spira_runtime::watchers::
//! supersede_duty`): its observe and act effects are answered over the loop world's lifecycle
//! store, as `spira_runtime::world`'s `supersede_snapshot` and `supersede_act` answer them over
//! the live store, and every inbox line it appends is kept.
//!
//! The store the doubles answer from holds each bead's history as the lifecycle wrote it: every
//! event a scenario or the duty applies is kept as its evidence, and the observe reads a request
//! out of that history with the task's own `supersede_duty::request_in`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lifecycle::bead::{BeadEventKind, BeadState, BreakGlass, HoldKind};
use lifecycle::reason::HoldCause;
use sim::doubles::Doubles;
use spira_runtime::watchers::supersede_duty::{self as sd, Act, Held, Reading, Verb};
use spira_runtime::watchers::INBOX_APPEND;
use spira_sim::loopworld::{self, Bead, Run, Script, World, MIN};

/// What the doubles saw besides the store: each bead's applied events (its history's
/// evidence), the bd side (closes and notes) and every inbox line.
#[derive(Default)]
struct Side {
    evidence: BTreeMap<String, Vec<String>>,
    closed: Vec<(String, String)>,
    notes: Vec<(String, String)>,
    inbox: Vec<String>,
    acts: Vec<Act>,
    observed_at: Vec<u64>,
    /// When the scripted request was made, if it was.
    requested_at: Option<u64>,
}

type Shared = Arc<Mutex<Side>>;

fn side(s: &Shared) -> MutexGuard<'_, Side> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// One event through the store's machine, its evidence kept in the bead's history.
fn apply(w: &World, s: &Shared, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
    let now = w.now();
    let evidence = serde_json::to_string(&kind).map_err(|e| e.to_string())?;
    w.lc().apply(now, id, actor, kind)?;
    side(s).evidence.entry(id.into()).or_default().push(evidence);
    Ok(())
}

/// A bead filed outside the scenario's script, on the loop's own store.
fn file(w: &World, id: &str) {
    let now = w.now();
    w.lc().file(now, id);
}

/// `work superseded-by`: a manual hold whose cause is supersede-request, the successor its detail.
fn request(w: &World, s: &Shared, id: &str, succ: &str) {
    apply(w, s, id, "dwarf", BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::SupersedeRequest, detail: Some(succ.into()) }).unwrap_or_else(|e| panic!("the request on {id}: {e}"));
}

/// `spira-lc history <id> --machine bead`, as the rows `request_in` reads.
fn history_of(s: &Shared, id: &str) -> String {
    let rows: Vec<serde_json::Value> = side(s).evidence.get(id).cloned().unwrap_or_default().into_iter().map(|e| serde_json::json!({ "applied": "1", "evidence": e })).collect();
    serde_json::Value::Array(rows).to_string()
}

fn state_of(w: &World, id: &str) -> String {
    w.lc().rows.get(id).map(|r| r.state.as_str().to_string()).unwrap_or_default()
}

/// `supersede_snapshot`: every live bead under a manual hold, read unless its version was
/// already read as no request.
fn observe(w: &World, s: &Shared, input: &str) -> Result<String, String> {
    side(s).observed_at.push(w.now());
    let known: BTreeMap<String, String> = serde_json::from_str(input).unwrap_or_default();
    let listed: Vec<(String, String)> = w.lc().rows.values().filter(|r| !r.state.is_terminal() && r.holds.contains(&HoldKind::Manual)).map(|r| (r.bead_id.clone(), r.version.to_string())).collect();
    let mut held = vec![];
    for (id, version) in listed {
        let reading = if known.get(&id) == Some(&version) {
            Reading::Skipped
        } else {
            match sd::request_in(&history_of(s, &id)) {
                Err(_) => Reading::Unreadable,
                Ok(None) => Reading::NotARequest,
                Ok(Some(succ)) => {
                    let own = state_of(w, &id);
                    let succ_state = if own.is_empty() || sd::terminal(&own) { String::new() } else { state_of(w, &succ) };
                    Reading::Request { succ, own, succ_state }
                }
            }
        };
        held.push(Held { id, version, reading });
    }
    serde_json::to_string(&sd::Snapshot { held }).map_err(|e| e.to_string())
}

/// `supersede_act`: the lifecycle write, then for a supersede the bd-side close, then the
/// evidence note.
fn act(w: &World, s: &Shared, input: &str) -> Result<String, String> {
    let a: Act = serde_json::from_str(input).map_err(|e| e.to_string())?;
    side(s).acts.push(a.clone());
    let done = match a.verb {
        Verb::Unhold => apply(w, s, &a.id, sd::ACTOR, BeadEventKind::Unhold { kind: HoldKind::Manual, break_glass: BreakGlass::default() }),
        Verb::Supersede => apply(w, s, &a.id, sd::ACTOR, BeadEventKind::Supersede { by: a.succ.clone() }),
    };
    if let Err(e) = done {
        return if a.verb == Verb::Supersede && sd::terminal(&state_of(w, &a.id)) { Ok(sd::SETTLED.into()) } else { Err(e) };
    }
    let mut g = side(s);
    if a.verb == Verb::Supersede {
        g.closed.push((a.id.clone(), a.succ.clone()));
    }
    g.notes.push((a.id.clone(), a.note.clone()));
    Ok("done".into())
}

/// The loop's doubles with supersede-duty's and the inbox laid over them;
/// `step` runs before each observe, for what the scenario does between the duty's ticks.
fn duty(d: Doubles, w: &Arc<World>, s: &Shared, step: fn(&World, &Shared)) -> Doubles {
    let (w1, s1, w2, s2, s3) = (w.clone(), s.clone(), w.clone(), s.clone(), s.clone());
    d.double(sd::OBSERVE, move |input| {
        step(&w1, &s1);
        observe(&w1, &s1, input)
    })
    .double(sd::ACT, move |input| act(&w2, &s2, input))
    .double(INBOX_APPEND, move |line| {
        side(&s3).inbox.push(line.to_string());
        Ok(String::new())
    })
}

fn run(seed: u64, script: Script, s: &Shared, setup: fn(&World, &Shared), step: fn(&World, &Shared)) -> Run {
    loopworld::run_with(seed, script, |d, w| {
        setup(w, s);
        duty(d, w, s, step)
    })
    .unwrap_or_else(|e| panic!("the loop ran: {e}"))
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    assert!(r.unanswered().is_empty(), "every effect asked for had a double: {:?}", r.unanswered());
}

fn duty_lines(s: &Side, id: &str) -> Vec<String> {
    let tag = format!("[watch:{}] SUPERSEDE REQUEST {id}:", sd::NAME);
    s.inbox.iter().filter(|l| l.contains(&tag)).cloned().collect()
}

fn note_of(s: &Side, id: &str) -> Vec<String> {
    s.notes.iter().filter(|(b, _)| b == id).map(|(_, n)| n.clone()).collect()
}

/// Replaces `supersede` (sp-8jytkl, the duty superseding on any successor row): only a LANDED
/// successor supersedes. sp-su01 runs the loop to LANDED; once it has, sp-su02 is put under a
/// supersede-request naming it, and the duty's next tick supersedes sp-su02: the lifecycle row
/// goes SUPERSEDED by sp-su01, the bd side closes it with sp-su01 as the successor, the evidence
/// note says so, and the Concierge's inbox is told. A later tick does not adjudicate it again,
/// and no other bead is ever superseded.
#[test]
fn supersede_only_a_landed_successor_supersedes_with_its_note_close_and_inbox_line() {
    fn none(_: &World, _: &Shared) {}
    fn after_landed(w: &World, s: &Shared) {
        if state_of(w, "sp-su01") == "LANDED" && state_of(w, "sp-su02").is_empty() {
            file(w, "sp-su02");
            request(w, s, "sp-su02", "sp-su01");
            side(s).requested_at = Some(w.now());
        }
    }
    let s: Shared = Arc::default();
    let script = Script { beads: vec![Bead::new("sp-su01", 30_000)], max_concurrent: 1, until: 45 * MIN, ..Script::default() };
    let r = run(31, script, &s, none, after_landed);
    let g = side(&s);
    println!("supersede: wall {:?}", r.wall);
    let asked = g.requested_at.unwrap_or_else(|| panic!("sp-su01 landed and the request was made: {}", r.dump()));
    assert_eq!(r.lifecycle_of("sp-su01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", r.dump());
    assert_eq!(r.rows["sp-su02"].state, BeadState::Superseded, "the landed successor supersedes (landed_successor_supersedes): {}", r.dump());
    assert_eq!(r.rows["sp-su02"].reason.as_deref(), Some("sp-su01"), "superseded by sp-su01");
    let superseded: Vec<&String> = r.rows.values().filter(|row| row.state == BeadState::Superseded).map(|row| &row.bead_id).collect();
    assert_eq!(superseded, ["sp-su02"], "only a landed successor supersedes (only_a_landed_successor_supersedes)");
    assert_eq!(g.closed, [("sp-su02".to_string(), "sp-su01".to_string())], "the bd side closed sp-su02 with its successor");
    let notes = note_of(&g, "sp-su02");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("supersede-request confirmed: sp-su01 is LANDED"), "{notes:?}");
    let lines = duty_lines(&g, "sp-su02");
    assert_eq!(lines.len(), 2, "the Concierge is told once of the request and once of the outcome: {:?}", g.inbox);
    assert!(lines[0].ends_with("SUPERSEDE REQUEST sp-su02: successor sp-su01"), "{lines:?}");
    assert!(lines[1].ends_with("SUPERSEDE REQUEST sp-su02: confirmed, superseded by LANDED sp-su01"), "{lines:?}");
    assert_eq!(g.acts.len(), 1, "one adjudication: {:?}", g.acts);
    assert!(g.observed_at.iter().any(|t| *t > asked + 300_000), "a later tick read the world again (check-landed-again): {:?}", g.observed_at);
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// Replaces `supersede-holds` (sp-egvze5's split of `supersede`) and its suite
/// `spira/test-sim-supersede-holds.sh`: no landing, five beads. A request naming an unlanded
/// successor (sp-su03 by READY sp-su04), one naming the bead itself (sp-su05) and one naming a
/// bead with no row (sp-su06) each lift the hold with the refusal as the note, and the bead
/// stays READY; an operator's own manual hold (sp-su07) is not a request and is left alone, with
/// nothing said of it. Nothing is superseded, each request is adjudicated once however many
/// ticks follow, and the duty polls on its cadence (the scenario's liveness check).
#[test]
fn supersede_holds_an_unlanded_self_or_absent_successor_lifts_the_hold_and_an_operator_hold_stays() {
    fn holds(w: &World, s: &Shared) {
        for id in ["sp-su03", "sp-su04", "sp-su05", "sp-su06", "sp-su07"] {
            file(w, id);
        }
        request(w, s, "sp-su03", "sp-su04");
        request(w, s, "sp-su05", "sp-su05");
        request(w, s, "sp-su06", "sp-absent");
        apply(w, s, "sp-su07", "operator", BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::ManualHold, detail: Some("operator parked it".into()) }).expect("the operator's hold");
    }
    fn none(_: &World, _: &Shared) {}
    let s: Shared = Arc::default();
    // No dwarf runs: these beads are the duty's to adjudicate, and an unheld READY one
    // (sp-su04, or a bead whose hold was lifted) would otherwise be claimed.
    let script = Script { max_concurrent: 0, until: 16 * MIN, ..Script::default() };
    let r = run(32, script, &s, holds, none);
    let g = side(&s);
    println!("supersede-holds: wall {:?}", r.wall);
    for (id, note) in [("sp-su03", "supersede-request by sp-su04 refused: sp-su04 is READY"), ("sp-su05", "supersede-request names the bead itself"), ("sp-su06", "supersede-request by sp-absent refused")] {
        let row = &r.rows[id];
        assert_eq!((row.state, row.holds.is_empty()), (BeadState::Ready, true), "{id}'s hold was lifted and it stays READY: {}", r.dump());
        let notes = note_of(&g, id);
        assert!(notes.len() == 1 && notes[0].contains(note), "{id}'s note says {note:?}: {notes:?}");
        assert_eq!(duty_lines(&g, id).len(), 2, "{id}: told once of the request and once of the refusal: {:?}", g.inbox);
    }
    let parked = &r.rows["sp-su07"];
    assert_eq!((parked.state, parked.holds.iter().copied().collect::<Vec<_>>()), (BeadState::Ready, vec![HoldKind::Manual]), "the operator's hold is left alone (manual_hold_is_left_alone)");
    assert!(duty_lines(&g, "sp-su07").is_empty() && note_of(&g, "sp-su07").is_empty(), "the manual hold was not adjudicated: {:?}", g.inbox);
    assert!(r.rows.values().all(|row| row.state != BeadState::Superseded), "only a landed successor supersedes");
    assert!(g.closed.is_empty(), "no bd-side close: {:?}", g.closed);
    let mut by_bead: Vec<&str> = g.acts.iter().map(|a| a.id.as_str()).collect();
    by_bead.sort();
    assert_eq!(by_bead, ["sp-su03", "sp-su05", "sp-su06"], "each request is adjudicated once (check-holds-again)");
    assert!(g.acts.iter().all(|a| a.verb == Verb::Unhold), "{:?}", g.acts);
    let interval = 300_000;
    let polls = (16 * MIN).div_ceil(interval) as usize;
    assert!(g.observed_at.len() >= polls && g.observed_at.windows(2).all(|p| p[1] - p[0] == interval), "the duty polls every {interval} ms through the span (check-liveness): {:?}", g.observed_at);
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// The loop's own dwarf offers no held bead (spira-claim's ranking): a READY bead under an
/// operator's manual hold is never claimed, however many sentinel ticks and free slots follow,
/// and the duty leaves the operator's hold alone.
#[test]
fn a_held_ready_bead_is_never_claimed_by_the_loops_dwarf() {
    fn parked(w: &World, s: &Shared) {
        file(w, "sp-hd01");
        apply(w, s, "sp-hd01", "operator", BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::ManualHold, detail: Some("operator parked it".into()) }).expect("the operator's hold");
    }
    fn none(_: &World, _: &Shared) {}
    let s: Shared = Arc::default();
    let script = Script { max_concurrent: 1, until: 30 * MIN, ..Script::default() };
    let r = run(33, script, &s, parked, none);
    assert_eq!(r.lifecycle_of("sp-hd01"), ["READY"], "the held bead was never claimed: {}", r.dump());
    assert_eq!(r.rows["sp-hd01"].holds.iter().copied().collect::<Vec<_>>(), [HoldKind::Manual]);
    assert!(r.refused.is_empty(), "no claim was tried on it: {:?}", r.refused);
    assert_no_process(&r);
}
