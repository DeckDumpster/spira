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

fn without_events() -> Reader {
    let mut columns = full_layout();
    columns.retain(|(t, _)| t != "events");
    Reader::new(Box::new(Fake { columns: Mutex::new(columns), issues: vec![issue("sp-a", "A")], ..Fake::default() }), DB.into())
}

#[test]
fn a_store_without_the_events_columns_still_serves_show_list_and_label_list() {
    let reader = without_events();
    for args in [v(&["show", "sp-a"]), v(&["list"]), v(&["label", "list", "sp-a"]), v(&["search", "A"]), v(&["dep", "list", "sp-a"]), v(&["memories"])] {
        reader.read(&args).unwrap_or_else(|e| panic!("{args:?}: {e:?}"));
    }
    reader.layout().unwrap();
}

#[test]
fn a_store_without_the_events_columns_refuses_events_alone_naming_them() {
    let e = without_events().read(&v(&["events", "--id", "sp-a"])).unwrap_err();
    assert!(matches!(&e, Failure::Store(t) if ["events.issue_id", "events.event_type", "events.new_value", "events.created_at"].iter().all(|c| t.contains(c)) && t.contains("exit 2")), "{e:?}");
}

#[test]
fn events_is_served_by_a_layout_that_has_its_columns_and_a_gap_elsewhere_costs_it_no_reprobe() {
    let mut columns = full_layout();
    columns.retain(|(t, c)| !(t == "comments" && c == "text"));
    let f = Arc::new(Fake { columns: Mutex::new(columns), ..Fake::default() });
    let reader = Reader::new(Box::new(f.clone()), DB.into()).rechecking_at(std::time::Duration::ZERO);
    reader.read(&v(&["events", "--id", "sp-a"])).unwrap();
    reader.read(&v(&["events", "--id", "sp-a"])).unwrap();
    assert_eq!(f.seen.lock().unwrap().iter().filter(|q| q.contains("information_schema")).count(), 1);
}

#[test]
fn the_events_columns_are_the_ones_the_production_probe_reported() {
    let production = ["issue_id", "event_type", "actor", "old_value", "new_value", "comment", "created_at", "id"];
    let named = crate::plan::READER_COLUMNS.iter().find(|(t, _)| *t == "events").unwrap().1;
    assert!(named.iter().all(|c| production.contains(c)), "{named:?}");
}

#[test]
fn a_gap_outside_a_reads_tables_does_not_refuse_it() {
    let mut columns = full_layout();
    columns.retain(|(t, c)| !(t == "comments" && c == "text"));
    let reader = Reader::new(Box::new(Fake { columns: Mutex::new(columns), ..Fake::default() }), DB.into());
    reader.read(&v(&["label", "list", "sp-a"])).unwrap();
    assert!(matches!(reader.read(&v(&["comments", "sp-a"])), Err(Failure::Store(t)) if t.contains("comments.text")));
}

#[test]
fn every_reads_tables_are_in_the_readers_columns() {
    let known: Vec<&str> = crate::plan::READER_COLUMNS.iter().map(|(t, _)| *t).collect();
    for args in [&["list"][..], &["show", "x"], &["comments", "x"], &["gate", "list"], &["memories"], &["state", "x", "d"], &["config", "get", "k"], &["search", "q"], &["dep", "list", "x"], &["label", "list", "x"], &["events", "--id", "x"]] {
        for t in parse(&v(args)).unwrap().tables() {
            assert!(known.contains(t), "{args:?}: {t}");
        }
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

#[test]
fn kv_is_the_config_row_under_the_kv_prefix() {
    let f = Fake { reply: Mutex::new(vec![json!({ "value": "41" })]), ..Fake::default() };
    assert_eq!(kv(&f, DB, "spira.planning_watermark"), Ok(Some("41".to_string())));
    assert!(first_sql(&f).contains("`key` = 'kv.spira.planning_watermark'"), "{}", first_sql(&f));
    assert_eq!(kv(&Fake::default(), DB, "spira.planning_watermark"), Ok(None));
}

#[test]
fn statuses_are_the_rows_of_the_ids_in_any_status_without_labels() {
    let mut closed = issue("sp-a", "A");
    closed["status"] = json!("closed");
    closed["close_reason"] = json!("done");
    let f = Fake { issues: vec![closed], ..Fake::default() };
    let found = statuses(&f, DB, &v(&["sp-a", "sp-b"])).unwrap();
    assert_eq!((found[0].id.as_str(), found[0].status.as_str(), found[0].close_reason.as_deref()), ("sp-a", "closed", Some("done")));
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "no label or edge queries: {seen:?}");
    assert!(seen[0].contains("id IN ('sp-a','sp-b')") && !seen[0].contains("status <> 'closed'"), "{}", seen[0]);
    assert!(statuses(&f, DB, &[]).unwrap().is_empty());
}

#[test]
fn blockers_are_the_blocks_edges_onto_the_bead() {
    let f = Fake { reply: Mutex::new(vec![json!({ "issue_id": "sp-d" })]), ..Fake::default() };
    assert_eq!(blocked_by(&f, DB, "sp-a"), Ok(v(&["sp-d"])));
    let sql = first_sql(&f);
    assert!(sql.contains("type = 'blocks'") && sql.contains("depends_on_issue_id = 'sp-a'"), "{sql}");
}

#[test]
fn a_title_search_reads_open_beads_unlimited() {
    let f = Fake { issues: vec![issue("sp-m", "mend sp-t: does not merge onto main")], ..Fake::default() };
    let found = titled(&f, DB, "mend sp-t: does not merge onto").unwrap();
    assert_eq!(found[0].id, "sp-m");
    let sql = first_sql(&f);
    assert!(sql.contains("LOCATE(LOWER('mend sp-t: does not merge onto'), LOWER(title))") && sql.contains("status <> 'closed'") && !sql.contains("LIMIT"), "{sql}");
}

#[test]
fn show_text_leads_with_the_head_line_work_show_rewrites_and_carries_the_description() {
    let f = Fake {
        issues: vec![issue("sp-a", "A title")],
        labels: vec![json!({ "issue_id": "sp-a", "label": "repo:spira" })],
        reply: Mutex::new(vec![json!({ "author": "me", "text": "a comment" })]),
        ..Fake::default()
    };
    let text = show_text(&f, DB, "sp-a").unwrap().unwrap();
    assert!(text.starts_with("○ sp-a · A title   [P1 · open]\n"), "{text}");
    assert!(text.contains("Labels: repo:spira\n") && text.contains("DESCRIPTION\nthe body\n") && text.contains("me: a comment"), "{text}");
    assert_eq!(show_text(&Fake::default(), DB, "sp-none").unwrap(), None);
}

#[test]
fn search_is_one_issues_query_locating_the_text_in_four_columns_and_a_quote_cannot_end_the_string() {
    let f = Fake { issues: vec![issue("sp-a", "A")], ..Fake::default() };
    let out = answer(&f, DB, &v(&["search", "merge queue", "--json"])).unwrap();
    assert!(out.contains("sp-a"));
    let q = first_sql(&f);
    assert!(q.contains("LOCATE(LOWER('merge queue'), LOWER(title))") && q.contains("status <> 'closed'"), "{q}");
    for col in ["id", "description", "notes"] {
        assert!(q.contains(&format!("LOWER({col})")), "{col}");
    }
    assert!(q.contains("LIMIT 50"), "{q}");
    let f = Fake::default();
    answer(&f, DB, &v(&["search", "x') OR 1=1 --", "--all", "--limit", "0"])).unwrap();
    let q = first_sql(&f);
    assert!(q.contains("LOWER('x\\') OR 1=1 --')") && !q.contains("status <> 'closed'") && !q.contains("LIMIT"), "{q}");
}

#[test]
fn dep_list_up_with_a_type_is_one_dependencies_query_answering_the_other_end() {
    let f = Fake { reply: Mutex::new(vec![json!({ "issue_id": "sp-b", "depends_on_issue_id": "sp-a", "type": "blocks" })]), ..Fake::default() };
    let out = answer(&f, DB, &v(&["dep", "list", "sp-a", "--direction", "up", "--type", "blocks", "--json"])).unwrap();
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    let q = first_sql(&f);
    assert!(q.contains("depends_on_issue_id = 'sp-a'") && q.contains("type = 'blocks'"), "{q}");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!([{ "id": "sp-b", "type": "blocks" }]));
}

#[test]
fn dep_list_down_reads_the_beads_own_edges_and_accepts_the_equals_form() {
    let f = Fake { reply: Mutex::new(vec![json!({ "issue_id": "sp-a", "depends_on_issue_id": "sp-z", "type": "parent-child" })]), ..Fake::default() };
    let out = answer(&f, DB, &v(&["dep", "list", "sp-a"])).unwrap();
    assert!(first_sql(&f).contains("issue_id = 'sp-a'"));
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!([{ "id": "sp-z", "type": "parent-child" }]));
    let f = Fake::default();
    answer(&f, DB, &v(&["dep", "list", "sp-a", "--direction=up"])).unwrap();
    assert!(first_sql(&f).contains("depends_on_issue_id = 'sp-a'"));
    assert!(parse(&v(&["dep", "list", "sp-a", "--direction", "sideways"])).is_err());
}

#[test]
fn label_list_answers_the_labels_sorted_as_a_json_array() {
    let f = Fake { reply: Mutex::new(vec![json!({ "label": "rune:x" }), json!({ "label": "a" })]), ..Fake::default() };
    let out = answer(&f, DB, &v(&["label", "list", "sp-a", "--json"])).unwrap();
    assert!(first_sql(&f).contains(".labels WHERE issue_id = 'sp-a'"));
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!(["a", "rune:x"]));
}

#[test]
fn events_is_one_typed_query_over_the_ids_and_chunks_by_id_chunk() {
    let f = Fake { reply: Mutex::new(vec![json!({ "issue_id": "sp-a", "event_type": "claimed", "new_value": "", "created_at": "2026-11-01T00:00:00Z" })]), ..Fake::default() };
    let out = answer(&f, DB, &v(&["events", "--id", "sp-a,sp-b"])).unwrap();
    let q = first_sql(&f);
    assert!(q.contains(".events WHERE issue_id IN ('sp-a','sp-b')") && q.contains("'poison.cleared'") && q.contains("SUBSTRING(COALESCE(new_value,''),1,120)"), "{q}");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()[0]["event_type"], "claimed");
    let ids: Vec<String> = (0..201 * 5 / 2).map(|i| format!("sp-{i}")).collect();
    assert!(ids.len() > ID_CHUNK);
    let f = Fake::default();
    answer(&f, DB, &v(&["events", "--id", &ids.join(",")])).unwrap();
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), ids.len().div_ceil(ID_CHUNK));
    for (chunk, q) in ids.chunks(ID_CHUNK).zip(seen.iter()) {
        assert!(q.contains(&format!("issue_id IN ({})", quoted(chunk))), "{q}");
    }
    drop(seen);
    answer(&f, DB, &v(&["events", "--json", "--id", "sp-a"])).unwrap();
}

#[test]
fn events_with_no_id_is_a_request_failure() {
    assert!(matches!(answer(&Fake::default(), DB, &v(&["events"])), Err(Failure::Request(_))));
}

#[test]
fn the_readers_columns_include_the_events_columns() {
    let named = crate::plan::READER_COLUMNS.iter().find(|(t, _)| *t == "events").expect("events table").1;
    for col in ["issue_id", "event_type", "new_value", "created_at"] {
        assert!(named.contains(&col), "{col}");
    }
}
