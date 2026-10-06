//! Its being here makes cargo build the `spira-claim` binary for every `cargo test` of this
//! package (binaries are built only when an integration test exists) — the unit tests in
//! src/tests.rs exec that binary from the same target directory.

#[test]
fn the_spira_claim_binary_is_built() {
    assert!(std::path::Path::new(env!("CARGO_BIN_EXE_spira-claim")).is_file());
}
