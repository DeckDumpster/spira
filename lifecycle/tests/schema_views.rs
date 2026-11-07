//! `schema.sql` states each view as the latest migration that creates or replaces it does.
//! Reads repository files only: no program, no store.

use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn strip_comments(sql: &str) -> String {
    sql.lines().map(|l| l.split("--").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// The text of `CREATE [OR REPLACE] VIEW <name> AS ...;`, whitespace-normalised, or None.
fn view_text(sql: &str, name: &str) -> Option<String> {
    let sql = strip_comments(sql);
    let mut from = 0;
    while let Some(at) = sql[from..].find("VIEW ") {
        let start = from + at;
        let rest = &sql[start + "VIEW ".len()..];
        let head = &sql[..start];
        let creates = head.trim_end().ends_with("CREATE") || head.trim_end().ends_with("CREATE OR REPLACE");
        let mut words = rest.split_whitespace();
        if creates && words.next() == Some(name) && words.next() == Some("AS") {
            let end = rest.find(';')?;
            let body = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
            return Some(format!("CREATE VIEW {body}"));
        }
        from = start + "VIEW ".len();
    }
    None
}

fn view_names(sql: &str) -> Vec<String> {
    let sql = strip_comments(sql);
    let mut names = Vec::new();
    let words: Vec<&str> = sql.split_whitespace().collect();
    for w in words.windows(3) {
        if w[0] == "VIEW" && w[2] == "AS" {
            names.push(w[1].to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

fn migrations() -> Vec<(String, String)> {
    let mut files: Vec<_> = fs::read_dir(root().join("migrations")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "sql")).collect();
    files.sort();
    files.into_iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&p).unwrap())).collect()
}

#[test]
fn schema_sql_defines_each_view_as_the_latest_migration_that_replaces_it_does() {
    let schema = fs::read_to_string(root().join("schema.sql")).unwrap();
    let migrations = migrations();
    let names = view_names(&schema);
    assert!(names.len() >= 6, "the matcher must find the views it checks: {names:?}");
    for name in &names {
        let (file, latest) = migrations.iter().rev().find_map(|(f, sql)| view_text(sql, name).map(|t| (f, t))).unwrap_or_else(|| panic!("no migration creates {name}"));
        let in_schema = view_text(&schema, name).unwrap();
        assert_eq!(in_schema, latest, "{name} in schema.sql differs from {file}");
    }
}

#[test]
fn the_matcher_sees_a_view_that_differs() {
    let a = "-- c\nCREATE OR REPLACE VIEW v AS SELECT 1 FROM t;";
    let b = "CREATE VIEW v AS\n  SELECT 2\n  FROM t;";
    assert_ne!(view_text(a, "v"), view_text(b, "v"));
    assert_eq!(view_text(a, "v").as_deref(), Some("CREATE VIEW v AS SELECT 1 FROM t;").map(|s| s.trim_end_matches(';')));
    assert_eq!(view_text(a, "w"), None);
}

#[test]
fn ops_live_carries_the_columns_the_now_rows_render() {
    let schema = fs::read_to_string(root().join("schema.sql")).unwrap();
    let live = view_text(&schema, "ops_live").unwrap();
    for col in ["t.persona,", " AS rework", " AS claimable", " AS blocker"] {
        assert!(live.contains(col), "ops_live lacks {col:?}: {live}");
    }
}
