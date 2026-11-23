//! For one fixture world, supersede-duty.sh and the supersede-duty handler leave the same rows in
//! the Concierge inbox and make the same writes. The script is the oracle: it runs unmodified
//! against stubbed `spira-lc` and `bdq`; the handler runs on a virtual clock against a fake
//! spira-lc socket serving the same state, `bd-supersede` and `bd-note` doors included.

use std::fs;

#[test_id(tv1d9mg)]
#[test]
fn the_watchers_manifest_no_longer_carries_the_row() {
    let manifest = fs::read_to_string(testkit::manifest_dir().join("../spira/watchers")).unwrap();
    assert!(
        manifest.lines().any(|l| l.starts_with("concierge-inbox|")),
        "the scan must be able to see a row"
    );
    assert!(
        !manifest.lines().any(|l| l.starts_with("supersede-duty|")),
        "spira-runtime hosts supersede-duty"
    );
}
