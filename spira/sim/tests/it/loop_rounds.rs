//! The round scenarios of `spira/sim/scenarios/` ported to the loop world
//! (`spira_sim::loopworld`): the service's registrations on the `VirtualDriver`, every effect a
//! double (ruling D6). `batcher.local`'s machine decides; each test lays doubles over the legs its
//! property turns on (a stale open round's re-drive, a red-early corpus and its attribution) and
//! asserts the scenario's property on the service's own tasks. Where the property lives in a
//! crate's library rather than in the loop, the test names the unit test that covers it.

use std::sync::{Arc, Mutex};

use lifecycle::bead::BeadState;
use runtime::units::{self, Exit, ScopeSpec, UnitHandle, UnitSpec, UNIT_RUN_SCOPE};
use spira_runtime::tasks::a::batcher_local as bl;
use spira_sim::loopworld::{self, Bead, Run, Script, World, MIN, REPO};

fn show(name: &str, r: &Run) {
    println!("{name}: wall {:?}", r.wall);
    if std::env::var_os("W3_DUMP").is_some() {
        println!("=== {name}\n{}", r.dump());
    }
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    assert!(r.unanswered().is_empty(), "every effect asked for had a double: {:?}", r.unanswered());
}

/// The virtual times `effect` was answered at, from the run's exec.log
/// (`=== t=<vt> <effect> exit status: <rc> in <ms>ms`).
fn times(r: &Run, effect: &str) -> Vec<u64> {
    r.exec_log
        .iter()
        .filter_map(|l| {
            let rest = l.strip_prefix("=== t=")?;
            let (t, rest) = rest.split_once(' ')?;
            (rest.split_once(' ')?.0 == effect).then(|| t.parse().ok()).flatten()
        })
        .collect()
}

/// When the store first moved `bead` into `state`.
fn entered(r: &Run, bead: &str, state: &str) -> Option<u64> {
    r.history.iter().find(|(_, id, _, to)| id == bead && to == state).map(|(at, ..)| *at)
}

/// The batcher's carried machine state for the repo, each change once, in order; `Idle` when the
/// repo carries nothing.
fn batcher_states(r: &Run) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (_, stream, tag, body, _) in &r.trace {
        if stream != bl::STREAM || tag != bl::STATE {
            continue;
        }
        let carry: bl::Carry = serde_json::from_str(body).unwrap_or_default();
        let s = carry.repos.get(REPO).map_or("Idle".to_string(), |rc| format!("{:?}", rc.state));
        if out.last() != Some(&s) {
            out.push(s);
        }
    }
    out
}

fn leg(input: &str) -> Result<bl::Leg, String> {
    serde_json::from_str(input).map_err(|e| format!("leg: {e}"))
}

/// One run with `extra`'s doubles laid over the loop's; the world comes back with the run so a
/// test can read what its units saw.
fn run_keeping(seed: u64, script: Script, extra: impl FnOnce(sim::doubles::Doubles, &Arc<World>) -> sim::doubles::Doubles) -> (Run, Arc<World>) {
    let kept: Arc<Mutex<Option<Arc<World>>>> = Arc::default();
    let k = kept.clone();
    let r = loopworld::run_with(seed, script, move |d, w| {
        *k.lock().unwrap_or_else(|e| e.into_inner()) = Some(w.clone());
        extra(d, w)
    })
    .unwrap_or_else(|e| panic!("the loop ran: {e}"));
    let w = kept.lock().unwrap_or_else(|e| e.into_inner()).take().expect("the world");
    (r, w)
}

const STALE: &str = "rc-a";

/// A bead that submits one minute into its session, so it is in the pool well before the
/// batcher's orphan wait (`SPIRA_BATCHER_ORPHAN_REDRIVE_SECS`, 300 s in the loop's settings) ends.
fn early(id: &str) -> Bead {
    Bead { submits: vec![MIN], ..Bead::new(id, 30_000) }
}

/// The store opens with the record of an abandoned round still in the projection: `rc-a`, its
/// row ABANDONED by `requeue-orphans` and its member requeued, no driver. `redrive_ends_it` is
/// whether `round-recert` on it reconciles the ended record away (`reconcile_ended`).
fn stale_round(seed: u64, until: u64, redrive_ends_it: bool) -> (Run, Vec<String>) {
    let script = Script { beads: vec![early("sp-rp02")], max_concurrent: 1, until, ..Script::default() };
    let redriven: Arc<Mutex<Vec<String>>> = Arc::default();
    let filed: Arc<Mutex<Vec<String>>> = Arc::default();
    let (seen, refs) = (redriven.clone(), filed.clone());
    let (r, _) = run_keeping(seed, script, move |d, w| {
        w.lc().round = Some((STALE.into(), vec![]));
        let w2 = w.clone();
        d.double(bl::REDRIVE, move |input| {
            let l = leg(input)?;
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(l.batch.clone());
            let mut lc = w2.lc();
            if redrive_ends_it && l.batch == STALE && lc.round.as_ref().is_some_and(|(b, _)| *b == l.batch) {
                lc.round = None;
            }
            Ok("{}".into())
        })
        .double(bl::FILE, move |input| {
            refs.lock().unwrap_or_else(|e| e.into_inner()).push(leg(input)?.key);
            Ok("{}".into())
        })
    });
    let filed = filed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(*redriven.lock().unwrap_or_else(|e| e.into_inner()), [STALE], "the stale round was re-driven once, by its own id: {}", r.dump());
    (r, filed)
}

/// Replaces `rc-stale-round-pointer` (sp-2dba9l, sp-15abll): a round still certifies and lands
/// while an abandoned round's record is stale. On the service's tasks the stale record is an
/// open round with no driver: `batcher.local` waits out the orphan wait, re-drives it once (the
/// `redrive` leg, `round-recert`), and once the ended record is reconciled away it cuts the pool
/// into a round of its own, certifies it and lands it. The pool was SUBMITTED before the
/// re-drive, so the stale record held it, and nothing landed under the stale round's id. The
/// queue's half (land acts on the open round, never on an ended record) is
/// `queue::tests::round::land_acts_on_the_open_round_and_promotes_no_ended_record`.
#[test_id(t0svndh)]
#[test]
fn stale_round_pointer_a_redriven_stale_record_ends_and_the_next_round_certifies_and_lands() {
    let (r, filed) = stale_round(31, 30 * MIN, true);
    show("rc-stale-round-pointer", &r);
    let redrive = times(&r, bl::REDRIVE)[0];
    assert!(redrive >= 300_000, "the re-drive waited out the orphan wait: {redrive}");
    let submitted = entered(&r, "sp-rp02", "SUBMITTED").unwrap_or_else(|| panic!("the bead was submitted: {}", r.dump()));
    assert!(submitted < redrive, "the pool waited behind the stale record: submitted {submitted}, re-driven {redrive}");
    let opens = times(&r, bl::ROUND_OPEN);
    assert_eq!(opens.len(), 1, "one round opened: {}", r.dump());
    assert!(opens[0] > redrive, "no round was cut over the stale record: opened {opens:?}, re-driven {redrive}");
    assert_eq!(r.lifecycle_of("sp-rp02"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", r.dump());
    let tip = r.submitted["sp-rp02"][0].clone();
    assert_eq!(r.lands, [vec![("sp-rp02".to_string(), tip)]], "the new round landed its member");
    assert!(filed.is_empty(), "a stale record the re-drive ended files no incident: {filed:?}");
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(batcher_states(&r).last().map(String::as_str), Some("Idle"), "{}", r.dump());
    assert_no_process(&r);
}

/// The boundary of the same property: a stale record the re-drive cannot end is never cut over
/// while it stands. `batcher.local` re-drives it once, then (undriven again past the wait)
/// abandons it, naming why, and files no incident: no round stays open with nothing driving it.
#[test_id(tikot08)]
#[test]
fn stale_round_pointer_a_stale_record_the_redrive_cannot_end_is_abandoned_after_one_redrive() {
    let (r, filed) = stale_round(32, 20 * MIN, false);
    show("rc-stale-round-pointer/abandon", &r);
    assert!(filed.is_empty(), "an abandoned record files no incident: {filed:?}");
    let (redrive, abandon) = (times(&r, bl::REDRIVE)[0], times(&r, bl::ABANDON)[0]);
    assert!(abandon >= redrive + 300_000, "the abandon waited a second orphan wait after the re-drive: {abandon} vs {redrive}");
    assert!(r.asked.iter().filter(|n| *n == bl::ABANDON).count() == 1, "abandoned once: {:?}", r.asked);
    assert_no_process(&r);
}

/// A round whose certify unit never starts (the start reports success, no unit runs, no
/// `unit.ran` ever comes): the leg is started once more, then the round is abandoned and the
/// pool is cut again at once, with no wait. Nothing leaves the round open; a repeat is
/// braked by count (pace tests), never by wall time.
#[test_id(tz1o6df)]
#[test]
fn a_corpus_leg_that_never_runs_is_started_again_once_then_the_round_is_abandoned() {
    let script = Script { beads: vec![Bead::new("sp-lg01", 30_000)], max_concurrent: 1, until: 20 * MIN, ..Script::default() };
    let starts: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = starts.clone();
    let (r, _) = run_keeping(34, script, move |d, _| {
        d.double(bl::CERTIFY, move |input| {
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(leg(input)?.unit);
            Ok(String::new())
        })
    });
    show("corpus-leg-never-runs", &r);
    let starts = starts.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(starts.len() >= 2 && starts[0] != starts[1], "the certify leg was started again under a new unit: {starts:?}");
    let (first, abandon) = (times(&r, bl::CERTIFY)[0], times(&r, bl::ABANDON)[0]);
    assert!(abandon - first >= 60_000 && abandon - first < 5 * 60_000, "re-started within a minute, abandoned soon after: first {first}, abandon {abandon}");
    let abandons = times(&r, bl::ABANDON);
    assert!(abandons.len() >= 2, "abandoned, then re-cut with no backoff and abandoned again: {}", r.dump());
    assert!(abandons[1] - abandons[0] < 5 * 60_000, "the re-cut did not wait: {abandons:?}");
    assert_eq!(batcher_states(&r)[..5], ["Idle", "Assembling", "Opened", "Certifying", "Idle"], "{}", r.dump());
    assert_no_process(&r);
}

/// A round the queue opened whose `round open` answer never reached the batcher (the call ended
/// at the open) is still driven: the next tick finds the open round holding exactly its members
/// and starts its first pass, well inside the outcome wait, and the round lands.
#[test_id(t7nk32l)]
#[test]
fn a_round_whose_open_answer_is_lost_is_driven_by_the_next_tick_and_lands() {
    let script = Script { beads: vec![Bead::new("sp-lo01", 30_000)], max_concurrent: 1, until: 30 * MIN, ..Script::default() };
    let first = Arc::new(Mutex::new(true));
    let (r, _) = run_keeping(36, script, move |d, w| {
        let w = w.clone();
        d.double(bl::ROUND_OPEN, move |input| {
            let opened = loopworld::batcher_round_open(&w, input)?;
            let mut first = first.lock().unwrap_or_else(|e| e.into_inner());
            if std::mem::take(&mut *first) {
                return Err(runtime::engine::LOST_AT_RESTART.into());
            }
            Ok(opened)
        })
    });
    show("open-answer-lost", &r);
    let (open, certify) = (times(&r, bl::ROUND_OPEN)[0], times(&r, bl::CERTIFY)[0]);
    assert!(certify - open < 5 * MIN, "the first pass started on the next ticks, not after the outcome wait: open {open}, certify {certify}");
    assert!(times(&r, bl::ABANDON).is_empty(), "the round was not abandoned: {}", r.dump());
    assert_eq!(r.lifecycle_of("sp-lo01").last().map(String::as_str), Some("LANDED"), "{}", r.dump());
    assert_no_process(&r);
}

/// The attribution line `batcher attribute` writes on its captured stdout.
fn attribution_land(members: &[(String, String)]) -> String {
    format!("attribution\t{}\n", serde_json::json!({ "end": "land", "members": members, "attribution_secs": 60, "mended": false }))
}

/// Replaces `rc-attributing-redrive` (sp-argpox): a round whose first pass ended red at lint
/// before any suite ran (red-early, the round VM recording the verdict) is re-driven from
/// ATTRIBUTING, goes green, lands, and its member is never returned. On the service's tasks:
/// the corpus leg ends red, `batcher.local` goes to Attributing and starts the attribution leg on
/// the same batch (never a fresh cut), which re-runs the red and names every member a survivor;
/// the round is certified and landed with the member never REWORK and nothing ejected or
/// withdrawn. The queue's half, pass 1 judged red on the lint step and pass 2 started straight
/// from ATTRIBUTING with no PassRebuilt, is
/// `queue::tests::round::certify_ends_red_on_a_lint_red_the_round_vm_recorded_and_re_drives_pass_two`
/// with `pass_start_from_attributing_starts_the_next_pass_without_a_rebuild` and
/// `certify_records_a_lint_or_fence_red_that_ended_the_pass_before_any_suite_ran`.
#[test_id(t7xqqpi)]
#[test]
fn attributing_redrive_a_red_early_round_is_redriven_on_its_own_batch_and_lands_its_member() {
    let script = Script { beads: vec![Bead::new("sp-ar01", 30_000)], max_concurrent: 1, until: 40 * MIN, ..Script::default() };
    let legs: Arc<Mutex<Vec<bl::Leg>>> = Arc::default();
    let seen = legs.clone();
    let first = Arc::new(Mutex::new(true));
    let (r, _) = run_keeping(33, script, move |d, w| {
        let (wa, wc) = (w.clone(), w.clone());
        d.double(bl::CORPUS, move |input| {
            let l = leg(input)?;
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(l.clone());
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["round-vm".into(), "run".into(), l.wt.clone()], env: vec![], cwd: None };
            wa.units.script(bl::NAME, MIN, Exit::Code(0));
            wa.units.script_output(bl::NAME, attribution_land(&l.members).as_bytes(), b"");
            let scope = ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None };
            units::effect(&*wa.units, UNIT_RUN_SCOPE, &serde_json::to_string(&scope).map_err(|e| e.to_string())?)
        })
        .double(bl::CERTIFY, move |input| {
            let l = leg(input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), l.batch, REPO.into()], env: vec![], cwd: None };
            // red-early spira-lint: the pass ends before any suite ran.
            let red = std::mem::take(&mut *first.lock().unwrap_or_else(|e| e.into_inner()));
            wc.units.script(bl::NAME, MIN, Exit::Code(i32::from(red)));
            wc.units.script_output(bl::NAME, if red { b"red=spira-lint\n" } else { b"certified\n" }, b"");
            units::effect(&*wc.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
    });
    show("rc-attributing-redrive", &r);
    assert_eq!(batcher_states(&r), ["Idle", "Assembling", "Opened", "Certifying", "Attributing", "Certifying", "Landing", "Idle"], "{}", r.dump());
    let legs = legs.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(legs.iter().map(|l| l.why.as_str()).collect::<Vec<_>>(), [bl::ATTRIBUTE], "one attribution leg after the red certify: {legs:?}");
    assert!(legs.iter().all(|l| l.batch == "round-1"), "the attribution re-drives the open round's own batch: {legs:?}");
    assert_eq!(r.lifecycle_of("sp-ar01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "the member was never returned: {}", r.dump());
    assert!(!r.lifecycle_of("sp-ar01").iter().any(|s| s == "REWORK"));
    for never in [bl::EJECT, bl::WITHDRAW, bl::ABANDON] {
        assert!(!r.asked.iter().any(|n| n == never), "{never} was never asked: {:?}", r.asked);
    }
    assert_eq!(r.opened.len(), 1, "the round was re-driven, not re-cut: {:?}", r.opened);
    assert_eq!(r.lands, r.opened, "the round opened is the round landed");
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// An attribution leg that ends while its pass scope is still in its post phase: `round certify`
/// refuses a round whose pass scope is alive, so the batcher starts it only once the scope has
/// ended, and the round that re-ran green lands without a hand.
#[test_id(t3okg7d)]
#[test]
fn an_attribution_leg_that_ends_while_its_pass_scope_is_finishing_still_lands_the_round() {
    let script = Script { beads: vec![Bead::new("sp-pp01", 30_000)], max_concurrent: 1, until: 40 * MIN, ..Script::default() };
    let certifies: Arc<Mutex<Vec<u64>>> = Arc::default();
    let seen = certifies.clone();
    let (r, _) = run_keeping(35, script, move |d, w| {
        let (wa, wc) = (w.clone(), w.clone());
        d.double(bl::CORPUS, move |input| {
            let l = leg(input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["round-vm".into(), "run".into(), l.wt.clone()], env: vec![], cwd: None };
            wa.units.script(bl::NAME, MIN, Exit::Code(0));
            wa.units.script_output(bl::NAME, attribution_land(&l.members).as_bytes(), b"");
            let ends = wa.now() + MIN + 90_000;
            wa.pass_scopes.lock().unwrap_or_else(|e| e.into_inner()).push((l.batch.clone(), ends));
            let scope = ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None };
            units::effect(&*wa.units, UNIT_RUN_SCOPE, &serde_json::to_string(&scope).map_err(|e| e.to_string())?)
        })
        .double(bl::CERTIFY, move |input| {
            let l = leg(input)?;
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(wc.now());
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), l.batch, REPO.into()], env: vec![], cwd: None };
            let refused = !wc.live_pass_scopes().is_empty();
            let red = !refused && seen.lock().unwrap_or_else(|e| e.into_inner()).len() == 1;
            wc.units.script(bl::NAME, if refused { 784 } else { MIN }, Exit::Code(i32::from(refused || red)));
            let said: &[u8] = if refused { b"queue.sh round certify: round round-1 is already being certified (pid=7) \xe2\x80\x94 refused\n" } else if red { b"red=test-a.sh\n" } else { b"certified\n" };
            wc.units.script_output(bl::NAME, said, b"");
            units::effect(&*wc.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
    });
    show("pass-scope-still-finishing", &r);
    let certifies = certifies.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(certifies.len() >= 2, "the red certify, then certify again once the pass scope ended: {certifies:?}");
    assert_eq!(r.lifecycle_of("sp-pp01").last().map(String::as_str), Some("LANDED"), "{}", r.dump());
    assert!(!r.asked.iter().any(|n| n == bl::ABANDON), "{:?}", r.asked);
    assert_no_process(&r);
}

/// Replaces `rc-fences-in-vm` (sp-ol6x08): a round's fences run inside the round VM from the
/// tree's gate.steps, never on the host. On the service's tasks the round's checks are its two
/// legs, both units: the corpus (`round-vm run`, which lists and runs the fences in the VM) and
/// `queue round certify` (a scope). `batcher.local` declares no fence or gate effect (the host
/// `run_fences`/`GATE_FENCES` path is gone), asks for none, and no other task starts a unit or
/// asks a gate effect for the round: every unit started is a dwarf session or one of the
/// round's legs. The VM half, fences listed from the tree's gate.steps on the host and run in
/// the VM before the suites, is `round-vm`'s
/// `run::tests::the_fences_finish_before_the_unit_tier_starts` and
/// `a_tree_whose_fences_cannot_be_listed_is_refused_before_a_vm_is_taken`.
#[test_id(td1y8zp)]
#[test]
fn fences_in_vm_the_rounds_checks_are_its_unit_legs_and_no_host_fence_or_gate_runs() {
    assert!(!bl::EFFECTS.iter().any(|e| e.contains("fence") || e.contains("gate")), "batcher.local declares no host fence or gate effect: {:?}", bl::EFFECTS);
    let script = Script { beads: vec![Bead::new("sp-fv01", 30_000)], max_concurrent: 1, until: 40 * MIN, ..Script::default() };
    let (r, w) = run_keeping(34, script, |d, _| d);
    show("rc-fences-in-vm", &r);
    assert_eq!(r.rows["sp-fv01"].state, BeadState::Landed, "{}", r.dump());
    assert!(!r.asked.iter().any(|n| n.contains("fence")), "no fence effect was asked: {:?}", r.asked);
    let gate: Vec<&String> = r.asked.iter().filter(|n| n.starts_with("gate.") && !n.ends_with(".observe")).collect();
    assert!(gate.is_empty(), "no host gate took the round: {gate:?}");
    let calls = w.units.calls();
    let owners: Vec<(String, String)> = calls.iter().filter_map(|c| c.split_once(' ')).filter_map(|(_, n)| UnitHandle::named(n).owner()).collect();
    assert_eq!(owners.len(), calls.len(), "every unit started under the naming rule: {calls:?}");
    let rounds: Vec<&str> = owners.iter().filter(|(t, _)| t == bl::NAME).map(|(_, k)| k.as_str()).collect();
    assert_eq!(rounds.len(), 1, "the round's one leg ran as a unit: {calls:?}");
    assert!(rounds.iter().all(|k| k.contains(".certify")), "a green round runs certify alone, no corpus: {rounds:?}");
    let others: Vec<&(String, String)> = owners.iter().filter(|(t, _)| t != bl::NAME && t != spira_runtime::tasks::a::sentinel_call::NAME).collect();
    assert!(others.is_empty(), "no other task started a unit: {others:?}");
    assert_eq!(w.units.scopes().iter().filter(|s| s.unit.task == bl::NAME).count(), 1, "certify is the round's one scope");
    assert_no_process(&r);
}

/// Replaces `run-state-trail` (sp-6l2puc): the round lands logged with its member, the publish
/// record names what landed, the certificate exists before the landing, the queue lock is free
/// between events. On the service's tasks: the `land` leg is asked with the member at its tip;
/// the certify leg (the `queue round certify` that writes the certificate) has ended and the
/// store certified the member before `land` is asked; and the publish record is opened at the
/// head that landed the member. The library halves are the queue's: the `ROUND-LAND ...
/// members=` line and the certificate on the tree before land in
/// `queue::tests::round::open_certify_land_is_one_round_the_whole_way`, the lock given back by
/// every round verb in
/// `each_round_verb_holds_the_queue_lock_only_for_its_own_transition_so_a_hand_abandon_gets_in_at_once`.
#[test_id(t4giom2)]
#[test]
fn run_state_trail_the_landing_names_its_member_after_its_certificate_and_the_publish_is_at_its_head() {
    let script = Script { beads: vec![Bead::new("sp-rs01", 30_000)], max_concurrent: 1, publish: true, until: 40 * MIN, ..Script::default() };
    let (r, _) = run_keeping(35, script.clone(), |d, _| d);
    show("run-state-trail", &r);
    assert_eq!(r.missed(&script), None, "{}", r.dump());
    let tip = r.submitted["sp-rs01"][0].clone();
    assert_eq!(r.lands, [vec![("sp-rs01".to_string(), tip)]], "the landing names its member at its tip: {}", r.dump());
    let (certify, lc_certify, land) = (times(&r, bl::CERTIFY), times(&r, bl::LC_CERTIFY), times(&r, bl::LAND));
    assert_eq!((certify.len(), lc_certify.len(), land.len()), (1, 1, 1), "{:?}", r.exec_log);
    assert!(land[0] >= certify[0] + loopworld::CERTIFY_MS, "certify ran to its end before land: certify {certify:?}, land {land:?}");
    assert!(lc_certify[0] <= land[0] && entered(&r, "sp-rs01", "CERTIFIED").is_some_and(|c| c <= land[0]), "the member was certified before land: {}", r.dump());
    assert_eq!(r.published, [(100, loopworld::main_after(1))], "the publish record is at the head the member landed at: {}", r.dump());
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// A provisional member (cut while SUBMITTED, its gate still running) whose own gate goes red
/// minutes into a pass does not stop it: the pass runs to its end without a restart, the round is
/// not abandoned, and a green pass lands the round with that member in it.
#[test_id(tdo1ero)]
#[test]
fn a_gate_red_recorded_for_a_provisional_member_mid_pass_leaves_the_pass_running_and_a_green_round_lands_it() {
    let script = Script {
        beads: vec![early("sp-gr01"), early("sp-gr02")],
        max_concurrent: 2,
        verdicts: vec![loopworld::VerdictAt { at: 3 * MIN, bead: "sp-gr01".into(), verdict: loopworld::Verdict::Red }],
        until: 40 * MIN,
        ..Script::default()
    };
    let (r, _) = run_keeping(36, script, |d, _| d);
    show("provisional-gate-red-mid-pass", &r);
    assert_eq!(times(&r, bl::CERTIFY).len(), 1, "the pass ran once, never restarted: {:?}", r.exec_log);
    assert!(times(&r, bl::EJECT_GATE_RED).is_empty() && times(&r, bl::ABANDON).is_empty(), "{:?}", r.exec_log);
    assert_eq!(r.lands[0].iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-gr01", "sp-gr02"], "the round landed with the red member in it: {}", r.dump());
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_no_process(&r);
}

/// The same red, but the pass ends red: the recorded member is ejected first, with the gate's
/// mark, before any attribution, and the other member lands from the round cut again.
#[test_id(tiej985)]
#[test]
fn a_red_pass_ejects_the_provisional_member_whose_gate_went_red_first_and_the_other_lands() {
    let red = loopworld::VerdictAt { at: 3 * MIN, bead: "sp-gr03".into(), verdict: loopworld::Verdict::Red };
    let script = Script { beads: vec![early("sp-gr03"), early("sp-gr04")], max_concurrent: 2, verdicts: vec![red], until: 40 * MIN, ..Script::default() };
    let first = Arc::new(Mutex::new(true));
    let (r, _) = run_keeping(37, script, move |d, w| {
        let wc = w.clone();
        d.double(bl::CERTIFY, move |input| {
            let l = leg(input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), l.batch, REPO.into()], env: vec![], cwd: None };
            let red = std::mem::take(&mut *first.lock().unwrap_or_else(|e| e.into_inner()));
            wc.units.script(bl::NAME, 2 * MIN, Exit::Code(i32::from(red)));
            wc.units.script_output(bl::NAME, if red { b"red=test-x.sh\n" } else { b"certified\n" }, b"");
            units::effect(&*wc.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
    });
    show("provisional-gate-red-red-pass", &r);
    let (eject, attribute) = (times(&r, bl::EJECT_GATE_RED), times(&r, bl::CORPUS));
    assert_eq!(eject.len(), 1, "{:?}", r.exec_log);
    assert!(attribute.iter().all(|t| *t > eject[0]), "ejected before any attribution: eject {eject:?}, corpus {attribute:?}");
    assert!(entered(&r, "sp-gr03", "REWORK").is_some(), "the red member went back to its dwarf: {}", r.dump());
    let tip = r.submitted["sp-gr04"][0].clone();
    assert_eq!(r.lands[0], [("sp-gr04".to_string(), tip)], "the other member landed alone from the round cut again: {}", r.dump());
    assert_no_process(&r);
}

/// A round whose pass is red and which is abandoned without ejecting a member requeues each to
/// SUBMITTED at the tip the pass was red at; the gate then certifies that tip. A dependent must
/// not be claimed on it: it waits until the tip moves and is certified, and the blocker, sent
/// back to REWORK, is claimed first.
fn red_unattributed_round(seed: u64, red: bool) -> Run {
    let a = Bead { submits: vec![590_000], ..Bead::new("sp-rr01", 30_000) };
    let b = Bead { blocks: vec!["sp-rr01".into()], ..Bead::new("sp-rr02", 30_000) };
    let verdicts = vec![
        loopworld::VerdictAt { at: 781_000, bead: "sp-rr01".into(), verdict: loopworld::Verdict::Pass },
        loopworld::VerdictAt { at: 830_000, bead: "sp-rr01".into(), verdict: loopworld::Verdict::Red },
        loopworld::VerdictAt { at: 1_491_000, bead: "sp-rr01".into(), verdict: loopworld::Verdict::Pass },
    ];
    let script = Script { beads: vec![a, b], max_concurrent: 1, verdicts, until: 60 * MIN, ..Script::default() };
    let reds = Arc::new(Mutex::new(2));
    let (r, _) = run_keeping(seed, script, move |d, w| {
        let (wc, wf, wa) = (w.clone(), w.clone(), w.clone());
        d.double(bl::CERTIFY, move |input| {
            let l = leg(input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), l.batch, REPO.into()], env: vec![], cwd: None };
            let mut left = reds.lock().unwrap_or_else(|e| e.into_inner());
            let red = *left > 0;
            *left -= i32::from(red);
            wc.units.script(bl::NAME, MIN, Exit::Code(i32::from(red)));
            wc.units.script_output(bl::NAME, if red { b"red=test-x.sh\n" } else { b"certified\n" }, b"");
            units::effect(&*wc.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
        .double(bl::CORPUS, move |input| {
            let l = leg(input)?;
            let spec = UnitSpec { task: bl::NAME.into(), key: l.unit.clone(), argv: vec!["round-vm".into(), "run".into(), l.wt.clone()], env: vec![], cwd: None };
            wf.units.script(bl::NAME, MIN, Exit::Code(0));
            let said = if l.why == bl::ATTRIBUTE { "attribution\t{\"end\":\"fault\",\"why\":\"names no member that can be ejected\"}\n" } else { "" };
            wf.units.script_output(bl::NAME, said.as_bytes(), b"");
            units::effect(&*wf.units, UNIT_RUN_SCOPE, &serde_json::to_string(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None }).map_err(|e| e.to_string())?)
        })
        .double(bl::ABANDON, move |_| {
            let now = wa.now();
            let mut lc = wa.lc();
            let members = lc.round.take().map(|(_, m)| m).unwrap_or_default();
            for (id, tip) in members {
                lc.apply(now, &id, bl::NAME, lifecycle::bead::BeadEventKind::Deliver)?;
                lc.apply(now, &id, bl::NAME, lifecycle::bead::BeadEventKind::Requeued { tip, red })?;
            }
            Ok("{}".into())
        })
    });
    r
}

#[test_id(t0ligki)]
#[test]
fn a_dependent_is_not_claimed_on_the_tip_a_red_round_abandoned_and_the_blocker_is_reworked_first() {
    let r = red_unattributed_round(41, true);
    show("red-tip-not-stacked-on", &r);
    let red_tip = r.submitted["sp-rr01"][0].clone();
    let new_tip = r.submitted["sp-rr01"][1].clone();
    let claims: Vec<(&str, Vec<(&String, &String)>)> = r.claims.iter().map(|(id, s)| (id.as_str(), s.iter().collect())).collect();
    assert_eq!(
        claims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        ["sp-rr01", "sp-rr01", "sp-rr02"],
        "A, then A again from REWORK, and B only after: {}",
        r.dump()
    );
    assert_eq!(r.claims[2].1.get("sp-rr01"), Some(&new_tip), "B stacked on A's new certified tip, not the red one {red_tip}: {claims:?}");
    assert!(r.refused.is_empty(), "{:?}", r.refused);
}
