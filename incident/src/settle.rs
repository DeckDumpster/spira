//! An incident closes itself once its fix has landed and its detector is quiet; if the
//! detector still fires it stays with Ops, once per set of fixes. One with no probe to re-run
//! closes on the landing alone. An unlanded fix is a
//! `blocks` edge, which the claim already refuses, so nothing here touches those beads.
//! Every effect is a [`Settler`] method, so the decisions are unit-tested with no database.

use crate::decide::ref_hash;

const DROPPED: &str = "DROPPED";
const MAX_SUPERSEDE_HOPS: usize = 3;

pub const FIRED_LABEL_PREFIX: &str = "settle-fired:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub external_ref: Option<String>,
    pub labels: Vec<String>,
    pub fixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixState {
    Landed,
    Unlanded(String),
    /// Retired because `by` took over its work; `by`'s landing is the fix's.
    Superseded { by: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detector {
    Quiet,
    Firing(String),
    /// No probe can be re-run: the fix's landing on the landing ref, which passed its round, is the reading.
    NoProbe,
}

/// A fix whose landing commit the active release carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub sha: String,
}

pub trait Settler {
    fn candidates(&self) -> Result<Vec<Candidate>, String>;
    fn fix_state(&self, fix: &str) -> Result<FixState, String>;
    /// The commit on the repo's landing ref that names the fix.
    fn landing_on_base(&self, fix: &str) -> Result<Landing, String>;
    /// `Ok(None)`: the fix landed but the active release does not carry it yet.
    fn landing_in_force(&self, fix: &str) -> Result<Option<Landing>, String>;
    fn detect(&self, c: &Candidate) -> Result<Detector, String>;
    fn close(&self, id: &str, evidence: &str) -> Result<(), String>;
    fn persist(&self, id: &str, label: &str, note: &str) -> Result<(), String>;
    fn now_iso(&self) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Closed { id: String, evidence: String },
    Persists { id: String },
    Waiting { id: String, why: String },
    Skipped { id: String, why: String },
}

pub fn fired_label(fixes: &[String]) -> String {
    let mut f = fixes.to_vec();
    f.sort();
    format!("{FIRED_LABEL_PREFIX}{}", ref_hash(&f.join(",")))
}

pub fn settle_one(s: &dyn Settler, c: &Candidate) -> Outcome {
    let skip = |why: String| Outcome::Skipped { id: c.id.clone(), why };
    let wait = |why: String| Outcome::Waiting { id: c.id.clone(), why };
    let mut fixes = c.fixes.clone();
    if fixes.is_empty() {
        return skip("no fix dependency".into());
    }
    fixes.sort();
    // (the fix named by the edge, the bead whose landing counts: itself, or its landed successor)
    let mut landed: Vec<(String, String)> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for fix in &fixes {
        let mut target = fix.clone();
        let mut hops = 0;
        let state = loop {
            match s.fix_state(&target) {
                Ok(FixState::Superseded { by }) => {
                    hops += 1;
                    if hops > MAX_SUPERSEDE_HOPS {
                        return skip(format!("supersede chain of {fix} is too long"));
                    }
                    target = by;
                }
                other => break other,
            }
        };
        match state {
            Ok(FixState::Landed) => landed.push((fix.clone(), target)),
            Ok(FixState::Unlanded(st)) if st == DROPPED && target == *fix => dropped.push(fix.clone()),
            // The fix's own state is left out of the reason, so a standing wait reads the same while
            // the fix moves between unlanded states and is said once.
            Ok(_) => return wait(format!("fix {fix} not landed")),
            Err(e) => return skip(format!("cannot read {target}: {e}")),
        }
    }
    let detector = match s.detect(c) {
        Ok(d) => d,
        Err(e) => return skip(format!("detector could not run: {e}")),
    };
    let on_base = matches!(detector, Detector::NoProbe);
    let mut landings = Vec::new();
    for (fix, target) in &landed {
        let l = if on_base {
            s.landing_on_base(target).map_err(|e| format!("cannot find the landing of {target}: {e}"))
        } else {
            match s.landing_in_force(target) {
                Ok(Some(l)) => Ok(l),
                Ok(None) => return wait(format!("{target} landed but the active release does not carry it")),
                Err(e) => Err(format!("cannot tell whether {target} is in force: {e}")),
            }
        };
        match l {
            Ok(l) => landings.push(if fix == target { format!("fix landed: {fix} at {}", l.sha) } else { format!("fix {fix} superseded by {target}, landed at {}", l.sha) }),
            Err(e) => return skip(e),
        }
    }
    let fired = fired_label(&fixes);
    let mut report = landings;
    if !dropped.is_empty() {
        report.push(format!("fix dropped: {}", dropped.join(", ")));
    }
    let closed = |tail: String| {
        let evidence = format!("{}; {tail}", report.join("; "));
        match s.close(&c.id, &evidence) {
            Ok(()) => Outcome::Closed { id: c.id.clone(), evidence },
            Err(e) => skip(format!("close refused: {e}")),
        }
    };
    match detector {
        Detector::NoProbe => {
            if landed.is_empty() {
                return skip("no detector to re-run and no fix landed".into());
            }
            closed(format!("no detector to re-run: the landing passed its round on the landing ref at {}", s.now_iso()))
        }
        Detector::Quiet => closed(format!("detector quiet at {}", s.now_iso())),
        Detector::Firing(detail) => {
            if c.labels.contains(&fired) {
                return wait("fix landed but the alarm persists; already with Ops".into());
            }
            let note = format!("fix landed but the alarm persists: {} ({detail}) at {}", fixes.join(", "), s.now_iso());
            match s.persist(&c.id, &fired, &note) {
                Ok(()) => Outcome::Persists { id: c.id.clone() },
                Err(e) => skip(format!("cannot record the persisting alarm: {e}")),
            }
        }
    }
}

/// The Waiting outcomes whose reason differs from `prev` (one `id\twhy` per line, as returned
/// before), and the state to remember for the next pass.
pub fn changed_waiting<'a>(prev: &str, outcomes: &'a [Outcome]) -> (Vec<&'a Outcome>, String) {
    let prev: std::collections::HashSet<&str> = prev.lines().collect();
    let mut state = Vec::new();
    let mut fresh = Vec::new();
    for o in outcomes {
        match o {
            Outcome::Waiting { id, why } => {
                let line = format!("{id}\t{}", why.replace('\n', " "));
                if !prev.contains(line.as_str()) {
                    fresh.push(o);
                }
                state.push(line);
            }
            _ => fresh.push(o),
        }
    }
    state.sort();
    (fresh, state.join("\n"))
}

pub fn settle(s: &dyn Settler) -> Result<Vec<Outcome>, String> {
    Ok(s.candidates()?.iter().map(|c| settle_one(s, c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Fake {
        fixes: Vec<String>,
        states: HashMap<String, FixState>,
        in_force: bool,
        detector: Detector,
        closed: RefCell<Vec<(String, String)>>,
        persisted: RefCell<Vec<(String, String, String)>>,
    }

    impl Fake {
        fn new(state: FixState, detector: Detector) -> Fake {
            Fake {
                fixes: vec!["sp-fix".into()],
                states: [("sp-fix".to_string(), state)].into(),
                in_force: true,
                detector,
                closed: RefCell::default(),
                persisted: RefCell::default(),
            }
        }
    }

    impl Settler for Fake {
        fn candidates(&self) -> Result<Vec<Candidate>, String> {
            Ok(vec![Candidate { fixes: self.fixes.clone(), ..cand(&[]) }])
        }
        fn fix_state(&self, fix: &str) -> Result<FixState, String> {
            self.states.get(fix).cloned().ok_or_else(|| "no row".into())
        }
        fn landing_on_base(&self, _: &str) -> Result<Landing, String> {
            Ok(Landing { sha: "abc123".into() })
        }
        fn landing_in_force(&self, _: &str) -> Result<Option<Landing>, String> {
            Ok(self.in_force.then(|| Landing { sha: "abc123".into() }))
        }
        fn detect(&self, _: &Candidate) -> Result<Detector, String> {
            Ok(self.detector.clone())
        }
        fn close(&self, id: &str, evidence: &str) -> Result<(), String> {
            self.closed.borrow_mut().push((id.into(), evidence.into()));
            Ok(())
        }
        fn persist(&self, id: &str, label: &str, note: &str) -> Result<(), String> {
            self.persisted.borrow_mut().push((id.into(), label.into(), note.into()));
            Ok(())
        }
        fn now_iso(&self) -> String {
            "2026-10-07T00:00:00Z".into()
        }
    }

    fn cand(labels: &[&str]) -> Candidate {
        Candidate { id: "sp-inc".into(), external_ref: Some("incident:x.service".into()), labels: labels.iter().map(|s| s.to_string()).collect(), fixes: vec!["sp-fix".into()] }
    }

    #[test]
    fn a_landed_fix_and_a_quiet_detector_close_the_incident_with_evidence() {
        let f = Fake::new(FixState::Landed, Detector::Quiet);
        let out = settle_one(&f, &cand(&[]));
        assert!(matches!(out, Outcome::Closed { .. }));
        let closed = f.closed.borrow();
        assert_eq!(closed.len(), 1);
        for want in ["sp-fix", "abc123", "detector quiet at 2026-10-07T00:00:00Z"] {
            assert!(closed[0].1.contains(want), "{want} missing from {:?}", closed[0].1);
        }
        assert!(f.persisted.borrow().is_empty());
    }

    #[test]
    fn a_landed_fix_and_a_firing_detector_send_it_to_ops_once() {
        let f = Fake::new(FixState::Landed, Detector::Firing("unit is failed".into()));
        assert_eq!(settle_one(&f, &cand(&[])), Outcome::Persists { id: "sp-inc".into() });
        assert!(f.closed.borrow().is_empty());
        let p = f.persisted.borrow();
        assert!(p[0].2.contains("fix landed but the alarm persists"));
        let marked = cand(&[p[0].1.as_str()]);
        assert!(matches!(settle_one(&f, &marked), Outcome::Waiting { .. }), "a second pass over the same fixes records nothing more");
        assert_eq!(f.persisted.borrow().len(), 1);
    }

    #[test]
    fn an_unlanded_fix_is_left_alone_and_the_detector_is_never_run() {
        let f = Fake::new(FixState::Unlanded("CERTIFIED".into()), Detector::Quiet);
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Waiting { .. }));
        assert!(f.closed.borrow().is_empty() && f.persisted.borrow().is_empty());
    }

    #[test]
    fn a_dropped_fix_does_not_block_the_incident() {
        let f = Fake::new(FixState::Unlanded("DROPPED".into()), Detector::Quiet);
        let out = settle_one(&f, &cand(&[]));
        assert!(matches!(out, Outcome::Closed { .. }), "{out:?}");
        assert!(f.closed.borrow()[0].1.contains("fix dropped: sp-fix"));
        let g = Fake::new(FixState::Unlanded("DROPPED".into()), Detector::Firing("failed".into()));
        assert_eq!(settle_one(&g, &cand(&[])), Outcome::Persists { id: "sp-inc".into() });
    }

    #[test]
    fn a_waiting_line_is_logged_only_when_it_changes() {
        let w = |why: &str| vec![Outcome::Waiting { id: "a".into(), why: why.into() }, Outcome::Persists { id: "b".into() }];
        let (x, y) = (w("x"), w("y"));
        let (first, state) = changed_waiting("", &x);
        assert_eq!(first.len(), 2);
        let (again, state2) = changed_waiting(&state, &x);
        assert_eq!(again.len(), 1, "only the non-waiting outcome repeats");
        assert_eq!(state, state2);
        assert_eq!(changed_waiting(&state, &y).0.len(), 2);
    }

    #[test]
    fn a_landed_fix_the_active_release_lacks_waits() {
        let mut f = Fake::new(FixState::Landed, Detector::Quiet);
        f.in_force = false;
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Waiting { .. }));
        assert!(f.closed.borrow().is_empty());
    }

    #[test]
    fn a_landed_fix_closes_an_incident_with_no_probe() {
        let mut f = Fake::new(FixState::Landed, Detector::NoProbe);
        f.in_force = false;
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Closed { .. }));
        let closed = f.closed.borrow();
        for want in ["fix landed: sp-fix at abc123", "no detector to re-run", "2026-10-07T00:00:00Z"] {
            assert!(closed[0].1.contains(want), "{want} missing from {:?}", closed[0].1);
        }
    }

    #[test]
    fn an_incident_with_no_fix_is_never_closed() {
        let g = Fake::new(FixState::Landed, Detector::Quiet);
        let none = Candidate { fixes: vec![], ..cand(&[]) };
        assert!(matches!(settle_one(&g, &none), Outcome::Skipped { .. }));
        assert!(g.closed.borrow().is_empty());
    }

    fn chain(links: &[(&str, FixState)]) -> Fake {
        let mut f = Fake::new(FixState::Landed, Detector::Quiet);
        f.states = links.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        f
    }

    fn sup(by: &str) -> FixState {
        FixState::Superseded { by: by.into() }
    }

    #[test]
    fn a_superseded_fix_closes_through_its_landed_successor() {
        let f = chain(&[("sp-fix", sup("sp-succ")), ("sp-succ", FixState::Landed)]);
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Closed { .. }));
        assert!(f.closed.borrow()[0].1.contains("sp-fix superseded by sp-succ"), "{:?}", f.closed);
    }

    #[test]
    fn a_superseded_fix_whose_successor_is_unlanded_waits() {
        let f = chain(&[("sp-fix", sup("sp-succ")), ("sp-succ", FixState::Unlanded("REWORK".into()))]);
        assert_eq!(settle_one(&f, &cand(&[])), Outcome::Waiting { id: "sp-inc".into(), why: "fix sp-fix not landed".into() });
        assert!(f.closed.borrow().is_empty());
    }

    #[test]
    fn a_supersede_chain_longer_than_three_hops_is_skipped_though_it_ends_landed() {
        let f = chain(&[("sp-fix", sup("sp-b")), ("sp-b", sup("sp-c")), ("sp-c", sup("sp-d")), ("sp-d", sup("sp-e")), ("sp-e", FixState::Landed)]);
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Skipped { why, .. } if why == "supersede chain of sp-fix is too long"));
        let three = chain(&[("sp-fix", sup("sp-b")), ("sp-b", sup("sp-c")), ("sp-c", sup("sp-d")), ("sp-d", FixState::Landed)]);
        assert!(matches!(settle_one(&three, &cand(&[])), Outcome::Closed { .. }));
    }

    #[test]
    fn a_no_probe_incident_whose_only_fix_was_dropped_is_never_closed() {
        let f = Fake::new(FixState::Unlanded("DROPPED".into()), Detector::NoProbe);
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Skipped { .. }));
        assert!(f.closed.borrow().is_empty());
    }

    #[test]
    fn a_supersede_cycle_is_skipped() {
        let f = chain(&[("sp-fix", sup("sp-b")), ("sp-b", sup("sp-fix"))]);
        assert_eq!(settle_one(&f, &cand(&[])), Outcome::Skipped { id: "sp-inc".into(), why: "supersede chain of sp-fix is too long".into() });
    }

    #[test]
    fn the_fired_label_is_stable_across_fix_order_and_differs_per_fix_set() {
        let a = fired_label(&["b".into(), "a".into()]);
        assert_eq!(a, fired_label(&["a".into(), "b".into()]));
        assert_ne!(a, fired_label(&["a".into()]));
    }
}
