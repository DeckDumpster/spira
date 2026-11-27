//! The topic table, `spira/topics.toml`: the families subscribers filter by. Publishing does
//! not read it.

use std::sync::OnceLock;

use serde::Deserialize;

const TABLE: &str = include_str!("../../spira/topics.toml");

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    /// The pattern: `<name>` is any one segment, `<a|b>` one of the words.
    pub family: String,
    pub class: Class,
    pub publisher: String,
    /// The payload's field names.
    pub fields: Vec<String>,
    /// The publisher names that live as long as what they publish: one going is news to this
    /// family's subscribers. A publisher that exits after each message is not listed.
    #[serde(default)]
    pub resident: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    topic: Vec<Family>,
}

/// The whole table, in file order.
pub fn families() -> &'static [Family] {
    static F: OnceLock<Vec<Family>> = OnceLock::new();
    F.get_or_init(|| toml::from_str::<Table>(TABLE).expect("the compiled-in topics.toml parses").topic)
}

enum Segment<'a> {
    Literal(&'a str),
    Any,
    OneOf(&'a str),
}

impl<'a> Segment<'a> {
    fn of(s: &'a str) -> Segment<'a> {
        match s.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
            Some(inner) if inner.contains('|') => Segment::OneOf(inner),
            Some(_) => Segment::Any,
            None => Segment::Literal(s),
        }
    }

    fn admits(&self, word: &str) -> bool {
        match self {
            Segment::Literal(l) => *l == word,
            Segment::Any => !word.is_empty(),
            Segment::OneOf(words) => words.split('|').any(|w| w == word),
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

fn segments(pattern: &str) -> Vec<Segment<'_>> {
    // An alternation holds `|`, never `/`, so splitting on `/` keeps each `<...>` whole.
    pattern.split('/').map(Segment::of).collect()
}

/// The family a concrete topic belongs to; a topic holding a wildcard belongs to none.
pub fn family_of(topic: &str) -> Option<&'static Family> {
    if topic.contains(['+', '#', '\0']) {
        return None;
    }
    let words: Vec<&str> = topic.split('/').collect();
    families().iter().find(|f| {
        let pattern = segments(&f.family);
        pattern.len() == words.len() && pattern.iter().zip(&words).all(|(p, w)| p.admits(w))
    })
}

/// Whether an MQTT subscription filter (`+` one level, a final `#` the rest, the parent level
/// included) could match a topic of any declared family. A malformed filter matches none.
pub fn filter_matches_any(filter: &str) -> bool {
    families_matching(filter).next().is_some()
}

/// The declared families some topic of which the filter selects.
pub fn families_matching(filter: &str) -> impl Iterator<Item = &'static Family> + '_ {
    let words: Vec<&str> = filter.split('/').collect();
    let well_formed = !filter.is_empty()
        && !filter.contains('\0')
        && words.iter().enumerate().all(|(i, w)| match *w {
            "+" => true,
            "#" => i + 1 == words.len(),
            w => !w.contains(['+', '#']),
        });
    families().iter().filter(move |f| well_formed && filter_meets(&words, &segments(&f.family)))
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

    #[test_id(t274ltk)]
    #[test]
    fn a_filter_matches_when_some_declared_family_could_carry_a_topic_it_selects() {
        for yes in ["bead/+/state", "session/+/#", "session/+/age/+", "session/sp-a/age/30m", "release/active/#", "#"] {
            assert!(filter_matches_any(yes), "{yes}");
        }
        for no in ["nosuch/#", "bead/+/nosuch", "session/+/age/45m", "bead/+", "bead/#/state", "bead/a+/state", ""] {
            assert!(!filter_matches_any(no), "{no}");
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

    #[test_id(t6suhqh)]
    #[test]
    fn every_family_of_the_design_is_declared_with_its_class() {
        let of = |t: &str| family_of(t).map(|f| (f.family.as_str(), f.class));
        for topic in [
            "bead/sp-a/state", "bead/sp-a/sessions", "round/r/12", "gate/r/sp-a", "session/sp-a/state",
            "session/sp-a/age/15m", "session/sp-a/age/30m", "session/sp-a/age/60m", "test/t1/red", "release/active",
            "host/pressure", "pr/r/7", "concierge/inbox", "publisher/queue", "subscriber/concierge",
        ] {
            assert_eq!(of(topic).map(|(_, c)| c), Some(Class::State), "{topic}");
        }
        assert_eq!(of("progress/sp-a"), Some(("progress/<bead>", Class::Chatter)));
        for none in ["bead//state", "bead/+/state", "bead/sp-a/state/more", "session/sp-a/age/45m"] {
            assert!(of(none).is_none(), "{none}");
        }
    }

    #[test_id(tir0yka)]
    #[test]
    fn every_row_names_its_publisher_and_payload_fields_and_no_family_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for f in families() {
            assert!(!f.publisher.is_empty() && !f.fields.is_empty(), "{}", f.family);
            assert!(seen.insert(&f.family), "{} is declared twice", f.family);
        }
    }
}
