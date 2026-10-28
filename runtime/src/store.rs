//! The journal in the lifecycle store: the `runtime_*` tables of `spira_lifecycle`
//! (migrations/0030-runtime-journal.sql). The SQL reaches the server through the `Sql` port, so
//! a test supplies the port over a throwaway database; production reaches the store through `socket::SocketJournal`.

use std::collections::BTreeMap;

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::{Result, RuntimeError};

pub type Row = BTreeMap<String, String>;

pub trait Sql: Send + Sync {
    fn execute(&self, sql: &str) -> Result<()>;
    fn query(&self, sql: &str) -> Result<Vec<Row>>;
}

/// A SQL string literal. NUL cannot be quoted safely and is refused by the caller's data check.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        match c {
            '\'' => o.push_str("''"),
            '\\' => o.push_str("\\\\"),
            '\0' => o.push_str("\\0"),
            c => o.push(c),
        }
    }
    o.push('\'');
    o
}

fn opt(s: &Option<EventId>) -> String {
    s.as_ref().map_or("NULL".into(), |i| quote(&i.0))
}

pub struct StoreJournal<S: Sql> {
    sql: S,
}

impl<S: Sql> StoreJournal<S> {
    pub fn new(sql: S) -> Self {
        StoreJournal { sql }
    }

    /// Empties every `runtime_*` table: a throwaway database between conformance scenarios.
    pub fn reset(&self) -> Result<()> {
        for t in ["runtime_event", "runtime_applied", "runtime_cursor", "runtime_schedule", "runtime_poison"] {
            self.sql.execute(&format!("DELETE FROM {t};"))?;
        }
        Ok(())
    }

    fn num(r: &Row, k: &str) -> Result<u64> {
        r.get(k).and_then(|v| v.parse().ok()).ok_or_else(|| RuntimeError::new(format!("store row has no number {k}")))
    }

    fn text(r: &Row, k: &str) -> Result<String> {
        r.get(k).cloned().ok_or_else(|| RuntimeError::new(format!("store row has no {k}")))
    }
}

impl<S: Sql> Journal for StoreJournal<S> {
    fn append(&self, ev: &Event) -> Result<Offset> {
        let id = quote(&ev.id.0);
        self.sql.execute(&format!(
            "INSERT IGNORE INTO runtime_event (event_id, stream, kind_tag, kind_body, at, cause, correlation) VALUES ({id}, {}, {}, {}, {}, {}, {});",
            quote(&ev.stream.0),
            quote(&ev.kind.tag),
            quote(&ev.kind.body),
            ev.at,
            opt(&ev.cause),
            quote(&ev.correlation.0)
        ))?;
        let rows = self.sql.query(&format!("SELECT seq FROM runtime_event WHERE event_id = {id};"))?;
        rows.first().map_or_else(|| Err(RuntimeError::new("appended event not found")), |r| Self::num(r, "seq"))
    }

    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        let rows = self.sql.query(&format!(
            "SELECT seq, event_id, stream, kind_tag, kind_body, at, cause, correlation FROM runtime_event WHERE seq >= {from} ORDER BY seq LIMIT {limit};"
        ))?;
        rows.iter()
            .map(|r| {
                Ok((
                    Self::num(r, "seq")?,
                    Event {
                        id: EventId(Self::text(r, "event_id")?),
                        stream: StreamKey(Self::text(r, "stream")?),
                        kind: EventKind { tag: Self::text(r, "kind_tag")?, body: r.get("kind_body").cloned().unwrap_or_default() },
                        at: Self::num(r, "at")?,
                        cause: r.get("cause").map(|c| EventId(c.clone())),
                        correlation: CorrelationId(Self::text(r, "correlation")?),
                    },
                ))
            })
            .collect()
    }

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        let rows = self.sql.query(&format!("SELECT 1 AS hit FROM runtime_applied WHERE handler = {} AND event_id = {};", quote(handler), quote(&id.0)))?;
        Ok(!rows.is_empty())
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.sql.execute(&format!("INSERT IGNORE INTO runtime_applied (handler, event_id) VALUES ({}, {});", quote(handler), quote(&id.0)))
    }

    fn cursor(&self) -> Result<Offset> {
        let rows = self.sql.query("SELECT next FROM runtime_cursor WHERE id = 1;")?;
        rows.first().map_or(Ok(0), |r| Self::num(r, "next"))
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.sql.execute(&format!("INSERT INTO runtime_cursor (id, next) VALUES (1, {next}) ON DUPLICATE KEY UPDATE next = {next};"))
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.sql.execute(&format!(
            "INSERT IGNORE INTO runtime_schedule (row_id, at, stream, kind_tag, kind_body, cause, correlation, fired) VALUES ({}, {}, {}, {}, {}, {}, {}, 0);",
            quote(&row.id),
            row.at,
            quote(&row.stream.0),
            quote(&row.kind.tag),
            quote(&row.kind.body),
            opt(&row.cause),
            quote(&row.correlation.0)
        ))
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        let rows = self.sql.query("SELECT row_id, at, stream, kind_tag, kind_body, cause, correlation FROM runtime_schedule WHERE fired = 0 ORDER BY row_id;")?;
        rows.iter()
            .map(|r| {
                Ok(ScheduleRow {
                    id: Self::text(r, "row_id")?,
                    at: Self::num(r, "at")?,
                    stream: StreamKey(Self::text(r, "stream")?),
                    kind: EventKind { tag: Self::text(r, "kind_tag")?, body: r.get("kind_body").cloned().unwrap_or_default() },
                    cause: r.get("cause").map(|c| EventId(c.clone())),
                    correlation: CorrelationId(Self::text(r, "correlation")?),
                })
            })
            .collect()
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.sql.execute(&format!("UPDATE runtime_schedule SET fired = 1 WHERE row_id = {};", quote(row_id)))
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.sql.execute(&format!(
            "INSERT IGNORE INTO runtime_poison (stream, event_id, handler, message) VALUES ({}, {}, {}, {});",
            quote(&p.stream.0),
            quote(&p.event.0),
            quote(&p.handler),
            quote(&p.message)
        ))
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        let rows = self.sql.query(&format!("SELECT stream, event_id, handler, message FROM runtime_poison WHERE stream = {};", quote(&stream.0)))?;
        rows.first()
            .map(|r| Ok(Poison { stream: stream.clone(), event: EventId(Self::text(r, "event_id")?), handler: Self::text(r, "handler")?, message: Self::text(r, "message")? }))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_what_ends_a_literal() {
        assert_eq!(quote("a'b\\c"), "'a''b\\\\c'");
        assert_eq!(quote("x\0y"), "'x\\0y'");
    }
}
