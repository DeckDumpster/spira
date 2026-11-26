//! The two things a person reads: a delivery line and the `list` screen.

use serde_json::Value;
use spira_pub::topics;

use crate::core::Core;

/// The `list` screen's width: a phone held upright.
const WIDTH: usize = 40;

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

/// `k=v` per field, the family's declared fields first; a null field is left out.
fn fields(topic: &str, value: &Value) -> String {
    let Value::Object(map) = value else { return word(value) };
    let declared: &[String] = topics::family_of(topic).map_or(&[], |f| &f.fields);
    let first = declared.iter().filter_map(|k| map.get_key_value(k));
    let rest = map.iter().filter(|(k, _)| !declared.contains(k));
    first.chain(rest).filter(|(_, v)| !v.is_null()).map(|(k, v)| format!("{k}={}", word(v))).collect::<Vec<_>>().join(" ")
}

fn tagged(id: &str, what: &str, note: &str) -> String {
    let note = if note.is_empty() { String::new() } else { format!(" — {note}") };
    flat(&format!("[sub:{id}] {what}{note}"))
}

pub(crate) fn line(id: &str, topic: &str, value: &Value, note: &str) -> String {
    let fields = fields(topic, value);
    let what = if fields.is_empty() { topic.to_string() } else { format!("{topic} {fields}") };
    tagged(id, &what, note)
}

fn instant(ms: u64) -> String {
    strand::timefmt::utc_stamp(i64::try_from(ms / 1000).unwrap_or(i64::MAX))
}

pub(crate) fn at_line(id: &str, at_ms: u64, note: &str) -> String {
    tagged(id, &format!("at {}", instant(at_ms)), note)
}

fn ago(ms: u64) -> String {
    match ms / 1000 {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
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
    wrap(&mut out, "", &format!("{name}: broker {broker}, readers {readers}"));
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
        let fired = sub.last_fired_ms.map_or("never".to_string(), |t| ago(now.saturating_sub(t)));
        let once = if sub.entry.once && sub.entry.filter.is_some() { ", once" } else { "" };
        wrap(&mut out, "  ", &format!("fired {fired}{once}"));
        if !sub.entry.note.is_empty() {
            wrap(&mut out, "  ", &sub.entry.note);
        }
    }
    let filters = core.filters();
    let watched: Vec<_> = topics::families().iter().filter(|f| filters.iter().any(|x| topics::families_matching(x).any(|m| m.family == f.family))).collect();
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
