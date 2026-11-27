//! A round that lands green on a repo proves the red its earlier rounds filed is over: the
//! "local round red … nothing can land" incidents still open for that repo are closed with the
//! landing as evidence. One whose last recurrence is after the landing is a newer red and stays.

/// The prefix `batcher` gives a local-red incident's external_ref (`basefail-local:<repo>:…`).
pub fn red_ref_prefix(repo: &str) -> String {
    format!("basefail-local:{repo}:")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRed {
    pub id: String,
    pub external_ref: Option<String>,
    /// The latest of its filing, update and `recurrence:N@<time>` stamps, in epoch seconds; `None` if it cannot be read.
    pub last_seen: Option<i64>,
}

pub trait Superseder {
    fn open_incidents(&self) -> Result<Vec<OpenRed>, String>;
    fn close(&self, id: &str, evidence: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Closed(String),
    Kept { id: String, why: &'static str },
    Refused { id: String, why: String },
}

pub fn supersede(s: &dyn Superseder, repo: &str, landing: &str, landed_at: i64) -> Result<Vec<Outcome>, String> {
    let prefix = red_ref_prefix(repo);
    let evidence = format!("a later round landed green on {repo} at {landing}; the red this reported is over");
    Ok(s.open_incidents()?
        .into_iter()
        .filter(|b| b.external_ref.as_deref().is_some_and(|r| r.starts_with(&prefix)))
        .map(|b| match b.last_seen {
            Some(t) if t <= landed_at => match s.close(&b.id, &evidence) {
                Ok(()) => Outcome::Closed(b.id),
                Err(why) => Outcome::Refused { id: b.id, why },
            },
            Some(_) => Outcome::Kept { id: b.id, why: "recurred after the landing" },
            None => Outcome::Kept { id: b.id, why: "its last recurrence cannot be read" },
        })
        .collect())
}

/// The store as [`Superseder`]: the unfinished incidents, closed through the lifecycle row.
pub struct RealSuperseder<'a> {
    pub bd: &'a crate::real::RealBd,
    /// A red is filed under the incident label or, on a product repo, the plan label.
    pub labels: Vec<String>,
}

/// Each red once, however many of the lists hold it.
fn merge_open(lists: Vec<Vec<OpenRed>>) -> Vec<OpenRed> {
    let mut seen = std::collections::HashSet::new();
    lists.into_iter().flatten().filter(|b| seen.insert(b.id.clone())).collect()
}

impl Superseder for RealSuperseder<'_> {
    fn open_incidents(&self) -> Result<Vec<OpenRed>, String> {
        let lists = self.labels.iter().map(|label| {
            let rows = self.bd.list_beads(crate::decide::Scope::Unfinished, Some(label), None, None)?;
            Ok(rows
                .into_iter()
                .map(|(row, bead)| {
                    let stamp = |k: &str| bead.get(k).and_then(|v| v.as_str()).and_then(crate::run::parse_iso8601);
                    let bumped = row.labels.iter().filter_map(|l| l.strip_prefix("recurrence:")?.split_once('@')).filter_map(|(_, at)| crate::run::parse_iso8601(at));
                    let last_seen = bumped.chain(stamp("updated_at")).chain(stamp("created_at")).max();
                    OpenRed { id: row.id, external_ref: row.external_ref, last_seen }
                })
                .collect())
        });
        Ok(merge_open(lists.collect::<Result<_, String>>()?))
    }

    fn close(&self, id: &str, evidence: &str) -> Result<(), String> {
        spira_store::lifecycle_row::close(id, evidence, "incident", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        open: Vec<OpenRed>,
        closed: RefCell<Vec<(String, String)>>,
    }

    impl Superseder for Fake {
        fn open_incidents(&self) -> Result<Vec<OpenRed>, String> {
            Ok(self.open.clone())
        }
        fn close(&self, id: &str, evidence: &str) -> Result<(), String> {
            self.closed.borrow_mut().push((id.into(), evidence.into()));
            Ok(())
        }
    }

    fn red(id: &str, repo: &str, suite: &str, last_seen: Option<i64>) -> OpenRed {
        OpenRed { id: id.into(), external_ref: Some(format!("basefail-local:{repo}:{suite}:local-tier:1234abcd")), last_seen }
    }

    #[test_id(tsxn5ly)]
    #[test]
    fn a_green_landing_closes_every_earlier_round_red_of_its_repo_naming_the_landing() {
        let f = Fake { open: vec![red("sp-a", "fx", "t1.sh", Some(100)), red("sp-b", "fx", "t2.sh", Some(200))], closed: Default::default() };
        let out = supersede(&f, "fx", "deadbeef", 300).unwrap();
        assert_eq!(out, vec![Outcome::Closed("sp-a".into()), Outcome::Closed("sp-b".into())]);
        let closed = f.closed.borrow();
        assert_eq!(closed.len(), 2);
        assert!(closed.iter().all(|(_, e)| e.contains("deadbeef")), "{closed:?}");
    }

    #[test_id(trbr70j)]
    #[test]
    fn a_red_in_the_plan_lane_is_closed_with_the_incident_lanes_and_one_in_both_lists_once() {
        let incident = vec![red("sp-i", "fx", "t1.sh", Some(100)), red("sp-both", "fx", "t2.sh", Some(100))];
        let plan = vec![red("sp-both", "fx", "t2.sh", Some(100)), red("sp-plan", "fx", "t3.sh", Some(100))];
        let f = Fake { open: merge_open(vec![incident, plan]), closed: Default::default() };
        let out = supersede(&f, "fx", "deadbeef", 300).unwrap();
        assert_eq!(out, ["sp-i", "sp-both", "sp-plan"].map(|i| Outcome::Closed(i.into())));
    }

    #[test_id(ty0dti9)]
    #[test]
    fn a_red_of_another_repo_a_newer_red_and_one_with_no_readable_time_stay_open() {
        let mut other = red("sp-o", "other", "t.sh", Some(1));
        other.external_ref = Some("basefail-local:other:t.sh:x:1".into());
        let plain = OpenRed { id: "sp-p".into(), external_ref: Some("land-deploy-fault:fx:abc".into()), last_seen: Some(1) };
        let f = Fake {
            open: vec![other, plain, red("sp-new", "fx", "t.sh", Some(301)), red("sp-nil", "fx", "t.sh", None)],
            closed: Default::default(),
        };
        let out = supersede(&f, "fx", "deadbeef", 300).unwrap();
        assert_eq!(
            out,
            vec![
                Outcome::Kept { id: "sp-new".into(), why: "recurred after the landing" },
                Outcome::Kept { id: "sp-nil".into(), why: "its last recurrence cannot be read" },
            ]
        );
        assert!(f.closed.borrow().is_empty());
    }
}
