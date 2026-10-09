//! The ops read model (`lifecycle/migrations/0007-ops-read-model.sql`): `ops-view <view>`
//! returns one of the three views as a JSON array, and `backfill-titles` fills the mirrored
//! title and priority of rows filed before the mirror existed. Both ride `dispatch`, so a
//! refresh through `spira-lc serve` is one query on the held connection.

use serde_json::Value;

use crate::db::Conn;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 3] = ["ops_live", "ops_round", "ops_recent"];

const LIVE_STATES: &str = "'OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK'";

pub fn cmd_ops_view(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(view) = args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())) else {
        return (2, format!("ops-view: takes one of {}", VIEWS.join(" | ")));
    };
    match conn.query(&format!("SELECT * FROM {view}")) {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

pub fn title_priority(bd_show_json: &str) -> Option<(String, i64)> {
    let parsed: Value = serde_json::from_str(bd_show_json.trim()).ok()?;
    let doc = match &parsed {
        Value::Array(a) => a.first()?.clone(),
        v => v.clone(),
    };
    Some((doc.get("title")?.as_str()?.to_string(), doc.get("priority")?.as_i64()?))
}

/// Rows the views show that have no title yet: every live row, and the landings of the last day.
fn untitled_sql() -> String {
    format!(
        "SELECT bead_id FROM bead WHERE title IS NULL AND (state IN ({LIVE_STATES}) OR (state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400))"
    )
}

pub fn cmd_backfill_titles(_args: &[String], conn: &Conn) -> (i32, String) {
    let ids: Vec<String> = match conn.query(&untitled_sql()) {
        Ok(rows) => rows.iter().filter_map(|r| r.get("bead_id").and_then(|v| v.as_str()).map(str::to_string)).collect(),
        Err(e) => return (2, format!("cannot tell: {e:?}")),
    };
    let (mut filled, mut missed) = (0, Vec::new());
    for id in &ids {
        let known = crate::bd::run(&["show", id, "--json"]).ok().and_then(|o| title_priority(&o));
        let Some((title, priority)) = known else {
            missed.push(id.clone());
            continue;
        };
        let args = [id.clone(), "--title".into(), title, "--priority".into(), priority.to_string()];
        match crate::cutover::cmd_create_bead(&args, conn) {
            (0, _) => filled += 1,
            (_, why) => missed.push(format!("{id} ({why})")),
        }
    }
    let code = if missed.is_empty() { 0 } else { 1 };
    (code, format!("backfill-titles: {filled} of {} filled{}", ids.len(), if missed.is_empty() { String::new() } else { format!("; not filled: {}", missed.join(", ")) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bd_show_json_gives_title_and_priority_whether_array_or_object() {
        assert_eq!(title_priority(r#"[{"title":"t","priority":1}]"#), Some(("t".into(), 1)));
        assert_eq!(title_priority(r#"{"title":"t","priority":0}"#), Some(("t".into(), 0)));
        assert_eq!(title_priority(r#"{"title":"t"}"#), None);
        assert_eq!(title_priority("not json"), None);
    }

    #[test]
    fn the_backfill_reaches_exactly_the_rows_the_views_show() {
        let sql = untitled_sql();
        let migration = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations/0007-ops-read-model.sql")).unwrap();
        let one_line = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(one_line.contains(&format!("state IN ({LIVE_STATES})")), "ops_live and the backfill name the same states");
        assert!(sql.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400") && migration.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400"));
    }
}
