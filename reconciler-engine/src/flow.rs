//! The flow graph: every section's `flow { consumes, produces }` as one directed graph. Pure.
//! It answers three questions: how a section's output reaches goodput ([`Flow::path_to_goodput`]),
//! who feeds a resource ([`Flow::upstream`]), and what a step shares with others
//! ([`Flow::neighbourhood`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use spira_desired_state::section::ActionClass;

use crate::assemble::Assembled;

/// The resource a path to goodput ends at.
pub const GOODPUT: &str = "lifecycle/landed";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flow {
    consumes: BTreeMap<String, Vec<String>>,
    produces: BTreeMap<String, Vec<String>>,
}

/// What a step shares with others, and the consumed resources nothing feeds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Neighbourhood {
    pub members: BTreeSet<String>,
    pub absent_upstream: Vec<String>,
}

fn capacity(class: ActionClass) -> bool {
    matches!(class, ActionClass::CapacityRaise | ActionClass::CapacityLower)
}

impl Flow {
    pub fn of(a: &Assembled) -> Flow {
        let mut f = Flow::default();
        for (section, cur) in &a.current.sections {
            f.consumes.insert(section.clone(), cur.flow.consumes.clone());
            f.produces.insert(section.clone(), cur.flow.produces.clone());
        }
        f
    }

    pub fn consumes(&self, section: &str) -> &[String] {
        self.consumes.get(section).map_or(&[], Vec::as_slice)
    }

    /// The sections that produce `resource`.
    pub fn upstream(&self, resource: &str) -> Vec<&str> {
        self.produces.iter().filter(|(_, p)| p.iter().any(|r| r == resource)).map(|(s, _)| s.as_str()).collect()
    }

    /// The shortest chain `section, resource, section, …, lifecycle/landed`, each resource
    /// produced by the section before it and consumed by the section after; empty when the
    /// section's output never reaches it.
    pub fn path_to_goodput(&self, section: &str) -> Vec<String> {
        let mut came: BTreeMap<String, String> = BTreeMap::new();
        let mut seen = BTreeSet::from([section.to_string()]);
        let mut queue = VecDeque::from([section.to_string()]);
        while let Some(node) = queue.pop_front() {
            let next: Vec<String> = match self.produces.get(&node) {
                Some(made) => made.clone(),
                None => self.consumes.iter().filter(|(_, c)| c.contains(&node)).map(|(s, _)| s.clone()).collect(),
            };
            for n in next {
                if !seen.insert(n.clone()) {
                    continue;
                }
                came.insert(n.clone(), node.clone());
                if n == GOODPUT {
                    let mut path = vec![n.clone()];
                    let mut at = n;
                    while let Some(p) = came.get(&at) {
                        path.push(p.clone());
                        at = p.clone();
                    }
                    path.reverse();
                    return path;
                }
                queue.push_back(n);
            }
        }
        Vec::new()
    }

    /// A `local` step's neighbourhood is its target; a capacity step's adds every resource its
    /// section consumes and the sections that produce each.
    pub fn neighbourhood(&self, section: &str, class: ActionClass, target: &str) -> Neighbourhood {
        let mut n = Neighbourhood { members: BTreeSet::from([format!("target:{target}")]), absent_upstream: Vec::new() };
        if !capacity(class) {
            return n;
        }
        for r in self.consumes(section) {
            n.members.insert(format!("resource:{r}"));
            let producers = self.upstream(r);
            if producers.is_empty() {
                n.absent_upstream.push(r.clone());
            }
            n.members.extend(producers.into_iter().map(|p| format!("section:{p}")));
        }
        n
    }

    /// The sections a capacity step depends on: every producer of what `section` consumes.
    pub fn upstream_sections(&self, section: &str, class: ActionClass) -> Vec<&str> {
        if !capacity(class) {
            return Vec::new();
        }
        self.consumes(section).iter().flat_map(|r| self.upstream(r)).collect()
    }
}

#[cfg(test)]
mod tests;
