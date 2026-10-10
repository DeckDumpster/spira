//! The end of a red pass: once every mender has an outcome, the next pass is decided once.
//! A pure function of the outcomes, the round's members and what the previous pass left
//! behind, so every case is a table row.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::{Id, Member};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Mended,
    Environment,
    Flaky,
    Returned,
    Unsure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub suite: String,
    pub outcome: Outcome,
    pub diagnosis: String,
    /// The members the outcome concerns: who leaves on a return, who is suspected when unsure.
    pub members: Vec<Id>,
}

/// What a pass leaves for the next one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prior {
    /// The head of the previous pass when every one of its failures was environment or flaky.
    pub environment_head: Option<String>,
    /// Suites the previous pass judged unsure, with the diagnosis given.
    pub unsure: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Left {
    pub id: Id,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Run again at the round's head with `members` (mended tips in, `left` out). `retry` is
    /// the same head re-run unchanged.
    Pass { members: Vec<Member>, left: Vec<Left>, retry: bool },
    /// A box fault the loop cannot fix by retrying: the round stops and the Concierge is paged.
    Stop { page: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub next: Next,
    pub prior: Prior,
    /// Returns that named no member: nobody left, and the batcher says so loudly.
    pub unattributed: Vec<String>,
}

fn is_environment(o: Outcome) -> bool {
    matches!(o, Outcome::Environment | Outcome::Flaky)
}

pub fn decide(head: &str, round: &[Member], mended: &BTreeMap<Id, String>, failures: &[Failure], prior: &Prior) -> Decision {
    if !failures.is_empty() && failures.iter().all(|f| is_environment(f.outcome)) {
        let next_prior = Prior { environment_head: Some(head.to_string()), unsure: BTreeMap::new() };
        if prior.environment_head.as_deref() == Some(head) {
            let suites: Vec<&str> = failures.iter().map(|f| f.suite.as_str()).collect();
            return Decision {
                next: Next::Stop { page: format!("a second all-environment pass on head {head}: {}", suites.join(", ")) },
                prior: next_prior,
                unattributed: vec![],
            };
        }
        return Decision { next: Next::Pass { members: round.to_vec(), left: vec![], retry: true }, prior: next_prior, unattributed: vec![] };
    }

    let mut leaving: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    let mut unattributed = Vec::new();
    let mut unsure = BTreeMap::new();
    for f in failures {
        match f.outcome {
            Outcome::Returned if f.members.is_empty() => unattributed.push(f.suite.clone()),
            Outcome::Returned => {
                for id in &f.members {
                    leaving.entry(id.clone()).or_default().push(f.diagnosis.clone());
                }
            }
            Outcome::Unsure => match prior.unsure.get(&f.suite) {
                Some(earlier) if !f.members.is_empty() => {
                    for id in &f.members {
                        leaving.entry(id.clone()).or_default().extend([earlier.clone(), f.diagnosis.clone()]);
                    }
                }
                Some(_) => unattributed.push(f.suite.clone()),
                None => {
                    unsure.insert(f.suite.clone(), f.diagnosis.clone());
                }
            },
            Outcome::Mended | Outcome::Environment | Outcome::Flaky => {}
        }
    }
    // A member stacked on one that leaves leaves with it.
    loop {
        let before = leaving.len();
        for m in round {
            if let Some(p) = m.stack.keys().find(|p| leaving.contains_key(*p)) {
                let why = format!("stacked on {p}, which was returned from the round");
                leaving.entry(m.id.clone()).or_insert_with(|| vec![why]);
            }
        }
        if leaving.len() == before {
            break;
        }
    }
    let members = round
        .iter()
        .filter(|m| !leaving.contains_key(&m.id))
        .map(|m| Member { tip: mended.get(&m.id).cloned().unwrap_or_else(|| m.tip.clone()), ..m.clone() })
        .collect();
    let left = round.iter().filter_map(|m| leaving.get(&m.id).map(|d| Left { id: m.id.clone(), why: d.join("\n---\n") })).collect();
    Decision { next: Next::Pass { members, left, retry: false }, prior: Prior { environment_head: None, unsure }, unattributed }
}

/// The suites of a pass whose outcome is not yet known, for the pane and the wait.
pub fn unsettled(expected: &BTreeSet<String>, settled: &[Failure]) -> Vec<String> {
    expected.iter().filter(|s| !settled.iter().any(|f| &f.suite == *s)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, tip: &str) -> Member {
        Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![] }
    }
    fn round() -> Vec<Member> {
        vec![member("a", "a0"), member("b", "b0"), member("c", "c0")]
    }
    fn f(suite: &str, outcome: Outcome, members: &[&str]) -> Failure {
        Failure { suite: suite.into(), outcome, diagnosis: format!("{suite} diagnosis"), members: members.iter().map(|m| m.to_string()).collect() }
    }
    fn tips(next: &Next) -> Vec<(String, String)> {
        match next {
            Next::Pass { members, .. } => members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(),
            Next::Stop { .. } => panic!("stopped"),
        }
    }
    fn left(next: &Next) -> Vec<String> {
        match next {
            Next::Pass { left, .. } => left.iter().map(|l| l.id.clone()).collect(),
            Next::Stop { .. } => panic!("stopped"),
        }
    }
    fn mended(pairs: &[(&str, &str)]) -> BTreeMap<Id, String> {
        pairs.iter().map(|(b, t)| (b.to_string(), t.to_string())).collect()
    }
    fn all(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn mended_tips_go_in_and_nobody_leaves() {
        let d = decide("h", &round(), &mended(&[("b", "b1")]), &[f("t1", Outcome::Mended, &["b"])], &Prior::default());
        assert_eq!(tips(&d.next), all(&[("a", "a0"), ("b", "b1"), ("c", "c0")]));
        assert!(left(&d.next).is_empty());
        assert!(matches!(d.next, Next::Pass { retry: false, .. }));
    }

    #[test]
    fn a_returned_member_leaves_with_its_diagnosis_and_a_mended_tip_of_another_stays_in() {
        let d = decide("h", &round(), &mended(&[("a", "a1")]), &[f("t1", Outcome::Mended, &["a"]), f("t2", Outcome::Returned, &["c"])], &Prior::default());
        assert_eq!(tips(&d.next), all(&[("a", "a1"), ("b", "b0")]));
        let Next::Pass { left, .. } = &d.next else { panic!() };
        assert_eq!(left, &[Left { id: "c".into(), why: "t2 diagnosis".into() }]);
    }

    #[test]
    fn a_returned_member_that_was_also_mended_leaves_with_no_tip_carried() {
        let d = decide("h", &round(), &mended(&[("b", "b1")]), &[f("t1", Outcome::Mended, &["b"]), f("t2", Outcome::Returned, &["b"])], &Prior::default());
        assert_eq!(tips(&d.next), all(&[("a", "a0"), ("c", "c0")]));
        assert_eq!(left(&d.next), ["b"]);
    }

    #[test]
    fn two_returns_of_one_member_carry_both_diagnoses() {
        let d = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Returned, &["b"]), f("t2", Outcome::Returned, &["b"])], &Prior::default());
        let Next::Pass { left, .. } = &d.next else { panic!() };
        assert_eq!(left[0].why, "t1 diagnosis\n---\nt2 diagnosis");
    }

    #[test]
    fn an_interaction_return_removes_every_member_it_names() {
        let d = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Returned, &["a", "c"])], &Prior::default());
        assert_eq!(tips(&d.next), all(&[("b", "b0")]));
    }

    #[test]
    fn a_member_stacked_on_a_returned_one_leaves_with_it() {
        let mut r = round();
        r[1].stack.insert("a".into(), "a0".into());
        let d = decide("h", &r, &mended(&[]), &[f("t1", Outcome::Returned, &["a"])], &Prior::default());
        assert_eq!(left(&d.next), ["a", "b"]);
        assert_eq!(tips(&d.next), all(&[("c", "c0")]));
    }

    #[test]
    fn all_environment_or_flaky_reruns_the_same_head_unchanged() {
        for outcomes in [vec![Outcome::Environment], vec![Outcome::Flaky], vec![Outcome::Environment, Outcome::Flaky]] {
            let fs: Vec<Failure> = outcomes.iter().enumerate().map(|(i, o)| f(&format!("t{i}"), *o, &[])).collect();
            let d = decide("h", &round(), &mended(&[]), &fs, &Prior::default());
            assert_eq!(tips(&d.next), all(&[("a", "a0"), ("b", "b0"), ("c", "c0")]));
            assert!(matches!(d.next, Next::Pass { retry: true, .. }));
            assert_eq!(d.prior.environment_head.as_deref(), Some("h"));
        }
    }

    #[test]
    fn a_second_all_environment_pass_on_the_same_head_stops_and_pages() {
        let first = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Environment, &[])], &Prior::default());
        let second = decide("h", &round(), &mended(&[]), &[f("t2", Outcome::Flaky, &[])], &first.prior);
        let Next::Stop { page } = second.next else { panic!("must stop") };
        assert!(page.contains("t2") && page.contains("h"));
    }

    #[test]
    fn an_all_environment_pass_on_another_head_is_a_first() {
        let first = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Environment, &[])], &Prior::default());
        let d = decide("h2", &round(), &mended(&[]), &[f("t1", Outcome::Environment, &[])], &first.prior);
        assert!(matches!(d.next, Next::Pass { retry: true, .. }));
    }

    #[test]
    fn a_pass_with_one_real_failure_among_environment_ones_is_not_all_environment() {
        let first = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Environment, &[])], &Prior::default());
        let d = decide("h", &round(), &mended(&[("a", "a1")]), &[f("t1", Outcome::Environment, &[]), f("t2", Outcome::Mended, &["a"])], &first.prior);
        let Next::Pass { retry, .. } = d.next else { panic!("must not stop") };
        assert!(!retry);
        assert_eq!(d.prior.environment_head, None);
    }

    #[test]
    fn an_unsure_suite_rides_along_once() {
        let d = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Unsure, &["b"])], &Prior::default());
        assert_eq!(tips(&d.next).len(), 3);
        assert_eq!(d.prior.unsure.get("t1").map(String::as_str), Some("t1 diagnosis"));
    }

    #[test]
    fn a_suite_unsure_on_two_consecutive_passes_returns_its_member_with_both_diagnoses() {
        let first = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Unsure, &["b"])], &Prior::default());
        let mut again = f("t1", Outcome::Unsure, &["b"]);
        again.diagnosis = "second".into();
        let d = decide("h2", &round(), &mended(&[]), &[again], &first.prior);
        let Next::Pass { left, .. } = &d.next else { panic!() };
        assert_eq!(left, &[Left { id: "b".into(), why: "t1 diagnosis\n---\nsecond".into() }]);
        assert!(d.prior.unsure.is_empty());
    }

    #[test]
    fn unsure_on_a_suite_the_next_pass_does_not_fail_is_forgotten() {
        let first = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Unsure, &["b"])], &Prior::default());
        let d = decide("h2", &round(), &mended(&[("a", "a1")]), &[f("t2", Outcome::Mended, &["a"])], &first.prior);
        assert!(d.prior.unsure.is_empty());
        let third = decide("h3", &round(), &mended(&[]), &[f("t1", Outcome::Unsure, &["b"])], &d.prior);
        assert!(left(&third.next).is_empty(), "not consecutive");
    }

    #[test]
    fn a_return_naming_no_member_ejects_nobody_and_is_reported() {
        let d = decide("h", &round(), &mended(&[]), &[f("t1", Outcome::Returned, &[])], &Prior::default());
        assert_eq!(tips(&d.next).len(), 3);
        assert_eq!(d.unattributed, ["t1"]);
    }

    #[test]
    fn unsettled_names_the_suites_without_an_outcome() {
        let expected: BTreeSet<String> = ["t1", "t2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(unsettled(&expected, &[f("t1", Outcome::Flaky, &[])]), ["t2"]);
    }
}
