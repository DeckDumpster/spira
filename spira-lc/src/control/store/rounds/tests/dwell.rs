use super::*;

fn record_round(r: &Rig, row: lifecycle::batch::BatchRow, opened_at: i64) {
    let key = keys::batch(&row.repo, &row.batch_id);
    let rec = crate::control::records::BatchRecord { row, members: vec![], opened_at, round: Default::default(), batch_events: None };
    let mut c = super::Cascade::new();
    r.s.new_round(&mut c, &rec.row.repo, &rec.row.batch_id).unwrap().unwrap();
    r.s.put_round(&mut c, &key, &rec).unwrap();
    r.s.commit(c, &r.req(), "cut").unwrap();
}

#[test_id(tt2wkk9)]
#[test]
fn dwell_reports_the_newest_round_cut_per_repo_from_the_round_records_whatever_the_audit_log_holds() {
    let r = Rig::new();
    open_round(&r);
    record_round(&r, lifecycle::batch::BatchRow::cut("b-late", "spira", "H", "B"), AT + 30);
    record_round(&r, lifecycle::batch::BatchRow::staged("b-staged", "spira", "H", "B", "b-1"), AT + 50);
    record_round(&r, lifecycle::batch::BatchRow::cut("b-other", "other", "H", "B"), AT + 20);
    let d = r.s.dwell(AT + 100, 3_600, None).unwrap();
    assert_eq!(d.last["spira"]["Deliver"], AT + 30, "a staged round was not cut: {:?}", d.last);
    assert_eq!(d.last["other"]["Deliver"], AT + 20, "{:?}", d.last);
    let old = r.s.dwell(AT + 100_000, 3_600, None).unwrap();
    assert!(old.last.get("spira").is_none_or(|k| !k.contains_key("Deliver")), "a cut older than the window is not reported: {:?}", old.last);
}

/// The dwell verb's rounds come from the `opened_at` index, and a Staged round is still skipped.
#[test_id(th9mlzj)]
#[test]
fn dwell_reads_the_rounds_in_its_window_from_the_opened_at_index() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, 240, &[3, 7]);
    end_round(&r, "b-000", BatchState::Staged);
    r.s.contract().backend().take();
    let d = r.s.dwell(AT, 1000, None).unwrap();
    let reads = r.s.contract().backend().take();
    assert_eq!(ranges_of_rounds(&reads), 0, "{reads:?}");
    assert_eq!(reads.iter().filter(|(k, _, _)| *k == "range_newest_opened").count(), 1, "{reads:?}");
    assert_eq!(d.last["spira"]["Deliver"], AT - 100, "b-000 is staged and skipped; b-001 is next");
}

#[test_id(tua1kw3)]
#[test]
fn dwell_with_a_window_holding_the_cap_of_rounds_errors_naming_it() {
    let r = Rig::new();
    many_rounds(&r, 0, 8, &[]);
    let e = r.s.dwell_capped(AT, 100 * 8, None, 8).unwrap_err();
    assert!(e.to_string().contains("DWELL_ROUNDS_MAX"), "{e}");
    r.s.dwell_capped(AT, 100 * 8, None, 9).expect("one below the cap of rounds reads");
}
