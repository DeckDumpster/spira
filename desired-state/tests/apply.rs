//! End-to-end: the shipped canonical default validates against the shipped schema, and
//! `spira apply` behaves like the composer applied to a federation of one.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spira_desired_state::compose::compose;
use spira_desired_state::producer::parse_fragment;
use spira_desired_state::resource::json_schema;
use spira_desired_state::store::{now_rfc3339, FsStore, MaterializeOutcome};

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spira-desired-state-apply-test-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn t0_shipped_default_document_validates() {
    let text = fs::read_to_string(manifest_path("examples/default.toml")).expect("example exists");
    let fragment = parse_fragment(&text).expect("the canonical default must parse as a fragment");
    compose(&[fragment]).expect("the canonical default must compose with itself");
}

#[test]
fn shipped_schema_matches_the_types() {
    let shipped = fs::read_to_string(manifest_path("schema/resources.schema.json"))
        .expect("schema/resources.schema.json exists");
    let shipped: serde_json::Value = serde_json::from_str(&shipped).expect("shipped schema is valid JSON");
    let current = serde_json::to_value(json_schema()).expect("current schema serializes");
    assert_eq!(
        shipped, current,
        "schema/resources.schema.json is stale — regenerate with `cargo run --bin spira -- schema`"
    );
}

#[test]
fn applying_the_default_document_bootstraps_a_fresh_install() {
    let text = fs::read_to_string(manifest_path("examples/default.toml")).unwrap();
    let fragment = parse_fragment(&text).unwrap();
    let composite = compose(&[fragment.clone()]).unwrap();

    let dir = tempdir();
    let store = FsStore::new(&dir);
    assert!(store.latest().unwrap().is_none(), "a fresh install has no prior version");

    let outcome = store
        .write_version(&composite, &[fragment.producer.clone()], &now_rfc3339())
        .unwrap();
    assert_eq!(outcome, MaterializeOutcome::NewVersion { version: 1 });

    let outcome = store
        .write_version(&composite, &[fragment.producer], &now_rfc3339())
        .unwrap();
    assert_eq!(outcome, MaterializeOutcome::Unchanged { version: 1 }, "re-applying the same document must not manufacture a new version");

    fs::remove_dir_all(&dir).ok();
}
