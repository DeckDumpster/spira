//! The ranking: every assembled section by `weight × distance`, with hysteresis against the
//! previous pass record, and the top open gap named as the binding condition. Pure. A section
//! the pass cannot see ranks as its own gap and is never scored as healthy.

use std::collections::BTreeMap;

use spira_desired_state::section::{self, Freshness, Section};
use spira_desired_state::sections;

use crate::assemble::{Assembled, Current};
use crate::causal_map::CausalMap;
use crate::flow::Flow;

/// The distance of a gap whose section cannot say how far it is and declares no `open`.
pub const UNSEEN_DISTANCE: f64 = 1.0;

#[derive(Clone, Debug, PartialEq)]
pub struct RankedGap {
    pub section: String,
    pub distance: f64,
    pub weight: f64,
    pub score: f64,
    pub open: bool,
    pub path_to_goodput: Vec<String>,
    pub freshness: Freshness,
}

impl RankedGap {
    /// `cannot see <section>: <reason>` when the pass cannot judge this section.
    pub fn cannot_see(&self) -> Option<String> {
        match &self.freshness {
            Freshness::Fresh => None,
            Freshness::Stale { reason, .. } => Some(format!("cannot see {}: {reason}", self.section)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranking {
    /// Highest score first, ties by section name.
    pub gaps: Vec<RankedGap>,
    pub binding: Option<String>,
}

fn kind_of(section: &str) -> &str {
    section.split_once('/').map_or(section, |(k, _)| k)
}

fn as_section(r: &str, cur: &Current) -> Section {
    Section {
        api_version: section::API_VERSION.into(),
        kind: kind_of(r).into(),
        metadata: cur.metadata.clone(),
        spec: Default::default(),
        status: cur.status.clone(),
        flow: cur.flow.clone(),
        distance: cur.distance.clone(),
        actions: cur.actions.clone(),
    }
}

/// The section's distance over its current status; `None` when it declares none or the
/// function cannot be evaluated over what the section published.
fn distance_of(r: &str, cur: &Current) -> Option<f64> {
    let kind = sections::lookup(kind_of(r))?;
    as_section(r, cur).distance_value(kind)?.ok()
}

pub fn rank(a: &Assembled, map: &CausalMap) -> Ranking {
    let flow = Flow::of(a);
    let was_open = |r: &str| a.prev.as_ref().is_some_and(|p| p.gaps.iter().any(|g| g.section == r));
    let gap = |section: &str, distance: f64, open: bool, freshness: Freshness| {
        let weight = map.weight(kind_of(section)).unwrap_or(0.0);
        RankedGap { section: section.into(), distance, weight, score: weight * distance, open, path_to_goodput: flow.path_to_goodput(section), freshness }
    };
    let mut gaps = Vec::new();
    for (r, cur) in &a.current.sections {
        let declared = cur.distance.as_ref();
        if cur.freshness != Freshness::Fresh {
            gaps.push(gap(r, declared.map_or(UNSEEN_DISTANCE, |d| d.open), true, cur.freshness.clone()));
            continue;
        }
        let Some(d) = declared else {
            gaps.push(gap(r, 0.0, false, Freshness::Fresh));
            continue;
        };
        gaps.push(match distance_of(r, cur) {
            None => gap(r, d.open, true, Freshness::Stale { age_secs: None, reason: "its distance cannot be evaluated over its status".into() }),
            Some(v) => gap(r, v, v > d.open || (v >= d.close && was_open(r)), Freshness::Fresh),
        });
    }
    let named = a.prev.iter().flat_map(|p| p.gaps.iter().map(|g| (g.section.clone(), g.distance))).chain(a.unattributed.keys().map(|k| (k.clone(), UNSEEN_DISTANCE)));
    let mut absent = BTreeMap::new();
    for (r, distance) in named.filter(|(r, _)| !a.current.sections.contains_key(r)) {
        absent.entry(r).or_insert(if distance > 0.0 { distance } else { UNSEEN_DISTANCE });
    }
    for (r, distance) in absent {
        gaps.push(gap(&r, distance, true, Freshness::Stale { age_secs: None, reason: "no section of that name is listed".into() }));
    }
    gaps.sort_by(|x, y| y.score.total_cmp(&x.score).then_with(|| x.section.cmp(&y.section)));
    let binding = gaps.iter().find(|g| g.open).map(|g| g.section.clone());
    Ranking { gaps, binding }
}

#[cfg(test)]
mod tests;
