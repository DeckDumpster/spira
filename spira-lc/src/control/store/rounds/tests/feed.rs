use super::*;

#[test]
fn a_watch_past_compacted_history_answers_with_the_resume_revision_and_reads_no_row() {
    let r = Rig::on(Counting::default(), &clock());
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    let old = r.s.contract().head().unwrap();
    for i in 0..5 {
        r.ok(&["create-bead", &format!("sp-b{i}"), "--repo", "spira", "--ready"]);
    }
    let head = r.s.contract().head().unwrap();
    r.s.contract().compact(head).unwrap();
    r.s.contract().backend().take();
    let fed = r.s.watch(&[keys::LIFECYCLE, keys::READINESS], old, 0).unwrap();
    assert!(matches!(fed, crate::control::feed::Feed::Relist { list_rev, .. } if list_rev == head), "too old resumes from the head");
    let reads = r.s.contract().backend().take();
    assert!(reads.iter().all(|(k, ..)| *k != "range" && *k != "range_in_state"), "no row is read: {reads:?}");
}
