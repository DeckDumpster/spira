//! `admin-provision-reader`: the read-only SQL user `spira-lc content` reads bead content as, its
//! credential file, and its SELECT grants on the content database's tables, named by the store's
//! own `.beads/metadata.json`. A re-run that finds the reader already able to read every table
//! changes nothing.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;

use crate::admin_drop::Dolt;
use crate::db::Conn;
use crate::plan::{CORE_TABLES, READER_COLUMNS};
use crate::{CANNOT_TELL, REFUSED};

pub(crate) fn plain(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A secret that cannot leave its SQL string literal.
pub(crate) fn literal_safe(secret: &str) -> bool {
    !secret.is_empty() && secret.chars().all(|c| c.is_ascii_graphic() && c != '\'' && c != '"' && c != '\\')
}

/// The tables the reader is granted: those of the reader's columns the database holds.
fn granted(present: &BTreeSet<String>) -> Result<Vec<&'static str>, String> {
    if let Some(missing) = CORE_TABLES.iter().find(|t| !present.contains(**t)) {
        return Err(format!("the content database has no {missing} table: it is not a beads store"));
    }
    Ok(READER_COLUMNS.iter().map(|(t, _)| *t).filter(|t| present.contains(*t)).collect())
}

/// The statements that make `user` hold `password` and SELECT on `tables` of `db` (backquoted).
fn statements(db: &str, user: &str, password: &str, tables: &[&str]) -> Vec<String> {
    let mut sql = vec![format!("CREATE USER IF NOT EXISTS '{user}'@'%' IDENTIFIED BY '{password}'"), format!("ALTER USER '{user}'@'%' IDENTIFIED BY '{password}'")];
    sql.extend(tables.iter().map(|t| format!("GRANT SELECT ON {db}.`{t}` TO '{user}'@'%'")));
    sql
}

pub(crate) fn ensure_secret(path: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => return Ok(false),
        Ok(_) => return Err(format!("{path} exists and is empty: remove it so a secret can be generated")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("reading {path}: {e}")),
    }
    let mut raw = [0u8; 32];
    std::fs::File::open(system_paths::DEV_URANDOM).and_then(|mut f| f.read_exact(&mut raw)).map_err(|e| format!("reading {}: {e}", system_paths::DEV_URANDOM))?;
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| format!("creating {path}: {e}"))?;
    f.write_all(hex.as_bytes()).map_err(|e| format!("writing {path}: {e}"))?;
    Ok(true)
}

/// Provisions the reader on `admin`'s server. `reads` says whether the reader, logging in with its
/// credential file, can already SELECT from every one of the named tables.
fn provision(admin: &dyn Dolt, db: &str, user: &str, password: &str, reads: impl Fn(&[&str]) -> bool) -> Result<String, String> {
    let schema = db.trim_matches('`');
    let rows = admin.rows(&format!("SELECT TABLE_NAME AS name FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{schema}'"))?;
    let present: BTreeSet<String> = rows.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect();
    let tables = granted(&present)?;
    if reads(&tables) {
        return Ok(format!("{user} already reads {schema}: nothing changed"));
    }
    for sql in statements(db, user, password, &tables) {
        admin.execute(&sql).map_err(|e| e.replace(password, "<redacted>"))?;
    }
    Ok(format!("{user} reads {} of {schema}", tables.join(", ")))
}

pub fn run(_args: &[String]) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-provision-reader: {why}"));
    let settings = match crate::content_sql::Settings::from_config() {
        Ok(s) => s,
        Err(e) => return cannot(e),
    };
    let db = match crate::plan::content_db() {
        Ok(d) => d.to_string(),
        Err(e) => return cannot(e),
    };
    if !plain(&settings.user) {
        return (REFUSED, format!("admin-provision-reader: {:?} is not a plain user name", settings.user));
    }
    let (admin_user, admin_password) = match crate::admin_drop::local_administrator() {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let made = match ensure_secret(&settings.password_file) {
        Ok(m) => m,
        Err(e) => return cannot(e),
    };
    let password = match std::fs::read_to_string(&settings.password_file) {
        Ok(p) => p.trim().to_string(),
        Err(e) => return cannot(format!("reading {}: {e}", settings.password_file)),
    };
    if !literal_safe(&password) {
        return (REFUSED, format!("admin-provision-reader: the secret in {} has a character that cannot sit in a SQL string", settings.password_file));
    }
    let admin = Conn::administrator(&settings.host, settings.port, &admin_user, admin_password);
    let reads = |tables: &[&str]| {
        let Ok(reader) = Conn::reader(&settings.host, settings.port, &settings.user, &settings.password_file, db.trim_matches('`')) else { return false };
        tables.iter().all(|t| reader.query(&format!("SELECT 1 FROM {db}.`{t}` LIMIT 1")).is_ok())
    };
    match provision(&admin, &db, &settings.user, &password, reads) {
        Ok(msg) => (0, format!("admin-provision-reader: {msg}{}", if made { format!("; wrote {}", settings.password_file) } else { String::new() })),
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

    fn server(tables: &[&'static str]) -> Server {
        Server { tables: tables.to_vec(), seen: RefCell::new(vec![]) }
    }

    const ALL: [&str; 7] = ["issues", "labels", "dependencies", "comments", "config", "events", "wisps"];

    #[test_id(tq3fjbj)]
    #[test]
    fn a_reader_that_cannot_yet_read_gets_its_user_and_a_select_grant_per_table_it_reads() {
        let s = server(&ALL);
        provision(&s, "`beadsdb`", "rdr", "s3cret", |_| false).unwrap();
        let seen = s.seen.borrow();
        assert!(seen[0].starts_with("CREATE USER IF NOT EXISTS 'rdr'@'%'") && seen[1].starts_with("ALTER USER 'rdr'@'%'"), "{seen:?}");
        let grants: Vec<&String> = seen.iter().filter(|q| q.starts_with("GRANT")).collect();
        assert_eq!(grants.len(), 6, "{grants:?}");
        assert!(grants.iter().all(|g| g.starts_with("GRANT SELECT ON `beadsdb`.`") && g.ends_with("TO 'rdr'@'%'")), "{grants:?}");
        assert!(!seen.iter().any(|q| q.contains("wisps")), "a table the reader does not read is not granted");
    }

    #[test_id(txpbdlz)]
    #[test]
    fn a_reader_that_already_reads_every_table_changes_nothing() {
        let s = server(&ALL);
        let msg = provision(&s, "`beadsdb`", "rdr", "s3cret", |_| true).unwrap();
        assert!(s.seen.borrow().is_empty(), "{:?}", s.seen.borrow());
        assert!(msg.contains("nothing changed"), "{msg}");
    }

    #[test_id(tohbzge)]
    #[test]
    fn a_store_without_events_is_granted_what_it_has_and_one_without_a_core_table_is_refused() {
        let s = server(&["issues", "labels", "dependencies", "comments", "config"]);
        provision(&s, "`d`", "rdr", "pw", |_| false).unwrap();
        assert_eq!(s.seen.borrow().iter().filter(|q| q.starts_with("GRANT")).count(), 5);
        let bare = server(&["issues"]);
        assert!(provision(&bare, "`d`", "rdr", "pw", |_| false).is_err());
        assert!(bare.seen.borrow().is_empty(), "nothing is created on a database that is not a beads store");
    }

    #[test_id(th5br35)]
    #[test]
    fn a_secret_that_could_leave_its_literal_is_unsafe() {
        for bad in ["a'b", "a\\b", "a\"b", "a b", "", "a\nb"] {
            assert!(!literal_safe(bad), "{bad:?}");
        }
        assert!(literal_safe("AbC+/09=") && literal_safe(&"f".repeat(64)));
    }

    #[test_id(tlbziaq)]
    #[test]
    fn the_secret_file_is_written_once_at_0600_and_kept_afterwards() {
        use std::os::unix::fs::PermissionsExt;
        let d = testkit::TempDir::new("lc-reader-secret");
        let p = d.join("sub/cred-ro");
        let p = p.to_str().unwrap();
        assert!(ensure_secret(p).unwrap());
        let first = std::fs::read_to_string(p).unwrap();
        assert!(literal_safe(&first) && first.len() == 64, "{first}");
        assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!ensure_secret(p).unwrap());
        assert_eq!(std::fs::read_to_string(p).unwrap(), first);
    }
}
