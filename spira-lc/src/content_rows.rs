//! `content-rows --ids a,b,…` — the bead-content fields a claim decision reads (labels, type,
//! priority, defer, blockers), straight from the content store over this connection: no bd,
//! no git. `--full` adds description, notes and external_ref; `--external-contains S` selects
//! by external_ref instead of by id. Read-only; spira_lc holds SELECT on these three tables and
//! nothing else there.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::db::Conn;
use crate::rows;

const CANNOT_TELL: i32 = 2;
/// The content store's database: the one the bead store's own `.beads/metadata.json` names
/// (`dolt_database`) under `spira.db`, resolved once per process. Production's store names
/// `spira`; a testdb fixture and a sim world name their own (`sptest_…`) on the server lc-serve
/// reaches. A store that names none is refused by name, never defaulted
/// (law-a-config-value-has-no-fallback): a guessed name reads some other database.
fn content_db() -> Result<&'static str, String> {
    static DB: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    DB.get_or_init(|| spira_config::process::cfg("SPIRA_DB").and_then(|db| database_of(std::path::Path::new(db.trim()))))
        .as_deref()
        .map_err(|e| format!("cannot tell: {e}"))
}

/// The `dolt_database` that `<db>/.beads/metadata.json` names, as a quoted identifier.
pub fn database_of(db: &std::path::Path) -> Result<String, String> {
    if db.as_os_str().is_empty() {
        return Err("spira.db is empty: the content store's database is named by <spira.db>/.beads/metadata.json (dolt_database) — refusing".into());
    }
    let meta = db.join(".beads/metadata.json");
    let text = std::fs::read_to_string(&meta).map_err(|e| format!("{}: {e} — it names the content store's database (dolt_database); refusing", meta.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", meta.display()))?;
    let name = v.get("dolt_database").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        return Err(format!("{} names no dolt_database: the content store's database is not declared — refusing", meta.display()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("{}: dolt_database {name:?} is not a plain database name — refusing", meta.display()));
    }
    Ok(format!("`{name}`"))
}
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
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let full = args.iter().any(|a| a == "--full");
    let found: Result<Vec<Value>, String> = match (flag("--ids"), flag("--external-contains")) {
        (Some(ids), None) => fetch_with(conn, &ids.split(',').filter(|x| !x.is_empty()).collect::<Vec<_>>(), full),
        (None, Some(needle)) => by_external_ref(conn, needle, full),
        _ => Err("content-rows: exactly one of --ids <a,b,…> or --external-contains <text> is required".into()),
    };
    match found {
        Ok(v) => (0, serde_json::to_string(&Value::Array(v)).unwrap()),
        Err(e) => (CANNOT_TELL, e),
    }
}

/// The content rows of `ids`.
pub fn fetch(conn: &Conn, ids: &[&str]) -> Result<Vec<Value>, String> {
    fetch_with(conn, ids, false)
}

const TEXT_COLUMNS: &str = ", description, notes, external_ref";

fn by_external_ref(conn: &Conn, needle: &str, full: bool) -> Result<Vec<Value>, String> {
    let cdb = content_db()?;
    if needle.is_empty() {
        return Err("content-rows: --external-contains needs a non-empty text".into());
    }
    let sql = format!("SELECT id FROM {cdb}.issues WHERE LOCATE('{}', external_ref) > 0 ORDER BY id", rows::escape(needle));
    let found = conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"))?;
    let ids: Vec<&str> = found.iter().filter_map(|r| s(r, "id")).collect();
    fetch_with(conn, &ids, full)
}

fn fetch_with(conn: &Conn, ids: &[&str], full: bool) -> Result<Vec<Value>, String> {
    let cdb = content_db()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let list = ids.iter().map(|x| format!("'{}'", rows::escape(x))).collect::<Vec<_>>().join(",");
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let text = if full { TEXT_COLUMNS } else { "" };
    let issues = q(format!(
        "SELECT id, title, priority, issue_type, status, DATE_FORMAT(defer_until, '{RFC3339}') AS defer_until, DATE_FORMAT(created_at, '{RFC3339}') AS created_at, DATE_FORMAT(updated_at, '{RFC3339}') AS updated_at{text} \
         FROM {cdb}.issues WHERE id IN ({list}) ORDER BY id"
    ))?;
    let labels = q(format!("SELECT issue_id, label FROM {cdb}.labels WHERE issue_id IN ({list}) ORDER BY issue_id, label"))?;
    // beads' column is depends_on_issue_id (beside depends_on_wisp_id / depends_on_external);
    // the claim decision only follows issue-to-issue edges. sp-9j0937: the query named a
    // depends_on_id column that no beads schema has, and every claim check failed in prod.
    let deps = q(format!("SELECT issue_id, depends_on_issue_id AS depends_on_id, `type` FROM {cdb}.dependencies WHERE issue_id IN ({list}) AND depends_on_issue_id IS NOT NULL ORDER BY issue_id, depends_on_issue_id"))?;
    let mut out = assemble(&issues, &labels, &deps);
    if full {
        for (row, issue) in out.iter_mut().zip(issues.iter().filter(|i| s(i, "id").is_some())) {
            for col in ["description", "notes", "external_ref"] {
                row[col] = json!(issue.get(col).and_then(Value::as_str).unwrap_or(""));
            }
        }
    }
    Ok(out)
}

/// The pane's window on the content store: live beads plus the last day's closures.
const PANE_SCOPE: &str = "(status <> 'closed' OR closed_at >= UTC_TIMESTAMP() - INTERVAL 1 DAY)";
const INFLOW_ROWS: usize = 40;

fn count(conn: &Conn, sql: String) -> Result<i64, String> {
    let rows = conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"))?;
    Ok(rows.first().and_then(|r| r.get("n")).and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0))
}

/// `pane-facets inflow --window-secs N`: what the ops pane's inflow section draws, aggregated
/// here so the reply is the newest rows plus counts, never the table.
/// `pane-facets repo-labels --valid a,b`: live beads with no `repo:` label, and with only
/// `repo:` labels outside the valid set.
pub fn cmd_pane_facets(args: &[String], conn: &Conn) -> (i32, String) {
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let out = match args.first().map(String::as_str) {
        Some("inflow") => flag("--window-secs").and_then(|w| w.parse::<u32>().ok()).ok_or_else(|| "pane-facets inflow: --window-secs <n> is required".to_string()).and_then(|w| inflow(conn, w)),
        Some("repo-labels") => repo_labels(conn, flag("--valid").map_or("", String::as_str)),
        _ => Err("usage: pane-facets inflow --window-secs N | repo-labels --valid a,b".to_string()),
    };
    match out {
        Ok(v) => (0, v.to_string()),
        Err(e) => (CANNOT_TELL, e),
    }
}

fn inflow(conn: &Conn, window_secs: u32) -> Result<Value, String> {
    let cdb = content_db()?;
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let work = "COALESCE(issue_type, 'task') <> 'event'";
    let fresh = format!("{work} AND {PANE_SCOPE} AND created_at >= UTC_TIMESTAMP() - INTERVAL {window_secs} SECOND");
    let issues = q(format!(
        "SELECT id, title, priority, issue_type, status, DATE_FORMAT(created_at, '{RFC3339}') AS created_at FROM {cdb}.issues \
         WHERE {work} AND {PANE_SCOPE} AND created_at IS NOT NULL ORDER BY created_at DESC, id LIMIT {INFLOW_ROWS}"
    ))?;
    let ids: Vec<String> = issues.iter().filter_map(|i| s(i, "id")).map(|i| format!("'{}'", rows::escape(i))).collect();
    let labels = if ids.is_empty() {
        vec![]
    } else {
        q(format!("SELECT issue_id, label FROM {cdb}.labels WHERE issue_id IN ({}) ORDER BY issue_id, label", ids.join(",")))?
    };
    let kinds = q(format!("SELECT COALESCE(issue_type, 'task') AS kind, COUNT(*) AS n FROM {cdb}.issues WHERE {fresh} GROUP BY kind ORDER BY n DESC, kind"))?;
    let n = count(conn, format!("SELECT COUNT(*) AS n FROM {cdb}.issues WHERE {fresh}"))?;
    let defect = count(
        conn,
        format!("SELECT COUNT(*) AS n FROM {cdb}.issues WHERE {fresh} AND (issue_type = 'bug' OR id IN (SELECT issue_id FROM {cdb}.labels WHERE label = 'incident'))"),
    )?;
    let kinds: Vec<Value> = kinds.iter().map(|k| json!([s(k, "kind").unwrap_or("task"), k.get("n").and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0)])).collect();
    Ok(json!({ "n": n, "defect": defect, "kinds": kinds, "rows": assemble(&issues, &labels, &[]) }))
}

fn repo_labels(conn: &Conn, valid: &str) -> Result<Value, String> {
    let cdb = content_db()?;
    let live = format!("{cdb}.issues WHERE status <> 'closed'");
    let with_repo = format!("{live} AND id IN (SELECT issue_id FROM {cdb}.labels WHERE label LIKE 'repo:%')");
    let valid: Vec<String> = valid.split(',').filter(|v| !v.is_empty()).map(|v| format!("'repo:{}'", rows::escape(v))).collect();
    let mapped = if valid.is_empty() {
        0
    } else {
        count(conn, format!("SELECT COUNT(*) AS n FROM {live} AND id IN (SELECT issue_id FROM {cdb}.labels WHERE label IN ({}))", valid.join(",")))?
    };
    let total = count(conn, format!("SELECT COUNT(*) AS n FROM {live}"))?;
    let repo = count(conn, format!("SELECT COUNT(*) AS n FROM {with_repo}"))?;
    Ok(json!({ "unmapped": repo - mapped, "absent": total - repo }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &str) -> Vec<Value> {
        serde_json::from_str(x).unwrap()
    }

    /// sp-jf7spl: the content database is the one the store declares, never a default.
    #[test]
    fn the_content_database_is_the_stores_own_and_an_undeclared_one_is_refused() {
        let d = testkit::TempDir::new("content-db");
        std::fs::create_dir_all(d.join(".beads")).unwrap();
        let meta = d.join(".beads/metadata.json");
        std::fs::write(&meta, r#"{"dolt_database":"sptest_x"}"#).unwrap();
        assert_eq!(database_of(d.path()).unwrap(), "`sptest_x`");
        std::fs::write(&meta, r#"{"dolt_mode":"server"}"#).unwrap();
        let e = database_of(d.path()).unwrap_err();
        assert!(e.contains("names no dolt_database") && e.contains(&meta.display().to_string()), "{e}");
        std::fs::write(&meta, r#"{"dolt_database":"x`; DROP"}"#).unwrap();
        assert!(database_of(d.path()).is_err());
        assert!(database_of(std::path::Path::new("")).is_err());
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
