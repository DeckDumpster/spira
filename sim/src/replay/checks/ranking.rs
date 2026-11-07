//! The ranking checks: the goodput pass's `assemble` then `rank` over a replayed day, once a
//! minute, on the replay's virtual clock.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use reconciler_engine::assemble::{assemble, Snapshot};
use reconciler_engine::causal_map::CausalMap;
use reconciler_engine::pass_record::PassRecord;
use reconciler_engine::rank::{rank, Ranking};
use serde_json::json;

use super::{open, round_controller, rounds, stamp, Replay, Slice, HELD_OUT_LANES, STACKED_REPO};
use crate::replay::lifecycle;

/// What one pass ranked, under each map it was given.
struct Pass {
    at: u64,
    rankings: Vec<Ranking>,
}

/// A pass that ranks the store's snapshot under every map in `maps`, each carrying its own
/// previous pass record so its hysteresis follows its own history.
fn ranker(replay: &mut Replay, maps: Vec<CausalMap>) -> Rc<RefCell<Vec<Pass>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let out = seen.clone();
    let mut prev: Vec<Option<PassRecord>> = maps.iter().map(|_| None).collect();
    replay.register_pass(move |store, now| {
        let snap = Snapshot::parse(&store.ok(&["section", "snapshot"], None)).unwrap();
        let mut rankings = Vec::new();
        for (map, prev) in maps.iter().zip(prev.iter_mut()) {
            let mut a = assemble(&snap, now);
            a.prev = prev.clone();
            let r = rank(&a, map);
            *prev = Some(
                serde_json::from_value(json!({
                    "n": a.n, "list_rev": a.list_rev, "at": "2026-01-01T00:00:00Z", "holder": "replay", "mode": "shadow",
                    "gaps": r.gaps.iter().filter(|g| g.open).map(|g| json!({ "section": g.section, "distance": g.distance, "score": g.score })).collect::<Vec<_>>(),
                }))
                .unwrap(),
            );
            rankings.push(r);
        }
        out.borrow_mut().push(Pass { at: now / 1000, rankings });
    });
    seen
}

const RIGHT: usize = 0;
const WRONG: usize = 1;

/// Every weight inverted, and the outcome given the largest: the map the ranking test must fail
/// under.
fn inverted(seed: &CausalMap) -> CausalMap {
    let weigh = |kind: &str, w: f64| format!("[[weight]]\nkind = \"{kind}\"\nweight = {w}\nunit = \"u\"\nsource = \"inverted\"\n");
    let text: String = ["RoundController", "Alarms"].iter().map(|k| weigh(k, 1.0 / seed.weight(k).unwrap())).chain([weigh("Lifecycle", 1000.0)]).collect();
    CausalMap::load(&text, &["RoundController", "Alarms", "Lifecycle"]).unwrap()
}

/// 10-31, ranked under the seed and under the inverted map, from one run.
fn oct31() -> &'static [Pass] {
    static PASSES: OnceLock<Vec<Pass>> = OnceLock::new();
    PASSES.get_or_init(|| {
        let seed = CausalMap::seed().unwrap();
        let mut r = open(Slice::Oct31);
        round_controller::register(&mut r);
        lifecycle::register(&mut r);
        let seen = ranker(&mut r, vec![seed.clone(), inverted(&seed)]);
        r.run();
        let passes = std::mem::take(&mut *seen.borrow_mut());
        passes
    })
}

struct Stack {
    from: u64,
    until: u64,
}

/// From the stacked repo's first refused round until the parent it is stacked on ends.
fn stack() -> Stack {
    let rounds = rounds(Slice::Oct31);
    let (start, _) = Slice::Oct31.window();
    let first = rounds
        .iter()
        .find(|r| r.repo == STACKED_REPO && stamp(&r.at) >= start && r.verdict.as_deref() == Some("refused") && r.parent.is_some())
        .expect("a refused round stacked on a parent");
    let parent = first.parent.as_deref().unwrap();
    let ended = rounds.iter().find(|r| r.batch == parent && r.state_to.terminal()).expect("the parent ends in the slice");
    Stack { from: stamp(&first.at), until: stamp(&ended.at) }
}

/// Whether `STACKED_REPO`'s RoundController section is the binding condition under map `which`
/// at every pass of the stack once its own gap has opened, with a path that ends at goodput.
fn stacked_binds(passes: &[Pass], which: usize) -> Result<usize, String> {
    let s = stack();
    let binding = format!("RoundController/{STACKED_REPO}");
    let mut held = 0;
    for p in passes.iter().filter(|p| p.at >= s.from && p.at < s.until) {
        let r = &p.rankings[which];
        let top = r.gaps.iter().find(|g| Some(&g.section) == r.binding.as_ref());
        match top {
            Some(g) if g.section == binding => {
                if g.path_to_goodput.last().map(String::as_str) != Some(reconciler_engine::flow::GOODPUT) {
                    return Err(format!("at {}: its path {:?} does not end at goodput", p.at, g.path_to_goodput));
                }
                held += 1;
            }
            other if held > 0 => return Err(format!("at {} the binding is {:?}, not {binding}", p.at, other.map(|g| &g.section))),
            _ => {}
        }
    }
    if held == 0 { Err(format!("{binding} never binds")) } else { Ok(held) }
}

pub fn the_stacked_repos_round_controller_section_binds_from_its_first_refused_round() {
    let passes = oct31();
    let s = stack();
    let held = stacked_binds(passes, RIGHT).unwrap();
    let first = passes.iter().find(|p| p.at >= s.from && p.rankings[RIGHT].binding.as_deref() == Some(&format!("RoundController/{STACKED_REPO}"))).unwrap();
    assert!(first.at - s.from <= 600, "its gap opens within ten minutes of the first refusal: {} s", first.at - s.from);
    assert!(held as u64 >= (s.until - first.at) / 60 - 1, "it binds at every pass until the parent ends: {held}");
    assert!(held >= 180, "the stack stands for hours: {held} passes");
    let bound = &passes.iter().find(|p| p.at == first.at).unwrap().rankings[RIGHT];
    let g = &bound.gaps[0];
    assert!(g.distance > 1.0 && g.score == g.weight * g.distance && g.weight == 10.0, "{g:?}");
    assert_eq!(g.path_to_goodput, [format!("RoundController/{STACKED_REPO}"), "lifecycle/landed".to_string()]);
    let after = passes.iter().find(|p| p.at > s.until + 900).unwrap();
    assert_ne!(after.rankings[RIGHT].binding.as_deref(), Some(format!("RoundController/{STACKED_REPO}").as_str()), "a terminal parent ends the binding");
}

pub fn the_inverted_map_fails_the_stacked_repo_ranking_check() {
    let passes = oct31();
    assert!(stacked_binds(passes, RIGHT).is_ok(), "the check passes under the seed it is meant to guard");
    assert!(stacked_binds(passes, WRONG).is_err());
    let s = stack();
    let outcome = passes.iter().filter(|p| p.at >= s.from + 3600 && p.at < s.until).all(|p| p.rankings[WRONG].binding.as_deref() == Some("Lifecycle/all"));
    assert!(outcome, "the outcome outranks every cause under the inverted map");
}

pub fn the_held_out_lanes_rank_above_every_healthy_section() {
    let mut r = open(Slice::HeldOut);
    let lanes: Vec<&str> = HELD_OUT_LANES.iter().copied().chain(["repo-g", "repo-h"]).collect();
    round_controller::register_publisher(&mut r, round_controller::Publisher::only(&lanes));
    lifecycle::register(&mut r);
    let seen = ranker(&mut r, vec![CausalMap::seed().unwrap()]);
    r.run();
    let passes = seen.borrow();
    for lane in HELD_OUT_LANES {
        let section = format!("RoundController/{lane}");
        let mut blocked = 0;
        for p in passes.iter() {
            let gaps = &p.rankings[0].gaps;
            let Some(at) = gaps.iter().position(|g| g.section == section) else { continue };
            if gaps[at].distance < 1.0 {
                continue;
            }
            blocked += 1;
            let healthy = gaps.iter().position(|g| g.distance <= 0.5 && g.section != section);
            assert!(healthy.is_none_or(|h| at < h), "{lane} at {}: ranked {at}, a healthy section {healthy:?}", p.at);
            assert!(gaps[at].weight > 0.0 && gaps[at].score > gaps.iter().filter(|g| g.distance <= 0.5).map(|g| g.score).fold(0.0, f64::max));
        }
        assert!(blocked > 100, "{lane} stands blocked for hours: {blocked} passes");
    }
}
