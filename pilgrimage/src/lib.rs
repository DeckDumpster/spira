//! The pilgrimage workflow: the recorded machine, its file store, the OTLP points each move
//! pushes, and the clean-close hand-off to the Concierge.

pub mod machine;
pub mod store;

use std::io::Write;
use std::process::{Command, Stdio};

use machine::{Event, Pilgrimage};
use spira_otel::metrics::{MetricsSink, Point};
pub use store::FileStore;

pub const TRANSITIONS: &str = "spira_pilgrimage_transitions";
pub const STARTED: &str = "spira_pilgrimage_started";
pub const CLOSED: &str = "spira_pilgrimage_closed";
pub const DEFECTS_FILED: &str = "spira_pilgrimage_defects_filed";
pub const WALKS: &str = "spira_pilgrimage_walks";
pub const DEFECTS: &str = "spira_pilgrimage_defects";
pub const CLEAN_SECONDS: &str = "spira_pilgrimage_clean_seconds";

/// Where a clean pilgrimage's Concierge-walk ask goes.
pub trait Concierge {
    /// Files the ask; returns what names it.
    fn ask(&self, id: &str, subject: &str, body: &str) -> Result<String, String>;
}

pub struct Mail;

impl Concierge for Mail {
    fn ask(&self, id: &str, subject: &str, body: &str) -> Result<String, String> {
        let default = "Walk the same user path on the concierge tenant; if it is clean, tell the operator the feature is ready to use.";
        let mut child = Command::new("timeout")
            .args(["5", spira_config::release_env::tool("mail").as_str(), "send", "concierge", "--from", "Pilgrimage <pilgrimage@spira>", "--subject", subject, "--kind", "question", "--default", default, "--bead", id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("mail send concierge: {e}"))?;
        let text = format!("## Question\n{body}\n\n## Default\n{default}\n");
        child.stdin.take().ok_or("mail send concierge: no stdin")?.write_all(text.as_bytes()).map_err(|e| format!("mail send concierge: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("mail send concierge: {e}"))?;
        if !out.status.success() {
            return Err(format!("mail send concierge: exit {}: {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()));
        }
        let named = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
        Ok(if named.is_empty() { "mail:concierge".into() } else { named })
    }
}

/// Records `event` and pushes its points. A refusal writes nothing and pushes nothing.
pub fn record(store: &FileStore, sink: &MetricsSink, id: &str, event: Event, now: u64) -> Result<Pilgrimage, String> {
    let p = store.record(id, event.clone(), now)?;
    push(sink, &p, &event);
    Ok(p)
}

fn push(sink: &MetricsSink, p: &Pilgrimage, event: &Event) {
    let state = event.to().name();
    let labels = [("pilgrimage", p.id.as_str()), ("event", event.kind()), ("state", state)];
    let mut points = vec![
        Point::Count { name: TRANSITIONS, value: 1 },
        Point::Gauge { name: WALKS, value: f64::from(p.walks()) },
        Point::Gauge { name: DEFECTS, value: f64::from(p.defects()) },
    ];
    match event {
        Event::WalkStarted { n: 1, .. } => points.push(Point::Count { name: STARTED, value: 1 }),
        Event::DefectsFiled { count } => points.push(Point::Count { name: DEFECTS_FILED, value: u64::from(*count) }),
        Event::Clean => {
            points.push(Point::Count { name: CLOSED, value: 1 });
            if let (Some(landed), Some(clean)) = (p.landed_at(), p.clean_at()) {
                points.push(Point::Seconds { name: CLEAN_SECONDS, value: clean.saturating_sub(landed) as f64 });
            }
        }
        _ => {}
    }
    sink.push(&points, &labels);
}

/// Files the Concierge-walk ask for a clean pilgrimage and records it. Safe to repeat: a
/// pilgrimage already past `clean` is refused, so no second ask is filed.
pub fn hand_off(store: &FileStore, sink: &MetricsSink, concierge: &dyn Concierge, id: &str, now: u64) -> Result<String, String> {
    let p = store.load(id).ok_or_else(|| format!("{id} has no recorded events"))?;
    if let Some(e) = p.refusal(&Event::ConciergeWalkAsked { ask: String::new() }) {
        return Err(e);
    }
    let subject = format!("Concierge walk: {id} walked clean by the pilgrim — walk it last, then tell the operator");
    let body = format!(
        "Pilgrimage {id} closed clean after {} walk(s) and {} defect(s) filed. Walk the same user path from the pilgrimage bead on your own tenant; the pilgrim never touches the operator's.",
        p.walks(),
        p.defects()
    );
    let ask = concierge.ask(id, &subject, &body)?;
    record(store, sink, id, Event::ConciergeWalkAsked { ask: ask.clone() }, now)?;
    Ok(ask)
}

/// A clean walk: records `clean`, then hands off. If the ask cannot be filed the pilgrimage
/// stays `clean` and the error says so; `hand_off` retries it.
pub fn close_clean(store: &FileStore, sink: &MetricsSink, concierge: &dyn Concierge, id: &str, now: u64) -> Result<String, String> {
    record(store, sink, id, Event::Clean, now)?;
    hand_off(store, sink, concierge, id, now).map_err(|e| format!("{id} is clean but the Concierge-walk ask was not filed (retry with `pilgrimage handoff {id}`): {e}"))
}

#[cfg(test)]
mod tests;
