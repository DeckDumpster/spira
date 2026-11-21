use super::*;

/// law-hot-reads-answer-in-500ms: an unfiltered `list` reads its rows' readiness by key in
/// batched reads, never by ranging `readiness/`, and the rows are the ones a read by key answers.
#[test_id(tjgd74j)]
#[test]
fn a_whole_table_list_reads_readiness_by_key_never_by_range() {
    const BEADS: usize = 600;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..BEADS {
        r.ok(&["create-bead", &format!("sp-{i:04}"), "--repo", "spira", "--ready"]);
    }
    r.bead_event("sp-0002", "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    let blocked = crate::control::records::Readiness {
        blocked_by: vec!["sp-0001".into(), "sp-0002".into(), "sp-gone".into()],
        ..Default::default()
    };
    let seed = state_store::Txn::new().put(
        &keys::readiness("sp-0500"),
        state_store::Object::new(crate::control::records::encode(&blocked).unwrap()),
    );
    r.s.contract().txn(&seed, &r.req()).unwrap();

    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--live"]);
    assert_eq!(rc, 0, "{out}");
    let reads = r.s.contract().backend().take();
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), BEADS);
    assert!(!reads.iter().any(|(k, key, _)| *k == "range" && key.starts_with(keys::READINESS)), "{reads:?}");
    let readiness_keys = reads.iter().filter(|(k, key, _)| k.starts_with("get") && key.starts_with(keys::READINESS)).count();
    assert!(readiness_keys <= BEADS, "{readiness_keys} readiness keys read for {BEADS} rows");

    let probe = ["sp-0000", "sp-0002", "sp-0007", "sp-0014", "sp-0500", "sp-0599"];
    let (rc, by_key) = r.lc(&["list", "--ids", &probe.join(",")]);
    assert_eq!(rc, 0, "{by_key}");
    let by_key: serde_json::Value = serde_json::from_str(&by_key).unwrap();
    let ranged: Vec<&serde_json::Value> =
        rows.iter().filter(|x| probe.contains(&x["bead_id"].as_str().unwrap())).collect();
    assert_eq!(ranged, by_key.as_array().unwrap().iter().collect::<Vec<_>>());
    assert_eq!(ranged[4]["blocked_by"], serde_json::json!(["sp-0001", "sp-0002"]), "a blocker with no record no longer blocks");
}

fn seed_readiness(r: &Rig<impl Backend>, ids: impl Iterator<Item = String>) {
    let blocked = crate::control::records::Readiness { blocked_by: vec!["sp-x".into()], ..Default::default() };
    let value = crate::control::records::encode(&blocked).unwrap();
    let ids: Vec<String> = ids.collect();
    for chunk in ids.chunks(100) {
        let mut txn = state_store::Txn::new();
        for id in chunk {
            txn = txn.put(&keys::readiness(id), state_store::Object::new(value.clone()));
        }
        r.s.contract().txn(&txn, &r.req()).unwrap();
    }
}

/// law-list-reads-paginate-in-the-database: the readiness of a few hundred ids spread across a
/// large `readiness/` prefix is read by their keys, never by ranging the prefix.
#[test_id(tubc0be)]
#[test]
fn readiness_of_spread_ids_reads_their_keys_and_never_ranges_the_table() {
    let r = Rig::on(Counting::default(), &clock());
    seed_readiness(&r, (0..3_000).map(|i| format!("sp-{i:04}")));
    let ids: Vec<String> = (0..3_000).step_by(10).map(|i| format!("sp-{i:04}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let head = r.s.contract().head().unwrap();
    r.s.contract().backend().take();
    let got = r.s.readiness_of(&refs, head).unwrap();
    let reads = r.s.contract().backend().take();
    assert!(!reads.iter().any(|(k, key, _)| *k == "range" && key.starts_with(keys::READINESS)), "{reads:?}");
    let keys_read = reads.iter().filter(|(k, _, _)| k.starts_with("get")).count();
    assert!(keys_read <= ids.len(), "{keys_read} keys read for {} ids", ids.len());
    assert_eq!(got.len(), ids.len());
    for id in &refs {
        assert_eq!(Some(&got[*id]), r.s.readiness_at(id, head).unwrap().as_ref(), "{id}");
    }
}

/// On the real store, `list --live` costs what its rows cost, not what the readiness table behind
/// them holds.
#[test_id(tys75ow)]
#[test]
fn list_live_costs_the_same_over_a_larger_readiness_table_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-live-steps");
    let b = SqliteBackend::open(&dir.path().join("c.db"), state_store_sqlite::Options::default()).unwrap();
    let r = Rig::on(b, &clock());
    for i in 0..300 {
        r.ok(&["create-bead", &format!("sp-{:05}", i * 11), "--repo", "spira", "--ready"]);
    }
    let steps = || -> u64 {
        r.s.contract().backend().take_read_steps();
        let (rc, out) = r.lc(&["list", "--live"]);
        assert_eq!(rc, 0, "{out}");
        r.s.contract().backend().take_read_steps()
    };
    let small = steps();
    seed_readiness(&r, (0..3_300).filter(|i| i % 11 != 0).map(|i| format!("sp-{i:05}")));
    let large = steps();
    assert!(large <= small + small / 4 + 50, "{small} VM steps over 300 rows, {large} with 3000 more readiness rows");
}
