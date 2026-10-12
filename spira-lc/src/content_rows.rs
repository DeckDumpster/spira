//! `content-rows --ids a,b,…` — the bead-content fields a claim decision reads (labels, type,
//! priority, defer, blockers), straight from the content store over this connection: no bd,
//! no git. Read-only; spira_lc holds SELECT on these three tables and nothing else there.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::db::Conn;
use crate::rows;

const CANNOT_TELL: i32 = 2;
pub const CONTENT_DB: &str = "spira";
const RFC3339: &str = "%Y-%m-%dT%H:%i:%sZ";

fn s<'a>(row: &'a Value, col: &str) -> Option<&'a str> {
    row.get(col).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// bd's `list --json` row shape, assembled from the three tables' rows. A bead's parent is
/// its `parent-child` target, as bd reports it.
pub fn assemble(issues: &[Value], labels: &[Value], deps: &[Value]) -> Vec<Value> {
    let mut by_label: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in labels {
        if let (Some(id), Some(label)) = (s(l, "issue_id"), s(l, "label")) {
            by_label.entry(id).or_default().push(label);
        }
    }
    let mut by_dep: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for d in deps {
        if let (Some(id), Some(on)) = (s(d, "issue_id"), s(d, "depends_on_id")) {
            by_dep.entry(id).or_default().push(json!({ "issue_id": id, "depends_on_id": on, "type": s(d, "type").unwrap_or("blocks") }));
        }
    }
    issues
        .iter()
        .filter_map(|i| {
            let id = s(i, "id")?;
            let deps = by_dep.remove(id).unwrap_or_default();
            let parent = deps.iter().find(|d| d["type"] == "parent-child").and_then(|d| d["depends_on_id"].as_str().map(str::to_string));
            Some(json!({
                "id": id,
                "title": s(i, "title"),
                "priority": s(i, "priority").and_then(|p| p.parse::<i64>().ok()),
                "parent": parent,
                "created_at": s(i, "created_at"),
                "updated_at": s(i, "updated_at"),
                "labels": by_label.get(id).cloned().unwrap_or_default(),
                "issue_type": s(i, "issue_type"),
                "status": s(i, "status"),
                "defer_until": s(i, "defer_until"),
                "dependencies": deps,
            }))
        })
        .collect()
}

pub fn cmd_content_rows(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(ids) = args.iter().position(|a| a == "--ids").and_then(|i| args.get(i + 1)) else {
        return (CANNOT_TELL, "content-rows: --ids <a,b,…> is required".into());
    };
    let list: Vec<String> = ids.split(',').filter(|x| !x.is_empty()).map(|x| format!("'{}'", rows::escape(x))).collect();
    if list.is_empty() {
        return (0, "[]".into());
    }
    let list = list.join(",");
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let run = || -> Result<Vec<Value>, String> {
        let issues = q(format!(
            "SELECT id, title, priority, issue_type, status, DATE_FORMAT(defer_until, '{RFC3339}') AS defer_until, DATE_FORMAT(created_at, '{RFC3339}') AS created_at, DATE_FORMAT(updated_at, '{RFC3339}') AS updated_at \
             FROM {CONTENT_DB}.issues WHERE id IN ({list}) ORDER BY id"
        ))?;
        let labels = q(format!("SELECT issue_id, label FROM {CONTENT_DB}.labels WHERE issue_id IN ({list}) ORDER BY issue_id, label"))?;
        let deps = q(format!("SELECT issue_id, depends_on_id, `type` FROM {CONTENT_DB}.dependencies WHERE issue_id IN ({list}) ORDER BY issue_id, depends_on_id"))?;
        Ok(assemble(&issues, &labels, &deps))
    };
    match run() {
        Ok(v) => (0, serde_json::to_string(&Value::Array(v)).unwrap()),
        Err(e) => (CANNOT_TELL, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &str) -> Vec<Value> {
        serde_json::from_str(x).unwrap()
    }

    #[test]
    fn a_row_carries_labels_blockers_and_the_parent_bd_would_report() {
        let issues = v(r#"[{"id":"sp-a","title":"t","priority":"1","issue_type":"task","status":"open","defer_until":"","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}]"#);
        let labels = v(r#"[{"issue_id":"sp-a","label":"plan"},{"issue_id":"sp-a","label":"spira"},{"issue_id":"sp-z","label":"stray"}]"#);
        let deps = v(r#"[{"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"},{"issue_id":"sp-a","depends_on_id":"sp-e","type":"parent-child"}]"#);
        let out = assemble(&issues, &labels, &deps);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r["labels"], json!(["plan", "spira"]));
        assert_eq!(r["priority"], json!(1));
        assert_eq!(r["parent"], json!("sp-e"));
        assert_eq!(r["defer_until"], Value::Null);
        assert_eq!(r["dependencies"][0], json!({"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"}));
    }

    #[test]
    fn a_bead_with_no_labels_or_edges_has_empty_lists() {
        let out = assemble(&v(r#"[{"id":"sp-a","priority":"2"}]"#), &[], &[]);
        assert_eq!((out[0]["labels"].clone(), out[0]["dependencies"].clone(), out[0]["parent"].clone()), (json!([]), json!([]), Value::Null));
    }

    #[test]
    fn no_ids_is_an_empty_answer_and_a_missing_flag_is_refused() {
        let c = Conn::unreachable();
        assert_eq!(cmd_content_rows(&["--ids".into(), "".into()], &c), (0, "[]".into()));
        assert_eq!(cmd_content_rows(&[], &c).0, CANNOT_TELL);
    }
}
