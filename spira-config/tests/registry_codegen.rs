//! The config key registry is the one declaration of a key: the `[spira]` schema, the `KEY =>
//! field` mapping and the validation are read from it at run time, so a hand-kept list cannot
//! drift from it (conf_key_parity).

use std::collections::BTreeSet;
use std::fs;


fn only_in_first(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Vec<String> {
    a.difference(b).cloned().collect()
}

#[test]
fn the_difference_check_sees_a_planted_offender() {
    let a: BTreeSet<String> = ["X".to_string(), "Y".to_string()].into();
    let b: BTreeSet<String> = ["X".to_string()].into();
    assert_eq!(only_in_first(&a, &b), vec!["Y".to_string()]);
}

#[test]
fn the_required_key_check_sees_a_planted_offender() {
    let dir = testkit::TempDir::new("spira-config-registry-offender");
    fs::write(
        dir.path().join("SPIRA_PLANTED"),
        "TYPE=string\nGROUP=x\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT\nSPIRA_CONF_DEFAULT_EOF\n",
    )
    .unwrap();
    let registry = spira_config::registry::load(dir.path()).unwrap();
    assert!(registry["SPIRA_PLANTED"].default_expr().is_none());
}
