//! The attempt and poison history: facts that change no bead's state but that the attempt
//! count and the poison decision fold. Appended to the lifecycle event log under machine
//! `fact`, so the history shares the log's append-only guarantee and no bd write.

use serde_json::Value;

use crate::db::{self, Conn};
use crate::rows;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

/// Closed: a kind outside this list is refused, so a typo cannot start a new history.
pub const KINDS: &[&str] = &["claimed", "requeued", "reopen", "reclaimed", "poison.cleared", "recurred", "lapsed", "session", "checkpointed", "fast-tier-red", "blind-rework", "__doctor_probe__"];

pub const MACHINE: &str = "fact";
pub const CAUSE_MAX: usize = 200;
pub const READ_MAX: usize = 120;
/// Kinds whose cause is a multi-line digest rather than a one-line token.
pub const LONG_KINDS: &[&str] = &["fast-tier-red"];
pub const LONG_MAX: usize = 6000;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn bounded(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(CAUSE_MAX).collect()
}

fn bounded_long(s: &str) -> String {
    s.chars().filter(|c| *c == '\n' || !c.is_control()).take(LONG_MAX).collect()
}

pub fn insert_sql(key: &str, kind: &str, cause: &str, actor: &str, at: i64) -> String {
    let evidence = serde_json::json!({ "cause": cause }).to_string();
    format!(
        "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ('{}', '{}', '{}', '', '', '', 1, NULL, '{}', '{}', {at})",
        MACHINE,
        rows::escape(key),
        rows::escape(kind),
        rows::escape(&evidence),
        rows::escape(actor),
    )
}

/// `fact <bead-id> --kind K --actor A [--cause C]`.
pub fn cmd_fact(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(key) = args.first().filter(|a| !a.starts_with("--")) else {
        return (CANNOT_TELL, "fact: missing <bead-id>".into());
    };
    let (Some(kind), Some(actor)) = (flag(args, "--kind"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "fact: --kind and --actor are required".into());
    };
    if !KINDS.contains(&kind.as_str()) {
        return (REFUSED, format!("fact: unknown kind {kind:?} (want one of {})", KINDS.join(", ")));
    }
    if key.is_empty() || actor.is_empty() {
        return (CANNOT_TELL, "fact: empty <bead-id> or --actor".into());
    }
    let raw = flag(args, "--cause").unwrap_or_default();
    let cause = if LONG_KINDS.contains(&kind.as_str()) { bounded_long(&raw) } else { bounded(&raw) };
    match conn.append_event(&insert_sql(key, &kind, &cause, &bounded(&actor), db::now_epoch())) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

pub fn select_sql(ids: &[String], kinds: &[String], since: Option<i64>) -> String {
    let list = |v: &[String]| v.iter().map(|i| format!("'{}'", rows::escape(i))).collect::<Vec<_>>().join(",");
    let long = LONG_KINDS.iter().map(|k| format!("event = '{k}'")).collect::<Vec<_>>().join(" OR ");
    let mut sql = format!(
        "SELECT lc_key AS issue_id, event AS event_type, CASE WHEN ({long}) THEN SUBSTRING(COALESCE(JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.cause')), ''), 1, {LONG_MAX}) ELSE SUBSTRING(COALESCE(JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.cause')), ''), 1, {READ_MAX}) END AS new_value, actor, `at` FROM event WHERE machine = '{MACHINE}'"
    );
    if !ids.is_empty() {
        sql.push_str(&format!(" AND lc_key IN ({})", list(ids)));
    }
    if !kinds.is_empty() {
        sql.push_str(&format!(" AND event IN ({})", list(kinds)));
    }
    if let Some(t) = since {
        sql.push_str(&format!(" AND `at` > {t}"));
    }
    sql.push_str(" ORDER BY lc_key, seq");
    sql
}

fn csv(args: &[String], name: &str) -> Vec<String> {
    flag(args, name).map(|v| v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

/// Each row as bd's `events` rows read: `issue_id`, `event_type`, `new_value`, `actor`, `created_at`.
pub fn shape(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let at = s("at").parse::<i64>().unwrap_or(0);
            serde_json::json!({
                "issue_id": s("issue_id"),
                "event_type": s("event_type"),
                "new_value": s("new_value"),
                "actor": s("actor"),
                "created_at": crate::callers::fmt_utc(at),
            })
        })
        .collect()
}

/// `facts [--ids a,b] [--kinds x,y] [--since EPOCH]`: the facts, oldest first per bead.
pub fn cmd_facts(args: &[String], conn: &Conn) -> (i32, String) {
    let since = match flag(args, "--since").map(|v| v.parse::<i64>()) {
        Some(Err(_)) => return (CANNOT_TELL, "facts: --since must be an epoch second".into()),
        Some(Ok(t)) => Some(t),
        None => None,
    };
    match conn.query(&select_sql(&csv(args, "--ids"), &csv(args, "--kinds"), since)) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(shape(r))).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// The facts as a table named `events` with bd's column names, for a query written against
/// bd's own `events` table (census): `FROM events` becomes `FROM <this> events`.
pub fn events_table() -> String {
    format!(
        "(SELECT lc_key AS issue_id, event AS event_type, JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.cause')) AS new_value, actor, DATE_ADD(TIMESTAMP('1970-01-01 00:00:00'), INTERVAL `at` SECOND) AS created_at FROM event WHERE machine = '{MACHINE}')"
    )
}

const BD_TABLE: &str = "FROM events";

/// One SELECT written against bd's `events` table, with every `FROM events` pointed at the facts.
/// Refused when it is anything else.
pub fn retarget(sql: &str) -> Result<String, &'static str> {
    if !sql.trim_start().starts_with("SELECT ") {
        return Err("not a SELECT");
    }
    if sql.contains(';') {
        return Err("more than one statement");
    }
    if !sql.contains(BD_TABLE) {
        return Err("does not read the events table");
    }
    Ok(sql.replace(BD_TABLE, &format!("FROM {} events", events_table())))
}

/// bd's `sql` table rendering, which is what census's parsers read.
pub fn render_table(rows: &[Value]) -> String {
    let cell = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let Some(Value::Object(first)) = rows.first() else { return String::new() };
    let cols: Vec<&String> = first.keys().collect();
    let sep = format!("+{}+\n", cols.iter().map(|_| "---").collect::<Vec<_>>().join("+"));
    let line = |vals: Vec<String>| format!("| {} |\n", vals.join(" | "));
    let mut out = sep.clone();
    out.push_str(&line(cols.iter().map(|c| c.to_string()).collect()));
    out.push_str(&sep);
    for r in rows {
        out.push_str(&line(cols.iter().map(|c| cell(r.get(c.as_str()).unwrap_or(&Value::Null))).collect()));
    }
    out.push_str(&sep);
    out
}

/// `facts-query <sql>`: census's aggregate over the facts, answered as bd's `sql` table.
pub fn cmd_facts_query(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(sql) = args.first() else {
        return (CANNOT_TELL, "facts-query: missing <sql>".into());
    };
    let sql = match retarget(sql) {
        Ok(q) => q,
        Err(why) => return (REFUSED, format!("facts-query: refused: {why}")),
    };
    match conn.query(&sql) {
        Ok(r) => (0, render_table(&r)),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_insert_is_an_applied_fact_with_the_cause_as_evidence() {
        let q = insert_sql("sp-a", "requeued", "gate-red", "harness", 1_000_000_000);
        assert!(q.contains("'fact', 'sp-a', 'requeued'"), "{q}");
        assert!(q.contains(r#"{"cause":"gate-red"}"#), "{q}");
        assert!(q.contains(", 1, NULL,"), "{q}");
    }

    #[test]
    fn quotes_in_a_cause_do_not_escape_the_literal() {
        let q = insert_sql("sp-a", "requeued", "it's", "h", 1);
        assert!(q.contains(r"it\\'s") || q.contains(r"it\'s"), "{q}");
    }

    #[test]
    fn the_select_scopes_to_the_fact_machine_and_the_asked_ids() {
        let q = select_sql(&["sp-a".into(), "o'x".into()], &["claimed".into()], None);
        assert!(q.contains("machine = 'fact'") && q.contains("'sp-a','o\\'x'") && q.contains("event IN ('claimed')"), "{q}");
        let all = select_sql(&[], &[], None);
        assert!(!all.contains("lc_key IN") && !all.contains("event IN") && !all.contains("`at` >"), "{all}");
        assert!(select_sql(&[], &[], Some(1_700_000_000)).contains("AND `at` > 1700000000"));
    }

    #[test]
    fn rows_come_back_in_the_shape_of_bd_events() {
        let out = shape(vec![serde_json::json!({"issue_id": "sp-a", "event_type": "claimed", "new_value": "aeon", "actor": "h", "at": "1000000000"})]);
        assert_eq!(out[0]["created_at"], "2001-09-09T01:46:40Z");
        assert_eq!(out[0]["event_type"], "claimed");
    }

    #[test]
    fn a_cause_is_bounded_and_single_line() {
        assert_eq!(bounded(&format!("a\nb{}", "x".repeat(300))).len(), CAUSE_MAX);
        assert!(!bounded("a\nb").contains('\n'));
    }

    #[test]
    fn a_fast_tier_red_keeps_its_lines_and_reads_back_whole() {
        assert!(KINDS.contains(&"fast-tier-red"));
        assert!(bounded_long("a\nb").contains('\n'));
        assert_eq!(bounded_long(&"x".repeat(LONG_MAX + 50)).len(), LONG_MAX);
        let q = select_sql(&["sp-a".into()], &["fast-tier-red".into()], None);
        assert!(q.contains(&format!("1, {LONG_MAX}")), "{q}");
    }

    #[test]
    fn a_census_query_is_retargeted_from_bd_events_to_the_facts() {
        let q = retarget("SELECT a FROM events WHERE x IN (SELECT i FROM events WHERE y)").unwrap();
        assert_eq!(q.matches("machine = 'fact'").count(), 2, "{q}");
        assert!(!q.contains("FROM events WHERE"), "{q}");
        assert!(retarget("DELETE FROM events").is_err());
        assert!(retarget("SELECT 1 FROM events; DROP TABLE event").is_err());
        assert!(retarget("SELECT * FROM bead").is_err(), "a SELECT that never reads events is refused");
    }

    #[test]
    fn the_table_renders_as_bd_sql_does_with_the_columns_in_query_order() {
        let mut m = serde_json::Map::new();
        m.insert("event_type".into(), "requeued".into());
        m.insert("beads".into(), "2".into());
        m.insert("note".into(), Value::Null);
        let t = render_table(&[Value::Object(m)]);
        assert_eq!(t, "+---+---+---+\n| event_type | beads | note |\n+---+---+---+\n| requeued | 2 |  |\n+---+---+---+\n");
        assert_eq!(render_table(&[]), "");
    }

    #[test]
    fn every_kind_fits_the_event_column() {
        assert!(KINDS.iter().all(|k| k.len() <= 32));
    }
}
