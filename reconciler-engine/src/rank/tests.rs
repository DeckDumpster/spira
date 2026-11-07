use serde_json::{json, Value};

use super::*;
use crate::assemble::{assemble, Snapshot};

const NOW: &str = "2026-10-31T19:40:00Z";
/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;

fn map(weights: &[(&str, f64)]) -> CausalMap {
    let text: String = weights.iter().map(|(k, w)| format!("[[weight]]\nkind = \"{k}\"\nweight = {w}\nunit = \"u\"\nsource = \"s\"\n")).collect();
    CausalMap::load(&text, &[]).unwrap()
}

/// A `Probe` section whose distance is its `depth`, open above 1.0 and closed below 0.5.
fn probe(name: &str, depth: f64, observed_at: &str, produces: &[&str]) -> Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "Probe",
        "metadata": { "name": name, "owner": "probe", "observedAt": observed_at, "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0, "depth": { "value": depth, "window": "level" } },
        "flow": { "consumes": [], "produces": produces },
        "distance": { "fn": "depth", "open": 1.0, "close": 0.5 },
    }})
}

fn lifecycle_term(name: &str) -> Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "Lifecycle",
        "metadata": { "name": name, "owner": "spira-lc", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0 },
    }})
}

fn prev(open: &[(&str, f64)]) -> Value {
    json!({
        "n": 7, "list_rev": 3, "at": NOW, "holder": "h", "mode": "acting",
        "gaps": open.iter().map(|(s, d)| json!({ "section": s, "distance": d, "score": d })).collect::<Vec<_>>(),
    })
}

fn assembled(sections: Vec<Value>, pass: Value) -> Assembled {
    let snap = Snapshot::parse(&json!({ "list_rev": 4, "sections": sections, "intents": [], "pass": pass, "knobs": {}, "undecodable": [] })).unwrap();
    assemble(&snap, T0)
}

fn names(r: &Ranking) -> Vec<&str> {
    r.gaps.iter().map(|g| g.section.as_str()).collect()
}

fn gap<'a>(r: &'a Ranking, section: &str) -> &'a RankedGap {
    r.gaps.iter().find(|g| g.section == section).unwrap_or_else(|| panic!("{section} is not ranked: {:?}", names(r)))
}

#[test]
fn sections_rank_by_weight_times_distance_and_the_top_open_gap_binds() {
    let a = assembled(vec![probe("near", 3.0, NOW, &[]), probe("far", 9.0, NOW, &[]), probe("healthy", 0.1, NOW, &[])], Value::Null);
    let r = rank(&a, &map(&[("Probe", 2.0)]));
    assert_eq!(names(&r), ["Probe/far", "Probe/near", "Probe/healthy"]);
    assert_eq!((gap(&r, "Probe/far").weight, gap(&r, "Probe/far").score), (2.0, 18.0));
    assert_eq!(r.binding.as_deref(), Some("Probe/far"));
    assert!(!gap(&r, "Probe/healthy").open);
}

#[test]
fn a_healthy_section_never_binds_however_it_scores() {
    let a = assembled(vec![probe("healthy", 0.9, NOW, &[]), lifecycle_term("all")], Value::Null);
    let r = rank(&a, &map(&[("Probe", 1.0), ("Lifecycle", 0.0)]));
    assert_eq!(r.binding, None);
    assert_eq!(r.gaps.len(), 2, "terms rank too");
}

#[test]
fn equal_scores_are_ordered_by_section_name_and_the_first_binds() {
    let a = assembled(vec![probe("b", 4.0, NOW, &[]), probe("a", 4.0, NOW, &[])], Value::Null);
    let r = rank(&a, &map(&[("Probe", 1.0)]));
    assert_eq!(names(&r), ["Probe/a", "Probe/b"]);
    assert_eq!(r.binding.as_deref(), Some("Probe/a"));
}

#[test]
fn a_distance_between_close_and_open_keeps_the_gap_as_the_last_pass_left_it() {
    let m = map(&[("Probe", 1.0)]);
    let between = || vec![probe("a", 0.7, NOW, &[])];
    let was_closed = rank(&assembled(between(), prev(&[])), &m);
    assert!(!gap(&was_closed, "Probe/a").open, "it does not open between close and open");
    let was_open = rank(&assembled(between(), prev(&[("Probe/a", 1.4)])), &m);
    assert!(gap(&was_open, "Probe/a").open, "it does not close between close and open");
    assert_eq!(was_open.binding.as_deref(), Some("Probe/a"));
}

#[test]
fn a_gap_opens_above_open_and_closes_below_close_whatever_the_last_pass_said() {
    let m = map(&[("Probe", 1.0)]);
    assert!(gap(&rank(&assembled(vec![probe("a", 1.01, NOW, &[])], prev(&[])), &m), "Probe/a").open);
    assert!(!gap(&rank(&assembled(vec![probe("a", 0.49, NOW, &[])], prev(&[("Probe/a", 2.0)])), &m), "Probe/a").open);
}

#[test]
fn a_stale_section_ranks_as_cannot_see_with_its_reason_and_is_never_scored_zero() {
    let a = assembled(vec![probe("lost", 0.0, "2026-10-31T18:00:00Z", &[]), probe("seen", 1.5, NOW, &[]), probe("healthy", 0.2, NOW, &[])], Value::Null);
    let r = rank(&a, &map(&[("Probe", 3.0)]));
    let lost = gap(&r, "Probe/lost");
    assert!(lost.open);
    assert_eq!((lost.distance, lost.score), (1.0, 3.0), "weight × open");
    let seen = lost.cannot_see().unwrap();
    assert!(seen.starts_with("cannot see Probe/lost: ") && seen.contains("max_age"), "{seen}");
    assert_eq!(gap(&r, "Probe/seen").cannot_see(), None);
    assert!(r.gaps.iter().position(|g| g.section == "Probe/lost") < r.gaps.iter().position(|g| g.section == "Probe/healthy"));
}

#[test]
fn a_section_whose_distance_cannot_be_evaluated_is_a_gap_at_its_open() {
    let mut s = probe("odd", 0.0, NOW, &[]);
    s["section"]["distance"]["fn"] = json!("missing_field");
    let r = rank(&assembled(vec![s], Value::Null), &map(&[("Probe", 2.0)]));
    let g = gap(&r, "Probe/odd");
    assert!(g.open && g.score == 2.0 && g.cannot_see().is_some(), "{g:?}");
}

#[test]
fn a_section_the_last_pass_had_open_and_the_snapshot_no_longer_lists_is_a_gap() {
    let a = assembled(vec![probe("here", 0.0, NOW, &[])], prev(&[("Probe/gone", 4.0)]));
    let r = rank(&a, &map(&[("Probe", 2.0)]));
    let g = gap(&r, "Probe/gone");
    assert!(g.open);
    assert_eq!(g.score, 8.0);
    assert!(g.cannot_see().unwrap().starts_with("cannot see Probe/gone"));
}

#[test]
fn a_kind_the_map_does_not_weigh_is_ranked_at_weight_zero_not_hidden() {
    let r = rank(&assembled(vec![probe("a", 5.0, NOW, &[])], Value::Null), &map(&[]));
    assert_eq!((gap(&r, "Probe/a").weight, gap(&r, "Probe/a").score), (0.0, 0.0));
    assert!(gap(&r, "Probe/a").open);
}

#[test]
fn every_gap_carries_the_path_its_section_takes_to_goodput() {
    let a = assembled(vec![probe("cause", 5.0, NOW, &["lifecycle/landed"]), probe("aside", 5.0, NOW, &[])], Value::Null);
    let r = rank(&a, &map(&[("Probe", 1.0)]));
    assert_eq!(gap(&r, "Probe/cause").path_to_goodput, ["Probe/cause", "lifecycle/landed"]);
    assert!(gap(&r, "Probe/aside").path_to_goodput.is_empty());
}

#[test]
fn inverting_the_weights_changes_what_binds() {
    let a = assembled(vec![probe("cause", 4.0, NOW, &[]), lifecycle_term("all")], Value::Null);
    let mut term = a.clone();
    term.current.sections.get_mut("Lifecycle/all").unwrap().distance = Some(spira_desired_state::section::Distance { expr: "0.5".into(), open: 0.05, close: 0.02 });
    let right = rank(&term, &map(&[("Probe", 10.0), ("Lifecycle", 0.0)]));
    let wrong = rank(&term, &map(&[("Probe", 0.1), ("Lifecycle", 100.0)]));
    assert_eq!(right.binding.as_deref(), Some("Probe/cause"));
    assert_eq!(wrong.binding.as_deref(), Some("Lifecycle/all"));
}
