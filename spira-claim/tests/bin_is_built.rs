//! Its being here makes cargo build the `spira-claim` binary for every `cargo test` of this
//! package (binaries are built only when an integration test exists) — the unit tests in
//! src/tests.rs exec that binary from the same target directory.

#[macro_use]
extern crate spira_test_id;
#[test_id(tfa1ocu)]
#[test]
fn the_spira_claim_binary_is_built() {
    assert!(testkit::bin_exe("spira-claim").is_file());
}
