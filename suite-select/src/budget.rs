//! The per-bead gate's budget cut (DESIGN.md "The gate pipeline", step 4) — pure. Rank the
//! candidates, then place each on the least-loaded of the gate's parallel lanes in rank order
//! (the order testenv runs them) and keep it while that lane's predicted wall stays within the
//! budget. Ejected suites never reach this (the caller adds them after the cut).

use crate::corpus::Suite;
use crate::header::Tier;
use crate::{refuse, Refusal};
use std::cmp::Reverse;
use std::collections::HashMap;

/// Per-tier cost caps in milliseconds (docs/test-plan/README.md): the predicted cost of a
/// suite with no timing row — an unmeasured cost is never free.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierCaps {
    pub t0_ms: f64,
    pub t1_ms: f64,
    pub t2_ms: f64,
    pub t3_ms: f64,
}

impl Default for TierCaps {
    fn default() -> Self {
        TierCaps {
            t0_ms: 1000.0,
            t1_ms: 1000.0,
            t2_ms: 10000.0,
            t3_ms: 60000.0,
        }
    }
}

impl TierCaps {
    /// `SPIRA_TIER_BUDGET_T{0,1,2,3}_MS`, each its default when unset or empty. A value that
    /// is not a non-negative number is a refusal: the tier table is unreadable.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<TierCaps, Refusal> {
        let d = TierCaps::default();
        let one = |k: &str, dflt: f64| -> Result<f64, Refusal> {
            match get(k).filter(|v| !v.is_empty()) {
                None => Ok(dflt),
                Some(v) => match number(&v) {
                    Some(n) => Ok(n),
                    None => refuse(format!("{k}={v:?} is not a number of milliseconds")),
                },
            }
        };
        Ok(TierCaps {
            t0_ms: one("SPIRA_TIER_BUDGET_T0_MS", d.t0_ms)?,
            t1_ms: one("SPIRA_TIER_BUDGET_T1_MS", d.t1_ms)?,
            t2_ms: one("SPIRA_TIER_BUDGET_T2_MS", d.t2_ms)?,
            t3_ms: one("SPIRA_TIER_BUDGET_T3_MS", d.t3_ms)?,
        })
    }

    /// The cap for `tier` in seconds. Undeclared and T4 count as T1 (never "no budget").
    pub fn secs(&self, tier: Option<Tier>) -> f64 {
        let ms = match tier {
            Some(Tier::T0) => self.t0_ms,
            Some(Tier::T2) => self.t2_ms,
            Some(Tier::T3) => self.t3_ms,
            Some(Tier::T1) | Some(Tier::T4) | None => self.t1_ms,
        };
        ms / 1000.0
    }
}

/// A non-negative decimal number (`[0-9.]+`, as the bash's `case` admitted).
pub fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Round to the thousandth, as the bash's `awk printf "%.3f"` did at every step of the fill.
fn milli(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub name: String,
    pub tier: Tier,
    pub cost: f64,
    /// Where the cost came from: `p90` or `cap`.
    pub source: &'static str,
    key: (u8, Reverse<u32>, usize, u8),
}

/// Rank key: tier bucket (T0/T1/untiered 0, else 1), recent reds (most first: the suite most
/// likely to catch runs first), specificity (the count of `# covers:` tokens; an always-run
/// suite is least specific), untagged after tagged (a `UC-` token), then the name.
fn key(s: &Suite, reds: u32) -> (u8, Reverse<u32>, usize, u8) {
    let bucket = match s.tier {
        None | Some(Tier::T0) | Some(Tier::T1) => 0,
        _ => 1,
    };
    let (spec, taginv) = match &s.covers {
        Some(c) => (c.len(), u8::from(!c.iter().any(|t| t.starts_with("UC-")))),
        None => (999_999, 1),
    };
    (bucket, Reverse(reds), spec, taginv)
}

pub fn rank(cands: &[&Suite], p90: &HashMap<String, f64>, reds: &HashMap<String, u32>, caps: &TierCaps) -> Vec<Ranked> {
    let mut v: Vec<Ranked> = cands
        .iter()
        .map(|s| {
            let (cost, source) = match p90.get(&s.name) {
                Some(c) => (*c, "p90"),
                None => (milli(caps.secs(s.tier)), "cap"),
            };
            Ranked {
                name: s.name.clone(),
                tier: s.tier_or_t1(),
                cost,
                source,
                key: key(s, reds.get(&s.name).copied().unwrap_or(0)),
            }
        })
        .collect();
    v.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.name.cmp(&b.name)));
    v.dedup_by(|a, b| a.name == b.name);
    v
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    /// In rank order.
    pub selected: Vec<String>,
    pub dropped: Vec<String>,
    pub log: Vec<String>,
}

/// FILL: walk the ranked candidates in run order, each onto the least-loaded of `width` lanes;
/// keep it while `lane + cost <= budget`, else drop it and go on (a cheaper one may fit). A
/// suite whose own cost exceeds the budget is therefore always dropped, never started.
pub fn fill(ranked: &[Ranked], budget_secs: f64, width: u64) -> Cut {
    let width = width.max(1);
    let mut lanes = vec![0.0_f64; width as usize];
    let mut cut = Cut {
        selected: vec![],
        dropped: vec![],
        log: vec![],
    };
    if ranked.is_empty() {
        cut.log.push(format!(
            "suite-select budget: selected 0, dropped 0, predicted 0s (budget {budget_secs}s, width {width}) — nothing to rank"
        ));
        return cut;
    }
    for r in ranked {
        let (at, load) = lanes
            .iter()
            .copied()
            .enumerate()
            .fold((0, f64::INFINITY), |m, (i, l)| if l < m.1 { (i, l) } else { m });
        let would = milli(load + r.cost);
        if would <= budget_secs {
            cut.selected.push(r.name.clone());
            lanes[at] = would;
        } else {
            cut.dropped.push(r.name.clone());
            cut.log.push(format!(
                "suite-select budget: dropped {} (tier={} predicted={:.3}s from {} — over budget)",
                r.name,
                r.tier.as_str(),
                r.cost,
                r.source
            ));
        }
    }
    cut.log.push(format!(
        "suite-select budget: selected {}, dropped {}, predicted {:.0}s (budget {budget_secs}s, width {width})",
        cut.selected.len(),
        cut.dropped.len(),
        lanes.iter().copied().fold(0.0, f64::max)
    ));
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Suite;

    fn s(name: &str, tier: Option<Tier>, covers: Option<&[&str]>) -> Suite {
        Suite {
            name: name.into(),
            covers: covers.map(|c| c.iter().map(|x| x.to_string()).collect()),
            tier,
            selects_on: vec![],
            words: vec![],
            invokes: vec![],
            tokens: vec![],
        }
    }

    #[test_id(tk57ad6)]
    #[test]
    fn rank_is_bucket_then_specificity_then_tag_then_name() {
        let a = s("test-a.sh", Some(Tier::T2), Some(&["x"]));
        let b = s("test-b.sh", None, None);
        let c = s("test-c.sh", Some(Tier::T1), Some(&["x", "y"]));
        let d = s("test-d.sh", Some(Tier::T0), Some(&["x", "UC-a-01"]));
        let e = s("test-e.sh", Some(Tier::T1), Some(&["x"]));
        let r = rank(&[&a, &b, &c, &d, &e], &HashMap::new(), &HashMap::new(), &TierCaps::default());
        let names: Vec<&str> = r.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["test-e.sh", "test-d.sh", "test-c.sh", "test-b.sh", "test-a.sh"]);
    }

    #[test_id(tsj34ub)]
    #[test]
    fn unmeasured_suites_cost_their_tier_cap_and_measured_their_p90() {
        let a = s("test-a.sh", Some(Tier::T3), Some(&["x"]));
        let b = s("test-b.sh", Some(Tier::T4), Some(&["x"]));
        let c = s("test-c.sh", Some(Tier::T2), Some(&["x"]));
        let p: HashMap<String, f64> = [("test-c.sh".to_string(), 2.5)].into();
        let r = rank(&[&a, &b, &c], &p, &HashMap::new(), &TierCaps::default());
        let cost: HashMap<&str, f64> = r.iter().map(|x| (x.name.as_str(), x.cost)).collect();
        assert_eq!(cost["test-a.sh"], 60.0);
        assert_eq!(cost["test-b.sh"], 1.0, "T4 counts as T1");
        assert_eq!(cost["test-c.sh"], 2.5);
    }

    #[test_id(thkz6on)]
    #[test]
    fn fill_keeps_walking_after_a_drop_and_divides_by_width() {
        let r: Vec<Ranked> = [("test-a.sh", 100.0), ("test-b.sh", 250.0), ("test-c.sh", 90.0)]
            .iter()
            .map(|(n, c)| Ranked {
                name: n.to_string(),
                tier: Tier::T1,
                cost: *c,
                source: "p90",
                key: (0, Reverse(0), 1, 1),
            })
            .collect();
        let cut = fill(&r, 200.0, 1);
        assert_eq!(cut.selected, ["test-a.sh", "test-c.sh"]);
        assert_eq!(cut.dropped, ["test-b.sh"]);
        assert!(cut.log[0].contains("dropped test-b.sh"));
        assert!(cut.log.last().unwrap().contains("selected 2, dropped 1, predicted 190s"));
        // Two lanes: the 250 s suite fits alone on one; the 90 s one lands on the other.
        let wide = fill(&r, 250.0, 2);
        assert_eq!(wide.selected.len(), 3);
        assert!(wide.log.last().unwrap().contains("predicted 250s"), "{:?}", wide.log);
        let narrow = fill(&r, 220.0, 2);
        assert_eq!(narrow.selected, ["test-a.sh", "test-c.sh"]);
        assert_eq!(narrow.dropped, ["test-b.sh"]);
        // Exactly at the budget fits.
        assert_eq!(fill(&r[..1], 100.0, 1).selected, ["test-a.sh"]);
        assert!(fill(&[], 300.0, 4).log[0].contains("nothing to rank"));
    }

    fn ranked(name: &str, cost: f64) -> Ranked {
        Ranked { name: name.into(), tier: Tier::T1, cost, source: "p90", key: (0, Reverse(0), 1, 1) }
    }

    #[test_id(tfbfp5w)]
    #[test]
    fn a_suite_longer_than_the_suites_budget_is_dropped_however_wide_the_gate_is() {
        let r = [ranked("test-long.sh", 92.0), ranked("test-short.sh", 10.0)];
        let cut = fill(&r, 70.0, 4);
        assert_eq!(cut.dropped, ["test-long.sh"], "{:?}", cut.log);
        assert_eq!(cut.selected, ["test-short.sh"], "a cheaper suite after a drop is still kept");
    }

    #[test_id(tmretj6)]
    #[test]
    fn lanes_fill_in_rank_order_so_the_last_suite_is_judged_on_the_lane_it_would_get() {
        // Two lanes of 60 s: 50 and 50 take one each, the 30 s one then fits neither.
        let r = [ranked("test-a.sh", 50.0), ranked("test-b.sh", 50.0), ranked("test-c.sh", 30.0)];
        let cut = fill(&r, 60.0, 2);
        assert_eq!(cut.selected, ["test-a.sh", "test-b.sh"]);
        assert_eq!(cut.dropped, ["test-c.sh"]);
    }

    #[test_id(trwml9f)]
    #[test]
    fn recent_reds_rank_a_suite_ahead_of_an_equally_tiered_more_specific_one() {
        let specific = s("test-specific.sh", Some(Tier::T1), Some(&["x"]));
        let red = s("test-red.sh", Some(Tier::T1), Some(&["x", "y", "z"]));
        let reds: HashMap<String, u32> = [("test-red.sh".to_string(), 2)].into();
        let names = |r: Vec<Ranked>| r.into_iter().map(|x| x.name).collect::<Vec<_>>();
        assert_eq!(names(rank(&[&specific, &red], &HashMap::new(), &reds, &TierCaps::default())), ["test-red.sh", "test-specific.sh"]);
        assert_eq!(
            names(rank(&[&specific, &red], &HashMap::new(), &HashMap::new(), &TierCaps::default())),
            ["test-specific.sh", "test-red.sh"],
            "control: with no reds, specificity decides"
        );
        let slow = s("test-slow.sh", Some(Tier::T3), Some(&["x", "y", "z"]));
        let reds: HashMap<String, u32> = [("test-slow.sh".to_string(), 9)].into();
        assert_eq!(names(rank(&[&specific, &slow], &HashMap::new(), &reds, &TierCaps::default()))[0], "test-specific.sh", "the tier bucket still outranks reds");
    }
}
