use super::*;
use std::sync::Mutex;

struct Statistics {
    indexed: Vec<(&'static str, &'static str)>,
    seen: Mutex<Vec<String>>,
}

impl Sql for Statistics {
    fn rows(&self, sql: &str) -> Res<Vec<Value>> {
        self.seen.lock().unwrap().push(sql.to_string());
        assert!(sql.contains("information_schema.statistics"), "{sql}");
        Ok(self.indexed.iter().map(|(t, c)| json!({ "t": t, "c": c })).collect())
    }
}

#[test_id(ttjzawx)]
#[test]
fn index_gaps_names_each_window_index_the_statistics_lack() {
    let none = Statistics { indexed: vec![("issues", "status")], seen: Mutex::new(vec![]) };
    assert_eq!(index_gaps(&none, "`beads`").unwrap(), vec!["issues.updated_at".to_string(), "issues.closed_at".to_string()]);
    let one = Statistics { indexed: vec![("issues", "updated_at")], seen: Mutex::new(vec![]) };
    assert_eq!(index_gaps(&one, "`beads`").unwrap(), vec!["issues.closed_at".to_string()]);
}

#[test_id(tl8ae5e)]
#[test]
fn index_gaps_is_empty_when_both_indexes_are_present() {
    let both = Statistics { indexed: vec![("issues", "closed_at"), ("issues", "updated_at")], seen: Mutex::new(vec![]) };
    assert_eq!(index_gaps(&both, "`beads`").unwrap(), Vec::<String>::new());
}

#[test_id(t0wqy3y)]
#[test]
fn index_gaps_asks_only_for_the_leading_column_of_the_named_schema() {
    let f = Statistics { indexed: vec![], seen: Mutex::new(vec![]) };
    index_gaps(&f, "`beads`").unwrap();
    let q = f.seen.lock().unwrap()[0].clone();
    assert!(q.contains("table_schema = 'beads'") && q.contains("seq_in_index = 1"), "{q}");
}
