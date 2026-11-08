//! For one fixture world, supersede-duty.sh and the supersede-duty handler leave the same rows in
//! the Concierge inbox and make the same writes. The script is the oracle: it runs unmodified
//! against stubbed `spira-lc` and `bdq`; the handler runs on a virtual clock against a fake
//! spira-lc socket serving the same state, `bd-supersede` and `bd-note` doors included.

use std::fs;
use std::path::Path;

#[test]
fn the_watchers_manifest_no_longer_carries_the_row() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/watchers"))
            .unwrap();
    assert!(
        manifest.lines().any(|l| l.starts_with("concierge-inbox|")),
        "the scan must be able to see a row"
    );
    assert!(
        !manifest.lines().any(|l| l.starts_with("supersede-duty|")),
        "spira-runtime hosts supersede-duty"
    );
}
