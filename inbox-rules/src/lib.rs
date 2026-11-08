//! The drop and dedup rules for the Concierge inbox. One implementation, two readers
//! (`inbox-triage`, `watchd next`): two copies of "what is noise" is how they come to disagree.
//!
//! And the one write leg, [`record`], for every writer of the inbox (`inbox-triage append`,
//! spira-runtime's `World::inbox_append`, the scheduler watchdog's page): a line that matches the
//! push pattern is spooled and handed to `inbox-triage flush`. It needs no store and no
//! scheduler, so a writer that runs while the scheduler is down can call it (sp-nasmjd).

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// The push decision every writer of the inbox applies (sp-nasmjd): a line wakes the session
/// when a wake command is configured (`SPIRA_CONCIERGE_PUSH_WAKE`) and the line matches
/// `SPIRA_CONCIERGE_PUSH_PATTERN`. A pattern that will not compile is an error naming the key.
pub fn pushes(line: &str, wake: &str, pattern: &str) -> Result<bool, String> {
    if wake.is_empty() {
        return Ok(false);
    }
    let re = regex::Regex::new(pattern).map_err(|e| format!("bad SPIRA_CONCIERGE_PUSH_PATTERN: {e}"))?;
    Ok(is_urgent(line, &re))
}

/// Appends one line to `path`, creating the file and its directory.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// The push spool beside an inbox: `<inbox>.push`, drained by `inbox-triage flush`.
pub fn spool_path(inbox: &Path) -> PathBuf {
    PathBuf::from(format!("{}.push", inbox.display()))
}

/// Records `line` in `inbox`; when it [`pushes`] under `wake` and `pattern`, also spools it and
/// starts `<flusher> flush` (the `inbox-triage` program, resolved only for a line that pushes),
/// detached, which wakes the session once per burst. Returns whether it pushed. A bad pattern
/// disables the push and says so on stderr; a spool that cannot be written or a flusher that
/// cannot be named is an error, after the line is recorded.
pub fn record(inbox: &Path, line: &str, wake: &str, pattern: &str, flusher: impl FnOnce() -> Result<PathBuf, String>) -> Result<bool, String> {
    append_line(inbox, line).map_err(|e| format!("cannot append to {}: {e}", inbox.display()))?;
    match pushes(line, wake, pattern) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(e) => {
            eprintln!("inbox push disabled: {e}");
            return Ok(false);
        }
    }
    let spool = spool_path(inbox);
    append_line(&spool, line).map_err(|e| format!("cannot spool to {}: {e}", spool.display()))?;
    let flusher = flusher().map_err(|e| format!("pushed line spooled, but no flusher to deliver it: {e}"))?;
    // batch-job: spawned detached and never waited on; the flusher outlives the writer
    let _ = Command::new(flusher).arg("flush").process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    Ok(true)
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
    fn a_line_pushes_only_with_a_wake_and_a_matching_pattern() {
        assert_eq!(pushes("x P0 down", "wake", r"\bP0\b"), Ok(true), "positive control");
        assert_eq!(pushes("x P01 down", "wake", r"\bP0\b"), Ok(false));
        assert_eq!(pushes("x P0 down", "", r"\bP0\b"), Ok(false), "no wake, no push");
        assert!(pushes("x P0 down", "wake", "(").is_err(), "a bad pattern is named, not a silent no");
    }

    #[test]
    fn append_creates_the_inbox_and_a_second_call_does_not_truncate() {
        let d = testkit::TempDir::new("inbox-append");
        let inbox = d.join("elsewhere/inbox.log");
        assert!(!inbox.exists(), "the inbox does not exist before the first append");
        append_line(&inbox, "2026-11-06T00:00:00Z hello world").unwrap();
        assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "2026-11-06T00:00:00Z hello world\n");
        append_line(&inbox, "2026-11-06T00:00:01Z second event").unwrap();
        assert_eq!(std::fs::read_to_string(&inbox).unwrap().lines().count(), 2);
    }

    #[test]
    fn record_spools_only_a_line_that_pushes() {
        let d = testkit::TempDir::new("inbox-record");
        let inbox = d.join("inbox.log");
        let none = || Ok(PathBuf::from("/nonexistent/inbox-triage"));
        assert_eq!(record(&inbox, "t plain line", "wake", r"\bP0\b", none), Ok(false));
        assert!(!d.join("inbox.log.push").exists());
        assert_eq!(record(&inbox, "t P0 down", "wake", "(", none), Ok(false), "a bad pattern records and does not push");
        assert_eq!(record(&inbox, "t P0 down", "wake", r"\bP0\b", none), Ok(true));
        assert_eq!(std::fs::read_to_string(d.join("inbox.log.push")).unwrap(), "t P0 down\n");
        assert!(record(&inbox, "t P0 again", "wake", r"\bP0\b", || Err("no release".into())).unwrap_err().contains("no release"), "an unnamed flusher is said");
        assert_eq!(std::fs::read_to_string(&inbox).unwrap().lines().count(), 4, "every line is recorded first");
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
