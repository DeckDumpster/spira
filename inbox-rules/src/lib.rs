//! The drop and dedup rules for the Concierge inbox. One implementation, two readers
//! (`inbox-triage`, `watchd next`): two copies of "what is noise" is how they come to disagree.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Strips the leading UTC timestamp token: up to and including the first space.
pub fn strip_timestamp(line: &str) -> &str {
    match line.find(' ') {
        Some(i) => &line[i + 1..],
        None => "",
    }
}

/// Echoes of the concierge's own actions and purely informational events.
pub fn should_drop(body: &str) -> bool {
    body.contains("ROUND RESULT") || body.contains(" OPENED ") || body.contains("pool: NEW CERTIFIED")
}

fn key_res() -> &'static (regex::Regex, regex::Regex) {
    static RE: OnceLock<(regex::Regex, regex::Regex)> = OnceLock::new();
    RE.get_or_init(|| (regex::Regex::new(r"\d{2}:\d{2}:\d{2}Z?").unwrap(), regex::Regex::new(r"oldest \d+s").unwrap()))
}

/// Strips any `HH:MM:SS(Z?)` and normalises `oldest <n>s`, so the same standing condition
/// with a different elapsed time still dedupes.
pub fn dedup_key(body: &str) -> String {
    let (time_re, oldest_re) = key_res();
    let step1 = time_re.replace_all(body, "");
    oldest_re.replace_all(&step1, "oldest Ns").into_owned()
}

/// Dedup memory: key -> epoch it was last passed.
#[derive(Default)]
pub struct Dedup {
    seen: HashMap<String, u64>,
}

impl Dedup {
    /// Parses `key<TAB>epoch` lines, discarding entries older than `window` and anything malformed.
    pub fn parse(text: &str, now: u64, window: u64) -> Dedup {
        let mut seen = HashMap::new();
        for l in text.lines() {
            let Some((k, t)) = l.rsplit_once('\t') else { continue };
            let Ok(t) = t.parse::<u64>() else { continue };
            if now.saturating_sub(t) < window {
                seen.insert(k.to_string(), t);
            }
        }
        Dedup { seen }
    }

    pub fn render(&self) -> String {
        let mut keys: Vec<_> = self.seen.iter().collect();
        keys.sort();
        keys.iter().map(|(k, t)| format!("{k}\t{t}\n")).collect()
    }

    /// The body to deliver for a raw log line, or `None` when it is dropped or a repeat.
    pub fn pass<'a>(&mut self, line: &'a str, now: u64, window: u64) -> Option<&'a str> {
        let body = strip_timestamp(line);
        if should_drop(body) {
            return None;
        }
        let key = dedup_key(body);
        if let Some(&t) = self.seen.get(&key) {
            if now.saturating_sub(t) < window {
                return None;
            }
        }
        self.seen.insert(key, now);
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_leading_timestamp_token() {
        assert_eq!(strip_timestamp("2026-09-30T00:00:00Z the rest of it"), "the rest of it");
        assert_eq!(strip_timestamp("no-space-at-all"), "");
    }

    #[test]
    fn drops_the_three_known_noise_patterns() {
        assert!(should_drop("pool: ROUND RESULT green"));
        assert!(should_drop("[watch:pr-notify] sp-abc12 OPENED against main"));
        assert!(should_drop("pool: NEW CERTIFIED sp-abc12"));
        assert!(!should_drop("[watch:round-duty] asks: NEW ASK sp-f63uj: something"));
    }

    #[test]
    fn dedup_key_strips_times_and_normalises_oldest() {
        let a = dedup_key("[watch:dolt] oldest 42s waiting on the store, seen at 11:34:05Z");
        let b = dedup_key("[watch:dolt] oldest 900s waiting on the store, seen at 11:40:12Z");
        assert_eq!(a, b);
    }

    #[test]
    fn dedup_key_leaves_unrelated_text_alone() {
        assert_eq!(dedup_key("plain text with no times in it"), "plain text with no times in it");
    }

    #[test]
    fn a_repeat_inside_the_window_is_dropped_and_after_it_is_passed() {
        let mut d = Dedup::default();
        assert_eq!(d.pass("T x needs you", 100, 600), Some("x needs you"));
        assert_eq!(d.pass("T x needs you", 400, 600), None);
        assert_eq!(d.pass("T x needs you", 701, 600), Some("x needs you"));
    }

    #[test]
    fn memory_survives_a_render_parse_round_trip_and_expires() {
        let mut d = Dedup::default();
        d.pass("T x needs you", 100, 600);
        let text = d.render();
        let mut again = Dedup::parse(&text, 200, 600);
        assert_eq!(again.pass("T x needs you", 200, 600), None);
        let mut expired = Dedup::parse(&text, 800, 600);
        assert_eq!(expired.pass("T x needs you", 800, 600), Some("x needs you"));
    }
}
