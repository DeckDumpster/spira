//! The world scenarios of `spira/sim/scenarios/` whose property is the loop's own, ported to the
//! loop world (`spira_sim::loopworld`): the service's registrations on the `VirtualDriver`, every
//! effect a double (ruling D6). Each test names the scenario it replaces and asserts that
//! scenario's property on the service's own tasks.

use lifecycle::bead::BeadState;
use spira_sim::loopworld::{self, Bead, Run, Script, Verdict, VerdictAt, MIN};

fn run(seed: u64, script: Script) -> Run {
    loopworld::run(seed, script).unwrap_or_else(|e| panic!("the loop ran: {e}"))
}

fn verdict(at: u64, bead: &str, verdict: Verdict) -> VerdictAt {
    VerdictAt { at, bead: bead.into(), verdict }
}

fn show(name: &str, r: &Run) {
    println!("{name}: wall {:?}", r.wall);
    if std::env::var_os("W3_DUMP").is_some() {
        println!("=== {name}\n{}", r.dump());
    }
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    assert!(r.unanswered().is_empty(), "every effect asked for had a double: {:?}", r.unanswered());
    assert!(r.wall.as_secs() < 30, "the scenario ran in {:?}", r.wall);
}

/// Replaces `resubmit-moved-tip` (sp-2adk7): a dwarf that submits, commits again and submits
/// once more has the moved tip recorded on its SUBMITTED row (`lc_tip = branch_tip`). On the
/// service's tasks the moved tip is also the one sift screens and the round cuts, certifies and
/// lands; the first tip never reaches a round. The `rounds` lane is stopped until both submits
/// are in, as the world's cut waited for its sift pass.
#[test]
fn resubmit_moved_tip_the_moved_tip_is_recorded_screened_cut_and_landed() {
    let bead = Bead { submits: vec![4 * MIN, 9 * MIN], ..Bead::new("sp-rm01", 30_000) };
    let script = Script { beads: vec![bead], max_concurrent: 1, stopped_until: 14 * MIN, verdicts: vec![], until: 40 * MIN, ..Script::default() };
    let r = run(21, script);
    show("resubmit-moved-tip", &r);
    let tips = &r.submitted["sp-rm01"];
    assert_eq!(tips.len(), 2, "both submits reached the store: {}", r.dump());
    let moved = tips[1].clone();
    assert_ne!(tips[0], moved, "the second submit is at a moved tip");
    assert!(r.refused.is_empty(), "the store refused nothing, the second submit on SUBMITTED included: {:?}", r.refused);
    assert_eq!(r.lifecycle_of("sp-rm01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", r.dump());
    assert_eq!(r.rows["sp-rm01"].tip.as_deref(), Some(moved.as_str()), "the row carries the moved tip (moved_tip_was_recorded)");
    assert!(r.sifted.iter().any(|(id, t)| id == "sp-rm01" && *t == moved), "sift screened the moved tip: {:?}", r.sifted);
    assert_eq!(r.opened, [vec![("sp-rm01".to_string(), moved.clone())]], "one round, cut at the moved tip");
    assert_eq!(r.lands, [vec![("sp-rm01".to_string(), moved)]], "the moved tip is what landed");
    assert_no_process(&r);
}

/// Replaces `certify-twice` (sp-2adk7): with the cut stood down, a gate certifies the bead at
/// its tip, and the same verdict delivered twice more (the gate's repeat runs) is answered
/// without moving the row — a CERTIFIED bead stays CERTIFIED at its tip. When the cut resumes
/// it takes the gate-certified bead as it stands (its own certify is one more repeat verdict at
/// the same tip) and lands it: the row never falls back to SUBMITTED.
#[test]
fn certify_twice_a_repeat_verdict_keeps_the_row_certified_and_the_round_lands_it() {
    let at = |m: u64| 12 * MIN + m * 30_000;
    let verdicts = vec![verdict(at(0), "sp-ct01", Verdict::Pass), verdict(at(1), "sp-ct01", Verdict::Pass), verdict(at(2), "sp-ct01", Verdict::Pass)];
    let script = Script { beads: vec![Bead::new("sp-ct01", 30_000)], max_concurrent: 1, stopped_until: 15 * MIN, verdicts, until: 40 * MIN, ..Script::default() };
    let r = run(22, script);
    show("certify-twice", &r);
    let states: Vec<BeadState> = r.outside.iter().map(|o| o.state_before).collect();
    assert_eq!(states, [BeadState::Submitted, BeadState::Certified, BeadState::Certified], "the gate's three verdicts met the row SUBMITTED, then CERTIFIED twice: {}", r.dump());
    assert!(r.outside.iter().all(|o| o.applied), "the first verdict certified the bead and both repeats were accepted (repeat_verdict_accepted): {}", r.dump());
    assert!(r.refused.is_empty(), "the loop's own writes were all applied, the cut's certify of a CERTIFIED member included: {:?}", r.refused);
    assert_eq!(r.lifecycle_of("sp-ct01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "repeat verdicts never moved the row back: {}", r.dump());
    let tip = r.submitted["sp-ct01"][0].clone();
    assert_eq!(r.lands, [vec![("sp-ct01".to_string(), tip)]], "the gate-certified tip is what landed");
    assert_no_process(&r);
}

/// Replaces `long-gate-interleaving` (sp-2adk7): a slow gate whose verdict arrives after the
/// round landed the bead must not move it. LANDED is final (`landed_is_final`), and nothing
/// readmits the landed bead to a cut (`no_stale_admission`): one round, opened once.
#[test]
fn long_gate_interleaving_a_verdict_after_the_landing_leaves_the_bead_landed() {
    let verdicts = vec![verdict(30 * MIN, "sp-lg01", Verdict::Pass), verdict(31 * MIN, "sp-lg01", Verdict::Red)];
    let script = Script { beads: vec![Bead::new("sp-lg01", 30_000)], max_concurrent: 1, stopped_until: 0, verdicts, until: 36 * MIN, ..Script::default() };
    let r = run(23, script);
    show("long-gate-interleaving", &r);
    let landed = r.landed_at("sp-lg01").unwrap_or_else(|| panic!("the bead landed: {}", r.dump()));
    assert!(landed < 30 * MIN, "the round landed before the slow gate finished: {}", r.dump());
    assert_eq!(r.outside.len(), 2, "{}", r.dump());
    for o in &r.outside {
        assert_eq!(o.state_before, BeadState::Landed, "{o:?}");
        assert!(!o.applied, "a gate verdict on a LANDED row is refused: {o:?}");
    }
    let after: Vec<&(u64, String, String, String)> = r.history.iter().filter(|(at, id, ..)| id == "sp-lg01" && *at > landed).collect();
    assert!(after.is_empty(), "LANDED is final: {after:?}");
    assert_eq!(r.rows["sp-lg01"].state, BeadState::Landed);
    assert_eq!(r.opened.len(), 1, "no landed bead is admitted to another cut: {:?}", r.opened);
    assert_no_process(&r);
}

/// Replaces `conflicting-siblings` (sp-2adk7): two siblings change the same file, the
/// higher-priority one (priority 0) filed alongside a priority-3 one. Both are sifted before the
/// cut (the `rounds` lane is stopped until then), so one cut sees both: the round takes the
/// priority-0 sibling and the other conflicts with it. The priority-0 sibling lands first; the
/// loser, now stale on the moved base, is returned to its dwarf (REWORK, `loser_was_returned`),
/// is claimed again, resubmits a tip built on the new base and lands after the winner
/// (`priority_won_the_conflict`).
#[test]
fn conflicting_siblings_priority_wins_the_conflict_and_the_loser_is_returned_and_lands_after() {
    let winner = Bead { priority: 0, file: "shared".into(), ..Bead::new("sp-cs02", 30_000) };
    let loser = Bead { priority: 3, file: "shared".into(), ..Bead::new("sp-cs01", 30_000) };
    let script = Script { beads: vec![loser, winner], max_concurrent: 2, stopped_until: 15 * MIN, verdicts: vec![], until: 70 * MIN, ..Script::default() };
    let r = run(24, script);
    show("conflicting-siblings", &r);
    assert!(r.refused.is_empty(), "the store refused nothing: {:?}", r.refused);
    let first = r.lands.first().unwrap_or_else(|| panic!("a round landed: {}", r.dump()));
    assert_eq!(first.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-cs02"], "the first round landed the priority-0 sibling alone: {}", r.dump());
    let loser = r.lifecycle_of("sp-cs01");
    assert!(loser.iter().any(|s| s == "REWORK"), "the loser was returned (loser_was_returned): {}", r.dump());
    let (w, l) = (r.landed_at("sp-cs02"), r.landed_at("sp-cs01"));
    assert!(w.is_some() && l.is_some() && w < l, "priority won the conflict: the winner landed at {w:?}, before the loser at {l:?}: {}", r.dump());
    let last = r.submitted["sp-cs01"].last().cloned();
    assert_eq!(r.rows["sp-cs01"].tip, last, "the loser landed its reworked tip");
    assert_no_process(&r);
}

/// Replaces the world scenario `happy-path` (sp-npsese), which `watch.perf_happy` runs in
/// production: the shipped scenario file, now a loop world, files one bead and ends with it
/// LANDED and its landed head published by PR, as `sim run happy-path` checks it. The run's
/// exec.log, one line per effect, is what `perf-watch` reads its slowest phase from.
#[test]
fn happy_path_the_shipped_loop_scenario_lands_and_publishes_and_logs_its_effects() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios/happy-path.toml")).expect("the shipped scenario");
    assert!(loopworld::is_loop(&text), "happy-path is a loop world");
    let script = loopworld::parse(&text).expect("it parses");
    let r = run(7, script.clone());
    show("happy-path", &r);
    assert_eq!(r.missed(&script), None, "{}", r.dump());
    assert_eq!(r.lifecycle_of("sp-hp01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", r.dump());
    assert_eq!(r.published, [(100, loopworld::main_after(1))], "one publish PR, at the landed head: {}", r.dump());
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    let log = r.exec_log.join("\n");
    let slowest = perf_watch::happy::slowest_phase(&log);
    assert!(slowest.is_some(), "perf-watch reads a slowest phase from the run's exec.log: {log}");
    assert!(log.contains(" publish.pr_create exit status: 0 in "), "{log}");
    assert_no_process(&r);
}

/// A script's `settings` reach the service `run_with` boots, over the loop's own defaults
/// (its batcher wall included).
#[test]
fn a_scripts_settings_reach_the_service_the_run_boots() {
    let set = |k: &str, v: &str| Script { settings: [(k.to_string(), v.to_string())].into(), ..Script::default() };
    let booted = loopworld::settings_for(&set("SPIRA_RUNTIME_HEARTBEAT_SECS", "11")).expect("the script's settings resolve");
    assert_eq!(booted.heartbeat_secs, 11, "the script's value is the one booted");
    assert_eq!(loopworld::settings_for(&Script::default()).expect("resolved").heartbeat_secs, 300, "and not the loop's default");
    let wall = loopworld::settings_for(&set("SPIRA_BATCHER_WALL_SECS", "60")).expect("resolved");
    assert_eq!(wall, loopworld::settings_with(&[("SPIRA_BATCHER_WALL_SECS", "60")]).expect("resolved"), "a script's batcher wall wins over the loop's 3600");
    assert_ne!(wall, loopworld::settings_for(&Script::default()).expect("resolved"));
}
