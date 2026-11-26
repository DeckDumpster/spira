//! `admin-provision-writer`: the SQL user lc-serve applies content writes as (the reader's name
//! without its `_ro` suffix), its credential file, write grants on exactly the tables `content_write`
//! changes, SELECT on the rest, and EXECUTE for the Dolt commit procedure. A re-run re-issues the same statements and changes nothing.

use std::collections::BTreeSet;

use crate::admin_drop::Dolt;
use crate::admin_reader::{ensure_secret, literal_safe, plain};
use crate::db::Conn;
use crate::{CANNOT_TELL, REFUSED};

/// The tables a content write changes.
const WRITTEN: [&str; 7] = ["issues", "labels", "comments", "events", "dependencies", "child_counters", "config"];

/// The tables a content write changes when the store has them: leases and the coordination
/// cells are local state older stores do not carry.
const WRITTEN_WHEN_PRESENT: [&str; 2] = ["leases", "local_metadata"];

fn statements(db: &str, user: &str, password: &str, tables: &[&str], readable: &[String]) -> Vec<String> {
    let mut sql = vec![format!("CREATE USER IF NOT EXISTS '{user}'@'%' IDENTIFIED BY '{password}'"), format!("ALTER USER '{user}'@'%' IDENTIFIED BY '{password}'")];
    sql.extend(tables.iter().map(|t| format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {db}.`{t}` TO '{user}'@'%'")));
    sql.push(format!("GRANT EXECUTE ON {db}.* TO '{user}'@'%'"));
    sql.extend(readable.iter().map(|t| format!("GRANT SELECT ON {db}.`{t}` TO '{user}'@'%'")));
    sql
}

/// The tables the writer needs write grants on in a store holding `present`.
pub fn written(present: &BTreeSet<String>) -> Result<Vec<&'static str>, String> {
    if let Some(missing) = WRITTEN.iter().find(|t| !present.contains(**t)) {
        return Err(format!("the content database has no {missing} table: it is not a beads store"));
    }
    Ok(WRITTEN.iter().chain(WRITTEN_WHEN_PRESENT.iter().filter(|t| present.contains(**t))).copied().collect())
}

const TABLES_SQL: &str = "SELECT TABLE_NAME AS name FROM information_schema.TABLES WHERE TABLE_SCHEMA = ";

fn names(rows: &[serde_json::Value]) -> BTreeSet<String> {
    rows.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect()
}

const PRIVILEGES: [&str; 4] = ["SELECT", "INSERT", "UPDATE", "DELETE"];

/// Each table the writer must write that no grant in `grants` (one `GRANT <privilege> ON <object> TO <user>` line per privilege held) covers with
/// all of SELECT, INSERT, UPDATE and DELETE. Lines that name nothing on `db` are not a verdict.
pub(crate) fn missing_writes(grants: &[String], db: &str, present: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let db = db.trim_matches('`');
    let wanted = written(present)?;
    let mut named = false;
    let mut held: Vec<(String, BTreeSet<&'static str>)> = Vec::new();
    for line in grants {
        let Some(rest) = line.trim().strip_prefix("GRANT ") else { continue };
        let Some((privileges, rest)) = rest.split_once(" ON ") else { continue };
        let object = rest.split(" TO ").next().unwrap_or(rest).replace('`', "").replace("\\_", "_");
        let Some((on_db, on_table)) = object.split_once('.') else { continue };
        if on_db != "*" && on_db != db {
            continue;
        }
        named = true;
        let upper = privileges.to_ascii_uppercase();
        let given: BTreeSet<&'static str> = if upper.trim() == "ALL" || upper.trim() == "ALL PRIVILEGES" {
            PRIVILEGES.into_iter().collect()
        } else {
            PRIVILEGES.into_iter().filter(|p| upper.split(',').any(|g| g.trim() == *p)).collect()
        };
        held.push((on_table.to_string(), given));
    }
    if !named {
        return Err(format!("cannot check: SHOW GRANTS named no grant on {db}"));
    }
    Ok(wanted
        .into_iter()
        .filter(|t| {
            let have: BTreeSet<&str> = held.iter().filter(|(on, _)| on == "*" || on == t).flat_map(|(_, g)| g.iter().copied()).collect();
            !PRIVILEGES.iter().all(|p| have.contains(p))
        })
        .map(str::to_string)
        .collect())
}

/// The tables the code writes that the configured writer lacks grants on, read on the writer's
/// own connection; `None` when no writer is configured. Reads only, from information_schema's privilege tables, which a user sees for itself where SHOW GRANTS needs `mysql` access.
pub(crate) fn check_grants() -> Option<Result<Vec<String>, String>> {
    let w = match crate::content_write::writer()? {
        Ok(w) => w,
        Err(e) => return Some(Err(e)),
    };
    let schema = w.conn.database.trim_matches('`').to_string();
    let run = || -> Result<Vec<String>, String> {
        let present = names(&w.conn.query(&format!("{TABLES_SQL}'{}'", crate::rows::escape(&schema))).map_err(|e| format!("{e:?}"))?);
        let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let held = |sql: String, object: &dyn Fn(&serde_json::Value) -> String| -> Result<Vec<String>, String> {
            let rows = w.conn.query(&sql).map_err(|e| format!("{e:?}"))?;
            Ok(rows.iter().map(|r| format!("GRANT {} ON {} TO writer", text(r, "PRIVILEGE_TYPE"), object(r))).collect())
        };
        let lit = crate::rows::escape(&schema);
        let mut grants = held(format!("SELECT PRIVILEGE_TYPE, TABLE_NAME FROM information_schema.TABLE_PRIVILEGES WHERE TABLE_SCHEMA = '{lit}'"), &|r| format!("`{schema}`.`{}`", text(r, "TABLE_NAME")))?;
        grants.extend(held(format!("SELECT PRIVILEGE_TYPE FROM information_schema.SCHEMA_PRIVILEGES WHERE TABLE_SCHEMA = '{lit}'"), &|_| format!("`{schema}`.*"))?);
        grants.extend(held("SELECT PRIVILEGE_TYPE FROM information_schema.USER_PRIVILEGES".to_string(), &|_| "*.*".to_string())?);
        missing_writes(&grants, &schema, &present)
    };
    Some(run())
}

type Verdict = Option<Result<Vec<String>, String>>;

static CHECKED: std::sync::OnceLock<(u64, Verdict)> = std::sync::OnceLock::new();

/// Runs the check and keeps its verdict, with the time it was taken, for `grants_json`.
pub(crate) fn record_check() -> Verdict {
    let verdict = check_grants();
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    CHECKED.get_or_init(|| (at, verdict)).1.clone()
}

/// The kept verdict as the `writer-grants` verb answers it; no store read.
pub(crate) fn grants_json() -> String {
    answer(CHECKED.get())
}

fn answer(kept: Option<&(u64, Verdict)>) -> String {
    let v = match kept {
        None => serde_json::json!({"checked_at": null, "writer": false, "missing": [], "error": null}),
        Some((at, verdict)) => {
            let (missing, error) = match verdict {
                Some(Ok(m)) => (m.clone(), None),
                Some(Err(e)) => (vec![], Some(e.clone())),
                None => (vec![], None),
            };
            serde_json::json!({"checked_at": at, "writer": verdict.is_some(), "missing": missing, "error": error})
        }
    };
    v.to_string()
}

fn provision(admin: &dyn Dolt, db: &str, user: &str, password: &str) -> Result<String, String> {
    let schema = db.trim_matches('`');
    let present = names(&admin.rows(&format!("{TABLES_SQL}'{schema}'"))?);
    let written = written(&present)?;
    // Dolt checks a write against the whole foreign-key graph around issues, so the writer reads every table.
    let referencing: Vec<String> = present.iter().filter(|t| !written.contains(&t.as_str())).cloned().collect();
    for sql in statements(db, user, password, &written, &referencing) {
        admin.execute(&sql).map_err(|e| e.replace(password, "<redacted>"))?;
    }
    Ok(format!("{user} writes {} of {schema} and reads the {} others", written.join(", "), referencing.len()))
}

pub fn run(_args: &[String]) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-provision-writer: {why}"));
    let settings = match crate::content_sql::Settings::from_config() {
        Ok(s) => s,
        Err(e) => return cannot(e),
    };
    let db = match crate::plan::content_db() {
        Ok(d) => d.to_string(),
        Err(e) => return cannot(e),
    };
    let Some(user) = settings.user.strip_suffix("_ro").map(str::to_string).filter(|u| plain(u)) else {
        return (REFUSED, format!("admin-provision-writer: {:?} is the reader's name without a `_ro` suffix to derive the writer from", settings.user));
    };
    let file = match spira_config::process::cfg("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE") {
        Ok(f) if !f.trim().is_empty() => f,
        Ok(_) => return (0, "admin-provision-writer: no writer is configured (SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE is empty): nothing changed".to_string()),
        Err(e) => return cannot(e),
    };
    let (admin_user, admin_password) = match crate::admin_drop::local_administrator() {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let made = match ensure_secret(&file) {
        Ok(m) => m,
        Err(e) => return cannot(e),
    };
    let password = match std::fs::read_to_string(&file) {
        Ok(p) => p.trim().to_string(),
        Err(e) => return cannot(format!("reading {file}: {e}")),
    };
    if !literal_safe(&password) {
        return (REFUSED, format!("admin-provision-writer: the secret in {file} has a character that cannot sit in a SQL string"));
    }
    let admin = Conn::administrator(&settings.host, settings.port, &admin_user, admin_password);
    match provision(&admin, &db, &user, &password) {
        Ok(msg) => (0, format!("admin-provision-writer: {msg}{}", if made { format!("; wrote {file}") } else { String::new() })),
        Err(e) => cannot(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::cell::RefCell;

    #[test_id(tagzzkj)]
    #[test]
    fn the_writer_grants_answer_tells_not_yet_checked_no_writer_missing_and_cannot_apart() {
        let parse = |k: Option<&(u64, Verdict)>| serde_json::from_str::<Value>(&answer(k)).unwrap();
        assert_eq!(parse(None), json!({"checked_at": null, "writer": false, "missing": [], "error": null}));
        assert_eq!(parse(Some(&(7, None))), json!({"checked_at": 7, "writer": false, "missing": [], "error": null}));
        assert_eq!(
            parse(Some(&(7, Some(Ok(vec!["dependencies".into()]))))),
            json!({"checked_at": 7, "writer": true, "missing": ["dependencies"], "error": null})
        );
        assert_eq!(
            parse(Some(&(7, Some(Err("no route".into()))))),
            json!({"checked_at": 7, "writer": true, "missing": [], "error": "no route"})
        );
    }

    struct Server {
        tables: Vec<&'static str>,
        seen: RefCell<Vec<String>>,
    }

    impl Dolt for Server {
        fn rows(&self, _sql: &str) -> Result<Vec<Value>, String> {
            Ok(self.tables.iter().map(|t| json!({ "name": t })).collect())
        }
        fn execute(&self, sql: &str) -> Result<(), String> {
            self.seen.borrow_mut().push(sql.to_string());
            Ok(())
        }
    }

    #[test_id(ttct17b)]
    #[test]
    fn the_writer_gets_its_user_and_write_grants_on_the_tables_content_writes_change() {
        let s = Server { tables: vec!["issues", "labels", "comments", "events", "dependencies", "child_counters", "leases", "config", "wisps"], seen: RefCell::new(vec![]) };
        provision(&s, "`beadsdb`", "wr", "s3cret").unwrap();
        let seen = s.seen.borrow();
        assert!(seen[0].starts_with("CREATE USER IF NOT EXISTS 'wr'@'%'") && seen[1].starts_with("ALTER USER 'wr'@'%'"), "{seen:?}");
        let grants: Vec<&String> = seen.iter().filter(|q| q.starts_with("GRANT")).collect();
        assert!(grants.contains(&&"GRANT EXECUTE ON `beadsdb`.* TO 'wr'@'%'".to_string()), "the Dolt commit is a procedure: {grants:?}");
        let grants: Vec<&String> = grants.into_iter().filter(|g| !g.contains("EXECUTE")).collect();
        let (write, read): (Vec<&&String>, Vec<&&String>) = grants.iter().partition(|g| g.contains("INSERT"));
        let written: Vec<&str> = write.iter().map(|g| g.split('`').nth(3).unwrap()).collect();
        assert_eq!(written, ["issues", "labels", "comments", "events", "dependencies", "child_counters", "config", "leases"], "a table the store lacks is not granted");
        assert!(write.iter().all(|g| g.starts_with("GRANT SELECT, INSERT, UPDATE, DELETE ON `beadsdb`.`") && g.ends_with("TO 'wr'@'%'")), "{grants:?}");
        assert_eq!(read.iter().map(|g| g.as_str()).collect::<Vec<_>>(), ["GRANT SELECT ON `beadsdb`.`wisps` TO 'wr'@'%'"], "every other table is readable, not writable");
        assert!(!seen.iter().any(|q| q.contains(" ALL")), "no blanket grant");
    }

    fn present(tables: &[&str]) -> BTreeSet<String> {
        tables.iter().map(|t| t.to_string()).collect()
    }

    const ALL: [&str; 9] = ["issues", "labels", "comments", "events", "dependencies", "child_counters", "config", "leases", "local_metadata"];
    const FOUR: &str = "SELECT, INSERT, UPDATE, DELETE";

    fn grants_on(db: &str, except: &[&str], privileges: &str) -> Vec<String> {
        let mut g = vec![format!("GRANT SELECT ON `{db}`.`wisps` TO `wr`@`%`")];
        g.extend(ALL.iter().filter(|t| !except.contains(t)).map(|t| format!("GRANT {privileges} ON `{db}`.`{t}` TO `wr`@`%`")));
        g
    }

    #[test_id(twdt52q)]
    #[test]
    fn a_writer_holding_every_written_table_is_missing_none() {
        assert_eq!(missing_writes(&grants_on("d", &[], FOUR), "`d`", &present(&ALL)), Ok(vec![]));
    }

    #[test_id(tkavs99)]
    #[test]
    fn a_writer_without_dependencies_is_missing_exactly_that() {
        assert_eq!(missing_writes(&grants_on("d", &["dependencies"], FOUR), "d", &present(&ALL)), Ok(vec!["dependencies".to_string()]));
    }

    #[test_id(toyeydy)]
    #[test]
    fn select_alone_on_a_present_table_is_missing_it() {
        let mut g = grants_on("d", &["leases"], FOUR);
        g.push("GRANT SELECT ON `d`.`leases` TO `wr`@`%`".into());
        assert_eq!(missing_writes(&g, "d", &present(&ALL)), Ok(vec!["leases".to_string()]));
    }

    #[test_id(two2xbi)]
    #[test]
    fn a_table_the_store_lacks_is_not_required() {
        let some: Vec<&str> = ALL.iter().copied().filter(|t| *t != "local_metadata").collect();
        assert_eq!(missing_writes(&grants_on("d", &["local_metadata"], FOUR), "d", &present(&some)), Ok(vec![]));
    }

    #[test_id(tz4b59l)]
    #[test]
    fn all_privileges_on_everything_covers_every_table() {
        for line in ["GRANT ALL PRIVILEGES ON *.* TO `wr`@`%`", "GRANT ALL ON *.* TO `wr`@`%`", "GRANT ALL ON `d`.* TO `wr`@`%`"] {
            assert_eq!(missing_writes(&[line.to_string()], "d", &present(&ALL)), Ok(vec![]), "{line}");
        }
    }

    #[test_id(tur5bt5)]
    #[test]
    fn grants_naming_another_database_or_nothing_are_not_a_verdict() {
        assert!(missing_writes(&grants_on("other", &[], FOUR), "d", &present(&ALL)).is_err());
        assert!(missing_writes(&[], "d", &present(&ALL)).is_err());
        assert!(missing_writes(&["not a grant".to_string()], "d", &present(&ALL)).is_err());
    }

    #[test_id(ttkkifw)]
    #[test]
    fn a_database_without_an_events_table_is_not_provisioned() {
        let s = Server { tables: vec!["issues", "labels", "comments"], seen: RefCell::new(vec![]) };
        assert!(provision(&s, "`d`", "wr", "pw").is_err());
        assert!(s.seen.borrow().is_empty(), "no user is created on a database that is not a beads store");
    }
}
