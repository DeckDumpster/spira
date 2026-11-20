//! `admin-provision-writer`: the SQL user lc-serve applies content writes as (the reader's name
//! without its `_ro` suffix), its credential file, write grants on exactly the tables `content_write`
//! changes, SELECT on the rest, and EXECUTE for the Dolt commit procedure. A re-run re-issues the same statements and changes nothing.

use std::collections::BTreeSet;

use crate::admin_drop::Dolt;
use crate::admin_reader::{ensure_secret, literal_safe, plain};
use crate::db::Conn;
use crate::{CANNOT_TELL, REFUSED};

/// The tables a content write changes.
const WRITTEN: [&str; 4] = ["issues", "labels", "comments", "events"];

fn statements(db: &str, user: &str, password: &str, tables: &[&str], readable: &[String]) -> Vec<String> {
    let mut sql = vec![format!("CREATE USER IF NOT EXISTS '{user}'@'%' IDENTIFIED BY '{password}'"), format!("ALTER USER '{user}'@'%' IDENTIFIED BY '{password}'")];
    sql.extend(tables.iter().map(|t| format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {db}.`{t}` TO '{user}'@'%'")));
    sql.push(format!("GRANT EXECUTE ON {db}.* TO '{user}'@'%'"));
    sql.extend(readable.iter().map(|t| format!("GRANT SELECT ON {db}.`{t}` TO '{user}'@'%'")));
    sql
}

fn provision(admin: &dyn Dolt, db: &str, user: &str, password: &str) -> Result<String, String> {
    let schema = db.trim_matches('`');
    let rows = admin.rows(&format!("SELECT TABLE_NAME AS name FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{schema}'"))?;
    let present: BTreeSet<String> = rows.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect();
    if let Some(missing) = WRITTEN.iter().find(|t| !present.contains(**t)) {
        return Err(format!("the content database has no {missing} table: it is not a beads store"));
    }
    // Dolt checks a write against the whole foreign-key graph around issues, so the writer reads every table.
    let referencing: Vec<String> = present.iter().filter(|t| !WRITTEN.contains(&t.as_str())).cloned().collect();
    for sql in statements(db, user, password, &WRITTEN, &referencing) {
        admin.execute(&sql).map_err(|e| e.replace(password, "<redacted>"))?;
    }
    Ok(format!("{user} writes {} of {schema} and reads the {} others", WRITTEN.join(", "), referencing.len()))
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
    let get = |k: &str| std::env::var(k).ok();
    let provisioned = || {
        let env = std::env::vars().collect();
        Ok(Some(("root".to_string(), spira_config::resolve::lc_admin_password_file(&env).map(|p| std::fs::read_to_string(&p).map_err(|e| format!("reading {p}: {e}"))).transpose()?.map(|s| s.trim().to_string()).unwrap_or_default())))
    };
    let (admin_user, admin_password) = match crate::admin_drop::administrator(get, provisioned) {
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

    #[test]
    fn the_writer_gets_its_user_and_write_grants_on_the_four_tables_content_writes_change() {
        let s = Server { tables: vec!["issues", "labels", "comments", "events", "dependencies", "config"], seen: RefCell::new(vec![]) };
        provision(&s, "`beadsdb`", "wr", "s3cret").unwrap();
        let seen = s.seen.borrow();
        assert!(seen[0].starts_with("CREATE USER IF NOT EXISTS 'wr'@'%'") && seen[1].starts_with("ALTER USER 'wr'@'%'"), "{seen:?}");
        let grants: Vec<&String> = seen.iter().filter(|q| q.starts_with("GRANT")).collect();
        assert!(grants.contains(&&"GRANT EXECUTE ON `beadsdb`.* TO 'wr'@'%'".to_string()), "the Dolt commit is a procedure: {grants:?}");
        let grants: Vec<&String> = grants.into_iter().filter(|g| !g.contains("EXECUTE")).collect();
        assert_eq!(grants.len(), 6, "{grants:?}");
        let (write, read): (Vec<&&String>, Vec<&&String>) = grants.iter().partition(|g| g.contains("INSERT"));
        assert!(write.len() == 4 && write.iter().all(|g| g.starts_with("GRANT SELECT, INSERT, UPDATE, DELETE ON `beadsdb`.`") && g.ends_with("TO 'wr'@'%'")), "{grants:?}");
        assert_eq!(read.iter().map(|g| g.as_str()).collect::<Vec<_>>(), ["GRANT SELECT ON `beadsdb`.`config` TO 'wr'@'%'", "GRANT SELECT ON `beadsdb`.`dependencies` TO 'wr'@'%'"], "every other table is readable, not writable");
        assert!(!seen.iter().any(|q| q.contains(" ALL")), "no blanket grant");
    }

    #[test]
    fn a_database_without_an_events_table_is_not_provisioned() {
        let s = Server { tables: vec!["issues", "labels", "comments"], seen: RefCell::new(vec![]) };
        assert!(provision(&s, "`d`", "wr", "pw").is_err());
        assert!(s.seen.borrow().is_empty(), "no user is created on a database that is not a beads store");
    }
}
