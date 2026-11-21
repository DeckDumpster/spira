//! `parse_catalogue` refuses an unknown field naming its path.

#[macro_use]
extern crate spira_test_id;
use test_plan::parse_catalogue;

#[test_id(t62m6kb)]
#[test]
fn unknown_field_is_refused_with_its_path() {
    let err = parse_catalogue(
        "api_version = \"test-plan/v1\"\narea = \"x\"\nspelled_rong = 1\n",
    )
    .unwrap_err();
    assert!(err.starts_with("spelled_rong"), "{err}");
}
