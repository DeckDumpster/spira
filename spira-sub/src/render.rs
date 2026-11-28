//! The two things a person reads: a delivery line and the `list` screen.

use serde_json::Value;
use spira_pub::topics;
use spira_world::clock::format_age;

use crate::core::Core;

/// The `list` screen's width: a phone held upright.
const WIDTH: usize = 40;

const BATCH_HEAD: &str = "[batch] n=";

/// A line is one line whatever a publisher put in it.
fn flat(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn word(value: &Value) -> String {
    match value {
        Value::String(s) if !s.is_empty() && !s.contains(|c: char| c.is_whitespace() || c.is_control() || c == '"') => s.clone(),
        other => other.to_string(),
    }
}

/// `k=v` per field in the payload's key order; a null field is left out.
fn fields(value: &Value) -> String {
    let Value::Object(map) = value else { return word(value) };
    map.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| format!("{k}={}", word(v))).collect::<Vec<_>>().join(" ")
}

fn tagged(id: &str, what: &str, note: &str) -> String {
    let note = if note.is_empty() { String::new() } else { format!(" — {note}") };
    flat(&format!("[sub:{id}] {what}{note}"))
}

pub(crate) fn line(id: &str, topic: &str, value: &Value, note: &str) -> String {
    let fields = fields(value);
    let what = if fields.is_empty() { topic.to_string() } else { format!("{topic} {fields}") };
    tagged(id, &what, note)
}

fn instant(ms: u64) -> String {
    strand::timefmt::utc_stamp(i64::try_from(ms / 1000).unwrap_or(i64::MAX))
}

pub(crate) fn at_line(id: &str, at_ms: u64, note: &str) -> String {
    tagged(id, &format!("at {}", instant(at_ms)), note)
}

pub(crate) fn batch_line(lines: usize, since_ms: u64) -> String {
    format!("{BATCH_HEAD}{lines} since {}", instant(since_ms))
}

/// How many lines follow `line`, when it heads a batch.
pub(crate) fn batch_size(line: &str) -> Option<usize> {
    line.strip_prefix(BATCH_HEAD)?.split_whitespace().next()?.parse().ok()
}

/// The interval as `--batch` takes it.
fn span(secs: u64) -> String {
    match secs {
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// `text` under `lead`, continued on indented lines, none wider than the screen.
fn wrap(out: &mut String, lead: &str, text: &str) {
    let mut line = lead.to_string();
    for c in flat(text).chars() {
        if line.chars().count() == WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = "    ".to_string();
        }
        line.push(c);
    }
    out.push_str(&line);
    out.push('\n');
}

pub(crate) fn list(core: &Core, name: &str, connected: bool, readers: usize) -> String {
    let mut out = String::new();
    let broker = if connected { "up" } else { "DOWN" };
    let undelivered = |n: usize| if n == 0 { String::new() } else { format!(", {n} undelivered") };
    let waiting = match core.unacked() {
        0 => String::new(),
        n => format!(", undelivered {n}"),
    };
    wrap(&mut out, "", &format!("{name}: broker {broker}, readers {readers}{waiting}"));
    if core.is_empty() {
        out.push_str("no subscriptions\n");
        return out;
    }
    let now = core.now_ms();
    for sub in &core.subs {
        let what = match (&sub.entry.filter, sub.entry.at_ms) {
            (Some(filter), _) => filter.clone(),
            (None, at) => format!("at {}", instant(at.unwrap_or(0))),
        };
        wrap(&mut out, "", &format!("{} {what}", sub.entry.id));
        let fired = sub.last_fired_ms.map_or("never".to_string(), |t| format!("{} ago", format_age(i64::try_from(now.saturating_sub(t) / 1000).unwrap_or(i64::MAX))));
        let once = if sub.entry.once && sub.entry.filter.is_some() { ", once" } else { "" };
        let batch = sub.entry.batch_s.map_or(String::new(), |s| format!(", batch {}", span(s)));
        let pending = if sub.pending.is_empty() { String::new() } else { format!(", {} pending", sub.pending.len()) };
        let unacked = undelivered(core.unacked_of(&sub.entry.id));
        wrap(&mut out, "  ", &format!("fired {fired}{once}{batch}{pending}{unacked}"));
        if !sub.entry.note.is_empty() {
            wrap(&mut out, "  ", &sub.entry.note);
        }
    }
    let filters = core.filters();
    let watched: Vec<_> = core.topics.iter().filter(|row| filters.iter().any(|f| topics::filter_meets_family(f, &row.family))).collect();
    if !watched.is_empty() {
        out.push_str("publishers\n");
    }
    for family in watched {
        wrap(&mut out, " ", &family.family);
        if family.resident.is_empty() {
            wrap(&mut out, "   ", "on demand");
        }
        for publisher in &family.resident {
            let state = core.publishers.get(&format!("publisher/{publisher}")).and_then(|v| v.get("state")).and_then(Value::as_str);
            wrap(&mut out, "   ", &format!("{publisher} {}", state.unwrap_or("not seen")));
        }
    }
    out
}
