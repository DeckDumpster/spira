//! The attempt and poison history: facts that change no bead's state but that the attempt
//! count and the poison decision fold. Appended to the lifecycle event log under machine
//! `fact`, so the history shares the log's append-only guarantee and no bd write.

use serde_json::Value;

use crate::db::{self, Conn};
use crate::rows;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

/// Closed: a kind outside this list is refused, so a typo cannot start a new history.
pub const KINDS: &[&str] = &["claimed", "requeued", "reopen", "reclaimed", "poison.cleared", "recurred", "lapsed", "session", "checkpointed", "fast-tier-red", "blind-rework", "rejection", "escalated", "no-progress", "gate-start", "__doctor_probe__"];

pub const MACHINE: &str = "fact";
pub const CAUSE_MAX: usize = 200;
pub const READ_MAX: usize = 120;
/// Kinds whose cause is a multi-line digest rather than a one-line token.
pub const LONG_KINDS: &[&str] = &["fast-tier-red", "rejection"];
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

const FROM_EVENTS_ONLY: &str = "the only table it may read is `events`; for anything else use the typed form `facts [--ids a,b] [--kinds x,y] [--since EPOCH]`";

#[derive(PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    Punct(char),
    Literal,
}

fn scan(sql: &str) -> Result<Vec<(usize, usize, Tok<'_>)>, &'static str> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'\'' {
            let start = i;
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err("unterminated string"),
                    Some(b'\\') => i += 2,
                    Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                    Some(b'\'') => break,
                    Some(_) => i += 1,
                }
            }
            i += 1;
            out.push((start, i, Tok::Literal));
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                i += 1;
            }
            out.push((start, i, Tok::Word(&sql[start..i])));
        } else if matches!(c, b'`' | b'"' | b'#' | b';' | b'@') || (c == b'-' && b.get(i + 1) == Some(&b'-')) || (c == b'/' && b.get(i + 1) == Some(&b'*')) {
            return Err("quoted identifiers, comments, variables and statement separators are not allowed");
        } else {
            out.push((i, i + 1, Tok::Punct(c as char)));
            i += 1;
        }
    }
    Ok(out)
}

/// One SELECT written against bd's `events` table, with every `FROM events` pointed at the facts.
/// Every table reference must be `events` (a UNION of such SELECTs and a subquery over it are
/// fine); any other table, join, comma list, table function or INTO is refused, so the
/// fact view is the only thing a caller can read.
pub fn retarget(sql: &str) -> Result<String, String> {
    let toks = scan(sql).map_err(|why| format!("{why}; {FROM_EVENTS_ONLY}"))?;
    let word = |i: usize| match toks.get(i) {
        Some((_, _, Tok::Word(w))) => Some(*w),
        _ => None,
    };
    if !word(0).is_some_and(|w| w.eq_ignore_ascii_case("SELECT")) {
        return Err("not a SELECT".into());
    }
    let mut out = String::new();
    let mut last = 0;
    let mut reads_events = false;
    let mut in_from = vec![false];
    for (i, (start, _, tok)) in toks.iter().enumerate() {
        match tok {
            Tok::Punct('(') => in_from.push(false),
            Tok::Punct(')') => {
                in_from.pop();
                if in_from.is_empty() {
                    return Err("unbalanced parentheses".into());
                }
            }
            Tok::Punct(',') if in_from.last() == Some(&true) => return Err(FROM_EVENTS_ONLY.into()),
            _ => {}
        }
        let Tok::Word(w) = tok else { continue };
        let upper = w.to_ascii_uppercase();
        match upper.as_str() {
            "WHERE" | "GROUP" | "ORDER" | "HAVING" | "UNION" | "LIMIT" | "WINDOW" => *in_from.last_mut().unwrap() = false,
            "TABLE" | "VALUES" | "ROW" | "JOIN" | "INTO" | "STRAIGHT_JOIN" | "OUTFILE" | "DUMPFILE" | "LOAD_FILE" => {
                return Err(format!("{upper} is not allowed; {FROM_EVENTS_ONLY}"));
            }
            "FROM" => {
                let derived = matches!(toks.get(i + 1), Some((_, _, Tok::Punct('(')))) && word(i + 2).is_some_and(|t| t.eq_ignore_ascii_case("SELECT"));
                *in_from.last_mut().unwrap() = true;
                if derived {
                    continue;
                }
                if !word(i + 1).is_some_and(|t| t.eq_ignore_ascii_case("events")) {
                    return Err(FROM_EVENTS_ONLY.into());
                }
                let mut after = i + 2;
                if word(after).is_some_and(|a| a.eq_ignore_ascii_case("AS")) {
                    after += 1;
                }
                let aliased = word(after).is_some_and(|a| !is_clause_word(a));
                let list_or_qualified = |at: usize| matches!(toks.get(at), Some((_, _, Tok::Punct('.' | ',' | '('))));
                if list_or_qualified(i + 2) || list_or_qualified(after) || (aliased && list_or_qualified(after + 1)) {
                    return Err(FROM_EVENTS_ONLY.into());
                }
                out.push_str(&sql[last..*start]);
                out.push_str(&format!("FROM {} events", events_table()));
                last = toks[i + 1].1;
                reads_events = true;
            }
            _ => {}
        }
    }
    if !reads_events {
        return Err("does not read the events table".into());
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

fn is_clause_word(w: &str) -> bool {
    ["WHERE", "GROUP", "ORDER", "HAVING", "UNION", "LIMIT", "WINDOW", "INTERSECT", "EXCEPT"].iter().any(|k| w.eq_ignore_ascii_case(k))
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
    fn the_gate_start_kind_the_gate_sends_is_one_the_fact_verb_accepts() {
        assert!(KINDS.contains(&"gate-start"));
        assert!(insert_sql("sp-a", "gate-start", "", "gate", 5).contains("'fact', 'sp-a', 'gate-start'"));
    }

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
        let out = shape(vec![serde_json::json!({"issue_id": "sp-a", "event_type": "claimed", "new_value": "dwarf", "actor": "h", "at": "1000000000"})]);
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
    fn a_union_subquery_or_second_table_over_another_table_is_refused_naming_the_typed_form() {
        let bad = [
            "SELECT a FROM events WHERE 1=0 UNION ALL SELECT id FROM bead",
            "SELECT a FROM events WHERE x IN (SELECT id FROM ask)",
            "SELECT a FROM events e JOIN bead b ON b.id = e.issue_id",
            "SELECT a FROM events, bead",
            "SELECT a FROM events e, bead",
            "SELECT a FROM events WHERE 1=0 UNION ALL SELECT id FROM `event`",
            "SELECT a FROM events WHERE 1=0 UNION ALL SELECT id FROM spira.event",
            "SELECT a FROM events WHERE 1=0 UNION ALL SELECT id FROM (bead)",
            "SELECT a FROM events WHERE 1=0 UNION ALL SELECT id FROM dolt_log()",
            "SELECT x FROM (SELECT a FROM events) t, bead",
            "SELECT x FROM (SELECT a FROM events) t JOIN bead",
            "SELECT a FROM events UNION TABLE bead",
            "SELECT a FROM events UNION VALUES ROW(1)",
            "SELECT a FROM events /* x */ WHERE 1=0",
            "SELECT a FROM events INTO OUTFILE '/tmp/x'",
            "select a from events where 1=0 union all select id from bead",
        ];
        for q in bad {
            let why = retarget(q).expect_err(q);
            assert!(why.contains("facts [--ids"), "{q}: {why}");
        }
    }

    #[test]
    fn the_verb_refuses_a_free_query_over_another_table_before_it_touches_the_store() {
        let conn = Conn::unreachable();
        let q = "SELECT a FROM events WHERE 1=0 UNION ALL SELECT lc_key FROM bead".to_string();
        let (code, msg) = cmd_facts_query(&[q], &conn);
        assert_eq!(code, REFUSED, "{msg}");
        assert!(msg.contains("facts [--ids"), "{msg}");
    }

    #[test]
    fn a_string_literal_that_says_from_bead_is_data_not_a_table() {
        let q = retarget("SELECT a FROM events WHERE new_value = 'FROM bead' AND x = 'it''s FROM events'").unwrap();
        assert!(q.contains("'FROM bead'") && q.contains("'it''s FROM events'"), "{q}");
        assert_eq!(q.matches("machine = 'fact'").count(), 1, "{q}");
    }

    #[test]
    fn a_union_of_fact_selects_and_an_aliased_derived_table_over_events_still_run() {
        let q = retarget("SELECT x FROM (SELECT a AS x FROM events WHERE y) b UNION ALL SELECT a FROM events e WHERE z ORDER BY 1").unwrap();
        assert_eq!(q.matches("machine = 'fact'").count(), 2, "{q}");
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
