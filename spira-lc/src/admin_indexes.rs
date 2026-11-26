//! `admin-ensure-indexes`: the indexes on the beads database's window columns, which bd's own
//! schema does not carry. Without them a `--updated-after` / `--closed-after` read walks every issue ever filed.

/// `(table, column)` of each index a time-window read needs.
pub const WINDOW_INDEXES: [(&str, &str); 2] = [("issues", "updated_at"), ("issues", "closed_at")];

use crate::admin_drop::Dolt;
use crate::db::Conn;
use crate::{CANNOT_TELL, REFUSED};

struct Via<'a>(&'a dyn Dolt);

impl crate::content_sql::Sql for Via<'_> {
    fn rows(&self, sql: &str) -> Result<Vec<serde_json::Value>, crate::content_sql::Failure> {
        self.0.rows(sql).map_err(crate::content_sql::Failure::Store)
    }
}

fn index_name((table, column): (&str, &str)) -> String {
    format!("idx_{table}_{column}")
}

/// Creates each window index the database lacks and no other; returns the `table.column` names it created.
pub(crate) fn ensure(admin: &dyn Dolt, db: &str) -> Result<Vec<String>, String> {
    let missing = crate::content_sql::index_gaps(&Via(admin), db).map_err(|f| f.text().to_string())?;
    let mut made = Vec::new();
    for (table, column) in WINDOW_INDEXES.iter().copied().filter(|(t, c)| missing.contains(&format!("{t}.{c}"))) {
        admin.execute(&format!("CREATE INDEX `{}` ON {db}.`{table}` (`{column}`)", index_name((table, column))))?;
        made.push(format!("{table}.{column}"));
    }
    Ok(made)
}

pub fn run(_args: &[String]) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-ensure-indexes: {why}"));
    let settings = match crate::content_sql::Settings::from_config() {
        Ok(s) => s,
        Err(e) => return cannot(e),
    };
    let db = match crate::plan::content_db() {
        Ok(d) => d.to_string(),
        Err(e) => return cannot(e),
    };
    let (user, password) = match crate::admin_drop::local_administrator() {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let admin = Conn::administrator(&settings.host, settings.port, &user, password);
    match ensure(&admin, &db) {
        Ok(made) if made.is_empty() => (0, "admin-ensure-indexes: the window indexes are present: nothing changed".to_string()),
        Ok(made) => (0, format!("admin-ensure-indexes: created {}", made.join(", "))),
        Err(e) => (REFUSED, format!("admin-ensure-indexes: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::cell::RefCell;

    struct Server {
        indexed: Vec<&'static str>,
        seen: RefCell<Vec<String>>,
    }

    impl Dolt for Server {
        fn rows(&self, _sql: &str) -> Result<Vec<Value>, String> {
            Ok(self.indexed.iter().map(|c| json!({ "t": "issues", "c": c })).collect())
        }
        fn execute(&self, sql: &str) -> Result<(), String> {
            self.seen.borrow_mut().push(sql.to_string());
            Ok(())
        }
    }

    #[test_id(tj5y0ur)]
    #[test]
    fn a_missing_window_index_is_created_and_a_present_one_is_left() {
        let s = Server { indexed: vec!["updated_at"], seen: RefCell::new(vec![]) };
        assert_eq!(ensure(&s, "`beadsdb`").unwrap(), vec!["issues.closed_at".to_string()]);
        let seen = s.seen.borrow();
        assert_eq!(*seen, vec!["CREATE INDEX `idx_issues_closed_at` ON `beadsdb`.`issues` (`closed_at`)".to_string()]);
    }

    #[test_id(t0wgtpn)]
    #[test]
    fn a_database_with_both_indexes_gets_no_statement() {
        let s = Server { indexed: vec!["updated_at", "closed_at"], seen: RefCell::new(vec![]) };
        assert!(ensure(&s, "`beadsdb`").unwrap().is_empty());
        assert!(s.seen.borrow().is_empty());
    }
}
