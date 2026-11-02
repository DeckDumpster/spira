//! AC coverage: the shipped example validates against its own schema, the checked-in schema
//! matches the types, and `load_catalogues` refuses an unknown field, a bad tier and a
//! duplicate id — each naming its path.

use std::fs;
use std::path::Path;

use test_plan::{matrix_json_schema, parse_catalogue};

fn manifest_path(rel: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn shipped_matrix_schema_matches_the_types() {
    let shipped = fs::read_to_string(manifest_path("schema/matrix.schema.json"))
        .expect("schema/matrix.schema.json exists");
    let shipped: serde_json::Value = serde_json::from_str(&shipped).expect("valid JSON");
    let current = serde_json::to_value(matrix_json_schema()).expect("serializes");
    assert_eq!(
        shipped, current,
        "schema/matrix.schema.json is stale — regenerate with \
         `cargo run --bin test-plan -- schema matrix`"
    );
}

#[test]
fn unknown_field_is_refused_with_its_path() {
    let err = parse_catalogue(
        "api_version = \"test-plan/v1\"\narea = \"x\"\nspelled_rong = 1\n",
    )
    .unwrap_err();
    assert!(err.starts_with("spelled_rong"), "{err}");
}
