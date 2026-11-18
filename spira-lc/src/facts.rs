//! The attempt and poison history: facts that change no bead's state but that the attempt
//! count and the poison decision fold, kept on the control store (`control::facts`).

use serde_json::Value;

use crate::control::facts::Fact;
use crate::db;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

/// Closed: a kind outside this list is refused, so a typo cannot start a new history.
pub const KINDS: &[&str] = &["claimed", "requeued", "reopen", "reclaimed", "poison.cleared", "recurred", "lapsed", "session", "checkpointed", "fast-tier-red", "eject-failure", "send-back", "send-back-logs", "blind-rework", "rejection", "escalated", "no-progress", "gate-start", "build-dir", "suite-dropped", "__doctor_probe__"];

pub const CAUSE_MAX: usize = 200;
pub const READ_MAX: usize = 120;
/// Kinds whose cause is a multi-line digest rather than a one-line token.
pub const LONG_KINDS: &[&str] = &["fast-tier-red", "eject-failure", "send-back", "rejection"];
pub const LONG_MAX: usize = 6000;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub(crate) fn bounded(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(CAUSE_MAX).collect()
}

fn bounded_tabbed(s: &str) -> String {
    s.chars().filter(|c| *c == '\t' || !c.is_control()).take(CAUSE_MAX).collect()
}

fn bounded_long(s: &str) -> String {
    s.chars().filter(|c| *c == '\n' || !c.is_control()).take(LONG_MAX).collect()
}

pub fn fact_of(key: &str, kind: &str, cause: &str, actor: &str, at: i64) -> Fact {
    Fact { bead: key.to_string(), kind: kind.to_string(), cause: cause.to_string(), actor: actor.to_string(), at }
}

fn store() -> Result<std::sync::Arc<crate::control::store::Store<crate::control::served::Served>>, String> {
    crate::control::served::store()
}

/// Appends `facts` in one txn.
pub fn append(facts: &[Fact]) -> Result<(), String> {
    append_on(&*store()?, facts)
}

pub(crate) fn append_on<B: state_store::Backend>(store: &crate::control::store::Store<B>, facts: &[Fact]) -> Result<(), String> {
    let req = state_store::Request::new(&crate::control::served::request_id(), "spira-lc", "fact");
    store.append_facts(facts, &req).map_err(|e| e.to_string())
}

/// `fact <bead-id> --kind K --actor A [--cause C]`.
pub fn cmd_fact(args: &[String]) -> (i32, String) {
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
    let cause = match kind.as_str() {
        k if LONG_KINDS.contains(&k) => bounded_long(&raw),
        "build-dir" => bounded_tabbed(&raw),
        _ => bounded(&raw),
    };
    match append(&[fact_of(key, &kind, &cause, &bounded(&actor), db::now_epoch())]) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

/// The fact of a submit's `build-dir`: the tip submitted and the directory it was built in.
pub fn build_dir(bead: &str, tip: &str, dir: &str, actor: &str, at: i64) -> Fact {
    fact_of(bead, "build-dir", &format!("{tip}\t{dir}"), &bounded(actor), at)
}

/// The facts of `ids` of `kinds` after `since`, each bead's oldest first, as rows of
/// `issue_id`, `event_type`, `new_value` (clipped as a read clips), `actor` and `at`.
pub fn read(ids: &[String], kinds: &[String], since: Option<i64>) -> Result<Vec<Value>, String> {
    read_on(&*store()?, ids, kinds, since)
}

pub(crate) fn read_on<B: state_store::Backend>(store: &crate::control::store::Store<B>, ids: &[String], kinds: &[String], since: Option<i64>) -> Result<Vec<Value>, String> {
    let facts = store.facts(ids).map_err(|e| e.to_string())?;
    Ok(facts
        .into_iter()
        .filter(|f| kinds.is_empty() || kinds.contains(&f.kind))
        .filter(|f| since.is_none_or(|t| f.at > t))
        .map(|f| {
            let max = if LONG_KINDS.contains(&f.kind.as_str()) { LONG_MAX } else { READ_MAX };
            serde_json::json!({ "issue_id": f.bead, "event_type": f.kind, "new_value": f.cause.chars().take(max).collect::<String>(), "actor": f.actor, "at": f.at.to_string() })
        })
        .collect())
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
pub fn cmd_facts(args: &[String]) -> (i32, String) {
    let since = match flag(args, "--since").map(|v| v.parse::<i64>()) {
        Some(Err(_)) => return (CANNOT_TELL, "facts: --since must be an epoch second".into()),
        Some(Ok(t)) => Some(t),
        None => None,
    };
    match read(&csv(args, "--ids"), &csv(args, "--kinds"), since) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(shape(r))).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
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

/// One SELECT written against bd's `events` table. Every table reference must be `events` (a
/// UNION of such SELECTs and a subquery over it are fine); any other table, join, comma list,
/// table function or INTO is refused, so the facts are the only thing a caller can read.
pub fn check(sql: &str) -> Result<(), String> {
    let toks = scan(sql).map_err(|why| format!("{why}; {FROM_EVENTS_ONLY}"))?;
    let word = |i: usize| match toks.get(i) {
        Some((_, _, Tok::Word(w))) => Some(*w),
        _ => None,
    };
    if !word(0).is_some_and(|w| w.eq_ignore_ascii_case("SELECT")) {
        return Err("not a SELECT".into());
    }
    let mut reads_events = false;
    let mut in_from = vec![false];
    for (i, (_, _, tok)) in toks.iter().enumerate() {
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
                reads_events = true;
            }
            _ => {}
        }
    }
    if !reads_events {
        return Err("does not read the events table".into());
    }
    Ok(())
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
pub fn cmd_facts_query(args: &[String]) -> (i32, String) {
    let Some(sql) = args.first() else {
        return (CANNOT_TELL, "facts-query: missing <sql>".into());
    };
    if let Err(why) = check(sql) {
        return (REFUSED, format!("facts-query: refused: {why}"));
    }
    let facts = match store().and_then(|s| s.facts(&[]).map_err(|e| e.to_string())) {
        Ok(f) => f,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match crate::fact_sql::run(&facts, sql) {
        Ok(r) => (0, render_table(&r)),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn put(s: &crate::control::store::Store<MemBackend>, bead: &str, kind: &str, cause: &str, at: i64) {
        append_on(s, &[fact_of(bead, kind, cause, "harness", at)]).unwrap();
    }

    #[test]
    fn the_send_back_kind_is_accepted_and_is_a_long_kind() {
        assert!(KINDS.contains(&spira_config::send_back::KIND));
        assert!(LONG_KINDS.contains(&spira_config::send_back::KIND));
    }

    #[test]
    fn the_gate_start_kind_the_gate_sends_is_one_the_fact_verb_accepts() {
        assert!(KINDS.contains(&"gate-start"));
    }

    #[test]
    fn a_build_dir_fact_keeps_the_tab_between_the_tip_and_the_directory() {
        assert!(KINDS.contains(&"build-dir"));
        assert_eq!(bounded_tabbed("abc\tb/dir.1\n"), "abc\tb/dir.1");
        assert_eq!(bounded("abc\tdef"), "abcdef", "no other kind keeps a tab");
        assert_eq!(build_dir("sp-a", "abc", "b/dir.3", "builder", 7).cause, "abc\tb/dir.3");
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
        let s = store();
        let long = "x\n".repeat(LONG_MAX / 2);
        put(&s, "sp-a", "fast-tier-red", &long, 5);
        put(&s, "sp-a", "requeued", &"y".repeat(300), 6);
        let rows = read_on(&s, &["sp-a".into()], &[], None).unwrap();
        assert_eq!(rows[0]["new_value"].as_str().unwrap().len(), LONG_MAX);
        assert_eq!(rows[1]["new_value"].as_str().unwrap().len(), READ_MAX);
    }

    // covers: UC-ops-detection-remediation-12
    #[test]
    fn facts_read_back_per_bead_in_the_order_they_were_written_and_scope_to_ids_kinds_and_since() {
        let s = store();
        put(&s, "sp-b", "claimed", "w1", 10);
        put(&s, "sp-a", "claimed", "w2", 20);
        put(&s, "sp-a", "requeued", "gate-red", 30);
        put(&s, "sp-b", "requeued", "it's", 40);
        let all = read_on(&s, &[], &[], None).unwrap();
        let order: Vec<(&str, &str)> = all.iter().map(|r| (r["issue_id"].as_str().unwrap(), r["event_type"].as_str().unwrap())).collect();
        assert_eq!(order, [("sp-a", "claimed"), ("sp-a", "requeued"), ("sp-b", "claimed"), ("sp-b", "requeued")]);
        assert_eq!(read_on(&s, &["sp-a".into()], &["requeued".into()], None).unwrap().len(), 1);
        let since = read_on(&s, &[], &[], Some(30)).unwrap();
        assert_eq!(since.len(), 1, "a fact at the watermark itself is excluded: {since:?}");
        assert_eq!(since[0]["new_value"], "it's");
        assert_eq!(shape(all)[0]["created_at"], "1970-01-01T00:00:20Z");
    }

    // covers: UC-ops-detection-remediation-12
    #[test]
    fn a_census_query_runs_over_the_facts_and_answers_as_bd_sql_does() {
        let s = store();
        put(&s, "sp-a", "reopen", "eject", 1_700_000_000);
        put(&s, "sp-b", "reopen", "eject", 1_700_000_100);
        put(&s, "sp-b", "claimed", "w", 1_700_000_200);
        let facts = s.facts(&[]).unwrap();
        let rows = crate::fact_sql::run(&facts, "SELECT event_type, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type = 'reopen' GROUP BY event_type").unwrap();
        assert_eq!(render_table(&rows), "+---+---+---+\n| event_type | beads | events |\n+---+---+---+\n| reopen | 2 | 2 |\n+---+---+---+\n");
    }

    #[test]
    fn a_free_query_over_another_table_is_refused_before_it_touches_the_store() {
        let (code, msg) = cmd_facts_query(&["SELECT a FROM events WHERE 1=0 UNION ALL SELECT lc_key FROM bead".to_string()]);
        assert_eq!(code, REFUSED, "{msg}");
        assert!(msg.contains("facts [--ids"), "{msg}");
    }

    #[test]
    fn a_query_that_names_only_events_passes_the_check() {
        assert!(check("SELECT x FROM (SELECT a AS x FROM events WHERE y) b UNION ALL SELECT a FROM events e WHERE z ORDER BY 1").is_ok());
        assert!(check("SELECT a FROM events WHERE new_value = 'FROM bead' AND x = 'it''s FROM events'").is_ok());
        assert!(check("DELETE FROM events").is_err());
        assert!(check("SELECT 1 FROM events; DROP TABLE event").is_err());
        assert!(check("SELECT * FROM bead").is_err(), "a SELECT that never reads events is refused");
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
            let why = check(q).expect_err(q);
            assert!(why.contains("facts [--ids"), "{q}: {why}");
        }
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
}
