//! A repo tier's status as testenv records it in `run.events`: `<at> suite <tier> <word>`.
//! The gate reads the typed record, never the summary row testenv prints for people.

/// The file the gate names in a run's environment for testenv to append each tier's record to.
/// A per-run hand-off the config file cannot carry, so testenv reads it from the environment.
pub const EVENTS_ENV: &str = "SPIRA_TESTENV_TIER_EVENTS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Red,
    Fault,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Red => "RED",
            Status::Fault => "FAULT",
        }
    }

    fn of_word(w: &str) -> Option<Status> {
        [Status::Ok, Status::Red, Status::Fault].into_iter().find(|s| s.word() == w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    pub name: String,
    pub status: Status,
}

/// Every tier a `run.events` text records a status for, in order. A line that is no tier
/// record (a phase, a verdict, a status this reader does not know) names nothing.
pub fn read(events: &str) -> Vec<Tier> {
    events
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace().skip(1);
            let (kind, name, word) = (f.next()?, f.next()?, f.next()?);
            (kind == "suite" && f.next().is_none()).then_some(())?;
            Some(Tier { name: name.to_string(), status: Status::of_word(word)? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suite_event_with_a_tier_status_is_a_tier() {
        let t = read("10 phase setup\n11 suite gate RED\n12 suite build ok\n13 suite lint FAULT\n14 verdict RED -");
        let got: Vec<_> = t.iter().map(|t| (t.name.as_str(), t.status)).collect();
        assert_eq!(got, [("gate", Status::Red), ("build", Status::Ok), ("lint", Status::Fault)]);
    }

    #[test]
    fn lines_that_are_no_tier_record_name_nothing() {
        for l in ["", "11 suite gate", "11 suite gate bogus", "11 suite gate ok extra", "11 phase gate", "suite gate ok"] {
            assert!(read(l).is_empty(), "{l:?}");
        }
    }
}
