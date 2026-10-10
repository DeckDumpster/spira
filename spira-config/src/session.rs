//! A claim's holder is a session, not a name: `aeon-<name>@<pid>.<starttime>`. Names are
//! reused across sessions and personas, so only pid plus starttime tells a dead session
//! from a live one that happens to carry the same name.

use crate::admission::{Procs, RealProcs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub pid: u32,
    pub start: u64,
}

pub fn holder_for(name: &str, pid: u32, procs: &dyn Procs) -> String {
    match procs.start_of(pid) {
        Some(start) => format!("{name}@{pid}.{start}"),
        None => name.to_string(),
    }
}

pub fn own_holder(name: &str) -> String {
    holder_for(name, std::process::id(), &RealProcs)
}

pub fn parse(holder: &str) -> Option<Session> {
    let (name, rest) = holder.rsplit_once('@')?;
    let (pid, start) = rest.split_once('.')?;
    (!name.is_empty()).then_some(())?;
    Some(Session { name: name.to_string(), pid: pid.parse().ok()?, start: start.parse().ok()? })
}

/// True only when the holder names a session and that session is provably gone. A bare-name
/// holder proves nothing either way.
pub fn session_gone(holder: &str, procs: &dyn Procs) -> bool {
    parse(holder).is_some_and(|s| procs.start_of(s.pid) != Some(s.start))
}

pub fn same_name_other_session(holder: &str, mine: &str) -> bool {
    match (parse(holder), parse(mine)) {
        (Some(h), Some(m)) => h.name == m.name && (h.pid, h.start) != (m.pid, m.start),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Table(HashMap<u32, u64>);
    impl Procs for Table {
        fn start_of(&self, pid: u32) -> Option<u64> {
            self.0.get(&pid).copied()
        }
        fn ppid_of(&self, _: u32) -> Option<u32> {
            None
        }
    }

    #[test]
    fn a_holder_round_trips_and_names_its_session() {
        let t = Table(HashMap::from([(7, 99)]));
        let h = holder_for("aeon-mindy", 7, &t);
        assert_eq!(h, "aeon-mindy@7.99");
        assert_eq!(parse(&h), Some(Session { name: "aeon-mindy".into(), pid: 7, start: 99 }));
        assert_eq!(parse("aeon-mindy"), None);
    }

    #[test]
    fn a_dead_session_is_gone_even_when_its_name_is_live_in_another_session() {
        let t = Table(HashMap::from([(8, 120)]));
        assert!(session_gone("aeon-mindy@7.99", &t), "pid 7 does not exist");
        assert!(session_gone("aeon-mindy@8.99", &t), "pid 8 was reused: a different starttime");
        assert!(!session_gone("aeon-mindy@8.120", &t));
        assert!(!session_gone("aeon-mindy", &t), "a bare name proves nothing");
    }

    #[test]
    fn the_same_name_under_another_session_is_another_session() {
        assert!(same_name_other_session("aeon-mindy@7.99", "aeon-mindy@8.120"));
        assert!(!same_name_other_session("aeon-mindy@7.99", "aeon-mindy@7.99"));
        assert!(!same_name_other_session("aeon-cindy@7.99", "aeon-mindy@8.120"));
        assert!(!same_name_other_session("aeon-mindy", "aeon-mindy@8.120"));
    }
}
