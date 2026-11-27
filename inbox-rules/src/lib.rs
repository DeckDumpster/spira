//! The drop and dedup rules for the Concierge inbox, shared by `inbox-triage`.
//!
//! And the one write leg, [`record`], for every writer of the inbox (`inbox-triage append`,
//! spira-runtime's `World::inbox_append`, the scheduler watchdog's page). It needs no store and no
//! scheduler, so a writer that runs while the scheduler is down can call it (sp-nasmjd).
//! It also publishes what is unread as `concierge/inbox`, which a broker that is down only logs.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ports::{SeededRng, WallClock};
use spira_pub::{Class, Options, PubError, Publisher};

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

/// Appends one line to `path`, creating the file and its directory.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// The most of the oldest unread line `concierge/inbox` carries.
const OLDEST_CHARS: usize = 500;

/// Where watchd keeps the count of lines its row `name` has been read.
pub fn cursorfile(run: &str, name: &str) -> PathBuf {
    Path::new(run).join("watchd").join(format!("{name}.cursor"))
}

/// The cursor of the Concierge inbox's reader.
pub fn concierge_cursor(run: &str) -> PathBuf {
    cursorfile(run, "concierge-inbox")
}

/// The finished lines in a log: an unterminated last one is still being written.
pub fn count_lines(log: &[u8]) -> u64 {
    log.iter().filter(|b| **b == b'\n').count() as u64
}

/// The position the cursor file holds in a log of `total` lines.
pub fn read_pos(cursor: &Path, total: u64) -> u64 {
    clamp_pos(std::fs::read_to_string(cursor).ok().and_then(|c| parse_cursor(&c)), total)
}

/// A cursor file's first token; empty or non-numeric is no cursor.
pub fn parse_cursor(content: &str) -> Option<i64> {
    content.split_whitespace().next()?.parse::<i64>().ok()
}

/// A raw cursor held to `[0, total]`: a missing or negative one is the start, one past the end
/// (rotation, truncation, a hand edit) the end.
pub fn clamp_pos(raw: Option<i64>, total: u64) -> u64 {
    raw.map_or(0, |p| u64::try_from(p).map_or(0, |p| p.min(total)))
}

/// How many lines of `inbox` lie past the position `cursor` holds, and the oldest of them.
pub fn unread(inbox: &Path, cursor: &Path) -> std::io::Result<(u64, Option<String>)> {
    let bytes = std::fs::read(inbox)?;
    let total = count_lines(&bytes);
    let read = read_pos(cursor, total);
    let mut finished = bytes.split_inclusive(|b| *b == b'\n').filter(|l| l.ends_with(b"\n"));
    let oldest = finished.nth(read as usize).map(|l| String::from_utf8_lossy(l).trim_end().chars().take(OLDEST_CHARS).collect());
    Ok((total - read, oldest))
}

/// Which broker a writer announces the inbox on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Bus {
    /// The one the configuration names.
    #[default]
    Configured,
    /// The one on this socket.
    At(PathBuf),
    /// None: the writer only appends.
    Off,
}

impl Bus {
    /// A publisher called `name`.
    pub fn publisher(&self, name: &str) -> Result<Publisher, PubError> {
        match self {
            Bus::Configured => Publisher::new(name),
            Bus::At(socket) => Publisher::with(name, Options::new(socket, Arc::new(WallClock), Box::new(SeededRng::new(u64::from(std::process::id()))))),
            Bus::Off => Err(PubError::Config("this writer announces on no broker".into())),
        }
    }
}

/// Publishes `concierge/inbox`. Every failure is logged here or by the publisher, and none is
/// the writer's: the line is already on disk.
fn announce(inbox: &Path, cursor: &Path, publisher: impl FnOnce() -> Result<Publisher, PubError>) {
    let (count, oldest) = match unread(inbox, cursor) {
        Ok(found) => found,
        Err(e) => return eprintln!("concierge/inbox not published: cannot read {}: {e}", inbox.display()),
    };
    match publisher() {
        // A failed publish is logged by the publisher; dropping it flushes, and logs that.
        Ok(p) => drop(p.publish(Class::State, "concierge/inbox", &serde_json::json!({ "unread": count, "oldest": oldest }))),
        Err(e) => eprintln!("concierge/inbox not published: {e}"),
    }
}

/// Records `line` in `inbox`, then publishes `concierge/inbox`, counted from the position in
/// `cursor`, through `publisher` (named for the caller). An error means the line is not in the
/// inbox.
pub fn record(inbox: &Path, line: &str, cursor: &Path, publisher: impl FnOnce() -> Result<Publisher, PubError>) -> Result<(), String> {
    record_lines(inbox, &[line], cursor, publisher)
}

/// [`record`] for a batch: every line is recorded, then `concierge/inbox` is published once, so
/// a slow broker costs the caller one wait however many lines it holds.
pub fn record_lines(inbox: &Path, lines: &[&str], cursor: &Path, publisher: impl FnOnce() -> Result<Publisher, PubError>) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    let written = lines.iter().try_for_each(|line| append_line(inbox, line).map_err(|e| format!("cannot append to {}: {e}", inbox.display())));
    // Even after a failure: what did reach the inbox is unread.
    announce(inbox, cursor, publisher);
    written
}

/// One writer of the inbox: where it writes and the name it publishes as.
#[derive(Debug, Clone)]
pub struct Sink {
    pub inbox: PathBuf,
    pub cursor: PathBuf,
    pub bus: Bus,
    pub name: String,
}

impl Sink {
    /// The configured Concierge inbox, written as `name`. A key that is missing is an error.
    pub fn configured(name: &str) -> Result<Sink, String> {
        use spira_config::process::cfg;
        Ok(Sink {
            inbox: PathBuf::from(cfg("SPIRA_CONCIERGE_INBOX")?),
            cursor: concierge_cursor(&cfg("SPIRA_RUN")?),
            bus: Bus::Configured,
            name: name.to_string(),
        })
    }

    /// [`record`] of one finished line.
    pub fn record(&self, line: &str) -> Result<(), String> {
        self.record_lines(&[line])
    }

    /// [`record_lines`] of finished lines.
    pub fn record_lines(&self, lines: &[&str]) -> Result<(), String> {
        record_lines(&self.inbox, lines, &self.cursor, || self.bus.publisher(&self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tw8n49t)]
    #[test]
    fn strips_the_leading_timestamp_token() {
        assert_eq!(strip_timestamp("2026-09-30T00:00:00Z the rest of it"), "the rest of it");
        assert_eq!(strip_timestamp("no-space-at-all"), "");
    }

    #[test_id(th464bb)]
    #[test]
    fn drops_the_three_known_noise_patterns() {
        assert!(should_drop("pool: ROUND RESULT green"));
        assert!(should_drop("[watch:pr-notify] sp-abc12 OPENED against main"));
        assert!(should_drop("pool: NEW CERTIFIED sp-abc12"));
        assert!(!should_drop("[watch:round-duty] asks: NEW ASK sp-f63uj: something"));
    }

    #[test_id(tuv8yw4)]
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

    #[test_id(topczwl)]
    #[test]
    fn a_line_carries_an_iso_z_prefix_and_the_text() {
        assert_eq!(line("2026-11-06T01:02:03Z", "hello world"), "2026-11-06T01:02:03Z hello world");
    }
}
