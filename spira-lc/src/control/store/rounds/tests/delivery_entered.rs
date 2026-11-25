use super::*;

const DELIVERIES: usize = 3_000;

const AUDITS: usize = 2;

/// `DELIVERIES` queued deliveries, each with `AUDITS` history entries, written to the backend
/// as the import writes them: each delivery's `entered` beside its row, its history under
/// `history/<id>/`. Answers the `entered_at` each should list with.
fn many_deliveries(b: &Counting) -> Vec<(String, i64)> {
    use crate::control::records::{fold_entered, ENTERED};
    use state_store::{Object, Rev, Write};
    let mut writes = Vec::new();
    let mut want = Vec::new();
    for n in 0..DELIVERIES {
        let id = format!("sp-{n:05}");
        let audits: Vec<Audit> = (0..AUDITS)
            .map(|k| Audit {
                machine: if k % 2 == 0 { "delivery" } else { "bead" }.into(),
                lc_key: id.clone(),
                event: "Probe".into(),
                expect: "QUEUED".into(),
                from_state: "QUEUED".into(),
                to_state: "QUEUED".into(),
                applied: true,
                refusal: None,
                evidence: serde_json::Value::Null,
                actor: "test".into(),
                at: AT + (n * AUDITS + k) as i64,
            })
            .collect();
        let mut entered = serde_json::Map::new();
        fold_entered(&mut entered, &id, &audits);
        want.push((id.clone(), entered["QUEUED"].as_i64().unwrap()));
        let mut row = serde_json::to_value(lifecycle::delivery::DeliveryRow::start_queue(id.as_str())).unwrap();
        row[ENTERED] = serde_json::Value::Object(entered);
        writes.push((keys::delivery(&id), Object::new(row)));
        for (k, a) in audits.into_iter().enumerate() {
            let seq = (n * AUDITS + k) as u64;
            let entry = serde_json::json!({ "seq": seq, "audit": a });
            writes.push((keys::history_entry(&id, 0, &format!("import/{seq:020}")), Object::new(entry)));
        }
    }
    for chunk in writes.chunks(20_000) {
        let head = b.inner.head().unwrap();
        let batch: Vec<Write> = chunk
            .iter()
            .enumerate()
            .map(|(sub, (key, v))| Write::Put {
                key: key.clone(),
                value: v.encode(),
                create_rev: Rev::new(head + 1, sub as u32),
                prev: None,
            })
            .collect();
        b.inner.commit(head, &batch).unwrap();
    }
    want
}

/// law: a list reads its page from the current record, in the store. `list --delivery` over
/// 3k deliveries of 50 audits each reads the deliveries alone, a page at a time, and never a
/// delivery's history; a page of 100 is one bounded read.
#[test_id(tgm9ss2)]
#[test]
fn list_delivery_reads_entered_at_from_the_record_never_history() {
    let r = Rig::on(Counting::default(), &clock());
    let want = many_deliveries(r.s.contract().backend());
    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--delivery"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let got: Vec<(String, i64)> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["bead_id"].as_str().unwrap().to_string(), x["entered_at"].as_str().unwrap().parse().unwrap()))
        .collect();
    assert_eq!(got, want);
    let reads = r.s.contract().backend().take();
    let elsewhere: Vec<_> = reads.iter().filter(|(_, k, _)| k != keys::DELIVERIES).take(3).collect();
    assert!(elsewhere.is_empty(), "{} reads, some outside deliveries/: {elsewhere:?}", reads.len());
    assert!(reads.len() <= DELIVERIES / 1_000 + 1, "{} reads for {DELIVERIES} deliveries", reads.len());

    let (rc, out) = r.lc(&["list", "--delivery", "--state", "QUEUED", "--limit", "100", "--after", "sp-01000"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 100);
    assert_eq!(rows[0]["bead_id"], "sp-01001");
    assert_eq!(
        r.s.contract().backend().take(),
        [("range_in_state", format!("{}Queued", keys::DELIVERIES), 100)],
        "one bounded read of the state index"
    );
}

/// Every txn that moves a delivery keeps its `entered` current, so the record answers
/// `entered_at` as its history does.
#[test_id(th7dllm)]
#[test]
fn a_deliverys_entered_at_from_its_record_matches_its_history() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-a", T1, "b-1"), ("sp-b", T2, "b-2")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    let (rc, out) = r.lc(&["list", "--delivery"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{out}");
    for row in rows.as_array().unwrap() {
        let id = row["bead_id"].as_str().unwrap();
        let state = row["state"].as_str().unwrap();
        let from_history = r
            .s
            .history(id, Some("delivery"))
            .unwrap()
            .into_iter()
            .filter(|c| c.audit.applied && c.audit.to_state == state)
            .map(|c| c.audit.at)
            .max();
        assert!(from_history.is_some(), "{id}: no applied event into {state}");
        assert_eq!(row["entered_at"], serde_json::json!(from_history.map(|t| t.to_string())), "{id}");
        let item = r.s.contract().get(&keys::delivery(id)).unwrap().unwrap();
        assert!(item.value.data.get(crate::control::records::ENTERED).is_some(), "{id}: no entered on the record");
    }
}
