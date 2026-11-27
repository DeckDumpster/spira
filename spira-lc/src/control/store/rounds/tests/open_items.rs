use super::*;

use crate::control::records::LifecycleRecord;
use crate::plane::Plane;
use crate::work::{open_items, plan, split_reserved, Step};
use serde_json::{json, Value};

fn put_records<B: Backend>(r: &Rig<B>, rows: &[(String, &str, BeadState)]) {
    for chunk in rows.chunks(100) {
        let mut txn = state_store::Txn::new();
        for (id, repo, state) in chunk {
            let mut rec = LifecycleRecord::born(id, Some(repo), None, AT);
            rec.row.state = *state;
            txn = txn.put(&keys::lifecycle(id), state_store::Object::new(crate::control::records::encode(&rec).unwrap()));
        }
        r.s.contract().txn(&txn, &r.req()).unwrap();
    }
}

fn landed<B: Backend>(r: &Rig<B>, from: usize, to: usize) {
    let rows: Vec<_> = (from..to).map(|i| (format!("sp-z{i:04}"), "spira", BeadState::Landed)).collect();
    put_records(r, &rows);
}

/// READY and REWORK items in two repositories, one WORKING item, and 200 LANDED ones.
fn open_fixture<B: Backend>(r: &Rig<B>) {
    landed(r, 0, 200);
    put_records(
        r,
        &[
            ("sp-a1".into(), "spira", BeadState::Ready),
            ("sp-a2".into(), "spira", BeadState::Rework),
            ("sp-a3".into(), "spira", BeadState::Ready),
            ("sp-a4".into(), "spira", BeadState::Working),
            ("sp-b1".into(), "other", BeadState::Ready),
            ("sp-b2".into(), "other", BeadState::Rework),
        ],
    );
}

fn content(ids: &[&str]) -> Result<Vec<Value>, String> {
    Ok(ids
        .iter()
        .map(|id| {
            let body = if *id == "sp-a3" { "## Build\nno outcome here".to_string() } else { format!("## Outcome\n{id} works\n\n## Build\nx") };
            json!({ "id": id, "title": format!("title of {id}"), "priority": 1, "description": body })
        })
        .collect())
}

/// `work open-items <args>` as the service runs it, over the rig's store.
fn ask<B: Backend>(r: &Rig<B>, args: &[&str]) -> (i32, String) {
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let Step::OpenItems { repo, exclude, json } = plan("open-items", "-", &split_reserved(&argv)).unwrap().remove(0) else { panic!("not open-items") };
    let store = Plane(&r.s);
    open_items(&|a| store.read(a), &content, &repo, &exclude, json)
}

fn ids(out: &str) -> Vec<String> {
    let rows: Vec<Value> = serde_json::from_str(out).unwrap();
    rows.iter().map(|x| x["id"].as_str().unwrap().to_string()).collect()
}

#[test_id(tp4ewbg)]
#[test]
fn open_items_answers_one_repositorys_ready_and_rework_items_with_title_and_outcome() {
    let r = Rig::new();
    open_fixture(&r);
    let (rc, out) = ask(&r, &["--repo", "spira", "--json"]);
    assert_eq!(rc, 0, "{out}");
    let rows: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        rows,
        json!([
            { "id": "sp-a1", "state": "READY", "priority": 1, "title": "title of sp-a1", "outcome": "sp-a1 works" },
            { "id": "sp-a2", "state": "REWORK", "priority": 1, "title": "title of sp-a2", "outcome": "sp-a2 works" },
            { "id": "sp-a3", "state": "READY", "priority": 1, "title": "title of sp-a3", "outcome": null },
        ])
    );
    let (rc, out) = ask(&r, &["--repo", "spira", "--exclude", "sp-a1,sp-a3"]);
    assert_eq!((rc, out.as_str()), (0, "sp-a2 REWORK P1 title of sp-a2\n    sp-a2 works"));
}

#[test_id(t4aep9a)]
#[test]
fn open_items_reads_the_content_once_and_never_for_an_excluded_item() {
    let r = Rig::new();
    open_fixture(&r);
    let store = Plane(&r.s);
    let asked = std::cell::RefCell::new(Vec::new());
    let seen = |ids: &[&str]| {
        asked.borrow_mut().push(ids.join(","));
        content(ids)
    };
    let (rc, out) = open_items(&|a| store.read(a), &seen, "spira", &["sp-a2".to_string()], true);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(*asked.borrow(), vec!["sp-a1,sp-a3".to_string()]);
    let (rc, out) = open_items(&|a| store.read(a), &|_| Err("cannot tell: the content store is gone".into()), "spira", &[], true);
    assert_eq!((rc, out.as_str()), (crate::CANNOT_TELL, "cannot tell: the content store is gone"));
}

/// law-list-reads-paginate-in-the-database: only the state index of READY and REWORK is read.
#[test_id(t2fc13w)]
#[test]
fn open_items_reads_no_landed_record_however_many_there_are() {
    let r = Rig::on(Counting::default(), &clock());
    open_fixture(&r);
    let reads = || {
        r.s.contract().backend().take();
        let (rc, out) = ask(&r, &["--repo", "spira", "--json"]);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(ids(&out), ["sp-a1", "sp-a2", "sp-a3"]);
        r.s.contract().backend().take()
    };
    let small = reads();
    let states: Vec<&str> = small.iter().filter(|(k, _, _)| *k == "range_in_state").map(|(_, key, _)| key.as_str()).collect();
    let live = [format!("{}Ready", keys::LIFECYCLE), format!("{}Rework", keys::LIFECYCLE)];
    assert!(!states.is_empty() && states.iter().all(|s| live.iter().any(|l| l == s)), "{small:?}");
    assert!(!small.iter().any(|(k, key, _)| *k == "range" && key.starts_with(keys::LIFECYCLE)), "{small:?}");
    assert!(!small.iter().any(|(_, key, _)| key.contains("sp-z")), "a LANDED record was read: {small:?}");
    landed(&r, 200, 2_000);
    let large = reads();
    assert_eq!(small.len(), large.len(), "{large:?}");
    assert!(!large.iter().any(|(_, key, _)| key.contains("sp-z")), "{large:?}");
}

/// On the real store, `open-items` costs what the waiting items cost, not what history holds.
#[test_id(ty727vy)]
#[test]
fn open_items_costs_the_same_over_a_longer_history_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-open-items-steps");
    let b = SqliteBackend::open(&dir.path().join("c.db"), state_store_sqlite::Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Default::default() }).unwrap();
    let r = Rig::on(b, &clock());
    open_fixture(&r);
    let steps = || -> u64 {
        r.s.contract().backend().take_read_steps();
        let (rc, out) = ask(&r, &["--repo", "spira", "--json"]);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(ids(&out), ["sp-a1", "sp-a2", "sp-a3"]);
        r.s.contract().backend().take_read_steps()
    };
    let small = steps();
    landed(&r, 200, 2_000);
    let large = steps();
    assert!(large <= small + small / 4 + 50, "{small} VM steps over 200 LANDED, {large} over 2000");
}
