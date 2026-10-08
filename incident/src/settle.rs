//! An incident closes itself once its fix has landed and its detector is quiet; if the
//! detector still fires it stays with Ops, once per set of fixes. An unlanded fix is a
//! `blocks` edge, which the claim already refuses, so nothing here touches those beads.
//! Every effect is a [`Settler`] method, so the decisions are unit-tested with no database.

use crate::decide::ref_hash;

pub const FIRED_LABEL_PREFIX: &str = "settle-fired:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub external_ref: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixState {
    Landed,
    Unlanded(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detector {
    Quiet,
    Firing(String),
    /// No probe can be re-run for this incident; it is left alone, never closed on a guess.
    NoProbe,
}

/// A fix whose landing commit the active release carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub sha: String,
}

pub trait Settler {
    fn candidates(&self) -> Result<Vec<Candidate>, String>;
    fn fixes(&self, incident: &str) -> Result<Vec<String>, String>;
    fn fix_state(&self, fix: &str) -> Result<FixState, String>;
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
    let mut fixes = match s.fixes(&c.id) {
        Ok(f) => f,
        Err(e) => return skip(format!("cannot read its fixes: {e}")),
    };
    if fixes.is_empty() {
        return skip("no fix dependency".into());
    }
    fixes.sort();
    let mut landings = Vec::new();
    for fix in &fixes {
        match s.fix_state(fix) {
            Ok(FixState::Landed) => {}
            Ok(FixState::Unlanded(st)) => return wait(format!("{fix} is {st}")),
            Err(e) => return skip(format!("cannot read {fix}: {e}")),
        }
        match s.landing_in_force(fix) {
            Ok(Some(l)) => landings.push((fix.clone(), l.sha)),
            Ok(None) => return wait(format!("{fix} landed but the active release does not carry it")),
            Err(e) => return skip(format!("cannot tell whether {fix} is in force: {e}")),
        }
    }
    let fired = fired_label(&fixes);
    match s.detect(c) {
        Ok(Detector::NoProbe) => skip("no detector to re-run".into()),
        Ok(Detector::Quiet) => {
            let landed = landings.iter().map(|(f, sha)| format!("{f} at {sha}")).collect::<Vec<_>>().join(", ");
            let evidence = format!("fix landed: {landed}; detector quiet at {}", s.now_iso());
            match s.close(&c.id, &evidence) {
                Ok(()) => Outcome::Closed { id: c.id.clone(), evidence },
                Err(e) => skip(format!("close refused: {e}")),
            }
        }
        Ok(Detector::Firing(detail)) => {
            if c.labels.contains(&fired) {
                return wait("fix landed but the alarm persists; already with Ops".into());
            }
            let note = format!("fix landed but the alarm persists: {} ({detail}) at {}", fixes.join(", "), s.now_iso());
            match s.persist(&c.id, &fired, &note) {
                Ok(()) => Outcome::Persists { id: c.id.clone() },
                Err(e) => skip(format!("cannot record the persisting alarm: {e}")),
            }
        }
        Err(e) => skip(format!("detector could not run: {e}")),
    }
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
            Ok(vec![cand(&[])])
        }
        fn fixes(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(self.fixes.clone())
        }
        fn fix_state(&self, fix: &str) -> Result<FixState, String> {
            self.states.get(fix).cloned().ok_or_else(|| "no row".into())
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
        Candidate { id: "sp-inc".into(), external_ref: Some("incident:x.service".into()), labels: labels.iter().map(|s| s.to_string()).collect() }
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
    fn a_landed_fix_the_active_release_lacks_waits() {
        let mut f = Fake::new(FixState::Landed, Detector::Quiet);
        f.in_force = false;
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Waiting { .. }));
        assert!(f.closed.borrow().is_empty());
    }

    #[test]
    fn an_incident_with_no_probe_or_no_fix_is_never_closed() {
        let f = Fake::new(FixState::Landed, Detector::NoProbe);
        assert!(matches!(settle_one(&f, &cand(&[])), Outcome::Skipped { .. }));
        let mut g = Fake::new(FixState::Landed, Detector::Quiet);
        g.fixes.clear();
        assert!(matches!(settle_one(&g, &cand(&[])), Outcome::Skipped { .. }));
        assert!(f.closed.borrow().is_empty() && g.closed.borrow().is_empty());
    }

    #[test]
    fn the_fired_label_is_stable_across_fix_order_and_differs_per_fix_set() {
        let a = fired_label(&["b".into(), "a".into()]);
        assert_eq!(a, fired_label(&["a".into(), "b".into()]));
        assert_ne!(a, fired_label(&["a".into()]));
    }
}
