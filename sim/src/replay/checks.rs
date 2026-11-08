//! The whole-slice replay checks: what the round-controller sections a reader would have seen
//! say about the captured days, read back through the store, and what the replay guarantees of
//! itself. Every expectation is taken from the fixtures. Each check panics on a failure; they
//! cost seconds each, so `sim-replay` runs them (spira/test-sim-replay.sh) and the unit tier's
//! copies in sim/tests/it/replay.rs are ignored. Wall time is reported against the 100 ms goal,
//! never asserted.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::OnceLock;

use serde_json::{json, Value};
use spira_desired_state::section::parse_rfc3339;

use super::{round_controller, BatchState, Record, Replay, Scripted, ScriptedOwner, Slice};
use crate::replay;
const SEED: u64 = 31;

mod lifecycle;
pub use lifecycle::the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands;
mod ranking;
pub use ranking::{the_held_out_lanes_rank_above_every_healthy_section, the_inverted_map_fails_the_stacked_repo_ranking_check, the_stacked_repos_round_controller_section_binds_from_its_first_refused_round};

static FIXTURES: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Reads the slices from `dir` instead of the source tree's: an installed binary was built
/// elsewhere. Set it before the first check runs.
pub fn use_fixtures(dir: std::path::PathBuf) {
    assert!(FIXTURES.set(dir).is_ok(), "the fixtures root is set once, before any check");
}

fn fixtures() -> std::path::PathBuf {
    FIXTURES.get().cloned().unwrap_or_else(replay::fixtures_root)
}

fn open(slice: Slice) -> Replay {
    Replay::from_dir(SEED, &fixtures().join(slice.dir()), slice.window()).unwrap()
}
const STACKED_REPO: &str = "repo-b";
const REPEAT_REPO: &str = "repo-c";
/// The manifest's repeat-red member, at the tip it was re-cut at.
const REPEAT_IDENTITY: &str = "b-1254@05c3a8aaa5c7";
const HELD_OUT_LANES: [&str; 2] = ["repo-d", "repo-e"];

/// One pass's read of one repo's published section.
#[derive(Clone, Debug, PartialEq)]
struct Reading {
    at: u64,
    repo: String,
    observed_at: u64,
    status: Value,
}

impl Reading {
    fn level(&self, field: &str) -> f64 {
        self.status[field]["value"].as_f64().unwrap_or_else(|| panic!("{field} in {}", self.status))
    }

    fn condition(&self, type_: &str) -> (&str, &str) {
        let c = self.status["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == type_)
            .unwrap_or_else(|| panic!("no {type_} in {}", self.status));
        (c["status"].as_str().unwrap(), c["reason"].as_str().unwrap())
    }
}

/// A pass that reads `repos`' sections through `section get` and keeps what it read.
fn reader(replay: &mut Replay, repos: &[&str]) -> Rc<RefCell<Vec<Reading>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (out, repos): (_, Vec<String>) = (seen.clone(), repos.iter().map(|r| r.to_string()).collect());
    replay.register_pass(move |store, now| {
        for repo in &repos {
            let (code, v) = store.call(&["section", "get", &format!("RoundController/{repo}")], None);
            if code != 0 {
                continue;
            }
            let s = &v["section"];
            out.borrow_mut().push(Reading {
                at: now / 1000,
                repo: repo.clone(),
                observed_at: parse_rfc3339(s["metadata"]["observedAt"].as_str().unwrap()).unwrap(),
                status: s["status"].clone(),
            });
        }
    });
    seen
}

fn stamp(s: &str) -> u64 {
    parse_rfc3339(s).unwrap()
}

/// The slice's round lines in replay order, loaded once per test binary.
fn rounds(slice: Slice) -> &'static [replay::Round] {
    static OCT31: OnceLock<Vec<replay::Round>> = OnceLock::new();
    static HELD_OUT: OnceLock<Vec<replay::Round>> = OnceLock::new();
    let cell = if slice == Slice::Oct31 { &OCT31 } else { &HELD_OUT };
    cell.get_or_init(|| {
        replay::load(&fixtures().join(slice.dir()))
            .unwrap()
            .into_iter()
            .filter_map(|(_, r)| match r {
                Record::Round(r) => Some(r),
                _ => None,
            })
            .collect()
    })
}

fn line_count(slice: Slice) -> usize {
    let dir = fixtures().join(slice.dir());
    replay::FILES
        .iter()
        .map(|(f, _)| std::fs::read_to_string(dir.join(f)).unwrap().lines().count())
        .sum()
}

/// What one run of the 10-31 slice left behind, kept as plain data so several tests share it.
struct Run {
    readings: Vec<Reading>,
    fingerprints: Vec<u64>,
    trace: Vec<(u64, crate::EventKind, bool)>,
    applied: usize,
    lines: usize,
    pass_runs: usize,
    rc_runs: Option<usize>,
    keepalive_runs: Option<usize>,
    acked: Vec<(String, Scripted)>,
    intents: Vec<Value>,
}

/// A publisher that only records the world's fingerprint each minute.
fn fingerprints(replay: &mut Replay) -> Rc<RefCell<Vec<u64>>> {
    let out = Rc::new(RefCell::new(Vec::new()));
    let o = out.clone();
    replay.register_publisher("fingerprint", 60_000, move |w, _, _| o.borrow_mut().push(w.fingerprint()));
    out
}

/// A stand-in for the reconciler's pass: under its lock, one `hold-member` intent per
/// distinct RepeatIdentity reason on each of `repos`' sections.
fn intent_writer(replay: &mut Replay, repos: &'static [&'static str]) {
    let mut token: Option<String> = None;
    let mut taken = BTreeSet::new();
    replay.register_pass(move |store, now| {
        for repo in repos {
            let (code, v) = store.call(&["section", "get", &format!("RoundController/{repo}")], None);
            if code != 0 {
                continue;
            }
            let s = &v["section"]["status"];
            let raised = s["conditions"].as_array().unwrap().iter().find(|c| c["type"] == "RepeatIdentity" && c["status"] == "True");
            let Some(reason) = raised.map(|c| c["reason"].as_str().unwrap().to_string()) else {
                continue;
            };
            if !taken.insert((repo, reason.clone())) {
                continue;
            }
            let token = token.get_or_insert_with(|| {
                let v = store.ok(&["lock", "acquire", "reconciler", "--ttl", "604800", "--holder", "replay-test"], None);
                v["token"].as_str().unwrap().to_string()
            });
            let id = format!("hold-{}", taken.len());
            let body = json!({
                "id": id, "action": "hold-member", "target": reason.split_whitespace().next().unwrap(),
                "from": false, "to": true,
                "moves": "repeat_identity_open", "baseline": s["repeat_identity_open"]["value"],
                "expected_effect": -1.0, "horizon": "10m", "addresses": ["RepeatIdentity"],
                "taken_at": spira_desired_state::store::format_rfc3339(now / 1000),
                "by": { "pass": format!("replay-{now}"), "lock_token": token },
            });
            let key = spira_desired_state::intent::intent_key("RoundController", repo, &id);
            store.ok(&["intent", "put", &key, "--token", token, "--actor", "reconciler"], Some(&body));
        }
    });
}

fn run_oct31(with_intents: bool) -> Run {
    let t0 = std::time::Instant::now(); // clock-safe: reported against the 100 ms goal, never asserted
    let mut r = open(Slice::Oct31);
    round_controller::register(&mut r);
    r.register_publisher("keepalive-probe", 300_000, |_, _, _| {});
    let prints = fingerprints(&mut r);
    let seen = reader(&mut r, &[STACKED_REPO, REPEAT_REPO, "repo-a"]);
    if with_intents {
        intent_writer(&mut r, &[REPEAT_REPO, STACKED_REPO]);
        r.register_owner(ScriptedOwner::new("RoundController", REPEAT_REPO));
        let mut n = 0;
        r.register_owner(ScriptedOwner::new("RoundController", STACKED_REPO).script(move |_| {
            n += 1;
            if n == 1 {
                Scripted::Refused("held by the operator".into())
            } else {
                Scripted::NoAck
            }
        }));
    }
    r.run();
    eprintln!("replay 1031 (intents: {with_intents}): {} ms (goal: 100 ms)", t0.elapsed().as_millis()); // clock-safe
    let snap = r.store().ok(&["section", "snapshot"], None);
    let acked = r.owners().iter().flat_map(|o| o.acked.clone()).collect();
    let run = Run {
        readings: seen.borrow().clone(),
        fingerprints: prints.borrow().clone(),
        trace: r.trace_signature(),
        applied: r.world().applied,
        lines: r.lines(),
        pass_runs: r.pass_runs(),
        rc_runs: r.publisher_runs(round_controller::PUBLISHER),
        keepalive_runs: r.publisher_runs("keepalive-probe"),
        acked,
        intents: snap["intents"].as_array().unwrap().clone(),
    };
    run
}

fn baseline() -> &'static Run {
    static RUN: OnceLock<Run> = OnceLock::new();
    RUN.get_or_init(|| run_oct31(false))
}

/// The same slice and seed with intents written and scripted acks.
fn with_intents() -> &'static Run {
    static RUN: OnceLock<Run> = OnceLock::new();
    RUN.get_or_init(|| run_oct31(true))
}

fn of<'a>(run: &'a Run, repo: &str) -> Vec<&'a Reading> {
    run.readings.iter().filter(|r| r.repo == repo).collect()
}

pub fn the_publisher_sees_the_stacked_parent_from_its_first_refused_child() {
    let rounds = rounds(Slice::Oct31);
    let (start, _) = Slice::Oct31.window();
    let first = rounds
        .iter()
        .find(|r| r.repo == STACKED_REPO && stamp(&r.at) >= start && r.verdict.as_deref() == Some("refused") && r.parent.is_some())
        .expect("a refused round stacked on a parent");
    let parent = first.parent.clone().unwrap();
    let ended = rounds.iter().find(|r| r.batch == parent && r.state_to.terminal()).expect("the parent ends in the slice");
    let first_green = rounds
        .iter()
        .find(|r| r.repo == STACKED_REPO && r.parent.as_deref() == Some(parent.as_str()) && r.state_to == BatchState::Green)
        .unwrap();
    let green_member = &first_green.members[0];
    let (from, until) = (stamp(&first.at) + 60, stamp(&ended.at));

    let readings: Vec<_> = of(baseline(), STACKED_REPO).into_iter().filter(|r| r.at >= from && r.at < until).collect();
    assert!(readings.len() as u64 >= (until - from) / 60 - 1, "a reading every pass: {}", readings.len());
    for r in &readings {
        let (status, reason) = r.condition("StackedOnUndrivenParent");
        assert_eq!(status, "True", "at {}: {reason}", r.at);
        assert!(reason.contains(&parent) && reason.contains("unleased"), "the reason names the unleased parent: {reason}");
        let waiting = r.status["green_unlanded"].as_array().unwrap();
        let entry = waiting.iter().find(|e| e["bead"] == green_member.bead.as_str()).expect("the stacked member waits");
        assert_eq!(entry["since"], first_green.at.as_str());
        assert!(entry["blocker"].as_str().unwrap().contains(&parent));
        let age = (r.observed_at - stamp(&first_green.at)) / 60;
        assert!(r.level("green_unlanded_minutes") >= age as f64, "at {}: it is at least the stacked member's wait, {age} min", r.at);
    }
    let (first, last) = (readings[0], readings[readings.len() - 1]);
    let span = (last.observed_at - first.observed_at) / 60;
    assert!(span >= 180, "the stack stands for hours: {span} min");
    assert!(last.level("green_unlanded_minutes") - first.level("green_unlanded_minutes") >= span as f64 - 1.0, "it rises with the gap's age");

    let after: Vec<_> = of(baseline(), STACKED_REPO).into_iter().filter(|r| r.observed_at > until + 60).collect();
    assert_eq!(after[0].condition("StackedOnUndrivenParent").0, "False", "a terminal parent is no longer a blocker");
}

pub fn the_publisher_sees_the_repeat_red_member_re_cut_at_one_tip() {
    let rounds = rounds(Slice::Oct31);
    let (bead, tip) = REPEAT_IDENTITY.split_once('@').unwrap();
    let mut red = false;
    let mut first_recut = None;
    for r in rounds.iter().filter(|r| r.repo == REPEAT_REPO && r.members.iter().any(|m| m.bead == bead && m.tip == tip)) {
        if red && r.state_from == BatchState::None {
            first_recut = Some(stamp(&r.at));
            break;
        }
        red |= r.state_from == BatchState::CiRunning && r.cause == "red-attributed";
    }
    let first_recut = first_recut.expect("the member is re-cut after a red");

    let readings = of(baseline(), REPEAT_REPO);
    let next = readings.iter().find(|r| r.observed_at >= first_recut).unwrap();
    assert!(next.observed_at - first_recut <= 60, "published within one period");
    let (status, reason) = next.condition("RepeatIdentity");
    assert_eq!(status, "True", "{reason}");
    assert!(next.level("repeat_identity_open") >= 1.0);
    let named: Vec<_> = readings.iter().filter(|r| r.condition("RepeatIdentity").1.starts_with(REPEAT_IDENTITY)).collect();
    assert!(named.len() > 60, "the repeat stays open for as long as it is re-cut: {} readings", named.len());
    assert!(named.iter().all(|r| r.level("repeat_identity_open") >= 1.0));
}

pub fn the_held_out_lanes_sections_show_their_recorded_rounds_against_landings() {
    let t0 = std::time::Instant::now(); // clock-safe: reported, never asserted
    let mut r = open(Slice::HeldOut);
    round_controller::register_publisher(&mut r, round_controller::Publisher::only(&HELD_OUT_LANES));
    let seen = reader(&mut r, &HELD_OUT_LANES);
    r.run();
    eprintln!("replay 1021-1025: {} ms (goal: 100 ms)", t0.elapsed().as_millis()); // clock-safe

    let rounds = rounds(Slice::HeldOut);
    let (start, _) = Slice::HeldOut.window();
    let readings = seen.borrow();
    for lane in HELD_OUT_LANES {
        let lane_rounds: Vec<_> = rounds.iter().filter(|r| r.repo == lane).collect();
        let cut = lane_rounds.iter().filter(|r| r.state_from == BatchState::None && stamp(&r.at) >= start).count();
        let landed = lane_rounds.iter().filter(|r| r.state_to == BatchState::Landed && stamp(&r.at) >= start).count();
        let manifest = if lane == "repo-d" { (299, 26) } else { (88, 6) };
        assert_eq!((cut, landed), manifest, "{lane}: the manifest's rounds cut and landed in the window");
        let ours: Vec<_> = readings.iter().filter(|x| x.repo == lane).collect();
        assert!(!ours.is_empty());
        let mut most = 0.0_f64;
        for x in &ours {
            let applied = lane_rounds.iter().take_while(|r| stamp(&r.at) <= x.observed_at).count();
            let since_land = lane_rounds[..applied].iter().rposition(|r| r.state_to == BatchState::Landed).map_or(0, |i| i + 1);
            let want = lane_rounds[since_land..applied].iter().filter(|r| r.state_from == BatchState::None).count();
            assert_eq!(x.level("rounds_since_landing"), want as f64, "{lane} at {}", x.observed_at);
            most = most.max(x.level("rounds_since_landing"));
        }
        assert!(most >= 10.0, "{lane}'s section shows a run of rounds with no landing: {most}");
        assert!(ours.iter().any(|x| x.condition("LaneBlocked").0 == "True" && x.level("lanes_blocked") == 1.0), "{lane} is shown blocked");
    }
}

pub fn an_applied_ack_changes_nothing_in_the_replayed_world_and_no_record_is_dropped() {
    let (with, without) = (with_intents(), baseline());
    assert_eq!(with.fingerprints, without.fingerprints, "every world value is the run with no intents'");
    assert_eq!(with.applied, line_count(Slice::Oct31));
    assert_eq!(with.applied, with.lines);
    assert_eq!(without.applied, with.applied);

    let ack = |key: &str| with.intents.iter().find(|i| i["key"] == key).map(|i| i["intent"]["ack"].clone()).unwrap();
    let applied: Vec<_> = with.acked.iter().filter(|(_, s)| *s == Scripted::Applied).collect();
    assert!(!applied.is_empty(), "the repeat-red repo's hold-member was acked");
    for (key, _) in &applied {
        assert_eq!(ack(key)["result"], "applied", "{key}");
    }
    let refused: Vec<_> = with.acked.iter().filter(|(_, s)| matches!(s, Scripted::Refused(_))).collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(ack(&refused[0].0)["result"], json!({ "refused": { "reason": "held by the operator" } }));
    let stacked_prefix = spira_desired_state::intent::intent_key("RoundController", STACKED_REPO, "");
    let unacked = with.intents.iter().filter(|i| i["key"].as_str().unwrap().starts_with(&stacked_prefix) && i["intent"]["ack"].is_null()).count();
    assert!(unacked >= 1, "a scripted no-ack leaves its intent unacknowledged");
}

pub fn two_runs_with_one_seed_trace_the_same() {
    // Owners and passes schedule nothing and change no world value, so the run with intents is
    // a second run of the same schedule under the same seed.
    let (a, b) = (baseline(), with_intents());
    assert!(a.trace.len() > 4 * 24 * 60, "the trace holds every hook and record step: {}", a.trace.len());
    assert_eq!(a.trace, b.trace);
    assert_eq!(a.readings, b.readings);
}

pub fn hooks_run_at_their_periods_across_the_whole_day_and_the_pass_reads_what_was_published() {
    let run = baseline();
    let day_minutes = 24 * 60;
    assert_eq!(run.pass_runs, day_minutes);
    assert_eq!(run.rc_runs, Some(day_minutes));
    assert_eq!(run.keepalive_runs, Some(day_minutes / 5));
    let read = of(run, "repo-a");
    assert_eq!(read.len(), day_minutes, "the pass reads the published section every minute");
    let keepalive = round_controller::KEEPALIVE_MS / 1000;
    for r in read {
        assert!(r.observed_at <= r.at && r.at - r.observed_at < keepalive, "at {} the pass read a write from {}", r.at, r.observed_at);
    }
    let first = of(run, "repo-a")[0];
    assert_eq!(first.observed_at, first.at, "the first pass reads the same minute's first publish");
}

/// Every check, by the name its unit-tier test carries.
pub const CHECKS: [(&str, fn()); 10] = [
    ("the_publisher_sees_the_stacked_parent_from_its_first_refused_child", the_publisher_sees_the_stacked_parent_from_its_first_refused_child),
    ("the_publisher_sees_the_repeat_red_member_re_cut_at_one_tip", the_publisher_sees_the_repeat_red_member_re_cut_at_one_tip),
    ("the_held_out_lanes_sections_show_their_recorded_rounds_against_landings", the_held_out_lanes_sections_show_their_recorded_rounds_against_landings),
    ("an_applied_ack_changes_nothing_in_the_replayed_world_and_no_record_is_dropped", an_applied_ack_changes_nothing_in_the_replayed_world_and_no_record_is_dropped),
    ("two_runs_with_one_seed_trace_the_same", two_runs_with_one_seed_trace_the_same),
    ("hooks_run_at_their_periods_across_the_whole_day_and_the_pass_reads_what_was_published", hooks_run_at_their_periods_across_the_whole_day_and_the_pass_reads_what_was_published),
    ("the_stacked_repos_round_controller_section_binds_from_its_first_refused_round", the_stacked_repos_round_controller_section_binds_from_its_first_refused_round),
    ("the_inverted_map_fails_the_stacked_repo_ranking_check", the_inverted_map_fails_the_stacked_repo_ranking_check),
    ("the_held_out_lanes_rank_above_every_healthy_section", the_held_out_lanes_rank_above_every_healthy_section),
    ("the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands", the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands),
];
