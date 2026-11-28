//! Topic names: the class a publisher gives a message, the family a topic is recorded under,
//! and MQTT filter matching.

use serde::Deserialize;

/// A state topic is retained, so whoever connects gets the current value; chatter is best
/// effort. There is no third class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    State,
    Chatter,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::State => "state",
            Class::Chatter => "chatter",
        }
    }
}

impl std::str::FromStr for Class {
    type Err = String;
    fn from_str(s: &str) -> Result<Class, String> {
        match s {
            "state" => Ok(Class::State),
            "chatter" => Ok(Class::Chatter),
            other => Err(format!("{other:?} is not a topic class: it is state or chatter")),
        }
    }
}

pub enum Segment<'a> {
    Literal(&'a str),
    Any,
}

impl Segment<'_> {
    fn admits(&self, word: &str) -> bool {
        match self {
            Segment::Literal(l) => *l == word,
            Segment::Any => !word.is_empty(),
        }
    }
}

/// The family `topic` is recorded under: the first level kept, and each later level that holds
/// anything outside `a-z` (ids, ages, numbers) replaced by `+`. It is itself an MQTT filter
/// selecting `topic`.
pub fn family(topic: &str) -> String {
    let levels = topic.split('/').enumerate();
    let levels = levels.map(|(i, level)| if i == 0 || level.bytes().all(|b| b.is_ascii_lowercase()) { level } else { "+" });
    levels.collect::<Vec<_>>().join("/")
}

/// Whether `filter` is one a broker takes: `+` and `#` each a whole level, `#` only the last.
pub fn filter_is_well_formed(filter: &str) -> bool {
    let words: Vec<&str> = filter.split('/').collect();
    !filter.is_empty()
        && !filter.contains('\0')
        && words.iter().enumerate().all(|(i, w)| match *w {
            "+" => true,
            "#" => i + 1 == words.len(),
            w => !w.contains(['+', '#']),
        })
}

/// Whether the well-formed filter could select a topic of the recorded `family`, whose `+`
/// levels each stand for any one level.
pub fn filter_meets_family(filter: &str, family: &str) -> bool {
    let filter: Vec<&str> = filter.split('/').collect();
    let pattern = family.split('/').map(|l| if l == "+" { Segment::Any } else { Segment::Literal(l) });
    filter_meets(&filter, &pattern.collect::<Vec<_>>())
}

fn filter_meets(filter: &[&str], pattern: &[Segment]) -> bool {
    match filter.split_first() {
        None => pattern.is_empty(),
        Some((&"#", _)) => true,
        Some((word, rest)) => pattern
            .split_first()
            .is_some_and(|(p, more)| (*word == "+" || p.admits(word)) && filter_meets(rest, more)),
    }
}

/// Whether the well-formed subscription filter selects the concrete `topic`.
pub fn filter_selects(filter: &str, topic: &str) -> bool {
    let filter: Vec<&str> = filter.split('/').collect();
    filter_meets(&filter, &topic.split('/').map(Segment::Literal).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tpedv1y)]
    #[test]
    fn a_topics_family_keeps_the_first_level_and_wildcards_every_level_not_of_lowercase_letters() {
        for (topic, want) in [
            ("bead/sp-8wkgbs/state", "bead/+/state"),
            ("bead/sp-a/sessions", "bead/+/sessions"),
            ("session/sp-a/state", "session/+/state"),
            ("session/sp-a/age/15m", "session/+/age/+"),
            ("test/t274ltk/red", "test/+/red"),
            ("gate/spira/sp-a", "gate/spira/+"),
            ("round/spira/r-1", "round/spira/+"),
            ("pr/spira/7", "pr/spira/+"),
            ("progress/sp-a", "progress/+"),
            ("publisher/lc-serve", "publisher/+"),
            ("release/active", "release/active"),
            ("host/pressure", "host/pressure"),
            ("concierge/inbox", "concierge/inbox"),
            ("publisher/queue", "publisher/queue"),
            ("subscriber/concierge", "subscriber/concierge"),
            ("sp-a", "sp-a"),
            ("a/B/c", "a/+/c"),
            ("a/b_c/d", "a/+/d"),
        ] {
            assert_eq!(family(topic), want, "{topic}");
        }
    }

    #[test_id(tn8c8yg)]
    #[test]
    fn a_filter_is_well_formed_when_its_wildcards_are_whole_levels_and_hash_is_last() {
        for yes in ["bead/+/state", "nosuch/#", "#", "+", "a//b"] {
            assert!(filter_is_well_formed(yes), "{yes}");
        }
        for no in ["bead/#/state", "bead/a+/state", "a/#/b", "a/b#", "a\0", ""] {
            assert!(!filter_is_well_formed(no), "{no:?}");
        }
    }

    #[test_id(tkf58bj)]
    #[test]
    fn a_filter_meets_a_recorded_family_whose_plus_stands_for_any_one_level() {
        for (filter, family) in [("bead/sp-a/state", "bead/+/state"), ("bead/+/state", "bead/+/state"), ("bead/#", "bead/+/state"), ("session/+/age/45m", "session/+/age/+")] {
            assert!(filter_meets_family(filter, family), "{filter} {family}");
        }
        for (filter, family) in [("bead/+/nosuch", "bead/+/state"), ("bead/+", "bead/+/state"), ("nosuch/#", "bead/+/state")] {
            assert!(!filter_meets_family(filter, family), "{filter} {family}");
        }
    }

    #[test_id(t0c0ich)]
    #[test]
    fn a_filter_selects_a_topic_by_mqtt_wildcards() {
        for (filter, topic) in [("a/+/c", "a/b/c"), ("a/#", "a"), ("a/#", "a/b/c"), ("#", "a/b"), ("a/b", "a/b")] {
            assert!(filter_selects(filter, topic), "{filter} {topic}");
        }
        for (filter, topic) in [("a/+", "a/b/c"), ("a/b", "a"), ("a/c", "a/b"), ("a/b/c", "a/b")] {
            assert!(!filter_selects(filter, topic), "{filter} {topic}");
        }
    }
}
