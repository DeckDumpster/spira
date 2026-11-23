//! The one rule for "waiting on a live dependency", shared by `watch.transitions` and
//! `watch.throughput`: a bead the batcher refused for an unlanded prerequisite is waiting, not
//! stalled, only while that prerequisite is itself being worked. A prerequisite sitting idle, or
//! one whose state is unknown, is not a reason to stay quiet.

use std::collections::BTreeMap;

use runtime::event::Projection;

use crate::tasks::a::{batcher_forge, batcher_local, kit};

pub const LIVE: [&str; 4] = ["WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY"];

/// Every pool member either batcher refused, mapped to its first unlanded prerequisite.
pub fn batcher_blocks(view: &Projection) -> BTreeMap<String, String> {
    let mut blocked: BTreeMap<String, String> =
        kit::carry::<batcher_local::Carry>(&batcher_local::SHAPE, view)
            .repos
            .into_values()
            .flat_map(|rc| rc.blocked)
            .collect();
    blocked.extend(
        kit::carry::<batcher_forge::Carry>(&batcher_forge::SHAPE, view)
            .repos
            .into_values()
            .flat_map(|rc| rc.blocked),
    );
    blocked
}

/// `(dep, state)` when `bead` is blocked by a prerequisite whose state is live.
pub fn live_blocker(
    bead: &str,
    blocked: &BTreeMap<String, String>,
    state_of: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    let dep = blocked.get(bead)?;
    let state = state_of(dep).filter(|s| LIVE.contains(&s.as_str()))?;
    Some((dep.clone(), state))
}

/// `waiting on a dependency: <bead> waiting on <dep> (<state>), …` over (bead, dep, dep state) triples.
pub fn clause(waits: &[(String, String, String)]) -> Option<String> {
    let parts: Vec<String> = waits
        .iter()
        .map(|(b, d, s)| format!("{b} waiting on {d} ({s})"))
        .collect();
    (!parts.is_empty()).then(|| format!("waiting on a dependency: {}", parts.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked() -> BTreeMap<String, String> {
        BTreeMap::from([("b".to_string(), "d".to_string())])
    }

    fn live_with(state: &'static str) -> Option<(String, String)> {
        live_blocker("b", &blocked(), &|id| {
            (id == "d").then(|| state.to_string())
        })
    }

    #[test_id(tt2uvds)]
    #[test]
    fn a_prerequisite_in_each_live_state_is_a_live_blocker() {
        for s in LIVE {
            assert_eq!(live_with(s), Some(("d".to_string(), s.to_string())), "{s}");
        }
    }

    #[test_id(tgkoh7i)]
    #[test]
    fn an_idle_or_unknown_prerequisite_is_not_a_live_blocker() {
        assert_eq!(live_with("READY"), None);
        assert_eq!(live_with("REWORK"), None);
        assert_eq!(live_blocker("b", &blocked(), &|_| None), None);
    }

    #[test_id(t1ucom7)]
    #[test]
    fn a_bead_the_batcher_did_not_refuse_has_no_blocker() {
        assert_eq!(
            live_blocker("other", &blocked(), &|_| Some("WORKING".into())),
            None
        );
    }

    #[test_id(tyx62gj)]
    #[test]
    fn the_clause_names_each_wait_with_the_blockers_state_and_is_none_when_empty() {
        let w = vec![
            ("a".to_string(), "x".to_string(), "WORKING".to_string()),
            ("b".to_string(), "y".to_string(), "CERTIFIED".to_string()),
        ];
        assert_eq!(
            clause(&w).as_deref(),
            Some("waiting on a dependency: a waiting on x (WORKING), b waiting on y (CERTIFIED)")
        );
        assert_eq!(clause(&[]), None);
    }
}
