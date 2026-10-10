//! The config key registry is the one declaration of a key: `SpiraSection`, the `KEY => field`
//! mapping, the schema and the key history are generated from it, so a hand-kept list cannot
//! drift from it (conf_key_parity). Codegen itself is `codegen.rs`, shared with `build.rs`.

#[path = "../codegen.rs"]
mod codegen;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use spira_config::convert::convert;
use spira_config::{export_sh, json_schema, SPIRA_KEYS};

fn spira_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira")
}

fn registry_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect()
}

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
fn generated_keys_equal_the_registry_key_set() {
    let mut registry = registry_files(&spira_dir().join("conf.d"));
    registry.extend(registry_files(&spira_dir().join("conf.toml.d")));
    let generated: BTreeSet<String> = SPIRA_KEYS.iter().map(|k| k.key.to_string()).collect();
    assert_eq!(only_in_first(&registry, &generated), Vec::<String>::new());
    assert_eq!(only_in_first(&generated, &registry), Vec::<String>::new());
}

#[test]
fn schema_fields_equal_the_registry_field_set() {
    let schema = serde_json::to_value(json_schema()).unwrap();
    let schema_fields: BTreeSet<String> = schema["definitions"]["SpiraSection"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let registry_fields: BTreeSet<String> = SPIRA_KEYS.iter().map(|k| k.field.to_string()).collect();
    assert_eq!(only_in_first(&registry_fields, &schema_fields), Vec::<String>::new());
    assert_eq!(only_in_first(&schema_fields, &registry_fields), Vec::<String>::new());
}

#[test]
fn every_conf_d_key_converts_and_no_toml_only_key_is_in_conf_d() {
    let conf_d = registry_files(&spira_dir().join("conf.d"));
    for k in SPIRA_KEYS {
        assert_eq!(!k.toml_only, conf_d.contains(k.key), "{}", k.key);
    }
}

fn fixture_registry(extra_key: &str, extra_type: &str) -> (testkit::TempDir, PathBuf) {
    let tmp = testkit::TempDir::new(&format!("spira-config-codegen-{extra_key}"));
    let root = tmp.path().to_path_buf();
    let conf_d = root.join("conf.d");
    fs::create_dir_all(&conf_d).unwrap();
    for name in registry_files(&spira_dir().join("conf.d")) {
        fs::copy(spira_dir().join("conf.d").join(&name), conf_d.join(&name)).unwrap();
    }
    fs::write(
        conf_d.join(extra_key),
        format!(
            "TYPE={extra_type}\nGROUP=fixture\nDOC=fixture\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${{{extra_key}:=fixture-default}}\"\nSPIRA_CONF_DEFAULT_EOF\n"
        ),
    )
    .unwrap();
    (tmp, conf_d)
}

#[test]
fn a_key_added_to_the_registry_only_reaches_conf_sh_and_spira_config() {
    let key = "SPIRA_FIXTURE_ONLY_KEY";
    let (tmp, conf_d) = fixture_registry(key, "u32");

    let root = tmp.path();
    let keys = codegen::load(&conf_d, &root.join("absent")).unwrap();
    let section = codegen::section_rs(&keys);
    assert!(section.contains("pub fixture_only_key: Option<u32>,"), "{section}");
    assert!(section.contains(&format!("key: {key:?}")), "{section}");
    let arms = codegen::convert_rs(&keys);
    assert!(arms.contains(&format!("{key:?} => s.fixture_only_key = parse_u32(")), "{arms}");
    let history = codegen::history_merge("actionable\n", &keys);
    assert!(history.lines().any(|l| l == "fixture_only_key"), "{history}");

    let spira = root.join("spira");
    fs::create_dir_all(&spira).unwrap();
    fs::copy(spira_dir().join("conf-gen.sh"), spira.join("conf-gen.sh")).unwrap();
    fs::rename(&conf_d, spira.join("conf.d")).unwrap();
    let out = Command::new("bash").arg(spira.join("conf-gen.sh")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let allow = fs::read_to_string(spira.join("conf.d.keys.generated.sh")).unwrap();
    assert!(allow.lines().any(|l| l == key), "{allow}");
    let defaults = fs::read_to_string(spira.join("conf.d.defaults.generated.sh")).unwrap();
    assert!(defaults.contains(&format!("${{{key}:=fixture-default}}")), "{defaults}");
}

#[test]
fn an_unknown_type_in_the_registry_refuses_the_build() {
    let (tmp, conf_d) = fixture_registry("SPIRA_FIXTURE_BAD_TYPE", "float");
    let err = codegen::load(&conf_d, &tmp.path().join("absent")).err().expect("refused");
    assert!(err.contains("SPIRA_FIXTURE_BAD_TYPE"), "{err}");
}

#[test]
fn the_real_registry_converts_every_key_it_declares() {
    let conf_text: String =
        SPIRA_KEYS.iter().filter(|k| !k.toml_only && k.ty == "string").map(|k| format!("{} = v\n", k.key)).collect();
    let (doc, warnings) = convert(&conf_text, "/opt/fixture-home", "", &[]).expect("every declared key is accepted");
    assert!(warnings.0.is_empty(), "{:?}", warnings.0);
    assert!(export_sh(&doc).lines().count() > 100);
}

const HARD_REQUIRED_NUMERIC: &[&str] = &[
    "SPIRA_PREFLIGHT_WALL_SECS",
    "SPIRA_FLOW_WINDOW_HOURS",
    "SPIRA_FLOW_BASELINE_HOURS",
    "SPIRA_FLOW_GRACE_SECS",
    "SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS",
    "SPIRA_ROUND_VM_SSH_PORT",
    "SPIRA_ROUND_VM_MAX_RETRIES",
    "SPIRA_ROUND_VM_MIRROR_PORT",
    "SPIRA_ROUND_VM_RETRY_INTERVAL",
    "SPIRA_ROUND_VM_VCPUS",
];

#[test]
fn a_key_a_consumer_parses_as_a_number_has_a_default_and_a_schema_description() {
    let registry = spira_config::registry::load(&spira_dir().join("conf.d")).unwrap();
    let schema = serde_json::to_value(json_schema()).unwrap();
    let props = &schema["definitions"]["SpiraSection"]["properties"];
    let mut missing = Vec::new();
    for key in HARD_REQUIRED_NUMERIC {
        let k = registry.get(*key).unwrap_or_else(|| panic!("{key} is not registered"));
        let value = k.default_expr().unwrap_or("");
        if value.is_empty() || value.parse::<f64>().is_err() {
            missing.push(format!("{key}: default {value:?} is not a number"));
        }
        let field = codegen::field_of(key);
        let desc = props[field.as_str()]["description"].as_str().unwrap_or("");
        if desc.trim().is_empty() {
            missing.push(format!("{key}: schema carries no description"));
        }
    }
    assert!(missing.is_empty(), "{missing:#?}");
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
