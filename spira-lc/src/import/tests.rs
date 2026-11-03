use std::collections::{BTreeMap, BTreeSet};

use runtime::ports::VirtualClock;
use serde_json::{json, Value};
use state_store::{Backend, Config, Contract, Object, Ports};
use state_store_sqlite::{Options, SqliteBackend};

use super::archive::{self, Archive};
use super::checks::{self, CONTINUITY_ACKS, DRIFT_CURSOR};
use super::map::{self, Projection};
use crate::control::keys;
use crate::control::state::{self, Call};
use crate::control::store::Store;
use crate::control::verbs;

const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// A `bead` row as the server returns it: every column a string or null.
fn bead(id: &str, state: &str, version: u64) -> Value {
    json!({
        "bead_id": id, "state": state, "tip": null, "gate_key": null, "holder": null,
        "persona": null, "lease_until": null, "holds": "[]", "reason": null,
        "version": version.to_string(), "stack": "{}", "stack_depth": "0", "since": "100",
        "title": "a title", "priority": "1", "repo": "harness", "express": "0",
        "dwarf_phase": null, "disposition": null, "disposition_note": null,
        "ejected_red_tip": null, "sifted_tip": null, "claimed_from": null,
        "claim_verdict": null, "claim_blocker": null, "claim_runes": null, "claim_at": null,
        "updated_at": "200"
    })
}

fn with(mut row: Value, pairs: &[(&str, Value)]) -> Value {
    for (k, v) in pairs {
        row[*k] = v.clone();
    }
    row
}

#[allow(clippy::too_many_arguments)]
fn event(seq: u64, machine: &str, key: &str, name: &str, from: &str, to: &str, applied: bool, evidence: Value) -> Value {
    json!({
        "seq": seq.to_string(), "machine": machine, "lc_key": key, "event": name,
        "expect": from, "from_state": from, "to_state": to,
        "applied": if applied { "1" } else { "0" },
        "refusal": if applied { Value::Null } else { json!("IllegalTransition") },
        "evidence": evidence.to_string(), "actor": "t", "at": (1000 + seq).to_string()
    })
}

fn claim(seq: u64, key: &str, holder: &str) -> Value {
    event(seq, "bead", key, "Claim", "READY", "WORKING", true, json!({"Claim": {"holder": holder, "lease_until": 9}}))
}

fn archived(tables: &[(&str, Vec<Value>)]) -> Archive {
    let rows: BTreeMap<&str, Vec<Value>> = tables.iter().cloned().collect();
    archive::parse(archive::render(&rows).as_bytes()).expect("a rendered archive reads back")
}

fn projected(a: &Archive, no_record: &[&str]) -> Projection {
    let skip: BTreeSet<String> = no_record.iter().map(|s| s.to_string()).collect();
    map::project(a, &skip).expect("the archive maps")
}

fn value(p: &Projection, key: &str) -> Object {
    let item = p.items.get(key).unwrap_or_else(|| panic!("no {key} in {:?}", p.items.keys().collect::<Vec<_>>()));
    Object::decode(&item.value).unwrap()
}

fn working(id: &str, holder: &str) -> Value {
    with(bead(id, "WORKING", 1), &[("holder", json!(holder)), ("lease_until", json!("9")), ("claimed_from", json!("READY")), ("dwarf_phase", json!("claimed"))])
}

#[test]
fn an_archive_reads_back_its_rows_and_a_changed_row_names_itself() {
    let rows: BTreeMap<&str, Vec<Value>> = [("bead", vec![bead("sp-a", "READY", 0), bead("sp-b", "READY", 0)])].into_iter().collect();
    let text = archive::render(&rows);
    let a = archive::parse(text.as_bytes()).unwrap();
    assert_eq!(a.rows("bead").iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"]);
    assert_eq!(a.manifest.tables["bead"].rows, 2);

    let corrupt: String = text
        .lines()
        .map(|l| if l.contains("\"key\":\"sp-b\"") { l.replace("\"READY\"", "\"LANDED\"") } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    let failures = archive::parse(corrupt.as_bytes()).unwrap_err();
    assert!(failures.iter().any(|f| f == "archive: bead row sp-b fails its checksum"), "{failures:?}");
}

#[test]
fn an_archive_missing_a_row_or_its_manifest_is_refused() {
    let rows: BTreeMap<&str, Vec<Value>> = [("bead", vec![bead("sp-a", "READY", 0), bead("sp-b", "READY", 0)])].into_iter().collect();
    let text = archive::render(&rows);
    let dropped: String = text.lines().filter(|l| !l.contains("\"key\":\"sp-a\"")).map(|l| format!("{l}\n")).collect();
    let failures = archive::parse(dropped.as_bytes()).unwrap_err();
    assert!(failures.iter().any(|f| f == "archive: bead has 1 rows, the manifest says 2"), "{failures:?}");
    let truncated: String = text.lines().filter(|l| !l.starts_with("{\"manifest\"")).map(|l| format!("{l}\n")).collect();
    let failures = archive::parse(truncated.as_bytes()).unwrap_err();
    assert!(failures.iter().any(|f| f.contains("no manifest")), "{failures:?}");
}

#[test]
fn a_record_keeps_the_machine_fields_and_repo_and_leaves_planning_fields_and_the_lease() {
    let row = with(working("sp-a", "d1"), &[("claim_verdict", json!("blocked")), ("claim_blocker", json!("sp-z")), ("claim_runes", json!("builder,ops"))]);
    let p = projected(&archived(&[("bead", vec![row])]), &[]);
    let rec = value(&p, "lifecycle/sp-a");
    for planning in ["title", "priority", "claim_verdict"] {
        assert!(rec.data.get(planning).is_none(), "{planning} in {}", rec.data);
    }
    assert!(rec.data["lease_until"].is_null(), "{}", rec.data);
    assert_eq!((rec.data["repo"].as_str(), rec.data["holder"].as_str(), rec.data["version"].as_u64()), (Some("harness"), Some("d1"), Some(1)));
    assert_eq!(rec.data["claimed_from"], json!("Ready"));
    assert_eq!(rec.metadata.finalizers, [keys::PLANNING_CLOSED]);
    assert_eq!(value(&p, "gates/sp-a").metadata.owner_references, ["lifecycle/sp-a"]);
    let judged = value(&p, "readiness/sp-a");
    assert_eq!((judged.data["verdict"].as_str(), judged.data["blocker"].as_str()), (Some("blocked"), Some("sp-z")));
    assert_eq!(judged.data["runes"], json!(["builder", "ops"]));
    assert!(!p.items.contains_key("readiness/sp-b"));
}

#[test]
fn a_record_with_no_record_type_its_delivery_and_events_and_every_bead_dep_row_are_not_imported() {
    let a = archived(&[
        ("bead", vec![bead("sp-a", "READY", 0), bead("sp-epic", "READY", 0)]),
        ("delivery", vec![json!({"bead_id": "sp-epic", "mode": "queue", "state": "QUEUED", "batch_id": null, "pr": null, "merge_sha": null, "ci": null, "version": "0"})]),
        ("event", vec![event(1, "bead", "sp-epic", "Express", "READY", "READY", true, json!("Express"))]),
        ("bead_dep", vec![json!({"bead_id": "sp-a", "depends_on": "sp-epic", "dep_type": "blocks"})]),
    ]);
    let p = projected(&a, &["sp-epic"]);
    assert!(p.items.keys().all(|k| !k.contains("sp-epic")), "{:?}", p.items.keys().collect::<Vec<_>>());
    assert!(p.items.contains_key("lifecycle/sp-a"));
    assert_eq!(p.excluded, [("bead", 1), ("bead_dep", 1), ("delivery", 1), ("event", 1)].into_iter().collect());
}

#[test]
fn a_rounds_members_go_inside_its_value_and_its_progress_and_index_get_their_own_keys() {
    let batch = json!({"batch_id": "b-1", "repo": "harness", "state": "OPEN", "parent": null, "head": "h", "base": "b", "pr": null, "run": null, "reason": null, "pass": "0", "phase": null, "progress": "{\"done\": 3}", "version": "0", "opened_at": "50"});
    let member = |id: &str| json!({"batch_id": "b-1", "bead_id": id, "tip": TIP, "outcome": null});
    let p = projected(&archived(&[("batch", vec![batch]), ("batch_member", vec![member("sp-a"), member("sp-b")])]), &[]);
    let round = value(&p, "batches/harness/b-1");
    assert_eq!(round.data["members"], json!([{"bead_id": "sp-a", "tip": TIP}, {"bead_id": "sp-b", "tip": TIP}]));
    assert_eq!(round.data["opened_at"], json!(50));
    assert_eq!(value(&p, "batches/harness/b-1/progress").data, json!({"done": 3}));
    let ix = value(&p, "batch-ids/b-1");
    assert_eq!((ix.data["repo"].as_str(), ix.metadata.owner_references.as_slice()), (Some("harness"), ["batches/harness/b-1".to_string()].as_slice()));
}

#[test]
fn a_member_of_no_round_is_named() {
    let a = archived(&[("batch_member", vec![json!({"batch_id": "b-9", "bead_id": "sp-a", "tip": TIP, "outcome": null})])]);
    let failures = map::project(&a, &BTreeSet::new()).unwrap_err();
    assert_eq!(failures, ["batch_member row b-9/sp-a: names no batch row b-9"]);
}

#[test]
fn each_event_is_a_history_entry_at_its_old_seq_and_the_import_revision_is_one_past_the_last() {
    let a = archived(&[("bead", vec![working("sp-a", "d1")]), ("event", vec![claim(3, "sp-a", "d1"), event(7, "bead", "sp-a", "Submit", "WORKING", "WORKING", false, json!({"Submit": {"tip": TIP}}))])]);
    let p = projected(&a, &[]);
    assert_eq!(p.rev, 8);
    let entry = value(&p, &format!("history/sp-a/{:020}/import/{:020}", 0, 7));
    assert_eq!(entry.data["seq"], json!(7));
    assert_eq!((entry.data["audit"]["event"].as_str(), entry.data["audit"]["applied"].as_bool()), (Some("Submit"), Some(false)));
    assert_eq!(entry.data["audit"]["evidence"], json!({"Submit": {"tip": TIP}}));
}

#[test]
fn the_runtime_tables_land_under_runtime() {
    let a = archived(&[
        ("runtime_event", vec![json!({"seq": "4", "event_id": "e1", "stream": "s", "kind_tag": "t", "kind_body": "b", "at": "1", "cause": null, "correlation": "c"})]),
        ("runtime_cursor", vec![json!({"id": "1", "next": "5"})]),
        ("runtime_applied", vec![json!({"handler": "h", "event_id": "e1"})]),
    ]);
    let p = projected(&a, &[]);
    assert_eq!(value(&p, &format!("runtime/event/{:020}", 4)).data["event_id"], json!("e1"));
    assert_eq!(value(&p, "runtime/cursor/1").data["next"], json!("5"));
    assert!(p.items.contains_key("runtime/applied/h/e1"));
}

#[test]
fn a_record_equal_to_its_fold_passes_and_one_that_is_not_names_the_field() {
    let log = vec![claim(1, "sp-a", "d1"), event(2, "bead", "sp-a", "Submit", "READY", "READY", false, json!({"Submit": {"tip": TIP}}))];
    let good = archived(&[("bead", vec![working("sp-a", "d1")]), ("event", log.clone())]);
    assert_eq!(checks::replay(&good, &BTreeSet::new()).refusals, Vec::<String>::new());
    let bad = archived(&[("bead", vec![working("sp-a", "d2")]), ("event", log)]);
    let failures = checks::replay(&bad, &BTreeSet::new()).refusals;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("bead sp-a: does not replay: holder: \"d2\" imported, \"d1\" replayed"), "{failures:?}");
}

#[test]
fn the_fields_that_bypass_events_are_not_compared() {
    let row = with(working("sp-a", "d1"), &[("repo", json!("elsewhere")), ("claim_verdict", json!("claimable")), ("claim_at", json!("2026-01-01T00:00:00Z")), ("since", json!("999"))]);
    let a = archived(&[("bead", vec![row]), ("event", vec![claim(1, "sp-a", "d1")])]);
    assert_eq!(checks::replay(&a, &BTreeSet::new()).refusals, Vec::<String>::new());
}

#[test]
fn an_applied_event_the_machine_refuses_on_replay_is_named_by_seq_and_refuses_only_a_live_record() {
    let refused_at = |state: &str| archived(&[("bead", vec![bead("sp-a", state, 1)]), ("event", vec![event(5, "bead", "sp-a", "Deliver", "READY", state, true, json!("Deliver"))])]);
    let live = checks::replay(&refused_at("CERTIFIED"), &BTreeSet::new());
    assert_eq!((live.refusals.len(), live.as_stored.len()), (1, 0), "{live:?}");
    assert!(live.refusals[0].starts_with("bead sp-a: seq 5: Deliver READY -> CERTIFIED replays as refused"), "{live:?}");
    let terminal = checks::replay(&refused_at("LANDED"), &BTreeSet::new());
    assert_eq!(terminal.refusals, Vec::<String>::new());
    assert_eq!((terminal.as_stored.len(), terminal.as_stored[0].state.as_str()), (1, "LANDED"), "{terminal:?}");
    assert!(terminal.as_stored[0].finding.starts_with("seq 5: Deliver READY -> LANDED replays as refused"), "{terminal:?}");
}

#[test]
fn a_cursor_above_the_import_revision_refuses_naming_it_and_one_at_or_below_passes() {
    let t = testkit::TempDir::new("import-cursors");
    let run = t.path();
    assert_eq!(checks::cursors(run, 10), Vec::<String>::new(), "no cursor file is no cursor");
    std::fs::create_dir_all(run.join("conditions")).unwrap();
    std::fs::write(run.join(DRIFT_CURSOR), "10\nsp-a\n").unwrap();
    std::fs::write(run.join(CONTINUITY_ACKS), "9\tbead\tsp-a\tknown\n").unwrap();
    assert_eq!(checks::cursors(run, 10), Vec::<String>::new());
    std::fs::write(run.join(DRIFT_CURSOR), "11\n").unwrap();
    std::fs::write(run.join(CONTINUITY_ACKS), "9\tbead\tsp-a\tknown\n12\tbead\tsp-b\tlater\n").unwrap();
    let failures = checks::cursors(run, 10);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(failures[0].contains("content-drift.state") && failures[0].contains("is at 11, above the import revision 10"), "{failures:?}");
    assert!(failures[1].contains("event-continuity.acknowledged (line 2) is at 12"), "{failures:?}");
    std::fs::write(run.join(DRIFT_CURSOR), "garbage\n").unwrap();
    assert!(checks::cursors(run, 10)[0].contains("its first line is not a seq"));
}

#[test]
fn a_key_the_store_lost_changed_or_added_is_named_by_table() {
    let a = archived(&[("bead", vec![bead("sp-a", "READY", 0)]), ("ask", vec![json!({"ask_id": "q-1", "state": "OPEN", "work_bead": null, "closed_by": null, "quote": null, "channel": null, "version": "0", "opened_at": "1", "closed_at": null})])]);
    let p = projected(&a, &[]);
    let exact: BTreeMap<String, Vec<u8>> = p.items.iter().map(|(k, i)| (k.clone(), i.value.clone())).collect();
    assert_eq!(checks::loaded(&a, &p, &exact), Vec::<String>::new());

    let mut changed = exact.clone();
    changed.insert("asks/q-1".into(), b"{}".to_vec());
    assert_eq!(checks::loaded(&a, &p, &changed), ["ask: the store's checksum differs from the import's, first at asks/q-1"]);
    let mut lost = exact.clone();
    lost.remove("gates/sp-a");
    assert_eq!(checks::loaded(&a, &p, &lost), ["bead: 2 keys imported, 1 read back; first missing gates/sp-a"]);
    let mut added = exact;
    added.insert("lifecycle/sp-x".into(), b"{}".to_vec());
    assert_eq!(checks::loaded(&a, &p, &added), ["the store holds lifecycle/sp-x, which no archived row maps to"]);
}

#[test]
fn an_imported_store_reads_old_history_in_seq_order_and_the_next_txn_is_one_past_the_import() {
    let a = archived(&[
        ("bead", vec![working("sp-a", "d1"), bead("sp-b", "READY", 0)]),
        ("event", vec![claim(2, "sp-a", "d1"), event(4, "bead", "sp-a", "Submit", "WORKING", "WORKING", false, json!({"Submit": {"tip": "f00"}}))]),
    ]);
    let p = projected(&a, &[]);
    let t = testkit::TempDir::new("import-store");
    let b = SqliteBackend::open(&t.path().join("store.db"), Options::default()).unwrap();
    b.bulk_load(p.rev, p.items.iter().map(|(k, i)| (k.clone(), i.value.clone()))).unwrap();
    let clock = VirtualClock::new(0);
    let store = Store::new(Contract::open(b, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000);
    let lc = |args: &[&str]| verbs::run(&store, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let r = p.rev;

    let (rc, out) = lc(&["event-seq", "--since", &(r - 1).to_string()]);
    assert_eq!((rc, serde_json::from_str::<Value>(&out).unwrap()), (0, json!({"seq": r, "keys": []})));
    let claim_kind = json!({"Claim": {"holder": "d2", "lease_until": 9}}).to_string();
    let argv: Vec<String> = ["event", "bead", "sp-b", "--expect", "READY", "--version", "0", "--actor", "t", "--kind", &claim_kind].iter().map(|s| s.to_string()).collect();
    let (rc, out) = state::run(&store, &argv, &Call { now: 1, request: "r-1" });
    assert_eq!(rc, 0, "{out}");
    let (_, out) = lc(&["event-seq", "--since", &r.to_string()]);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({"seq": r + 1, "keys": ["sp-b"]}));

    let (rc, out) = lc(&["history", "sp-a"]);
    assert_eq!(rc, 0, "{out}");
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.iter().map(|r| (r["seq"].as_u64().unwrap(), r["event"].as_str().unwrap())).collect::<Vec<_>>(), [(2, "Claim"), (4, "Submit")]);
    let (_, out) = lc(&["history", "sp-b"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.iter().map(|r| r["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [r + 1], "a post-import change is at its revision");
}

#[test]
fn an_exited_delivery_carries_the_exit_its_events_left() {
    let row = json!({"bead_id": "sp-a", "mode": "queue", "state": "EXITED", "batch_id": "b-1", "pr": null, "merge_sha": "m1", "ci": null, "version": "2"});
    let a = archived(&[
        ("bead", vec![bead("sp-a", "READY", 0)]),
        ("delivery", vec![row]),
        ("event", vec![
            event(1, "delivery", "sp-a", "Enqueued", "NONE", "QUEUED", true, Value::Null),
            event(2, "delivery", "sp-a", "Cut", "QUEUED", "BATCHED", true, json!({"Cut": {"batch_id": "b-1"}})),
            event(3, "delivery", "sp-a", "Delivered", "BATCHED", "EXITED", true, json!({"Delivered": {"merge_sha": "m1", "proof": "p"}})),
        ]),
    ]);
    assert_eq!(value(&projected(&a, &[]), "deliveries/sp-a").data["exit"], json!("Delivered"));
}

#[test]
fn a_mend_at_or_above_the_import_revision_is_named() {
    let mend = json!({"seq": "9", "batch_id": "b-1", "pass": "1", "suite": "u", "bead_id": "sp-a", "from_tip": "a", "tip": "b", "ask_id": null, "checked": "", "diagnosis": null, "at": "1"});
    let a = archived(&[("event", vec![claim(3, "sp-a", "d1")]), ("mend", vec![mend])]);
    assert_eq!(map::project(&a, &BTreeSet::new()).unwrap_err(), ["mend row 9: seq 9 is not below the import revision 4, so a later mend would sort before it"]);
}

/// An archive file on disk, as `export` leaves it, with a run directory holding no cursor.
fn written(t: &testkit::TempDir, tables: &[(&str, Vec<Value>)]) -> std::path::PathBuf {
    let rows: BTreeMap<&str, Vec<Value>> = tables.iter().cloned().collect();
    let path = t.path().join("export.jsonl");
    std::fs::write(&path, archive::render(&rows)).unwrap();
    path
}

fn import_of(t: &testkit::TempDir, archive_path: &std::path::Path) -> (i32, String) {
    super::import(archive_path, &BTreeSet::new(), &t.path().join("run"), &t.path().join("store.db"))
}

fn stored(t: &testkit::TempDir, key: &str) -> Object {
    let b = SqliteBackend::open(&t.path().join("store.db"), Options::default()).unwrap();
    let (_, v) = b.range(key, None, 1, u64::MAX).unwrap().into_iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("no {key} in the store"));
    Object::decode(&v.value).unwrap()
}

/// A record dropped by a write that left no event: the log takes it straight to its state, and
/// the stored row carries a `reason` no event wrote.
fn dropped_outside_the_log(state: &str) -> [(&'static str, Vec<Value>); 2] {
    [
        ("bead", vec![with(bead("sp-a", state, 0), &[("reason", json!("residue-closed-no-landing-evidence"))])]),
        ("event", vec![event(6, "bead", "sp-a", "Filed", "NONE", state, true, Value::Null)]),
    ]
}

#[test]
fn a_terminal_record_with_a_field_written_outside_the_event_log_imports_as_stored_and_is_reported() {
    let t = testkit::TempDir::new("import-as-stored");
    let path = written(&t, &dropped_outside_the_log("DROPPED"));
    let (rc, out) = import_of(&t, &path);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(stored(&t, "lifecycle/sp-a").data["reason"], json!("residue-closed-no-landing-evidence"), "imported as stored, never rewritten");
    let report: Value = serde_json::from_slice(&std::fs::read(super::report_path(&path)).unwrap()).unwrap();
    let listed = report["as_stored"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{report}");
    assert_eq!((listed[0]["table"].as_str(), listed[0]["key"].as_str(), listed[0]["state"].as_str()), (Some("bead"), Some("sp-a"), Some("DROPPED")));
    assert!(listed[0]["finding"].as_str().unwrap().contains("reason: \"residue-closed-no-landing-evidence\" imported, null replayed"), "{report}");
}

#[test]
fn a_live_record_with_a_field_written_outside_the_event_log_refuses_the_import() {
    let t = testkit::TempDir::new("import-live-refuses");
    let path = written(&t, &dropped_outside_the_log("READY"));
    let (rc, out) = import_of(&t, &path);
    assert_eq!(rc, crate::REFUSED, "{out}");
    assert!(out.contains("bead sp-a: does not replay: reason: \"residue-closed-no-landing-evidence\" imported, null replayed"), "{out}");
    assert!(!t.path().join("store.db").exists(), "a refused import leaves no store");
    assert!(!super::report_path(&path).exists(), "a refused import writes no report");
}

#[test]
fn the_baseline_is_recorded_on_the_store_at_one_past_the_last_event_seq() {
    let t = testkit::TempDir::new("import-baseline");
    let path = written(&t, &dropped_outside_the_log("DROPPED"));
    let (rc, out) = import_of(&t, &path);
    assert_eq!(rc, 0, "{out}");
    let baseline = stored(&t, keys::IMPORT_BASELINE);
    assert_eq!((baseline.data["rev"].as_u64(), baseline.data["max_event_seq"].as_u64()), (Some(7), Some(6)), "{}", baseline.data);
    assert_eq!(baseline.data["as_stored"].as_u64(), Some(1), "{}", baseline.data);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["import_rev"], json!(7));
}

#[test]
fn a_row_that_fails_its_checksum_refuses_the_import_even_when_its_record_is_terminal() {
    let t = testkit::TempDir::new("import-checksum");
    let path = written(&t, &dropped_outside_the_log("DROPPED"));
    let text = std::fs::read_to_string(&path).unwrap().replace("residue-closed-no-landing-evidence", "rewritten");
    std::fs::write(&path, text).unwrap();
    let (rc, out) = import_of(&t, &path);
    assert_eq!(rc, crate::REFUSED, "{out}");
    assert!(out.contains("archive: bead row sp-a fails its checksum"), "{out}");
    assert!(!t.path().join("store.db").exists());
}

#[test]
fn a_store_missing_the_baseline_fails_the_load_check() {
    let a = archived(&dropped_outside_the_log("DROPPED"));
    let mut p = projected(&a, &[]);
    map::record_baseline(&mut p, &map::Baseline { archive: "export.jsonl".into(), report: "export.jsonl.import-report.json".into(), as_stored: 1 });
    let mut got: BTreeMap<String, Vec<u8>> = p.items.iter().map(|(k, i)| (k.clone(), i.value.clone())).collect();
    assert_eq!(checks::loaded(&a, &p, &got), Vec::<String>::new());
    got.remove(keys::IMPORT_BASELINE);
    assert_eq!(checks::loaded(&a, &p, &got), [format!("import: 1 keys imported, 0 read back; first missing {}", keys::IMPORT_BASELINE)]);
}
