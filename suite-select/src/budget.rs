//! The per-bead gate's budget cut (DESIGN.md "The gate pipeline", step 4) — pure. What
//! `spira/gate-budget-select.sh` did: rank the candidates most specific first, then add each
//! while the running predicted wall, divided by the gate's parallel width, stays within the
//! budget. Ejected suites never reach this (the caller adds them after the cut).

use crate::corpus::Suite;
use crate::header::Tier;
use crate::{refuse, Refusal};
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
    key: (u8, usize, u8),
}

/// Rank key: tier bucket (T0/T1/untiered 0, else 1), specificity (the count of `# covers:`
/// tokens; an always-run suite is least specific), untagged after tagged (a `UC-` token),
/// then the name.
fn key(s: &Suite) -> (u8, usize, u8) {
    let bucket = match s.tier {
        None | Some(Tier::T0) | Some(Tier::T1) => 0,
        _ => 1,
    };
    let (spec, taginv) = match &s.covers {
        Some(c) => (c.len(), u8::from(!c.iter().any(|t| t.starts_with("UC-")))),
        None => (999_999, 1),
    };
    (bucket, spec, taginv)
}

pub fn rank(cands: &[&Suite], p90: &HashMap<String, f64>, caps: &TierCaps) -> Vec<Ranked> {
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
                key: key(s),
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

/// FILL: walk the ranked candidates, adding each while `(total + cost) / width <= budget`.
/// A candidate that does not fit is dropped and the walk goes on (a cheaper one may fit).
pub fn fill(ranked: &[Ranked], budget_secs: f64, width: u64) -> Cut {
    let width = width.max(1) as f64;
    let mut total = 0.0;
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
        let would = milli(total + r.cost);
        if would / width <= budget_secs {
            cut.selected.push(r.name.clone());
            total = would;
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
        total / width
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
        }
    }

    #[test]
    fn rank_is_bucket_then_specificity_then_tag_then_name() {
        let a = s("test-a.sh", Some(Tier::T2), Some(&["x"]));
        let b = s("test-b.sh", None, None);
        let c = s("test-c.sh", Some(Tier::T1), Some(&["x", "y"]));
        let d = s("test-d.sh", Some(Tier::T0), Some(&["x", "UC-a-01"]));
        let e = s("test-e.sh", Some(Tier::T1), Some(&["x"]));
        let r = rank(&[&a, &b, &c, &d, &e], &HashMap::new(), &TierCaps::default());
        let names: Vec<&str> = r.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["test-e.sh", "test-d.sh", "test-c.sh", "test-b.sh", "test-a.sh"]);
    }

    #[test]
    fn unmeasured_suites_cost_their_tier_cap_and_measured_their_p90() {
        let a = s("test-a.sh", Some(Tier::T3), Some(&["x"]));
        let b = s("test-b.sh", Some(Tier::T4), Some(&["x"]));
        let c = s("test-c.sh", Some(Tier::T2), Some(&["x"]));
        let p: HashMap<String, f64> = [("test-c.sh".to_string(), 2.5)].into();
        let r = rank(&[&a, &b, &c], &p, &TierCaps::default());
        let cost: HashMap<&str, f64> = r.iter().map(|x| (x.name.as_str(), x.cost)).collect();
        assert_eq!(cost["test-a.sh"], 60.0);
        assert_eq!(cost["test-b.sh"], 1.0, "T4 counts as T1");
        assert_eq!(cost["test-c.sh"], 2.5);
    }

    #[test]
    fn fill_keeps_walking_after_a_drop_and_divides_by_width() {
        let r: Vec<Ranked> = [("test-a.sh", 100.0), ("test-b.sh", 250.0), ("test-c.sh", 90.0)]
            .iter()
            .map(|(n, c)| Ranked {
                name: n.to_string(),
                tier: Tier::T1,
                cost: *c,
                source: "p90",
                key: (0, 1, 1),
            })
            .collect();
        let cut = fill(&r, 200.0, 1);
        assert_eq!(cut.selected, ["test-a.sh", "test-c.sh"]);
        assert_eq!(cut.dropped, ["test-b.sh"]);
        assert!(cut.log[0].contains("dropped test-b.sh"));
        assert!(cut.log.last().unwrap().contains("selected 2, dropped 1, predicted 190s"));
        let wide = fill(&r, 220.0, 2);
        assert_eq!(wide.selected.len(), 3);
        // Exactly at the budget fits.
        assert_eq!(fill(&r[..1], 100.0, 1).selected, ["test-a.sh"]);
        assert!(fill(&[], 300.0, 4).log[0].contains("nothing to rank"));
    }

    #[test]
    fn a_bad_tier_cap_or_budget_is_refused() {
        let env = |k: &str| (k == "SPIRA_TIER_BUDGET_T2_MS").then(|| "ten".to_string());
        assert!(TierCaps::from_env(&env).is_err());
        let env = |k: &str| (k == "SPIRA_TIER_BUDGET_T2_MS").then(|| "5000".to_string());
        assert_eq!(TierCaps::from_env(&env).unwrap().t2_ms, 5000.0);
        assert_eq!(number("300"), Some(300.0));
        assert_eq!(number("12.5"), Some(12.5));
        assert_eq!(number("-1"), None);
        assert_eq!(number(""), None);
        assert_eq!(number("1e3"), None);
    }
}
