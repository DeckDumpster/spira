//! Dependency edges in the lifecycle store (`lifecycle/migrations/0011-bead-dep.sql`):
//! `dep-add` / `dep-remove` mirror one `bd dep` edge, `backfill-deps` fills the table from bd
//! once. `ops_live` derives `claimable` and `blocker` from these rows, so the edge is the only
//! thing written here; whether a blocker has landed is read from the bead's own state.

use serde_json::Value;

use crate::cutover::{flag, is_row_key, q};
use crate::db::Conn;

const CANNOT_TELL: i32 = 2;
pub(crate) const LIVE_STATES: &str = "'OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK'";

fn edge_script(id: &str, dep: &str, dep_type: &str) -> String {
    format!(
        "INSERT INTO bead_dep (bead_id, depends_on, dep_type) VALUES ({}, {}, {}) ON DUPLICATE KEY UPDATE dep_type = VALUES(dep_type);\n",
        q(id),
        q(dep),
        q(dep_type)
    )
}

fn keys<'a>(verb: &str, args: &'a [String]) -> Result<(&'a str, &'a str), (i32, String)> {
    match (args.first(), args.get(1)) {
        (Some(id), Some(dep)) if is_row_key(id) && is_row_key(dep) => Ok((id, dep)),
        _ => Err((CANNOT_TELL, format!("{verb}: takes <bead-id> <depends-on-id>"))),
    }
}

/// `dep-add <id> <depends-on-id> [--type T]` (type defaults to bd's `blocks`).
pub fn cmd_dep_add(args: &[String], conn: &Conn) -> (i32, String) {
    let (id, dep) = match keys("dep-add", args) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let dep_type = flag(args, "--type").unwrap_or_else(|| "blocks".into());
    match conn.run_plain_touching(&edge_script(id, dep, &dep_type), crate::live::Touch::Keys(vec![id.to_string()])) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// `dep-remove <id> <depends-on-id>`.
pub fn cmd_dep_remove(args: &[String], conn: &Conn) -> (i32, String) {
    let (id, dep) = match keys("dep-remove", args) {
        Ok(k) => k,
        Err(e) => return e,
    };
    match conn.run_plain_touching(&format!("DELETE FROM bead_dep WHERE bead_id = {} AND depends_on = {};\n", q(id), q(dep)), crate::live::Touch::Keys(vec![id.to_string()])) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// Every `(depends_on, type)` of one `bd dep list <id> --json` payload.
pub fn parse_edges(json: &str) -> Vec<(String, String)> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json.trim()) else { return Vec::new() };
    rows.iter()
        .filter_map(|d| {
            let target = d.get("depends_on_id").or_else(|| d.get("id"))?.as_str()?;
            let kind = d.get("type").or_else(|| d.get("dependency_type")).and_then(Value::as_str).unwrap_or("blocks");
            (is_row_key(target)).then(|| (target.to_string(), kind.to_string()))
        })
        .collect()
}

/// `backfill-deps [--force]`: replaces the edges of every live bead with what bd lists. A
/// store that already holds edges is left alone unless `--force`, so activation can call it
/// every time and pay for bd only once.
pub fn cmd_backfill_deps(args: &[String], conn: &Conn) -> (i32, String) {
    if !args.iter().any(|a| a == "--force") {
        match conn.query("SELECT 1 FROM bead_dep LIMIT 1") {
            Ok(rows) if !rows.is_empty() => return (0, "backfill-deps: edges already mirrored".into()),
            Ok(_) => {}
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
        }
    }
    let ids: Vec<String> = match conn.query(&format!("SELECT bead_id FROM bead WHERE state IN ({LIVE_STATES})")) {
        Ok(rows) => rows.iter().filter_map(|r| r.get("bead_id").and_then(Value::as_str).map(str::to_string)).collect(),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let (mut edges, mut missed) = (0, Vec::new());
    for id in &ids {
        let listed = crate::bd::run(&["dep", "list", id, "--json"]);
        let Ok(out) = listed else {
            missed.push(id.clone());
            continue;
        };
        let found = parse_edges(&out);
        let mut script = format!("DELETE FROM bead_dep WHERE bead_id = {};\n", q(id));
        for (dep, kind) in &found {
            script.push_str(&edge_script(id, dep, kind));
        }
        match conn.run_plain_touching(&script, crate::live::Touch::Keys(vec![id.clone()])) {
            Ok(()) => edges += found.len(),
            Err(_) => missed.push(id.clone()),
        }
    }
    let code = if missed.is_empty() { 0 } else { 1 };
    (code, format!("backfill-deps: {edges} edge(s) of {} live bead(s){}", ids.len(), if missed.is_empty() { String::new() } else { format!("; not read: {}", missed.join(", ")) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bd_dep_list_gives_target_and_type_in_either_spelling() {
        let json = r#"[{"depends_on_id":"sp-a","type":"blocks"},{"id":"sp-b","dependency_type":"parent-child"},{"id":"sp-c"},{"id":"not a key"}]"#;
        assert_eq!(parse_edges(json), [("sp-a".into(), "blocks".into()), ("sp-b".into(), "parent-child".into()), ("sp-c".into(), "blocks".into())]);
        assert!(parse_edges("garbage").is_empty());
    }

    #[test]
    fn an_edge_is_one_idempotent_statement_with_quoted_keys() {
        let s = edge_script("sp-a", "sp-b", "blo'cks");
        assert!(s.contains("ON DUPLICATE KEY UPDATE") && s.contains("'blo\\'cks'"), "{s}");
    }
}
