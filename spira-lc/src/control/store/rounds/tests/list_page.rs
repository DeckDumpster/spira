use super::*;

#[test_id(tl2gr39)]
#[test]
fn a_list_with_after_and_limit_answers_one_page_of_rows_in_id_order() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-c", T1, "b-3"), ("sp-a", T2, "b-1"), ("sp-b", T1, "b-2")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    let page = |argv: &[&str], key: &str| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x[key].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(page(&["list", "--limit", "2"], "bead_id"), ["sp-a", "sp-b"]);
    assert_eq!(page(&["list", "--limit", "2", "--after", "sp-b"], "bead_id"), ["sp-c"]);
    assert_eq!(page(&["list", "--batches", "--limit", "2"], "batch_id"), ["b-1", "b-2"]);
    assert_eq!(page(&["list", "--batches", "--limit", "2", "--after", "b-2"], "batch_id"), ["b-3"]);
    assert_eq!(page(&["list", "--batches", "--after", "b-1"], "batch_id"), ["b-2", "b-3"]);
    for bad in [&["list", "--limit", "0"][..], &["list", "--batches", "--limit", "x"]] {
        assert_eq!(r.lc(bad).0, crate::CANNOT_TELL, "{bad:?}");
    }
}

#[test_id(t2qc7sb)]
#[test]
fn a_batch_filter_answers_the_round_for_an_id_with_a_slash_and_for_a_plain_id() {
    let r = Rig::new();
    for (id, tip, round) in [("sp-a", T1, "spira/queue/x"), ("sp-b", T2, "b-1")] {
        r.certified(id, tip);
        let members = vec![(id.to_string(), tip.to_string())];
        let p = r.s.plan_cut(&Cut { batch_id: round, ..r.cut_spec(&members) }).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
    }
    r.s.set_progress("spira", "spira/queue/x", serde_json::json!({"done": 1}), &r.req())
        .unwrap();
    let ids = |argv: &[&str]| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x["batch_id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&["list", "--batches", "--batch", "spira/queue/x"]), ["spira/queue/x"]);
    assert_eq!(ids(&["list", "--batches", "--batch", "b-1"]), ["b-1"]);
    assert_eq!(ids(&["list", "--batches", "--limit", "10"]), ["b-1", "spira/queue/x"]);
    assert_eq!(super::batch_key("batches/spira/spira/queue/x"), Some(("spira", "spira/queue/x")));
    assert_eq!(super::batch_key("batches/spira/b-1"), Some(("spira", "b-1")));
    assert_eq!(super::batch_key("batches/spira/spira/queue/x/progress"), None);
    assert_eq!(super::batch_key("batches/spira/b-1/progress"), None);
}

/// law: every list paginates in the store. `list --state X --limit N --after ID` over many
/// records is one state-index read bounded by the page plus a point read per row it answers;
/// it never ranges over `lifecycle/`, `readiness/` or the claim locks.
#[test_id(t5l3c5w)]
#[test]
fn a_state_filtered_page_is_one_bounded_read_of_the_state_index() {
    let r = Rig::on(Counting::default(), &clock());
    for i in 0..120 {
        r.ok(&["create-bead", &format!("sp-{i:03}"), "--repo", "spira", "--ready"]);
    }
    for i in (0..120).step_by(3) {
        r.bead_event(&format!("sp-{i:03}"), "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    }
    r.s.contract().backend().take();
    let (rc, out) = r.lc(&["list", "--state", "WORKING", "--limit", "5", "--after", "sp-030"]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = rows.as_array().unwrap().iter().map(|x| x["bead_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["sp-033", "sp-036", "sp-039", "sp-042", "sp-045"]);
    let reads = r.s.contract().backend().take();
    let ranges: Vec<_> = reads.iter().filter(|(k, _, _)| !k.starts_with("get")).collect();
    assert_eq!(
        ranges,
        [&("range_in_state", format!("{}Working", keys::LIFECYCLE), 5)],
        "one bounded read of the state index: {reads:?}"
    );
    let gets = reads.len() - ranges.len();
    assert!(gets <= 3 * ids.len(), "{gets} point reads for {} rows: {reads:?}", ids.len());
}

#[test_id(tj6zigz)]
#[test]
fn a_list_that_would_read_every_row_is_refused_naming_the_filters_that_do_not() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    r.ok(&["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    for argv in [
        &["list"][..],
        &["list", "--hold", "wait"],
        &["list", "--repo", "spira"],
        &["list", "--express"],
        &["list", "--since", "x"],
        &["list", "--state", "LANDED"],
        &["list", "--state", "READY,LANDED"],
    ] {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, crate::REFUSED, "{argv:?}: {out}");
        for exit in ["--live", "--ids", "--after", "--limit"] {
            assert!(out.contains(exit), "{argv:?} names {exit}: {out}");
        }
    }
    let ids = |argv: &[&str]| -> Vec<String> {
        let (rc, out) = r.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
        rows.as_array().unwrap().iter().map(|x| x["bead_id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&["list", "--live"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--state", "READY"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--ids", "sp-a"]), ["sp-a"]);
    assert_eq!(ids(&["list", "--after", "sp-a", "--limit", "2"]), ["sp-b"]);
    assert_eq!(ids(&["list", "--hold", "wait", "--live"]), Vec::<String>::new());
}
