//! The drop and dedup rules for the Concierge inbox. One implementation, two readers
//! (`inbox-triage`, `watchd next`): two copies of "what is noise" is how they come to disagree.
//!
//! And the one write leg, [`record`], for every writer of the inbox (`inbox-triage append`,
//! spira-runtime's `World::inbox_append`, the scheduler watchdog's page): a line that matches the
//! push pattern is spooled and handed to `inbox-triage flush`. It needs no store and no
//! scheduler, so a writer that runs while the scheduler is down can call it (sp-nasmjd).
//! It also publishes what is unread as `concierge/inbox`, which a broker that is down only logs.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use ports::{SeededRng, WallClock};
use spira_pub::{Options, PubError, Publisher};

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
        Ok(p) => drop(p.publish("concierge/inbox", &serde_json::json!({ "unread": count, "oldest": oldest }))),
        Err(e) => eprintln!("concierge/inbox not published: {e}"),
    }
}

/// Records `line` in `inbox`; when it [`pushes`] under `wake` and `pattern`, also spools it and
/// starts `<flusher> flush` (the `inbox-triage` program, resolved only for a line that pushes),
/// detached, which wakes the session once per burst. Then publishes `concierge/inbox`, counted
/// from the position in `cursor`, through `publisher` (named for the caller). Returns whether
/// it pushed. A bad pattern disables the push and says so on stderr; a spool that cannot be
/// written or a flusher that cannot be named is an error, after the line is recorded.
pub fn record(
    inbox: &Path,
    line: &str,
    wake: &str,
    pattern: &str,
    flusher: impl Fn() -> Result<PathBuf, String>,
    cursor: &Path,
    publisher: impl FnOnce() -> Result<Publisher, PubError>,
) -> Result<bool, String> {
    record_lines(inbox, &[line], wake, pattern, flusher, cursor, publisher)
}

/// [`record`] for a batch: every line is recorded, then `concierge/inbox` is published once, so
/// a slow broker costs the caller one wait however many lines it holds.
pub fn record_lines(
    inbox: &Path,
    lines: &[&str],
    wake: &str,
    pattern: &str,
    flusher: impl Fn() -> Result<PathBuf, String>,
    cursor: &Path,
    publisher: impl FnOnce() -> Result<Publisher, PubError>,
) -> Result<bool, String> {
    if lines.is_empty() {
        return Ok(false);
    }
    let pushed = lines.iter().try_fold(false, |pushed, line| append_and_push(inbox, line, wake, pattern, &flusher).map(|p| p || pushed));
    // Even after a failure: what did reach the inbox is unread.
    announce(inbox, cursor, publisher);
    pushed
}

fn append_and_push(inbox: &Path, line: &str, wake: &str, pattern: &str, flusher: &impl Fn() -> Result<PathBuf, String>) -> Result<bool, String> {
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

/// One writer of the inbox: where it writes, how it pushes, and the name it publishes as.
#[derive(Debug, Clone)]
pub struct Sink {
    pub inbox: PathBuf,
    pub cursor: PathBuf,
    pub bus: Bus,
    pub wake: String,
    pub pattern: String,
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
            wake: cfg("SPIRA_CONCIERGE_PUSH_WAKE")?,
            pattern: cfg("SPIRA_CONCIERGE_PUSH_PATTERN")?,
            name: name.to_string(),
        })
    }

    /// [`record`] of one finished line.
    pub fn record(&self, line: &str) -> Result<bool, String> {
        self.record_lines(&[line])
    }

    /// [`record_lines`] of finished lines.
    pub fn record_lines(&self, lines: &[&str]) -> Result<bool, String> {
        let flusher = || spira_config::release_env::tool("inbox-triage").map(PathBuf::from);
        record_lines(&self.inbox, lines, &self.wake, &self.pattern, flusher, &self.cursor, || self.bus.publisher(&self.name))
    }
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

    fn due_after(f: &mut Flush, sizes: impl IntoIterator<Item = u64>) -> Vec<bool> {
        sizes.into_iter().map(|s| f.tick(s)).collect()
    }

    #[test_id(tbmw1xt)]
    #[test]
    fn a_line_pushes_only_with_a_wake_and_a_matching_pattern() {
        assert_eq!(pushes("x P0 down", "wake", r"\bP0\b"), Ok(true), "positive control");
        assert_eq!(pushes("x P01 down", "wake", r"\bP0\b"), Ok(false));
        assert_eq!(pushes("x P0 down", "", r"\bP0\b"), Ok(false), "no wake, no push");
        assert!(pushes("x P0 down", "wake", "(").is_err(), "a bad pattern is named, not a silent no");
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

    #[test_id(tciwwma)]
    #[test]
    fn record_spools_only_a_line_that_pushes() {
        let d = testkit::TempDir::new("inbox-record");
        let inbox = d.join("inbox.log");
        let none = || Ok(PathBuf::from("nonexistent/inbox-triage"));
        let quiet = || Err(PubError::Config("no broker in a unit test".into()));
        assert_eq!(record(&inbox, "t plain line", "wake", r"\bP0\b", none, &d.join("no.cursor"), quiet), Ok(false));
        assert!(!d.join("inbox.log.push").exists());
        assert_eq!(record(&inbox, "t P0 down", "wake", "(", none, &d.join("no.cursor"), quiet), Ok(false), "a bad pattern records and does not push");
        assert_eq!(record(&inbox, "t P0 down", "wake", r"\bP0\b", none, &d.join("no.cursor"), quiet), Ok(true));
        assert_eq!(std::fs::read_to_string(d.join("inbox.log.push")).unwrap(), "t P0 down\n");
        assert!(record(&inbox, "t P0 again", "wake", r"\bP0\b", || Err("no release".into()), &d.join("no.cursor"), quiet).unwrap_err().contains("no release"), "an unnamed flusher is said");
        assert_eq!(std::fs::read_to_string(&inbox).unwrap().lines().count(), 4, "every line is recorded first");
    }

    #[test_id(t55qpc9)]
    #[test]
    fn a_non_urgent_line_is_not_urgent() {
        let pat = regex::Regex::new("URGENT-[0-9]+").unwrap();
        assert!(is_urgent("URGENT-3 burst", &pat), "positive control");
        assert!(!is_urgent("plain event, not urgent", &pat));
    }

    #[test_id(txsp0lu)]
    #[test]
    fn one_urgent_line_then_quiet_ticks_is_due_once() {
        let mut f = Flush::new(30, 2);
        assert_eq!(due_after(&mut f, [10, 10, 10]), [false, false, true]);
        let mut again = Flush::new(30, 2);
        assert_eq!(due_after(&mut again, [10, 10, 10]).iter().filter(|d| **d).count(), 1);
    }

    #[test_id(tzs2av5)]
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

    #[test_id(tirj47p)]
    #[test]
    fn a_spool_that_keeps_growing_is_due_at_the_window() {
        let mut f = Flush::new(5, 2);
        assert_eq!(due_after(&mut f, [1, 2, 3, 4, 5]), [false, false, false, false, true]);
    }

    #[test_id(topczwl)]
    #[test]
    fn a_line_carries_an_iso_z_prefix_and_the_text() {
        assert_eq!(line("2026-11-06T01:02:03Z", "hello world"), "2026-11-06T01:02:03Z hello world");
    }
}
