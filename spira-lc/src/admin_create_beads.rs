//! `admin-create-beads --prefix P --host H --port N --database D`: the beads store a fresh install
//! gets, made from the checked-in DDL, with `<SPIRA_DB>/.beads/metadata.json` written last so that
//! its presence means the store is whole. A complete store is left as it is; a database holding
//! only part of one is refused, never overwritten.

use std::collections::BTreeSet;
use std::path::Path;

use crate::admin_drop::Dolt;
use crate::admin_reader::plain;
use crate::db::Conn;
use crate::rows::escape;
use crate::{flag, CANNOT_TELL, REFUSED};

const SCHEMA: &str = include_str!("../beads-schema.sql");

const USAGE: &str = "admin-create-beads: usage admin-create-beads --prefix P --host H --port N --database D";

/// The tables and views the DDL makes: what a complete store holds.
fn required() -> BTreeSet<&'static str> {
    SCHEMA
        .lines()
        .filter_map(|l| l.strip_prefix("CREATE TABLE `").map(|r| r.split('`').next()).or_else(|| l.strip_prefix("CREATE OR REPLACE VIEW ").map(|r| r.split_whitespace().next())).flatten())
        .collect()
}

/// The DDL's statements, each ended by a `;` at the end of a line: Dolt does not split the
/// whole file itself.
fn statements() -> Vec<String> {
    let mut all = Vec::new();
    let mut open = String::new();
    for line in SCHEMA.lines().filter(|l| !l.starts_with("--")) {
        open.push_str(line);
        open.push('\n');
        if line.trim_end().ends_with(';') {
            all.push(std::mem::take(&mut open).trim_end().trim_end_matches(';').to_string());
        }
    }
    all
}

#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// No database, or one holding no table: nothing can be lost by creating into it.
    Nothing,
    Complete,
}

#[derive(Debug, PartialEq, Eq)]
enum Fail {
    Refused(String),
    CannotTell(String),
}

impl From<String> for Fail {
    fn from(e: String) -> Fail {
        Fail::CannotTell(e)
    }
}

fn text<'a>(row: &'a serde_json::Value, column: &str) -> &'a str {
    row.get(column).and_then(serde_json::Value::as_str).unwrap_or_default()
}

fn inspect(server: &dyn Dolt, database: &str, prefix: &str) -> Result<Found, Fail> {
    let rows = server.rows(&format!("SELECT TABLE_NAME AS name FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{}'", escape(database)))?;
    let present: BTreeSet<&str> = rows.iter().map(|r| text(r, "name")).collect();
    if present.is_empty() {
        return Ok(Found::Nothing);
    }
    let partial = |why: String| Fail::Refused(format!("database {database} is a partial beads store: {why}; it is not overwritten: drop it or name another"));
    let missing: Vec<&str> = required().into_iter().filter(|t| !present.contains(t)).collect();
    if !missing.is_empty() {
        return Err(partial(format!("it lacks {}", missing.join(", "))));
    }
    let held = server.rows(&format!("SELECT value FROM `{database}`.config WHERE `key` = 'issue_prefix'"))?;
    match held.first().map(|r| text(r, "value")).unwrap_or_default() {
        "" => Err(partial("it has no issue_prefix".to_string())),
        p if p == prefix => Ok(Found::Complete),
        p => Err(Fail::Refused(format!("database {database} holds issue_prefix {p:?}, not {prefix:?}"))),
    }
}

/// The database `<db>/.beads/metadata.json` names, if the file is there.
fn recorded(db: &Path) -> Result<Option<String>, Fail> {
    if !db.join(".beads/metadata.json").exists() {
        return Ok(None);
    }
    let quoted = crate::plan::database_of(db).map_err(Fail::Refused)?;
    Ok(Some(quoted.trim_matches('`').to_string()))
}

fn write_metadata(db: &Path, host: &str, port: u16, database: &str) -> Result<(), String> {
    let beads = db.join(".beads");
    let meta = serde_json::json!({"database": "dolt", "backend": "dolt", "dolt_mode": "server", "dolt_server_host": host, "dolt_server_port": port, "dolt_database": database});
    let body = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    let whole = beads.join("metadata.json.new");
    std::fs::create_dir_all(&beads)
        .and_then(|()| std::fs::write(&whole, format!("{body}\n")))
        .and_then(|()| std::fs::rename(&whole, beads.join("metadata.json")))
        .map_err(|e| format!("writing {}/metadata.json: {e}", beads.display()))
}

struct Ask<'a> {
    prefix: &'a str,
    host: &'a str,
    port: u16,
    database: &'a str,
}

/// `server` has no database selected; `inside` has `ask.database`, for the DDL's unqualified names.
fn create(server: &dyn Dolt, inside: &dyn Dolt, ask: &Ask, db: &Path, creating: &mut dyn FnMut()) -> Result<String, Fail> {
    let Ask { prefix, host, port, database } = *ask;
    let found = inspect(server, database, prefix)?;
    if let Some(other) = recorded(db)?.filter(|named| named != database) {
        return Err(Fail::Refused(format!("{}/.beads/metadata.json names database {other}, not {database}", db.display())));
    }
    let said = match found {
        Found::Complete => format!("database {database} already holds a complete beads store: nothing changed"),
        Found::Nothing => {
            creating();
            server.execute(&format!("CREATE DATABASE IF NOT EXISTS `{database}`"))?;
            // One session throughout, so the DDL's SETs hold for the statements after them.
            for statement in statements() {
                inside.execute(&statement)?;
            }
            inside.execute(&format!("REPLACE INTO config (`key`, value) VALUES ('issue_prefix', '{}')", escape(prefix)))?;
            inside.execute("CALL DOLT_COMMIT('-Am', 'spira: create the beads store')")?;
            format!("created the beads store in database {database}")
        }
    };
    if recorded(db)?.is_none() {
        write_metadata(db, host, port, database)?;
    }
    Ok(said)
}

pub fn run(args: &[String]) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-create-beads: {why}"));
    let (Some(prefix), Some(host), Some(port), Some(database)) = (flag(args, "--prefix"), flag(args, "--host"), flag(args, "--port"), flag(args, "--database")) else {
        return (CANNOT_TELL, USAGE.to_string());
    };
    let Ok(port) = port.parse::<u16>() else {
        return cannot(format!("--port {port:?} is not a port"));
    };
    for (what, value) in [("--prefix", &prefix), ("--database", &database)] {
        if !plain(value) {
            return (REFUSED, format!("admin-create-beads: {what} {value:?} is not a plain name"));
        }
    }
    let db = match spira_config::process::cfg("SPIRA_DB") {
        Ok(db) if !db.trim().is_empty() => db,
        Ok(_) => return cannot("SPIRA_DB is empty: there is nowhere to record the store".to_string()),
        Err(e) => return cannot(e),
    };
    let (user, password) = match crate::admin_drop::local_administrator() {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let server = Conn::administrator(&host, port, &user, password.clone());
    let mut inside = Conn::administrator(&host, port, &user, password);
    inside.database = database.clone();
    // Said before anything is made, so a caller knows a failure may have left this run's own
    // partial database, even when the run is killed.
    let mut creating = || eprintln!("{}{database}", spira_store::lifecycle_row::CREATING_BEADS_DATABASE);
    let ask = Ask { prefix: &prefix, host: &host, port, database: &database };
    match create(&server, &inside, &ask, Path::new(db.trim()), &mut creating) {
        Ok(said) => (0, format!("admin-create-beads: {said}")),
        Err(Fail::Refused(e)) => (REFUSED, format!("admin-create-beads: {e}")),
        Err(Fail::CannotTell(e)) => cannot(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::cell::RefCell;

    struct Server {
        tables: Vec<&'static str>,
        prefix: &'static str,
        seen: RefCell<Vec<String>>,
    }

    impl Dolt for Server {
        fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
            Ok(if sql.contains("information_schema") { self.tables.iter().map(|t| json!({ "name": t })).collect() } else { vec![json!({ "value": self.prefix })] })
        }
        fn execute(&self, sql: &str) -> Result<(), String> {
            self.seen.borrow_mut().push(sql.to_string());
            Ok(())
        }
    }

    fn server(tables: Vec<&'static str>, prefix: &'static str) -> Server {
        Server { tables, prefix, seen: RefCell::new(vec![]) }
    }

    fn run_on(s: &Server, db: &Path) -> (Result<String, Fail>, bool) {
        let mut said = false;
        let out = create(s, s, &Ask { prefix: "zz", host: "h", port: 1, database: "beads" }, db, &mut || said = true);
        (out, said)
    }

    #[test_id(tx54s1p)]
    #[test]
    fn the_ddl_names_every_table_and_view_of_the_store() {
        let all = required();
        for t in ["issues", "labels", "dependencies", "comments", "config", "events", "ready_issues", "blocked_issues"] {
            assert!(all.contains(t), "{t} not in {all:?}");
        }
    }

    #[test_id(ti8n6w1)]
    #[test]
    fn a_partial_store_is_refused_by_name_and_nothing_is_written() {
        let d = testkit::TempDir::new("lc-create-partial");
        for (tables, prefix, why) in [(vec!["issues"], "zz", "lacks"), (required().into_iter().collect(), "", "no issue_prefix"), (required().into_iter().collect(), "aa", "\"aa\"")] {
            let s = server(tables, prefix);
            let (out, creating) = run_on(&s, d.path());
            assert!(matches!(&out, Err(Fail::Refused(e)) if e.contains("beads") && e.contains(why)), "{why}: {out:?}");
            assert!(!creating && s.seen.borrow().is_empty() && !d.join(".beads").exists(), "{why}");
        }
    }

    #[test_id(t508yqr)]
    #[test]
    fn metadata_naming_another_database_is_refused_before_the_server_is_changed() {
        let d = testkit::TempDir::new("lc-create-other");
        write_metadata(d.path(), "h", 1, "elsewhere").unwrap();
        let s = server(vec![], "");
        let (out, creating) = run_on(&s, d.path());
        assert!(matches!(&out, Err(Fail::Refused(e)) if e.contains("elsewhere")), "{out:?}");
        assert!(!creating && s.seen.borrow().is_empty());
    }

    #[test_id(tul34tg)]
    #[test]
    fn a_database_holding_no_table_is_announced_then_filled_and_committed_before_it_is_recorded() {
        let d = testkit::TempDir::new("lc-create-fresh");
        let s = server(vec![], "");
        let (out, creating) = run_on(&s, d.path());
        assert!(out.is_ok() && creating, "{out:?}");
        let seen = s.seen.borrow();
        let ddl = statements();
        assert!(seen[0].starts_with("CREATE DATABASE") && seen[1..=ddl.len()] == ddl, "{seen:?}");
        assert!(seen[ddl.len() + 1].contains("'zz'") && seen[ddl.len() + 2].contains("DOLT_COMMIT") && seen.len() == ddl.len() + 3, "{seen:?}");
        assert_eq!(recorded(d.path()), Ok(Some("beads".to_string())));
    }
}
