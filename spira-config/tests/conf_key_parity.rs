//! sp-9nljd's regression: a key neither `spira/conf.sh` nor the schema accepts is refused by
//! [`convert`], never warned about and silently dropped.
//!
//! The other half — every key conf.sh's `SPIRA_CONF_KEYS` allowlist accepts survives
//! `convert` → `export --sh` unchanged — reads the real `spira/conf.sh`, so it is a property
//! of the tree, not of this crate: it is spira-lint's `conf-key-registry` rule (sp-l8gl3),
//! run by the gate's lint step on every branch, including one that touches only conf.sh.

use spira_config::convert::convert;

// POSITIVE CONTROL: a key conf.sh does not accept (planted, never a real key) must be
// refused, not silently dropped — what lets conf-key-registry's check fail at all
// (law-absence-needs-a-positive-control).
#[test]
fn a_planted_unmapped_key_is_refused_not_dropped() {
    let conf_text = "SPIRA_TOTALLY_MADE_UP_TEST_KEY = 1\n";
    let errors = convert(conf_text, "/opt/fixture-home", "", &[])
        .expect_err("a key neither conf.sh nor the schema accepts must refuse the whole convert");
    assert!(
        errors.iter().any(|e| e.contains("SPIRA_TOTALLY_MADE_UP_TEST_KEY")),
        "expected an error naming the planted key, got {errors:?}"
    );
}
