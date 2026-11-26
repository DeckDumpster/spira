//! The world scenarios `gate-verdict-red` and `gate-verdict-silent`, ported to the loop world
//! (`spira_sim::loopworld`): the service's registrations on the `VirtualDriver`, every effect a
//! double (ruling D6). The task under test is `gate.worker` (`spira_runtime::tasks::a::
//! gate_worker`): its queue effects are answered over an in-memory gate queue, its gate
//! legs are units on the loop's scripted manager, and its probe reads each ended leg's status
//! and classifies the gate's output with the task's own `classify`, as the live probe does from
//! the leg's files.
//!
//! The world scenarios' gate-mode steps become the gate leg's scripted answers, one per run of
//! a bead's gate: `fail-once` is FAIL then PASS, `no-verdict` and `infra` are NO_VERDICT (75) with
//! the gate's own reason, then PASS once the scenario steered the next mode.
//!
//! The landing pass's queue half (`landing-pass land`, CHECK 6 with gate-worker: `certify_queued`
//! and `certify_judge` in `landing-pass/src/pass.rs`) is not a task of the loop: sentinel-pass
//! starts it as a unit. Before each probe the doubles play it over the store: a SUBMITTED bead
//! whose tip has no filed verdict is queued; a filed verdict for its tip is taken and applied
//! (PASS certifies, FAIL is `gate-red`, NO_VERDICT is `infra` and the bead stays SUBMITTED to be
//! queued again). A SUBMITTED bead the open round holds has no gate of its own: nothing is queued,
//! its inbox job is dropped, a running gate leg is stopped and ends NO_VERDICT `cancelled`, and
//! that filed result is taken and charges nothing (`a_member_a_round_holds_has_no_gate_queued`,
//! `a_cancelled_gate_charges_its_round_held_member_nothing`). That mapping is the landing pass's own and is covered there:
//! `a_red_gate_reopens_with_the_gates_own_words_and_marks_red`,
//! `no_verdict_is_counted_by_the_seam_and_never_reopens` and
//! `a_filed_no_verdict_is_nobodys_and_charges_nothing` (`landing-pass/src/tests.rs`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lifecycle::bead::{BeadEventKind, BeadState};
use lifecycle::reason::GateRedReason;
use runtime::units::{self, Exit, ScopeSpec, StartSpec, UnitSpec, UNIT_RUN_SCOPE, UnitHandle, UnitState, Units, UNIT_START};
use sim::doubles::Doubles;
use spira_runtime::tasks::a::gate_worker::{self as gw, Carry, Filing, Job, Leg, SlotJob, Snapshot, State};
use spira_runtime::tasks::a::batcher_local as bl;
use spira_sim::loopworld::{self, Bead, Run, Script, World, MIN, REPO};

const GATE_MS: u64 = 90_000;
/// Free slots of the host gate admission.
const ADMISSION: u32 = 2;
const PASS: (i32, &str) = (0, "gate: VERDICT=PASS");

/// The gate queue, the legs' scripted answers and what the landing pass applied.
#[derive(Default)]
struct Gate {
    /// Per bead, the gate leg's (status, output) for each run in turn; the last repeats.
    answers: BTreeMap<String, Vec<(i32, String)>>,
    runs: BTreeMap<String, usize>,
    inbox: Vec<Job>,
    claimed: BTreeMap<usize, Vec<Job>>,
    /// Filed verdicts the landing pass has not yet taken: (job, verdict, reason).
    done: Vec<(Job, String, Option<String>)>,
    /// Each leg started, by unit key: its scripted (status, output).
    legs: BTreeMap<String, (i32, String)>,
    retained: Vec<(String, String)>,
    /// Every verdict filed: (bead, verdict, reason).
    filed: Vec<(String, String, Option<String>)>,
    /// Every verdict the landing pass applied: (bead, verdict, the row's state after).
    judged: Vec<(String, String, BeadState)>,
}

type Shared = Arc<Mutex<Gate>>;

fn gate(s: &Shared) -> MutexGuard<'_, Gate> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn branch(bead: &str) -> String {
    format!("spira/{bead}")
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|e| format!("{what}: bad input: {e}"))
}

/// The leg's unit key, as the task names it (slot first).
fn leg_key(slot: usize, leg: &str, job: &Job) -> String {
    format!("{slot}/{leg}/{}/{}/{}", job.repo, job.branch, job.tip)
}

/// `landing-pass land`'s CHECK 6 with gate-worker, over the store: take and apply each SUBMITTED
/// bead's filed verdict for its tip, or queue the tip.
fn landing_pass(w: &World, s: &Shared) {
    let now = w.now();
    let mut lc = w.lc();
    let mut g = gate(s);
    let submitted: Vec<(String, String)> = lc.rows.values().filter(|r| r.state == BeadState::Submitted).map(|r| (r.bead_id.clone(), r.tip.clone().unwrap_or_default())).collect();
    let held: Vec<String> = lc.round.as_ref().map(|(_, m)| m.iter().map(|(i, _)| i.clone()).collect()).unwrap_or_default();
    for (id, tip) in submitted {
        let job = Job { repo: REPO.into(), branch: branch(&id), tip: tip.clone(), bead: id.clone(), fix: false, express: false, priority: 2, enqueued_ms: now };
        if held.contains(&id) {
            g.inbox.retain(|j| j.key() != job.key());
            let cancelled = g.done.iter().position(|(j, v, why)| j.key() == job.key() && v == "NO_VERDICT" && why.as_deref() == Some("cancelled"));
            if let Some(at) = cancelled {
                g.done.remove(at);
            }
            let running: Vec<(usize, Job)> = g.claimed.iter().flat_map(|(slot, js)| js.iter().filter(|j| j.key() == job.key()).map(|j| (*slot, j.clone()))).collect();
            for (slot, j) in running {
                let key = leg_key(slot, "gate", &j);
                if let Ok(h) = UnitHandle::of(gw::NAME, &key) {
                    let _ = w.units.stop(&h);
                }
                g.legs.insert(key, (75, "gate: VERDICT=NO_VERDICT reason=cancelled".into()));
            }
            continue;
        }
        if let Some(at) = g.done.iter().position(|(j, ..)| j.key() == job.key()) {
            let (_, verdict, _) = g.done.remove(at);
            let kind = match verdict.as_str() {
                "PASS" => BeadEventKind::GatePass { tip, gate_key: format!("gate:{id}") },
                "FAIL" => BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed, step: None, message: None },
                _ => BeadEventKind::GateInfra { tip },
            };
            let _ = lc.apply(now, &id, "landing-pass", kind);
            let after = lc.rows[&id].state;
            g.judged.push((id, verdict, after));
        } else if !g.inbox.iter().chain(g.claimed.values().flatten()).any(|j| j.key() == job.key()) {
            g.inbox.push(job);
        }
    }
}

/// `gate.worker.observe`: the queue, the admission and each started leg's end as its status
/// reads, the gate leg's classified by the task's own `classify`.
fn observe(w: &World, s: &Shared, input: &str) -> Result<String, String> {
    landing_pass(w, s);
    let carry: Carry = serde_json::from_str(input).unwrap_or_default();
    let g = gate(s);
    let claimed = g.claimed.values().map(Vec::len).sum::<usize>() as u32;
    let mut snap = Snapshot { inbox: g.inbox.clone(), claimed: g.claimed.clone(), free_slots: ADMISSION.saturating_sub(claimed), gate_timeout: 300, lock_wait: 600, run_dir: "sim/run".into(), now_ms: w.now(), ..Snapshot::default() };
    for jobs in g.claimed.values() {
        for j in jobs {
            *snap.running_by_repo.entry(j.repo.clone()).or_default() += 1;
        }
    }
    for (i, slot) in carry.slots.iter().enumerate() {
        let (Some(job), true) = (&slot.job, slot.leg_started) else { continue };
        let leg = match slot.state {
            State::Gating => "gate",
            _ => continue,
        };
        let key = leg_key(i, leg, job);
        let facts = w.units.facts(&UnitHandle::of(gw::NAME, &key).map_err(|e| e.0)?).map_err(|e| e.0)?;
        let running = matches!(facts.state, UnitState::Activating | UnitState::Active | UnitState::Deactivating);
        let (rc, out) = match (running, g.legs.get(&key)) {
            (false, Some((rc, out))) => (Some(*rc), out.clone()),
            _ => (None, String::new()),
        };
        let run = (!running && leg == "gate").then(|| gw::classify(rc.unwrap_or(137), &out));
        snap.legs.insert(i, Leg { running, rc, verdict: run.as_ref().map(|r| r.verdict.clone()), reason: run.and_then(|r| r.reason) });
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `unit.start`: a gate.worker leg is scripted to its answer, then started on the loop's
/// manager like every other unit.
fn unit_start(w: &World, s: &Shared, input: &str) -> Result<String, String> {
    let start: StartSpec = parse(UNIT_START, input)?;
    if start.unit.task == gw::NAME {
        let mut g = gate(s);
        let bead = start.unit.env.iter().find(|(k, _)| k == "SPIRA_GATE_BEAD").map(|(_, v)| v.clone()).ok_or("a gate leg carries SPIRA_GATE_BEAD")?;
        let n = g.runs.get(&bead).copied().unwrap_or(0);
        g.runs.insert(bead.clone(), n + 1);
        let answers = g.answers.get(&bead).cloned().unwrap_or_default();
        let (rc, out) = answers.get(n).or(answers.last()).cloned().unwrap_or((PASS.0, PASS.1.into()));
        g.legs.insert(start.unit.key.clone(), (rc, out));
        w.units.script(gw::NAME, GATE_MS, Exit::Code(rc));
    }
    units::effect(&*w.units, UNIT_START, input)
}

fn tip(w: &World, input: &str) -> Result<String, String> {
    let sj: SlotJob = parse(gw::TIP, input)?;
    Ok(w.lc().submitted.get(&sj.job.bead).and_then(|t| t.last().cloned()).unwrap_or_default())
}

fn claim(s: &Shared, input: &str) -> Result<String, String> {
    let sj: SlotJob = parse(gw::CLAIM, input)?;
    let mut g = gate(s);
    let at = g.inbox.iter().position(|j| j.key() == sj.job.key()).ok_or_else(|| format!("{} is no longer queued — refused", sj.job.key()))?;
    let job = g.inbox.remove(at);
    g.claimed.entry(sj.slot).or_default().push(job);
    Ok(format!("slot {} claimed {}", sj.slot, sj.job.key()))
}

fn unclaim(g: &mut Gate, slot: usize, job: &Job) -> Result<Job, String> {
    let held = g.claimed.entry(slot).or_default();
    let at = held.iter().position(|j| j.key() == job.key()).ok_or_else(|| format!("slot {slot} does not hold {} — refused", job.key()))?;
    Ok(held.remove(at))
}

fn discard(s: &Shared, input: &str) -> Result<String, String> {
    let sj: SlotJob = parse(gw::DISCARD, input)?;
    unclaim(&mut gate(s), sj.slot, &sj.job).map(|_| String::new())
}

fn verdict_of(g: &Gate, f: &Filing) -> gw::GateRun {
    let out = g.legs.get(&leg_key(f.slot, "gate", &f.job)).map(|(_, o)| o.clone()).unwrap_or_default();
    gw::classify(f.rc.unwrap_or(137), &out)
}

fn retain_output(s: &Shared, input: &str) -> Result<String, String> {
    let f: Filing = parse(gw::RETAIN_OUTPUT, input)?;
    let mut g = gate(s);
    let run = verdict_of(&g, &f);
    g.retained.push((f.job.bead.clone(), run.verdict));
    Ok(format!("sim/run/gate-worker/out/{}.out", f.job.key()))
}

fn complete(s: &Shared, input: &str) -> Result<String, String> {
    let f: Filing = parse(gw::COMPLETE, input)?;
    let mut g = gate(s);
    let job = unclaim(&mut g, f.slot, &f.job)?;
    let run = verdict_of(&g, &f);
    g.filed.push((job.bead.clone(), run.verdict.clone(), run.reason.clone()));
    g.done.push((job, run.verdict.clone(), run.reason));
    Ok(format!("{} → {}", f.job.key(), run.verdict))
}

fn recover(s: &Shared, input: &str) -> Result<String, String> {
    let slot: usize = input.trim().parse().map_err(|_| format!("bad slot {input:?}"))?;
    let mut g = gate(s);
    let back = g.claimed.remove(&slot).unwrap_or_default();
    g.inbox.extend(back);
    Ok(String::new())
}

fn doubles(d: Doubles, w: &Arc<World>, s: &Shared) -> Doubles {
    let (w1, s1) = (w.clone(), s.clone());
    let (w2, s2) = (w.clone(), s.clone());
    let w3 = w.clone();
    let (s4, s5, s6, s7, s8) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
    d.double(gw::OBSERVE, move |i| observe(&w1, &s1, i))
        .double(UNIT_START, move |i| unit_start(&w2, &s2, i))
        .double(gw::TIP, move |i| tip(&w3, i))
        .double(gw::CLAIM, move |i| claim(&s4, i))
        .double(gw::DISCARD, move |i| discard(&s5, i))
        .double(gw::RETAIN_OUTPUT, move |i| retain_output(&s6, i))
        .double(gw::COMPLETE, move |i| complete(&s7, i))
        .double(gw::RECOVER, move |i| recover(&s8, i))
}

fn run(seed: u64, script: Script, answers: &[(&str, &[(i32, &str)])]) -> (Run, Shared) {
    run_with_doubles(seed, script, answers, |d, _| d)
}

fn run_with_doubles(seed: u64, script: Script, answers: &[(&str, &[(i32, &str)])], more: impl FnOnce(Doubles, &Arc<World>) -> Doubles) -> (Run, Shared) {
    let s: Shared = Arc::default();
    gate(&s).answers = answers.iter().map(|(b, a)| (b.to_string(), a.iter().map(|(rc, o)| (*rc, o.to_string())).collect())).collect();
    let r = loopworld::run_with(seed, script, |d, w| more(doubles(d, w, &s), w)).unwrap_or_else(|e| panic!("the loop ran: {e}"));
    (r, s)
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    assert!(r.unanswered().is_empty(), "every effect asked for had a double: {:?}", r.unanswered());
}

fn dump(r: &Run, g: &Gate) -> String {
    format!("filed {:?}\njudged {:?}\nretained {:?}\ninbox {:?}\nclaimed {:?}\n{}", g.filed, g.judged, g.retained, g.inbox, g.claimed, r.dump())
}

/// Replaces `gate-verdict-red` (sp-g7jnp9, the stub gate's verdict kinds; split out by
/// sp-egvze5): the bead's first gate fails (`fail-once`). gate.worker files FAIL with the suite's
/// reason and keeps the output; the landing pass returns the bead to its dwarf
/// (`red_gate_returned_it`). The dwarf reworks it, the moved tip is gated again and passes (no
/// output kept), the bead is CERTIFIED by that gate and the round, resumed after it, lands it
/// (`red_then_pass_landed`).
#[test_id(t78kss2)]
#[test]
fn gate_verdict_red_a_failed_gate_returns_the_bead_and_its_reworked_tip_passes_and_lands() {
    let script = Script { beads: vec![Bead::new("sp-gv01", 30_000)], max_concurrent: 1, stopped_until: 40 * MIN, until: 55 * MIN, ..Script::default() };
    let (r, s) = run(41, script, &[("sp-gv01", &[(1, "gate: VERDICT=FAIL reason=suite-red suite=sim-planted.sh"), PASS])]);
    let g = gate(&s);
    println!("gate-verdict-red: wall {:?}", r.wall);
    let tips = &r.submitted["sp-gv01"];
    assert_eq!(tips.len(), 2, "submitted, returned, reworked and submitted again: {}", dump(&r, &g));
    assert_eq!(g.filed, [("sp-gv01".to_string(), "FAIL".to_string(), Some("suite-red".to_string())), ("sp-gv01".to_string(), "PASS".to_string(), None)], "{}", dump(&r, &g));
    assert_eq!(g.judged, [("sp-gv01".to_string(), "FAIL".to_string(), BeadState::Rework), ("sp-gv01".to_string(), "PASS".to_string(), BeadState::Certified)], "the red gate returned it, the pass certified it: {}", dump(&r, &g));
    assert_eq!(g.retained, [("sp-gv01".to_string(), "FAIL".to_string())], "the failed gate's output is kept, the pass's is not");
    assert_eq!(r.lifecycle_of("sp-gv01"), ["READY", "WORKING", "SUBMITTED", "REWORK", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&r, &g));
    assert_eq!(r.lands, [vec![("sp-gv01".to_string(), tips[1].clone())]], "the reworked tip is what landed");
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// Replaces `gate-verdict-silent` (sp-g7jnp9; split out by sp-egvze5): one bead's gate times out
/// (`no-verdict`) and another's harness faults (`infra`). gate.worker files NO_VERDICT with the
/// gate's own reason for each and keeps the output; the landing pass records the infra attempt
/// and leaves each SUBMITTED, never returned to its dwarf. Queued again at the same tip, each
/// gate passes, each bead is CERTIFIED and the round lands both (`no_verdict_then_pass_landed`,
/// `infra_then_pass_landed`).
#[test_id(tlgxnjg)]
#[test]
fn gate_verdict_silent_a_gate_without_a_verdict_leaves_the_bead_submitted_and_the_next_pass_lands_it() {
    let script = Script { beads: vec![Bead::new("sp-gv02", 30_000), Bead::new("sp-gv03", 30_000)], max_concurrent: 2, stopped_until: 25 * MIN, until: 40 * MIN, ..Script::default() };
    let answers: &[(&str, &[(i32, &str)])] = &[("sp-gv02", &[(75, "gate: VERDICT=NO_VERDICT reason=timeout"), PASS]), ("sp-gv03", &[(75, "gate: VERDICT=NO_VERDICT reason=harness-fault"), PASS])];
    let (r, s) = run(42, script, answers);
    let g = gate(&s);
    println!("gate-verdict-silent: wall {:?}", r.wall);
    for (bead, reason) in [("sp-gv02", "timeout"), ("sp-gv03", "harness-fault")] {
        let filed: Vec<(String, Option<String>)> = g.filed.iter().filter(|(b, ..)| b == bead).map(|(_, v, why)| (v.clone(), why.clone())).collect();
        assert_eq!(filed, [("NO_VERDICT".to_string(), Some(reason.to_string())), ("PASS".to_string(), None)], "{bead}: {}", dump(&r, &g));
        let judged: Vec<(String, BeadState)> = g.judged.iter().filter(|(b, ..)| b == bead).map(|(_, v, st)| (v.clone(), *st)).collect();
        assert_eq!(judged, [("NO_VERDICT".to_string(), BeadState::Submitted), ("PASS".to_string(), BeadState::Certified)], "{bead} stayed SUBMITTED, then certified: {}", dump(&r, &g));
        assert_eq!(r.lifecycle_of(bead), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{bead} was never returned: {}", dump(&r, &g));
        assert_eq!(r.submitted[bead].len(), 1, "{bead} landed the tip it first submitted");
        assert!(g.retained.contains(&(bead.to_string(), "NO_VERDICT".to_string())), "{bead}'s silent gate's output is kept: {:?}", g.retained);
    }
    assert!(g.retained.iter().all(|(_, v)| v != "PASS"), "{:?}", g.retained);
    let landed: Vec<String> = r.lands.iter().flatten().map(|(id, _)| id.clone()).collect();
    assert_eq!(landed.len(), 2, "both landed: {}", dump(&r, &g));
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// A SUBMITTED item rides the next cut and its gate runs no test: every gate leg for a member
/// either never started after the round took it or ended NO_VERDICT `cancelled`, no gate PASS is
/// judged, and the round's pass certifies both.
#[test_id(tvz2qox)]
#[test]
fn a_submitted_item_rides_the_next_cut_and_its_gate_runs_no_test() {
    let script = Script { beads: vec![Bead { submits: vec![MIN], ..Bead::new("sp-gh01", 30_000) }, Bead { submits: vec![MIN], ..Bead::new("sp-gh02", 30_000) }], max_concurrent: 2, until: 40 * MIN, ..Script::default() };
    let (r, s) = run(43, script, &[("sp-gh01", &[PASS]), ("sp-gh02", &[PASS])]);
    let g = gate(&s);
    assert_eq!(r.lands.len(), 1, "both land in one round: {}", dump(&r, &g));
    assert_eq!(r.lands[0].len(), 2, "{}", dump(&r, &g));
    assert!(g.judged.iter().all(|(_, v, _)| v != "PASS"), "no gate PASS is judged for a round member: {}", dump(&r, &g));
    assert!(g.filed.iter().all(|(_, v, why)| v == "NO_VERDICT" && why.as_deref() == Some("cancelled")), "a gate leg that ran beside the round was cancelled: {}", dump(&r, &g));
    for bead in ["sp-gh01", "sp-gh02"] {
        assert_eq!(r.lifecycle_of(bead), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{bead}: {}", dump(&r, &g));
    }
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// A bad member is ejected on the round's attribution, not on a gate verdict: the pass is red on
/// a suite the member owns, `batcher.local.eject` carries that suite and returns the member to
/// REWORK, no gate PASS or FAIL is ever filed for it, and the other member lands.
#[test_id(t31dm5q)]
#[test]
fn a_bad_member_is_ejected_on_the_rounds_attribution_and_returns_to_rework() {
    let script = Script { beads: vec![Bead { submits: vec![MIN], ..Bead::new("sp-gh03", 30_000) }, Bead { submits: vec![MIN], ..Bead::new("sp-gh04", 30_000) }], max_concurrent: 2, until: 40 * MIN, ..Script::default() };
    let ejects: Arc<Mutex<Vec<bl::Leg>>> = Arc::default();
    let seen = ejects.clone();
    let reds = Arc::new(Mutex::new(1));
    let (r, s) = run_with_doubles(44, script, &[("sp-gh03", &[PASS]), ("sp-gh04", &[PASS])], move |d, w| {
        let (wc, we) = (w.clone(), w.clone());
        d.double(bl::CERTIFY, move |input| {
            let l: bl::Leg = parse(bl::CERTIFY, input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), l.batch, REPO.into()], env: vec![], cwd: None };
            let red = std::mem::take(&mut *reds.lock().unwrap_or_else(|e| e.into_inner())) > 0;
            wc.units.script(bl::NAME, MIN, Exit::Code(i32::from(red)));
            let said: &[u8] = if red { b"red=test-bad.sh\nred-output=sim/run/test-bad.sh.out\n" } else { b"certified\n" };
            wc.units.script_output(bl::NAME, said, b"");
            units::effect(&*wc.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
        .double(bl::EJECT, move |input| {
            let l: bl::Leg = parse(bl::EJECT, input)?;
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(l.clone());
            let now = we.now();
            let mut lc = we.lc();
            if let Some((_, members)) = &mut lc.round {
                members.retain(|(id, _)| id != "sp-gh04");
            }
            lc.apply(now, "sp-gh04", bl::NAME, BeadEventKind::Deliver)?;
            lc.apply(now, "sp-gh04", bl::NAME, BeadEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed })?;
            Ok("{}".into())
        })
    });
    let g = gate(&s);
    let ejects = ejects.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(ejects.len(), 1, "{}", dump(&r, &g));
    assert!(ejects[0].why.contains("test-bad.sh"), "the eject carries the round's red suite: {:?}", ejects[0].why);
    assert!(g.judged.is_empty(), "no gate verdict was ever applied: {}", dump(&r, &g));
    assert!(g.filed.iter().all(|(_, v, why)| v == "NO_VERDICT" && why.as_deref() == Some("cancelled")), "{}", dump(&r, &g));
    assert!(r.lifecycle_of("sp-gh04").contains(&"REWORK".to_string()), "{}", dump(&r, &g));
    assert_eq!(r.lifecycle_of("sp-gh03").last().map(String::as_str), Some("LANDED"), "{}", dump(&r, &g));
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}
