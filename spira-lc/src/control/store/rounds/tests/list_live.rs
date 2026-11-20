use super::*;

const LIVE_READY: usize = 100;

const LIVE_WORKING: usize = 40;

const LANDED: usize = 200;

/// `LIVE_READY` READY beads each blocked by the next READY bead and one LANDED bead, `LIVE_WORKING`
/// WORKING beads holding live claim locks, and `LANDED` LANDED beads. Answers the live ids.
fn live_fixture<B: Backend>(r: &Rig<B>) -> Vec<String> {
    for chunk in (0..LANDED).collect::<Vec<_>>().chunks(100) {
        let mut txn = state_store::Txn::new();
        for i in chunk {
            let mut rec = crate::control::records::LifecycleRecord::born(
                &format!("sp-z{i:03}"),
                Some("spira"),
                None,
                AT,
            );
            rec.row.state = BeadState::Landed;
            txn = txn.put(
                &keys::lifecycle(&format!("sp-z{i:03}")),
                state_store::Object::new(crate::control::records::encode(&rec).unwrap()),
            );
        }
        r.s.contract().txn(&txn, &r.req()).unwrap();
    }
    let mut live = Vec::new();
    for i in 0..LIVE_READY {
        let id = format!("sp-r{i:03}");
        r.ok(&["create-bead", &id, "--repo", "spira", "--ready"]);
        live.push(id);
    }
    for i in 0..LIVE_WORKING {
        let id = format!("sp-w{i:03}");
        r.ok(&["create-bead", &id, "--repo", "spira", "--ready"]);
        r.bead_event(
            &id,
            "READY",
            "0",
            r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#,
        );
        live.push(id);
    }
    let mut txn = state_store::Txn::new();
    for i in 0..LIVE_READY {
        let blocked = crate::control::records::Readiness {
            blocked_by: vec![
                format!("sp-r{:03}", (i + 1) % LIVE_READY),
                format!("sp-z{i:03}"),
            ],
            ..Default::default()
        };
        txn = txn.put(
            &keys::readiness(&format!("sp-r{i:03}")),
            state_store::Object::new(crate::control::records::encode(&blocked).unwrap()),
        );
    }
    r.s.contract().txn(&txn, &r.req()).unwrap();
    live
}

/// law-list-reads-paginate-in-the-database: the records named by `--ids`, the claim locks of
/// WORKING rows and the states of off-page blockers are each read in chunks, never a `get` per key.
#[test]
fn a_non_bare_list_reads_records_locks_and_blockers_in_chunks_not_per_key() {
    let r = Rig::on(Counting::default(), &clock());
    let live = live_fixture(&r);
    let lock_prefix = keys::claim_lock("");
    for argv in [
        vec!["list", "--ids", live.join(",").leak() as &str],
        vec!["list", "--live"],
    ] {
        r.s.contract().backend().take();
        r.s.contract().backend().take_many_calls();
        let (rc, out) = r.lc(&argv);
        assert_eq!(rc, 0, "{out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), live.len());
        let first_blocked = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["bead_id"] == "sp-r000")
            .unwrap();
        assert_eq!(
            first_blocked["blocked_by"],
            serde_json::json!(["sp-r001"]),
            "the LANDED blocker no longer blocks"
        );
        let reads = r.s.contract().backend().take();
        let gets: Vec<_> = reads
            .iter()
            .filter(|(k, key, _)| {
                *k == "get" && (key.starts_with(keys::LIFECYCLE) || key.starts_with(&lock_prefix))
            })
            .collect();
        assert!(gets.is_empty(), "{argv:?}: one get per key: {gets:?}");
        let calls = r.s.contract().backend().take_many_calls();
        let of = |p: &str| calls.iter().filter(|k| k.starts_with(p)).count();
        assert!(of(&lock_prefix) <= 1, "{argv:?}: {calls:?}");
        assert!(of(keys::LIFECYCLE) <= 2, "{argv:?}: {calls:?}");
    }
    let page = |argv: &[&str]| -> serde_json::Value {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    };
    let by_ids = page(&["list", "--ids", &live.join(",")]);
    let expected: Vec<&serde_json::Value> = by_ids.as_array().unwrap().iter().take(7).collect();
    let by_state = page(&["list", "--state", "READY,WORKING", "--limit", "7"]);
    assert_eq!(by_state.as_array().unwrap().iter().collect::<Vec<_>>(), expected, "a state page stops at its limit, in id order");
}

/// On the real store a `list --ids` costs the same read transactions for 140 ids as for 10.
#[test]
fn list_ids_read_transactions_do_not_grow_with_the_ids_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-ids-reads");
    let b = SqliteBackend::open(
        &dir.path().join("c.db"),
        state_store_sqlite::Options::default(),
    )
    .unwrap();
    let r = Rig::on(b, &clock());
    let live = live_fixture(&r);
    let reads = |n: usize| -> u64 {
        let rec = std::sync::Arc::new(state_store::phases::Recorder::default());
        let _t = state_store::phases::track_scoped(rec.clone());
        let (rc, out) = r.lc(&["list", "--ids", &live[..n].join(",")]);
        assert_eq!(rc, 0, "{out}");
        rec.snapshot().reads
    };
    let (few, many) = (reads(10), reads(live.len()));
    assert!(
        many <= few + 2,
        "{few} read transactions for 10 ids, {many} for {}",
        live.len()
    );
}
