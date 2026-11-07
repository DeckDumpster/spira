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

/// The inbox line for an event: `<ISO-8601 Z> <text>`.
pub fn line(now_utc: &str, text: &str) -> String {
    format!("{now_utc} {text}")
}

/// Whether an event also wakes the session, not only lands in the inbox.
pub fn is_urgent(text: &str, pattern: &regex::Regex) -> bool {
    pattern.is_match(text)
}

/// When a spooled burst is due: the spool has stopped growing for `idle_ticks`, or
/// `window_ticks` have passed. Time is the number of `tick` calls; the caller sleeps.
pub struct Flush {
    window_ticks: u32,
    idle_ticks: u32,
    waited: u32,
    quiet: u32,
    last: Option<u64>,
}

impl Flush {
    pub fn new(window_ticks: u32, idle_ticks: u32) -> Flush {
        Flush { window_ticks, idle_ticks, waited: 0, quiet: 0, last: None }
    }

    /// Observe the spool's size once per tick; true when the batch is due.
    pub fn tick(&mut self, spool_size: u64) -> bool {
        self.waited += 1;
        if self.last == Some(spool_size) {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        self.last = Some(spool_size);
        self.waited >= self.window_ticks || self.quiet >= self.idle_ticks
    }
}

/// The one wake a batch of spooled lines becomes.
pub fn wake_text(lines: &[String]) -> String {
    let joined: String = lines.iter().map(|l| format!("{l};")).collect();
    format!("[inbox-push] {} urgent event(s): {joined}", lines.len())
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

    fn due_after(f: &mut Flush, sizes: impl IntoIterator<Item = u64>) -> Vec<bool> {
        sizes.into_iter().map(|s| f.tick(s)).collect()
    }

    #[test]
    fn a_non_urgent_line_is_not_urgent() {
        let pat = regex::Regex::new("URGENT-[0-9]+").unwrap();
        assert!(is_urgent("URGENT-3 burst", &pat), "positive control");
        assert!(!is_urgent("plain event, not urgent", &pat));
    }

    #[test]
    fn one_urgent_line_then_quiet_ticks_is_due_once() {
        let mut f = Flush::new(30, 2);
        assert_eq!(due_after(&mut f, [10, 10, 10]), [false, false, true]);
        let mut again = Flush::new(30, 2);
        assert_eq!(due_after(&mut again, [10, 10, 10]).iter().filter(|d| **d).count(), 1);
    }

    #[test]
    fn ten_appends_one_tick_apart_inside_the_window_are_one_batch() {
        let mut f = Flush::new(30, 2);
        let mut lines = Vec::new();
        let mut size = 0;
        for k in 1..=10 {
            lines.push(format!("T URGENT-{k} burst"));
            size += 20;
            assert!(!f.tick(size), "still growing at append {k}");
            assert!(!f.tick(size), "one quiet tick after append {k} is inside the idle gap");
        }
        assert!(f.tick(size), "due once the spool is quiet");
        let text = wake_text(&lines);
        assert!(text.starts_with("[inbox-push] 10 urgent event(s): "), "{text}");
        for k in [1, 5, 10] {
            assert!(text.contains(&format!("URGENT-{k} burst")), "{text}");
        }
    }

    #[test]
    fn a_spool_that_keeps_growing_is_due_at_the_window() {
        let mut f = Flush::new(5, 2);
        assert_eq!(due_after(&mut f, [1, 2, 3, 4, 5]), [false, false, false, false, true]);
    }

    #[test]
    fn a_line_carries_an_iso_z_prefix_and_the_text() {
        assert_eq!(line("2026-11-06T01:02:03Z", "hello world"), "2026-11-06T01:02:03Z hello world");
    }
}
