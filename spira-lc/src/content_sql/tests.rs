use super::*;
use std::sync::{Arc, Mutex};

/// A beads database in memory: answers each query shape the reads send, and records them.
#[derive(Default)]
struct Fake {
    issues: Vec<Value>,
    labels: Vec<Value>,
    deps: Vec<Value>,
    counts: Vec<Value>,
    columns: Mutex<Vec<(String, String)>>,
    seen: Mutex<Vec<String>>,
    reply: Mutex<Vec<Value>>,
}

impl Sql for Fake {
    fn rows(&self, sql: &str) -> Res<Vec<Value>> {
        self.seen.lock().unwrap().push(sql.to_string());
        if sql.contains("FROM information_schema.columns") {
            return Ok(self.columns.lock().unwrap().iter().map(|(t, c)| json!({ "t": t, "c": c })).collect());
        }
        if sql.contains("COUNT(*) AS n FROM `beads`.comments") {
            return Ok(self.counts.clone());
        }
        if sql.contains(".labels WHERE issue_id IN") {
            return Ok(self.labels.clone());
        }
        if sql.contains(".dependencies WHERE issue_id IN") {
            return Ok(self.deps.clone());
        }
        if sql.contains(".issues") {
            return Ok(self.issues.clone());
        }
        Ok(self.reply.lock().unwrap().clone())
    }
}

const DB: &str = "`beads`";

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn issue(id: &str, title: &str) -> Value {
    json!({ "id": id, "title": title, "description": "the body", "status": "open", "priority": "1", "issue_type": "task", "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-02T00:00:00Z", "metadata": "{}" })
}

fn parsed_list(a: &[&str]) -> List {
    match parse(&v(a)).unwrap() {
        Read::List(l) => *l,
        other => panic!("{other:?}"),
    }
}

fn first_sql(f: &Fake) -> String {
    f.seen.lock().unwrap()[0].clone()
}

#[test]
fn a_label_any_list_is_one_where_clause_over_the_labels_table() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    answer(&f, DB, &v(&["list", "--limit", "0", "--label-any", "tag-one,tag-two", "--json"])).unwrap();
    let sql = first_sql(&f);
    assert!(sql.contains("id IN (SELECT issue_id FROM `beads`.labels WHERE label IN ('tag-one','tag-two'))"), "{sql}");
    assert!(sql.contains("status <> 'closed'") && !sql.contains("LIMIT"), "open beads, unlimited: {sql}");
}

#[test]
fn an_id_list_selects_exactly_those_ids_whatever_their_status() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    answer(&f, DB, &v(&["list", "--all", "--limit", "0", "--json", "--id", "sp-a,sp-b"])).unwrap();
    let sql = first_sql(&f);
    assert!(sql.contains("id IN ('sp-a','sp-b')"), "{sql}");
    assert!(!sql.contains("status <> 'closed'") && !sql.contains("issue_type NOT IN"), "--all and --id hide nothing: {sql}");
}

#[test]
fn a_filter_is_never_applied_in_process() {
    // The store answers every bead whatever the filter; the reader returns what the query
    // selected and does not narrow it again.
    let f = Fake { issues: vec![issue("sp-a", "A"), issue("sp-b", "B")], ..Fake::default() };
    let out: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["list", "--label", "x", "--parent", "sp-e"])).unwrap()).unwrap();
    assert_eq!(out.len(), 2);
}

#[test]
fn every_list_filter_the_call_sites_use_becomes_a_where_clause() {
    let l = parsed_list(&["list", "--all", "--limit", "0", "--brief", "--json", "--parent", "sp-e", "--label", "a,b", "--external-ref", "incident:x", "--status", "open,blocked", "--closed-after", "2026-11-01", "--updated-after", "2026-11-02T10:11:12Z"]);
    let w = l.where_clause(DB);
    for part in [
        "id IN (SELECT issue_id FROM `beads`.dependencies WHERE type = 'parent-child' AND depends_on_issue_id = 'sp-e')",
        "id IN (SELECT issue_id FROM `beads`.labels WHERE label = 'a')",
        "id IN (SELECT issue_id FROM `beads`.labels WHERE label = 'b')",
        "external_ref = 'incident:x'",
        "status IN ('open','blocked')",
        "closed_at > '2026-11-01 00:00:00'",
        "updated_at > '2026-11-02 10:11:12'",
    ] {
        assert!(w.contains(part), "{part} missing from {w}");
    }
}

#[test]
fn the_default_list_is_the_first_fifty_open_beads() {
    let l = parsed_list(&["list", "--json"]);
    let sql = l.issues_sql(DB);
    assert!(sql.ends_with("LIMIT 50") && sql.contains("status <> 'closed'"), "{sql}");
}

#[test]
fn a_quote_in_a_value_cannot_end_the_string() {
    let l = parsed_list(&["list", "--label-any", "x') OR 1=1 --"]);
    assert!(l.where_clause(DB).contains(r"'x\') OR 1=1 --'"), "{}", l.where_clause(DB));
}

#[test]
fn a_flag_the_door_does_not_serve_is_refused_by_name() {
    let e = parse(&v(&["list", "--metadata-field", "a=b"])).unwrap_err();
    assert!(e.contains("--metadata-field"), "{e}");
    assert!(parse(&v(&["list", "--closed-after", "yesterday"])).unwrap_err().contains("--closed-after"));
    assert!(parse(&v(&["list", "--limit"])).is_err());
}

#[test]
fn a_row_carries_labels_edges_and_the_parent_as_typed_fields() {
    let f = Fake {
        issues: vec![issue("sp-a", "A")],
        labels: vec![json!({ "issue_id": "sp-a", "label": "x" }), json!({ "issue_id": "sp-a", "label": "y" })],
        deps: vec![json!({ "issue_id": "sp-a", "depends_on_issue_id": "sp-e", "type": "parent-child" }), json!({ "issue_id": "sp-a", "depends_on_issue_id": "sp-b", "type": "blocks" })],
        ..Fake::default()
    };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["list"])).unwrap()).unwrap();
    let r = &rows[0];
    assert_eq!(r["labels"], json!(["x", "y"]));
    assert_eq!(r["parent"], "sp-e");
    assert_eq!(r["dependencies"][1], json!({ "issue_id": "sp-a", "depends_on_id": "sp-b", "type": "blocks" }));
    assert_eq!((r["priority"].clone(), r["description"].clone()), (json!(1), json!("the body")));
}

#[test]
fn a_bead_with_no_labels_or_edges_has_empty_arrays() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["list"])).unwrap()).unwrap();
    assert_eq!((rows[0]["labels"].clone(), rows[0]["dependencies"].clone(), rows[0]["parent"].clone()), (json!([]), json!([]), Value::Null));
}

#[test]
fn brief_leaves_out_the_free_text_and_the_metadata() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["list", "--brief"])).unwrap()).unwrap();
    assert!(rows[0].get("description").is_none() && rows[0].get("notes").is_none());
    assert_eq!(rows[0]["title"], "A");
}

#[test]
fn show_names_the_bead_it_cannot_find() {
    let f = Fake::default();
    let e = answer(&f, DB, &v(&["show", "sp-nope", "--json"])).unwrap_err();
    assert!(matches!(&e, Failure::Request(t) if t.contains("no issue found matching") && t.contains("sp-nope")), "{e:?}");
}

#[test]
fn show_of_several_ids_answers_the_ones_it_finds_and_refuses_only_when_it_finds_none() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["show", "sp-a", "sp-gone", "--json"])).unwrap()).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(answer(&Fake::default(), DB, &v(&["show", "sp-a", "sp-gone"])), Err(Failure::Request(t)) if t.contains("sp-gone")));
}

#[test]
fn a_row_carries_how_many_comments_its_bead_has() {
    let f = Fake { issues: vec![issue("sp-a", "A"), issue("sp-b", "B")], counts: vec![json!({ "issue_id": "sp-b", "n": "3" })], ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["list"])).unwrap()).unwrap();
    assert_eq!((rows[0]["comment_count"].clone(), rows[1]["comment_count"].clone()), (json!(0), json!(3)));
}

#[test]
fn show_answers_the_bead_with_its_text() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["show", "sp-a", "--long", "--json"])).unwrap()).unwrap();
    assert_eq!(rows[0]["description"], "the body");
}

#[test]
fn gate_list_is_the_open_gates() {
    let f = Fake { issues: vec![issue("sp-g", "G")], ..Fake::default() };
    answer(&f, DB, &v(&["gate", "list", "--json"])).unwrap();
    let sql = first_sql(&f);
    assert!(sql.contains("issue_type IN ('gate')") && sql.contains("status <> 'closed'"), "{sql}");
}

#[test]
fn memories_are_the_config_rows_under_the_memory_prefix_keyed_by_name() {
    let f = Fake { reply: Mutex::new(vec![json!({ "key": "kv.memory.sop-a", "value": "do a" })]), ..Fake::default() };
    let out = answer(&f, DB, &v(&["memories", "--json"])).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({ "sop-a": "do a" }));
    assert!(first_sql(&f).contains("LEFT(`key`, 10) = 'kv.memory.'"));
}

#[test]
fn state_is_the_value_of_the_dimension_label() {
    let f = Fake { reply: Mutex::new(vec![json!({ "label": "repo:spira" })]), ..Fake::default() };
    assert_eq!(answer(&f, DB, &v(&["state", "sp-a", "repo"])).unwrap(), "spira\n");
    let none = Fake::default();
    assert_eq!(answer(&none, DB, &v(&["state", "sp-a", "repo"])).unwrap(), "(no repo state set)\n");
}

#[test]
fn comments_are_the_comment_rows_in_order() {
    let f = Fake { reply: Mutex::new(vec![json!({ "id": "c1", "issue_id": "sp-a", "author": "me", "text": "hi", "created_at": "2026-11-01T00:00:00Z" })]), ..Fake::default() };
    let rows: Vec<Value> = serde_json::from_str(&answer(&f, DB, &v(&["comments", "sp-a", "--json"])).unwrap()).unwrap();
    assert_eq!((rows[0]["author"].clone(), rows[0]["text"].clone()), (json!("me"), json!("hi")));
}

fn full_layout() -> Vec<(String, String)> {
    crate::plan::READER_COLUMNS.iter().flat_map(|(t, cs)| cs.iter().map(move |c| (t.to_string(), c.to_string()))).collect()
}

#[test]
fn a_renamed_column_refuses_every_read_naming_it_and_the_exit_and_asks_no_bead_query() {
    let mut columns = full_layout();
    columns.retain(|(t, c)| !(t == "labels" && c == "label"));
    columns.push(("labels".into(), "tag".into()));
    let reader = Reader::new(Box::new(Fake { columns: Mutex::new(columns), issues: vec![issue("sp-a", "A")], ..Fake::default() }), DB.into());
    for args in [v(&["list"]), v(&["show", "sp-a"]), v(&["memories"])] {
        let e = reader.read(&args).unwrap_err();
        assert!(matches!(&e, Failure::Store(t) if t.contains("labels.label") && t.contains("exit 2")), "{e:?}");
    }
}

impl Sql for Arc<Fake> {
    fn rows(&self, sql: &str) -> Res<Vec<Value>> {
        self.as_ref().rows(sql)
    }
}

#[test]
fn a_known_layout_is_checked_once_and_serves_reads() {
    let f = Arc::new(Fake { columns: Mutex::new(full_layout()), issues: vec![issue("sp-a", "A")], ..Fake::default() });
    let reader = Reader::new(Box::new(f.clone()), DB.into());
    reader.read(&v(&["list"])).unwrap();
    reader.read(&v(&["list"])).unwrap();
    let checks = f.seen.lock().unwrap().iter().filter(|q| q.contains("information_schema")).count();
    assert_eq!(checks, 1);
}

#[test]
fn the_readers_columns_include_every_column_the_queries_name() {
    let l = parsed_list(&["list"]);
    let sql = l.issues_sql(DB);
    let named: Vec<&str> = crate::plan::READER_COLUMNS.iter().find(|(t, _)| *t == "issues").unwrap().1.to_vec();
    for col in ["id", "title", "description", "status", "priority", "issue_type", "is_template", "await_type", "metadata"] {
        assert!(named.contains(&col) && sql.contains(col), "{col}");
    }
}

#[test]
fn a_refused_layout_is_asked_again_and_a_repaired_store_is_served_without_a_restart() {
    let mut broken = full_layout();
    broken.retain(|(t, c)| !(t == "labels" && c == "label"));
    let f = Arc::new(Fake { columns: Mutex::new(broken), issues: vec![issue("sp-a", "A")], ..Fake::default() });
    let reader = Reader::new(Box::new(f.clone()), DB.into()).rechecking_at(std::time::Duration::ZERO);
    assert!(reader.read(&v(&["list"])).is_err());
    *f.columns.lock().unwrap() = full_layout();
    assert!(reader.read(&v(&["list"])).is_ok());
}

#[test]
fn a_refusal_stands_until_the_recheck_interval_has_passed() {
    let mut broken = full_layout();
    broken.retain(|(t, c)| !(t == "labels" && c == "label"));
    let f = Arc::new(Fake { columns: Mutex::new(broken), issues: vec![issue("sp-a", "A")], ..Fake::default() });
    let reader = Reader::new(Box::new(f.clone()), DB.into());
    assert!(reader.read(&v(&["list"])).is_err());
    *f.columns.lock().unwrap() = full_layout();
    assert!(reader.read(&v(&["list"])).is_err(), "inside the interval the refusal is not re-asked");
}
