//! `facts-query`'s engine: the facts as an in-memory table named `events` with bd's column names,
//! so census's aggregates over bd's own `events` run unchanged. The two MySQL functions they use
//! (`UNIX_TIMESTAMP`, `TIMESTAMPDIFF(SECOND, a, b)`) are registered by name.

use std::time::{Duration, Instant};

use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::control::facts::Fact;

/// A query that runs past this is abandoned, as the dolt statement deadline abandoned it.
const BUDGET: Duration = Duration::from_secs(25);

fn civil_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The epoch second of a `YYYY-MM-DD HH:MM:SS` text, as MySQL's `UNIX_TIMESTAMP` reads a UTC datetime.
fn epoch_of(text: &str) -> Option<i64> {
    let (date, time) = text.split_once([' ', 'T']).unwrap_or((text, "00:00:00"));
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let mut t = time.trim_end_matches('Z').split(':').map(|p| p.split('.').next().and_then(|x| x.parse::<i64>().ok()));
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (h, mi, s) = (t.next()??, t.next().flatten().unwrap_or(0), t.next().flatten().unwrap_or(0));
    Some(civil_days(y, mo, da) * 86_400 + h * 3_600 + mi * 60 + s)
}

pub fn stamp(at: i64) -> String {
    crate::callers::fmt_utc(at).replace('T', " ").trim_end_matches('Z').to_string()
}

/// `TIMESTAMPDIFF(SECOND,` is the one spelling census writes; SQLite would read `SECOND` as a column.
fn dialect(sql: &str) -> String {
    const SPELLING: &str = "timestampdiff(second,";
    let lower = sql.to_ascii_lowercase();
    let mut out = String::with_capacity(sql.len());
    let mut done = 0;
    while let Some(i) = lower[done..].find(SPELLING) {
        out.push_str(&sql[done..done + i]);
        out.push_str("tsdiff_s(");
        done += i + SPELLING.len();
    }
    out.push_str(&sql[done..]);
    out
}

fn text_of(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::String(n.to_string()),
        ValueRef::Real(f) => Value::String(f.to_string()),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
    }
}

pub fn run(facts: &[Fact], sql: &str) -> Result<Vec<Value>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    let db = Connection::open_in_memory().map_err(e)?;
    db.prepare("CREATE TABLE events (issue_id TEXT, event_type TEXT, new_value TEXT, actor TEXT, created_at TEXT)").map_err(e)?.execute([]).map_err(e)?;
    {
        let mut insert = db.prepare("INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5)").map_err(e)?;
        for f in facts {
            insert.execute((&f.bead, &f.kind, &f.cause, &f.actor, stamp(f.at))).map_err(e)?;
        }
    }
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("UNIX_TIMESTAMP", 1, flags, |c| Ok(c.get::<Option<String>>(0)?.and_then(|t| epoch_of(&t)))).map_err(e)?;
    db.create_scalar_function("tsdiff_s", 2, flags, |c| {
        let (a, b) = (c.get::<Option<String>>(0)?, c.get::<Option<String>>(1)?);
        Ok(a.and_then(|a| epoch_of(&a)).zip(b.and_then(|b| epoch_of(&b))).map(|(a, b)| b - a))
    })
    .map_err(e)?;
    let started = Instant::now();
    db.progress_handler(10_000, Some(move || started.elapsed() > BUDGET));
    let mut stmt = db.prepare(&dialect(sql)).map_err(e)?;
    let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
    let mut rows = stmt.query([]).map_err(e)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(e)? {
        let mut o = Map::new();
        for (i, n) in names.iter().enumerate() {
            o.insert(n.clone(), text_of(row.get_ref(i).map_err(e)?));
        }
        out.push(Value::Object(o));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(bead: &str, kind: &str, cause: &str, at: i64) -> Fact {
        Fact { bead: bead.into(), kind: kind.into(), cause: cause.into(), actor: "harness".into(), at }
    }

    #[test]
    fn a_burst_query_counts_events_a_minute_apart_as_one_burst() {
        let facts = [fact("sp-a", "requeued", "x", 1_000), fact("sp-a", "requeued", "x", 1_030), fact("sp-a", "requeued", "x", 1_200), fact("sp-b", "requeued", "x", 1_000)];
        let sql = "SELECT event_type, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events, SUM(burst_start) AS bursts FROM (SELECT event_type, issue_id, CASE WHEN LAG(created_at) OVER (PARTITION BY issue_id ORDER BY created_at) IS NULL OR TIMESTAMPDIFF(SECOND, LAG(created_at) OVER (PARTITION BY issue_id ORDER BY created_at), created_at) > 60 THEN 1 ELSE 0 END AS burst_start FROM events) b GROUP BY event_type";
        let rows = run(&facts, sql).unwrap();
        assert_eq!(rows, [serde_json::json!({"event_type": "requeued", "beads": "2", "events": "4", "bursts": "3"})]);
    }

    #[test]
    fn unix_timestamp_reads_the_created_at_the_facts_carry() {
        let rows = run(&[fact("sp-a", "reopen", "x", 1_700_000_000)], "SELECT CAST(UNIX_TIMESTAMP(created_at) AS SIGNED) AS t, created_at FROM events").unwrap();
        assert_eq!(rows[0]["t"], "1700000000");
        assert_eq!(rows[0]["created_at"], "2023-11-14 22:13:20");
    }

    #[test]
    fn a_created_at_comparison_against_a_datetime_literal_reads_as_text_in_order() {
        let facts = [fact("sp-a", "reopen", "x", 1_700_000_000), fact("sp-b", "reopen", "x", 1_600_000_000)];
        let rows = run(&facts, "SELECT issue_id FROM events WHERE created_at > '2023-11-14 22:13:20' OR actor LIKE 'harn%'").unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_bad_statement_is_an_error_not_a_panic() {
        assert!(run(&[], "SELECT nope FROM events").is_err());
    }
}
