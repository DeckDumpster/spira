use super::*;

/// A filing as a crate sends it: refused by the server for a missing build spec, written and
/// given its row with one; a description replacement from the request body is a served verb.
#[test_id(txdywgs)]
#[test]
fn a_work_filing_over_the_door_meets_the_contract_and_is_written_with_its_row() {
    let w = World::new();
    let f = Filing(&w, "sp-new");
    let create = ["content", "create", "T", "--labels", "builder-lane,repo:spira", "--body-file", "-", "--silent"];
    let ans = served_with(&w, &f, &create, "just an outcome");
    assert_eq!(ans.code, bead::guard::REFUSED, "{ans:?}");
    assert!(ans.stderr.starts_with("bead: refusing: a bead for builder must name what to build"), "{ans:?}");
    assert!(state(&w, "sp-new").is_none(), "nothing was filed");
    let ans = served_with(&w, &f, &create, "## Build\nthe artifact, and where\n");
    assert_eq!((ans.code, ans.stdout.as_str()), (0, "sp-new\n"), "{ans:?}");
    assert_eq!(state(&w, "sp-new").map(|r| r.state), Some("READY".to_string()));
    let ans = served_with(&w, &f, &["content", "update", "sp-new", "--body-file", "-"], "a new description");
    assert_eq!(ans.code, 0, "{ans:?}");
}
