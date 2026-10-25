//! The two release-tag forms and their one ordering. Mirrors `spira/release-order.sh`.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Stamp(String),
    Versioned(u64, u64, u64),
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Key::Stamp(a), Key::Stamp(b)) => a.cmp(b),
            (Key::Versioned(a, b, c), Key::Versioned(x, y, z)) => (a, b, c).cmp(&(x, y, z)),
            (Key::Stamp(_), Key::Versioned(..)) => Ordering::Less,
            (Key::Versioned(..), Key::Stamp(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn is_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..15].iter().all(u8::is_ascii_digit)
}

fn key(tag: &str) -> Option<Key> {
    if let Some(rest) = tag.strip_prefix("spira-release-") {
        let (head, tail) = rest.rsplit_once('-')?;
        let stamp_at_end = is_stamp(tail) || (tail.bytes().all(|b| b.is_ascii_digit()) && head.rsplit_once('-').is_some_and(|(_, s)| is_stamp(s)));
        return stamp_at_end.then(|| Key::Stamp(tag.to_string()));
    }
    let v = tag.strip_prefix("spira-")?;
    let mut it = v.split('.');
    let mut n = || {
        let p = it.next()?;
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse::<u64>().ok()).flatten()
    };
    let (a, b, c) = (n()?, n()?, n()?);
    it.next().is_none().then_some(Key::Versioned(a, b, c))
}

pub fn is_release_tag(tag: &str) -> bool {
    key(tag).is_some()
}

/// Release tags only, ascending, duplicates removed.
pub fn sort_release_tags<I: IntoIterator<Item = String>>(tags: I) -> Vec<String> {
    let mut v: Vec<(Key, String)> = tags.into_iter().filter_map(|t| key(&t).map(|k| (k, t))).collect();
    v.sort();
    v.dedup();
    v.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn versioned_orders_numerically_after_every_stamp() {
        let got = sort_release_tags(s(&["spira-0.1.10", "spira-release-spira-20261001T000000Z", "spira-0.1.9", "spira-release-spira-20260901T000000Z-2", "spira-0.1.104"]));
        assert_eq!(got, s(&["spira-release-spira-20260901T000000Z-2", "spira-release-spira-20261001T000000Z", "spira-0.1.9", "spira-0.1.10", "spira-0.1.104"]));
    }

    #[test]
    fn non_release_tags_are_dropped() {
        assert!(!is_release_tag("spira-release-foo"));
        assert!(!is_release_tag("spira-0.1"));
        assert!(!is_release_tag("spira-0.1.2.3"));
        assert!(!is_release_tag("v0.1.2"));
        assert!(is_release_tag("spira-0.1.2"));
        assert!(is_release_tag("spira-release-spira-20261001T000000Z"));
    }
}
