use super::*;

fn seed<T: serde::Serialize>(r: &Rig<Counting>, key: &str, rec: &T) {
    r.s.contract()
        .create(key, state_store::Object::new(crate::control::records::encode(rec).unwrap()), &r.req())
        .unwrap();
}

fn ask_rec(id: &str, state: lifecycle::ask::AskState, at: i64) -> crate::control::store::asks::AskRecord {
    let mut row = lifecycle::ask::AskRow::open(id, None);
    row.state = state;
    crate::control::store::asks::AskRecord { row, opened_at: at, closed_at: None }
}

fn reads_of(r: &Rig<Counting>) -> Vec<(&'static str, String)> {
    r.s.contract().backend().take().into_iter().map(|(k, p, _)| (k, p)).collect()
}

/// law: a list reads the live set. `list-asks` over many closed asks reads the OPEN state index
/// only, and `--state ALL` still answers every ask.
#[test_id(tb7o6w2)]
#[test]
fn list_asks_reads_only_the_open_asks_and_all_reads_every_one() {
    use lifecycle::ask::AskState;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..300 {
        let s = if i % 2 == 0 { AskState::Answered } else { AskState::DefaultTaken };
        seed(&r, &keys::ask(&format!("a-{i:03}")), &ask_rec(&format!("a-{i:03}"), s, 1_000 + i));
    }
    seed(&r, &keys::ask("a-z2"), &ask_rec("a-z2", AskState::Open, 5_000));
    seed(&r, &keys::ask("a-z1"), &ask_rec("a-z1", AskState::Open, 4_000));
    reads_of(&r);
    let (rc, out) = r.lc(&["list-asks"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = rows.as_array().unwrap().iter().map(|a| a["ask_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a-z1", "a-z2"]);
    let reads = reads_of(&r);
    assert!(reads.iter().any(|(k, p)| *k == "range_in_state" && p == "asks/Open"), "{reads:?}");
    assert!(!reads.iter().any(|(k, _)| *k == "range"), "{reads:?}");
    let (rc, out) = r.lc(&["list-asks", "--state", "ALL"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap().as_array().unwrap().len(), 302);
}

/// law: a list reads the live set. `mending list --live` over many returned rows ranges the
/// WAITING and MENDING state indexes only, and reads one ask per live row at most.
#[test_id(tz3xcw0)]
#[test]
fn mending_list_live_reads_only_the_waiting_and_mending_rows() {
    use lifecycle::mending::MendingState;
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..300 {
        let mut rec = crate::control::store::mending::MendingRecord::failed("b-old", 1, &format!("s{i:03}"), 1_000 + i);
        rec.row.state = MendingState::Returned;
        seed(&r, &keys::mending(&rec.row.key()), &rec);
    }
    let mut waiting = crate::control::store::mending::MendingRecord::failed("b-1", 1, "unit", 2_000);
    waiting.row.awaiting_ask = Some("a-1".into());
    seed(&r, &keys::mending(&waiting.row.key()), &waiting);
    let mut mending = crate::control::store::mending::MendingRecord::failed("b-1", 1, "lint", 3_000);
    mending.row.state = MendingState::Mending;
    seed(&r, &keys::mending(&mending.row.key()), &mending);
    seed(&r, &keys::ask("a-1"), &ask_rec("a-1", lifecycle::ask::AskState::Open, 1));
    reads_of(&r);
    let (rc, out) = r.lc(&["mending", "list", "--live"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let got: Vec<(&str, bool)> = rows.as_array().unwrap().iter().map(|m| (m["suite"].as_str().unwrap(), m["ask_open"].as_bool().unwrap())).collect();
    assert_eq!(got, [("unit", true), ("lint", false)]);
    let reads = reads_of(&r);
    assert!(!reads.iter().any(|(k, p)| *k == "range" && p.starts_with("mendings/")), "{reads:?}");
    assert!(reads.iter().filter(|(k, _)| *k == "range_in_state").all(|(_, p)| p == "mendings/Waiting" || p == "mendings/Mending"), "{reads:?}");
    assert!(reads.iter().filter(|(_, p)| p.starts_with("asks/")).count() <= 2, "{reads:?}");
}
