//! `admin-migrate`: the ledgered application of `lifecycle/migrations/*.sql`. A migration
//! such as `ALTER TABLE ... ADD COLUMN` is not idempotent, so what has run is recorded in
//! `schema_migration` and never replayed; a failure stops the run and exits non-zero, so a
//! release is never activated against a schema it cannot read.

use crate::db::{now_epoch, Conn};
use crate::{rows, CANNOT_TELL};

const LEDGER_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_migration (name VARCHAR(255) NOT NULL PRIMARY KEY, applied_at BIGINT NOT NULL);";

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    if args.iter().any(|a| a == "--if-enforced") && !spira_config::lifecycle_enforce(None) {
        return (
            0,
            "admin-migrate: lifecycle_enforce is off — nothing to migrate".into(),
        );
    }
    let args: Vec<String> = args
        .iter()
        .filter(|a| *a != "--if-enforced")
        .cloned()
        .collect();
    match args.first().map(String::as_str) {
        Some("--baseline") => baseline(&args[1..], conn),
        Some(dir) => apply_pending(dir, conn),
        None => (
            CANNOT_TELL,
            "admin-migrate: missing <migrations-dir> or --baseline <name>...".into(),
        ),
    }
}

fn record(name: &str) -> String {
    format!(
        "INSERT IGNORE INTO schema_migration (name, applied_at) VALUES ('{}', {});",
        rows::escape(name),
        now_epoch()
    )
}

fn baseline(names: &[String], conn: &Conn) -> (i32, String) {
    let mut script = format!("USE {};\n{LEDGER_DDL}\n", conn.database);
    for n in names {
        script.push_str(&record(n));
        script.push('\n');
    }
    match conn.apply_ddl(&script) {
        Ok(()) => (
            0,
            format!("admin-migrate: baselined {} migration(s)", names.len()),
        ),
        Err(e) => (
            CANNOT_TELL,
            format!("admin-migrate: baseline failed: {e:?}"),
        ),
    }
}

fn apply_pending(dir: &str, conn: &Conn) -> (i32, String) {
    let mut files: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".sql"))
            .collect(),
        Err(e) => return (CANNOT_TELL, format!("admin-migrate: reading {dir}: {e}")),
    };
    files.sort();
    let applied: Vec<String> = match conn.query("SELECT name FROM schema_migration") {
        Ok(r) => r.iter().filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(String::from)).collect(),
        Err(e) => {
            return (
                CANNOT_TELL,
                format!("admin-migrate: no readable migrations ledger ({e:?}); apply schema.sql, or `admin-migrate --baseline <name>...` on a database already migrated by hand"),
            )
        }
    };
    let mut out = Vec::new();
    for name in files.iter().filter(|f| !applied.contains(f)) {
        let body = match std::fs::read_to_string(std::path::Path::new(dir).join(name)) {
            Ok(b) => b,
            Err(e) => return (CANNOT_TELL, format!("admin-migrate: reading {name}: {e}")),
        };
        let script = format!("USE {};\n{body}\n{}\n", conn.database, record(name));
        if let Err(e) = conn.apply_ddl(&script) {
            return (
                CANNOT_TELL,
                format!("admin-migrate: {name} FAILED, nothing after it was run: {e:?}"),
            );
        }
        out.push(format!("admin-migrate: applied {name}"));
    }
    if out.is_empty() {
        out.push("admin-migrate: no pending migrations".into());
    }
    (0, out.join("\n"))
}
