//! AC coverage: the shipped example validates against its own schema, the checked-in schema
//! matches the types, and `load_catalogues` refuses an unknown field, a bad tier and a
//! duplicate id — each naming its path.

use std::fs;
use std::path::Path;

use test_plan::{catalogue_json_schema, load_catalogues, matrix_json_schema, parse_catalogue};

fn manifest_path(rel: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn t0_shipped_example_validates() {
    let text = fs::read_to_string(manifest_path("examples/example.toml")).expect("example exists");
    let c = parse_catalogue(&text).expect("the shipped example must validate against its own schema");
    assert_eq!(c.use_case.len(), 2);
    assert!(c.use_case[1].uncovered.is_some());
}

#[test]
fn shipped_catalogue_schema_matches_the_types() {
    let shipped = fs::read_to_string(manifest_path("schema/catalogue.schema.json"))
        .expect("schema/catalogue.schema.json exists");
    let shipped: serde_json::Value = serde_json::from_str(&shipped).expect("valid JSON");
    let current = serde_json::to_value(catalogue_json_schema()).expect("serializes");
    assert_eq!(
        shipped, current,
        "schema/catalogue.schema.json is stale — regenerate with \
         `cargo run --bin test-plan -- schema catalogue`"
    );
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

#[test]
fn bad_tier_is_refused_with_its_path() {
    let err = parse_catalogue(
        "api_version = \"test-plan/v1\"\narea = \"x\"\n\n[[use_case]]\nid = \"UC-x-01\"\ntier = \"T9\"\nstatement = \"s\"\n",
    )
    .unwrap_err();
    assert!(err.starts_with("use_case"), "{err}");
}

#[test]
fn duplicate_id_across_files_is_refused_naming_both_paths() {
    let dir = std::env::temp_dir().join(format!("test-plan-dup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.toml"),
        "api_version = \"test-plan/v1\"\narea = \"a\"\n\n[[use_case]]\nid = \"UC-shared-01\"\ntier = \"T1\"\nstatement = \"s\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("b.toml"),
        "api_version = \"test-plan/v1\"\narea = \"b\"\n\n[[use_case]]\nid = \"UC-shared-01\"\ntier = \"T1\"\nstatement = \"s\"\n",
    )
    .unwrap();
    let errs = load_catalogues(&dir).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("UC-shared-01") && e.contains("a.toml")),
        "{errs:?}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn area_mismatch_is_refused_naming_the_file() {
    let dir = std::env::temp_dir().join(format!("test-plan-area-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.toml"),
        "api_version = \"test-plan/v1\"\narea = \"wrong-name\"\n",
    )
    .unwrap();
    let errs = load_catalogues(&dir).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("wrong-name")), "{errs:?}");
    fs::remove_dir_all(&dir).ok();
}
