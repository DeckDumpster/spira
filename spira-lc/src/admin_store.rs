//! `admin-store <sub>`: the one door install, deploy and operator tooling reach the beads store
//! through. It runs in the caller's process, never over the socket. `sql`, `ping` and `config`
//! run on the beads server; the rest exec bd, passing its output and exit code through unchanged.

use std::collections::BTreeSet;
use std::os::unix::process::CommandExt;
use crate::proc::Command;

use serde_json::Value;

use crate::rows::escape;
use crate::admin_drop::Dolt;
use crate::db::Conn;
use crate::{CANNOT_TELL, REFUSED};

pub(crate) const SUBCOMMANDS: &str = "migrate | migrate-status | ping | version | dolt-start | config get|set <types.custom|status.custom> [value] | sql [--json] (statement on stdin)";
const CONFIG_KEYS: [&str; 2] = ["types.custom", "status.custom"];
const CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// What one subcommand asks of bd: its argv after `-C <db>`, and whether the caller wants bd's version line followed by the binary's path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub argv: Vec<String>,
    pub with_path: bool,
}

/// What one subcommand does: exec bd, or run on the beads server.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Bd(Plan),
    Ping,
    Sql { statement: String, json: bool },
    ConfigGet(String),
    ConfigSet(String, String),
}

fn plan(argv: &[&str], with_path: bool) -> Ask {
    Ask::Bd(Plan { argv: argv.iter().map(|s| s.to_string()).collect(), with_path })
}

pub(crate) fn ask_of(args: &[String], stdin: &mut dyn FnMut() -> Result<String, String>) -> Result<Ask, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["migrate"] => Ok(plan(&["migrate", "schema"], false)),
        ["migrate-status"] => Ok(plan(&["migrate", "status"], false)),
        ["ping"] => Ok(Ask::Ping),
        ["version"] => Ok(plan(&["version"], true)),
        ["dolt-start"] => Ok(plan(&["dolt", "start"], false)),
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
        Ask::Bd(_) => Err("a bd subcommand is not run on the server".to_string()),
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

pub(crate) fn command(p: &Plan, bd: &str, db: &str) -> Result<Command, String> {
    let mut cmd = crate::proc::command(bd)?;
    if !db.is_empty() {
        cmd.args(["-C", db]);
    }
    cmd.args(&p.argv);
    Ok(cmd)
}

fn resolved_path(bd: &str) -> String {
    if bd.contains('/') {
        return bd.to_string();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(|d| std::path::Path::new(d).join(bd))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// Runs `args` (the words after `admin-store`). A bd subcommand returns only on a refusal or a
/// failure to start bd; otherwise bd takes this process over, so a caller's `timeout` reaches it.
pub fn run(args: &[String]) -> i32 {
    let mut stdin = || {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map(|_| t).map_err(|e| e.to_string())
    };
    let p = match ask_of(args, &mut stdin) {
        Ok(Ask::Bd(p)) => p,
        Ok(ask) => return on_server(&ask),
        Err(e) => {
            eprintln!("{e}");
            return CANNOT_TELL;
        }
    };
    let (bd, db) = match (crate::bd::bd_bin(), spira_config::process::cfg("SPIRA_DB")) {
        (Ok(b), Ok(d)) => (b, d),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("admin-store: {e}");
            return CANNOT_TELL;
        }
    };
    let mut cmd = match command(&p, &bd, &db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("admin-store: {e}");
            return CANNOT_TELL;
        }
    };
    if p.with_path {
        return match state_store::phases::child(&bd, || cmd.output()) {
            Ok(o) => {
                let first = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string();
                println!("{first}\n{}", resolved_path(&bd));
                eprint!("{}", String::from_utf8_lossy(&o.stderr));
                o.status.code().unwrap_or(1)
            }
            Err(e) => {
                eprintln!("admin-store: running {bd}: {e}");
                CANNOT_TELL
            }
        };
    }
    let e = cmd.exec();
    eprintln!("admin-store: running {bd}: {e}");
    CANNOT_TELL
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
        assert!(crate::client::try_socket(&words(&["admin-store", "migrate"])).is_none());
        assert_eq!(run(&words(&["delete"])), 2);
        assert_eq!(run(&words(&["config", "get", "issue_prefix"])), 2);
        assert_eq!(run(&words(&["config", "set", "types.custom"])), 2);
    }

    fn bd_plan(args: &[&str]) -> Plan {
        match ask_of(&words(args), &mut || Err("no stdin".to_string())) {
            Ok(Ask::Bd(p)) => p,
            other => panic!("{args:?} is not bd's: {other:?}"),
        }
    }

    #[test_id(twxoo9a)]
    #[test]
    fn admin_store_execs_bd_with_the_stores_argv() {
        let dir = testkit::TempDir::new("admin-store-stub");
        let log = dir.join("argv");
        let stub = dir.join("bd");
        testkit::write_exe(&stub, &format!("#!/bin/sh\npwd >> '{0}'\nfor a in \"$@\"; do printf '[%s]' \"$a\" >> '{0}'; done\necho >> '{0}'\n", log.display()));
        let (bd, db) = (stub.to_str().unwrap(), dir.join("store"));
        std::fs::create_dir(&db).unwrap();
        let db = db.to_str().unwrap();
        for args in [vec!["migrate"], vec!["dolt-start"]] {
            assert!(command(&bd_plan(&args), bd, db).unwrap().status().unwrap().success());
        }
        let seen = std::fs::read_to_string(&log).unwrap();
        let want = [
            format!("[-C][{db}][migrate][schema]"),
            format!("[-C][{db}][dolt][start]"),
        ];
        for w in &want {
            assert!(seen.contains(w), "{w} not in {seen}");
        }
    }

    #[test_id(th3i9bi)]
    #[test]
    fn the_store_callers_name_no_bd() {
        let files: [(&str, &str); 7] = [
            ("spira/deploy.sh", include_str!("../../spira/deploy.sh")),
            ("spira/bd-pin.sh", include_str!("../../spira/bd-pin.sh")),
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
        let banned = ["bd -C", "\"${SPIRA_BD:", "command -v bd", "Command::new(\"bd-embedded\")", "bounded::bounded(&bd)"];
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

    #[test_id(t27rs3y)]
    #[test]
    fn version_prints_bds_first_line_then_its_path_and_a_failure_exits_2() {
        let dir = testkit::TempDir::new("admin-store-version");
        let stub = dir.join("bd");
        testkit::write_exe(&stub, "#!/bin/sh\necho 'bd version 9.9'\necho second\n");
        let p = bd_plan(&["version"]);
        let out = command(&p, stub.to_str().unwrap(), "").unwrap().output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("bd version 9.9"));
        assert_eq!(resolved_path(stub.to_str().unwrap()), stub.to_str().unwrap());
        let mut empty = || Ok("  \n".to_string());
        assert!(ask_of(&["sql".to_string()], &mut empty).is_err());
    }
}
