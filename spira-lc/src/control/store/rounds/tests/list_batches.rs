use super::*;

/// `ROUNDS` rounds cut `100 * i` seconds before `AT`, all but `open` ended.
const ROUNDS: usize = 60;


/// law-list-reads-paginate-in-the-database: `list --batches` reads the rounds its answer shows
/// (the newest eight, a window's, the open ones, a page's), never the `batches/` prefix.
#[test_id(t8tvybk)]
#[test]
fn list_batches_reads_only_the_rounds_it_shows() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, ROUNDS, &[25, 33]);
    let ids = |argv: &[&str]| -> (Vec<String>, Vec<(&'static str, String, usize)>) {
        r.s.contract().backend().take();
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ids = rows.as_array().unwrap().iter().map(|x| x["batch_id"].as_str().unwrap().to_string()).collect();
        (ids, r.s.contract().backend().take())
    };
    let of = |ns: &[usize]| -> Vec<String> { ns.iter().map(|i| format!("b-{i:03}")).collect() };
    let bounded = |reads: &[(&'static str, String, usize)]| {
        assert!(
            reads.iter().all(|(k, p, limit)| *k == "get" || !(p.starts_with(keys::BATCHES) && *k == "range" && *limit > 1_000)),
            "an unbounded read of the rounds: {reads:?}"
        );
        assert!(reads.len() <= 70, "{} reads: {reads:?}", reads.len());
    };

    let (bare, reads) = ids(&["list", "--batches"]);
    assert_eq!(bare, of(&[0, 1, 2, 3, 4, 5, 6, 7]));
    assert!(reads.iter().any(|(k, _, limit)| *k == "range_newest_opened" && *limit == 8), "{reads:?}");
    bounded(&reads);

    let (windowed, reads) = ids(&["list", "--batches", "--window", "1000"]);
    assert_eq!(windowed, of(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 25, 33]));
    assert!(reads.iter().any(|(k, _, _)| *k == "range_newest_opened"), "{reads:?}");
    bounded(&reads);

    let (one, reads) = ids(&["list", "--batches", "--batch", "b-042"]);
    assert_eq!(one, of(&[42]));
    assert!(reads.iter().all(|(k, p, _)| *k == "get" || !p.starts_with(keys::BATCHES)), "{reads:?}");

    let (page, reads) = ids(&["list", "--batches", "--limit", "3", "--after", "b-010"]);
    assert_eq!(page, of(&[11, 12, 13]));
    bounded(&reads);
}

/// On the real store, what `list --batches` costs does not grow with the rounds behind it: the
/// VM steps of a bare list and of a window are the same over 16 rounds and over 96.
#[test_id(tl3fcmi)]
#[test]
fn list_batches_costs_the_same_over_a_larger_history_on_the_real_store() {
    let dir = testkit::TempDir::new("lc-batches-steps");
    let b = SqliteBackend::open(&dir.path().join("c.db"), state_store_sqlite::Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Default::default() }).unwrap();
    let r = Rig::on(b, &clock());
    let steps = |argv: &[&str]| -> u64 {
        r.s.contract().backend().take_read_steps();
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        r.s.contract().backend().take_read_steps()
    };
    let probes = [&["list", "--batches"][..], &["list", "--batches", "--window", "300"], &["list", "--batches", "--limit", "5", "--after", "b-010"]];
    cloned_certified(&r, 0, 16);
    rounds_of_certified(&r, 0, 16, &[13]);
    let small: Vec<u64> = probes.iter().map(|p| steps(p)).collect();
    r.s.contract().backend().take_read_steps();
    cloned_certified(&r, 16, 96);
    rounds_of_certified(&r, 16, 96, &[]);
    let large: Vec<u64> = probes.iter().map(|p| steps(p)).collect();
    for ((p, s), l) in probes.iter().zip(&small).zip(&large) {
        assert!(*l <= *s + *s / 4 + 50, "{p:?}: {s} VM steps over 16 rounds, {l} over 96");
    }
}

/// Rounds 0-9 of `many_rounds`, each with five pass events and an `Eject`, then landed.
fn eventful_rounds<B: Backend>(r: &Rig<B>) {
    use lifecycle::batch::BatchState;
    let open: Vec<usize> = (0..10).chain([25, 33]).collect();
    many_rounds(r, 0, ROUNDS, &open);
    for i in 0..10 {
        let round = format!("b-{i:03}");
        let kinds = [
            BatchEventKind::PassStarted { n: 1, head: "H1".into() },
            BatchEventKind::SuitesStarted { n: 1 },
            BatchEventKind::PassRed { n: 1, red_suites: vec!["test-a.sh".into()], suites_s: 3, build_s: 2, runner: None },
            BatchEventKind::Eject { bead_id: format!("sp-{i:03}"), reason: format!("red {i}") },
            BatchEventKind::PassRebuilt { head: "H2".into() },
            BatchEventKind::PassStarted { n: 2, head: "H2".into() },
        ];
        for (k, kind) in kinds.into_iter().enumerate() {
            let (_, rec) = r.s.batch(&round).unwrap().unwrap();
            let ev = BatchEvent { expect: rec.value.row.state, version: rec.value.row.version, kind, actor: "test".into() };
            let ran = r.s.batch_event(&round, &ev, AT - 100 * i as i64 + k as i64, &r.req()).unwrap();
            assert!(matches!(&ran, Ran::Done { applied, .. } if applied == &[true]), "{round} event {k}: {ran:?}");
        }
        let (_, mut rec) = r.s.batch(&round).unwrap().unwrap();
        rec.value.row.state = BatchState::Landed;
        let put = state_store::Txn::new().put(
            &keys::batch("spira", &round),
            state_store::Object::new(crate::control::records::encode(&rec.value).unwrap()),
        );
        r.s.contract().txn(&put, &r.req()).unwrap();
    }
}

/// `list --batches` answers each round's passes, ejections and last event from the round's own
/// record: no history range and no `events/` read, and at most two point reads a round shown.
#[test_id(t6lkeqe)]
#[test]
fn list_batches_reads_each_rounds_events_from_its_record_never_history() {
    let r = Rig::on(Counting::default(), &clock());
    eventful_rounds(&r);
    for (argv, shown) in [
        (&["list", "--batches", "--window", "1000"][..], 13usize),
        (&["list", "--batches", "--limit", "3", "--after", "b-010"], 3),
        (&["list", "--batches", "--batch", "b-003"], 1),
    ] {
        r.s.contract().backend().take();
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let reads = r.s.contract().backend().take();
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), shown, "{argv:?}");
        let history: Vec<_> = reads.iter().filter(|(_, k, _)| k.starts_with(keys::HISTORY) || k.starts_with("events/")).collect();
        assert!(history.is_empty(), "{argv:?}: {history:?}");
        let points = reads.iter().filter(|(k, key, _)| k.starts_with("get") && key.starts_with("batches/")).count();
        assert!(points <= 2 * shown, "{argv:?}: {points} point reads for {shown} rounds: {reads:?}");
    }
}

/// The three scopes over `eventful_rounds`, as they were answered from history.
#[test_id(to10clf)]
#[test]
fn list_batches_answers_the_same_bytes_as_it_did_from_history() {
    let r = Rig::new();
    eventful_rounds(&r);
    let mut got = serde_json::Map::new();
    for (name, argv) in [
        ("window", &["list", "--batches", "--window", "1000"][..]),
        ("page", &["list", "--batches", "--limit", "3", "--after", "b-010"]),
        ("one", &["list", "--batches", "--batch", "b-003"]),
    ] {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        got.insert(name.into(), serde_json::from_str(&out).unwrap());
    }
    let got = serde_json::to_string_pretty(&serde_json::Value::Object(got)).unwrap();
    assert_eq!(got, include_str!("../list_batches_pinned.json").trim_end());
}

/// A round stored before migration 6 has no `batch_events`: `list --batches` says so and does
/// not read its history, and a later event does not give it a partial field.
#[test_id(t4hy1rr)]
#[test]
fn a_round_without_batch_events_makes_list_batches_name_the_migration() {
    let r = Rig::new();
    many_rounds(&r, 0, 2, &[0]);
    let key = keys::batch("spira", "b-000");
    let strip = || {
        let mut o = r.s.contract().get(&key).unwrap().unwrap().value;
        o.data.as_object_mut().unwrap().remove(crate::control::records::BATCH_EVENTS);
        r.s.contract().txn(&state_store::Txn::new().put(&key, o), &r.req()).unwrap();
    };
    strip();
    let (rc, out) = r.lc(&["list", "--batches", "--batch", "b-000"]);
    assert_eq!(rc, crate::CANNOT_TELL, "{out}");
    assert!(out.contains("migration 6") && out.contains(&key), "{out}");
    let (_, rec) = r.s.batch("b-000").unwrap().unwrap();
    let ev = BatchEvent { expect: rec.value.row.state, version: rec.value.row.version, kind: BatchEventKind::CiStarted { run: "r1".into() }, actor: "test".into() };
    r.s.batch_event("b-000", &ev, AT, &r.req()).unwrap();
    assert!(r.s.contract().get(&key).unwrap().unwrap().value.data.get(crate::control::records::BATCH_EVENTS).is_none());
    assert_eq!(r.lc(&["list", "--batches", "--batch", "b-001"]).0, 0, "a round that has the field still lists");
}

/// Migration 6 fills `batch_events` with the events `fold_batch_events` keeps: the two lists
/// name the same events.
#[test_id(tslzvkv)]
#[test]
fn migration_6_selects_the_events_the_fold_keeps() {
    let sql = state_store_sqlite::MIGRATIONS[5].join("\n");
    let listed: Vec<&str> = crate::passes::FETCHED.split(',').collect();
    assert_eq!(listed.len(), 8);
    for quoted in listed {
        assert!(sql.contains(quoted), "{quoted} is fetched but migration 6 does not select it");
    }
}

/// Every state the round machine can be in is either listed as open or is terminal: a round in
/// a state `list --batches --window` forgot would vanish from it.
#[test_id(thkn81q)]
#[test]
fn every_batch_state_is_listed_as_open_or_is_terminal() {
    use lifecycle::batch::BatchState::*;
    for s in [Staged, Open, CiRunning, Green, Landing, Deploying, Attributing, Rebuilding, Landed, Settled, Abandoned, Discarded] {
        match s {
            Staged | Open | CiRunning | Green | Landing | Deploying | Attributing | Rebuilding => {
                assert!(super::OPEN_BATCH_STATES.contains(&s) && !s.is_terminal(), "{s:?}")
            }
            Landed | Settled | Abandoned | Discarded => {
                assert!(!super::OPEN_BATCH_STATES.contains(&s) && s.is_terminal(), "{s:?}")
            }
        }
    }
}

/// A round prints the largest `pr` among its members' deliveries that belong to it, else null.
#[test_id(t12simv)]
#[test]
fn list_batches_prints_the_pr_of_a_members_delivery_in_the_round() {
    let r = Rig::on(Counting::default(), &clock());
    open_round(&r);
    let pr_of = |r: &Rig<Counting>| {
        let (rc, out) = r.lc(&["list", "--batches"]);
        assert_eq!(rc, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()[0]["pr"].clone()
    };
    assert_eq!(pr_of(&r), serde_json::Value::Null);
    let key = keys::delivery("sp-a");
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["pr"] = serde_json::json!(42);
    let t = state_store::Txn::new().when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev))).put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    assert_eq!(pr_of(&r), serde_json::json!(42));
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["batch_id"] = serde_json::json!("b-other");
    let t = state_store::Txn::new().when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev))).put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    assert_eq!(pr_of(&r), serde_json::Value::Null, "a delivery of another round is not this round's pr");
}
