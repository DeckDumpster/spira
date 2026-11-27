use super::*;

/// law-list-reads-run-on-indexed-tables: `batches-of` over a store of many rounds reads the
/// bead's own membership range and the rounds it names, never the `batches/` prefix.
#[test_id(tovqyvx)]
#[test]
fn batches_of_reads_the_beads_own_rounds_not_every_round() {
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..40 {
        let (id, round) = (format!("sp-{i:04}"), format!("b-{i:04}"));
        r.certified(&id, T1);
        let members = vec![(id, T1.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: &round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    r.s.contract().backend().take();
    let rows = r.s.batches_of("sp-0023").unwrap();
    assert_eq!(rows.iter().map(|b| b.batch_id.as_str()).collect::<Vec<_>>(), ["b-0023"]);
    assert!(r.s.batches_of("sp-absent").unwrap().is_empty());
    let reads = r.s.contract().backend().take();
    assert!(reads.len() <= 8, "{} reads for one bead among 40 rounds: {reads:?}", reads.len());
    assert!(
        reads.iter().all(|(k, key, _)| *k == "get" || !key.starts_with(keys::BATCHES)),
        "a range over the rounds: {reads:?}"
    );
}

/// A member added by `stack` is found by `batches-of` as one named at the cut is.
#[test_id(t3g5yps)]
#[test]
fn batches_of_finds_a_member_stacked_into_an_open_round() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let first = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&first)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    r.certified("sp-b", T2);
    r.certified("sp-c", T2);
    let added = [("sp-b".to_string(), T2.to_string()), ("sp-c".to_string(), T2.to_string())];
    let p = r.s.plan_stack("b-1", &added, "test", AT).unwrap();
    r.s.run_planned(p, &r.req(), "stack").unwrap();
    for bead in ["sp-a", "sp-b", "sp-c"] {
        assert_eq!(r.s.batches_of(bead).unwrap().len(), 1, "{bead}");
    }
}
