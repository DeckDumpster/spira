//! `admin-store <sub>`: the one door install, deploy and operator tooling reach the beads store
//! through. It runs in the caller's process, never over the socket, and answers every subcommand
//! on the beads server.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::rows::escape;
use crate::admin_drop::Dolt;
use crate::db::Conn;
use crate::{CANNOT_TELL, REFUSED};

pub(crate) const SUBCOMMANDS: &str = "check | ping | config get|set <types.custom|status.custom> [value] | sql [--json] (statement on stdin)";
const CONFIG_KEYS: [&str; 2] = ["types.custom", "status.custom"];
const CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
/// `check`'s exit when the store answered and lacks a column.
const GAPS: i32 = 1;

/// What one subcommand does on the beads server.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Check,
    Ping,
    Sql { statement: String, json: bool },
    ConfigGet(String),
    ConfigSet(String, String),
}

pub(crate) fn ask_of(args: &[String], stdin: &mut dyn FnMut() -> Result<String, String>) -> Result<Ask, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["check"] => Ok(Ask::Check),
        ["ping"] => Ok(Ask::Ping),
        ["config", "get", key] if CONFIG_KEYS.contains(key) => Ok(Ask::ConfigGet(key.to_string())),
        ["config", "set", key, value] if CONFIG_KEYS.contains(key) => Ok(Ask::ConfigSet(key.to_string(), value.to_string())),
        ["sql"] | ["sql", "--json"] => {
            let statement = stdin()?;
            let statement = statement.trim();
            if statement.is_empty() {
                return Err("admin-store sql: the statement is read from stdin and was empty".into());
            }
            Ok(Ask::Sql { statement: statement.to_string(), json: words.len() == 2 })
        }
        _ => Err(format!("admin-store: not a subcommand of the closed list: {SUBCOMMANDS}")),
    }
}

/// `--json`: an array of row objects. Plain: a header, a rule, a line per row, or nothing.
fn render(rows: &[Value], json: bool) -> String {
    if json {
        return format!("{}\n", Value::from(rows.to_vec()));
    }
    let Some(Value::Object(first)) = rows.first() else { return String::new() };
    let line = |cells: Vec<&str>| format!("{}\n", cells.iter().map(|c| c.replace(['\r', '\n'], " ")).collect::<Vec<_>>().join(" | "));
    let mut out = line(first.keys().map(String::as_str).collect());
    out.push_str(&vec!["---"; first.len()].join("+"));
    out.push('\n');
    for row in rows {
        out.push_str(&line(first.keys().map(|c| row[c.as_str()].as_str().unwrap_or_default()).collect()));
    }
    out
}

fn config_get(admin: &Conn, db: &str, key: &str) -> Result<String, String> {
    let rows = admin.rows(&format!("SELECT value FROM {db}.config WHERE `key` = '{}'", escape(key)))?;
    Ok(match rows.first().and_then(|r| r["value"].as_str()) {
        Some(v) => format!("{v}\n"),
        None => format!("{key} (not set)\n"),
    })
}

/// A create reads `types.custom` from the `custom_types` table too where the store has one, so
/// the two are written in one transaction.
fn config_set(admin: &Conn, db: &str, key: &str, value: &str) -> Result<String, String> {
    let mut script = vec!["START TRANSACTION".to_string(), format!("REPLACE INTO {db}.config (`key`, value) VALUES ('{}', '{}')", escape(key), escape(value))];
    if key == "types.custom" {
        let schema = db.trim_matches('`');
        let table = admin.rows(&format!("SELECT 1 FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = 'custom_types'"))?;
        if !table.is_empty() {
            script.push(format!("DELETE FROM {db}.custom_types"));
            let names: BTreeSet<&str> = value.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
            if !names.is_empty() {
                let rows = names.iter().map(|n| format!("('{}')", escape(n))).collect::<Vec<_>>().join(", ");
                script.push(format!("INSERT INTO {db}.custom_types (name) VALUES {rows}"));
            }
        }
    }
    script.push("COMMIT".to_string());
    Dolt::execute(admin, &script.join("; "))?;
    Ok(format!("Set {key} = {value}\n"))
}

/// The administrator on the beads server, with the content database selected so a caller's
/// unqualified table names resolve to it.
fn administrator() -> Result<(Conn, &'static str), String> {
    let settings = crate::content_sql::Settings::from_config()?;
    let db = crate::plan::content_db()?;
    let (user, password) = crate::admin_drop::local_administrator()?;
    let mut admin = Conn::administrator(&settings.host, settings.port, &user, password);
    admin.database = db.trim_matches('`').to_string();
    Ok((admin, db))
}

/// The text a server subcommand prints, or the server's error.
fn answer(admin: &Conn, db: &str, ask: &Ask) -> Result<String, String> {
    match ask {
        Ask::Ping => admin.rows("select 1").map(|rows| render(&rows, false)),
        Ask::Sql { statement, json } => admin.rows(statement).map(|rows| render(&rows, *json)),
        Ask::ConfigGet(key) => config_get(admin, db, key),
        Ask::ConfigSet(key, value) => config_set(admin, db, key, value),
        Ask::Check => Err("not answered by one statement".to_string()),
    }
}

/// Every `table.column` the content reads or the content writer need that `db` lacks.
fn store_gaps(sql: &dyn crate::content_sql::Sql, db: &str) -> Result<Vec<String>, String> {
    let have = crate::content_sql::columns(sql, db).map_err(|f| f.text().to_string())?;
    let mut gaps = crate::plan::missing_columns(&have);
    for gap in crate::admin_writer::missing_columns(&have) {
        if !gaps.contains(&gap) {
            gaps.push(gap);
        }
    }
    Ok(gaps)
}

/// `check`: each gap on its own line; exit 0 none, [`GAPS`] some, [`CANNOT_TELL`] when the store
/// could not be asked. No database is selected, so one the server lacks reads as all gaps.
fn check(mut admin: Conn, db: &str) -> i32 {
    admin.database = String::new();
    match store_gaps(&admin, db) {
        Ok(gaps) => {
            gaps.iter().for_each(|g| println!("{g}"));
            if gaps.is_empty() { 0 } else { GAPS }
        }
        Err(e) => {
            eprintln!("admin-store: the beads store is unreachable at {}:{}: {e}", admin.host, admin.port);
            CANNOT_TELL
        }
    }
}

/// A server's own error is a refusal; anything else leaves unknown whether the statement applied.
fn exit_of(error: &str) -> i32 {
    if error.starts_with("Error ") {
        REFUSED
    } else {
        CANNOT_TELL
    }
}

fn on_server(ask: &Ask) -> i32 {
    let (mut admin, db) = match administrator() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("admin-store: {e}");
            return CANNOT_TELL;
        }
    };
    if matches!(ask, Ask::Check) {
        // The caller's own limit is what tells a slow store from an unreachable one.
        return check(admin, db);
    }
    // A caller's `sql` may be DDL or a backfill over the whole store, so it keeps the administrator's limit.
    if !matches!(ask, Ask::Sql { .. }) {
        admin.io_timeout = CALL_DEADLINE;
    }
    match answer(&admin, db, ask) {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("admin-store: {e}");
            exit_of(&e)
        }
    }
}

/// Runs `args` (the words after `admin-store`).
pub fn run(args: &[String]) -> i32 {
    let mut stdin = || {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map(|_| t).map_err(|e| e.to_string())
    };
    match ask_of(args, &mut stdin) {
        Ok(ask) => on_server(&ask),
        Err(e) => {
            eprintln!("{e}");
            CANNOT_TELL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(t5n9pag)]
    #[test]
    fn admin_store_takes_only_its_closed_list_and_never_the_socket() {
        assert!(crate::client::try_socket(&words(&["admin-store", "check"])).is_none());
        assert_eq!(run(&words(&["delete"])), 2);
        assert_eq!(run(&words(&["config", "get", "issue_prefix"])), 2);
        assert_eq!(run(&words(&["config", "set", "types.custom"])), 2);
    }

    #[test_id(t6ieiyq)]
    #[test]
    fn the_subcommands_that_asked_bd_are_not_on_the_closed_list() {
        for gone in ["migrate", "migrate-status", "version", "dolt-start", "init"] {
            assert!(ask_of(&words(&[gone]), &mut || Err("no stdin".to_string())).is_err(), "{gone}");
        }
    }

    #[test_id(th3i9bi)]
    #[test]
    fn the_store_callers_name_no_bd() {
        let files: [(&str, &str); 6] = [
            ("spira/deploy.sh", include_str!("../../spira/deploy.sh")),
            ("spira/schema-apply.sh", include_str!("../../spira/schema-apply.sh")),
            ("spira/schema.sh", include_str!("../../spira/schema.sh")),
            ("spira/pre-activate.sh", include_str!("../../spira/pre-activate.sh")),
            ("env_bootstrap.rs", include_str!("../../spira-config/src/env_bootstrap.rs")),
            ("tsd-lifecycle-export", include_str!("../../tsd-lifecycle-export/src/main.rs")),
        ];
        for (name, text) in files {
            let hits = bd_command_words(text);
            assert!(hits.is_empty(), "{name} still names bd: {hits:?}");
        }
    }

    fn bd_command_words(text: &str) -> Vec<&str> {
        let banned = ["bd -C", "\"$OUT\" -C", "\"${SPIRA_BD:", "command -v bd", "Command::new(\"bd-embedded\")", "bounded::bounded(&bd)"];
        text.lines().filter(|l| banned.iter().any(|b| l.contains(b)) && !l.trim_start().starts_with("//") && !l.trim_start().starts_with('#')).collect()
    }

    #[test_id(tkqoib6)]
    #[test]
    fn the_bd_matcher_finds_a_planted_offender() {
        let planted = "x=$(timeout 30 bd -C \"$DB\" migrate schema)\nCommand::new(\"bd-embedded\")\n# bd -C in a comment\n";
        assert_eq!(bd_command_words(planted).len(), 2);
    }

    #[test_id(tgp71dk)]
    #[test]
    fn only_the_servers_own_error_is_a_refusal_and_a_lost_answer_cannot_tell() {
        assert_eq!(exit_of("Error 1146 (HY000): table not found: t"), REFUSED);
        for lost in ["connecting to 127.0.0.1:1: Connection refused", crate::db::DEADLINE_MESSAGE, "a statement lost a race"] {
            assert_eq!(exit_of(lost), CANNOT_TELL, "{lost}");
        }
    }

    #[test_id(tpn32b8)]
    #[test]
    fn an_empty_sql_statement_is_refused() {
        assert!(ask_of(&words(&["sql"]), &mut || Ok("  \n".to_string())).is_err());
    }
}
