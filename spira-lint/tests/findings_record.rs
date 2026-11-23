#[macro_use]
extern crate spira_test_id;
use std::process::Command;

use spira_lint::record::{FindingRecord, FINDINGS_SCHEMA};

fn run_fixture(tmp: &testkit::TempDir) -> std::process::Output {
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("app/a.rs"),
        "const U: &str = \"spira-widget.service\";\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    let findings = tmp.path().join("findings.jsonl");
    Command::new(testkit::bin_exe("spira-lint"))
        .args(["--only", "bare-unit-name", "--root"])
        .arg(&root)
        .arg("--findings")
        .arg(&findings)
        .env_clear()
        .env("PATH", system_paths::SYSTEM_PATH)
        .output()
        .unwrap()
}

#[test_id(trp1tsq)]
#[test]
fn findings_flag_writes_one_record_naming_the_rules_source_and_allow_file() {
    let tmp = testkit::TempDir::new("lint-findings");
    let out = run_fixture(&tmp);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(tmp.path().join("findings.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let r = FindingRecord::parse(lines[0]).unwrap();
    assert_eq!(r.schema, FINDINGS_SCHEMA);
    assert_eq!(r.rule, "bare-unit-name");
    assert_eq!(r.path, "app/a.rs");
    assert_eq!(r.rule_source, "spira-lint/src/rules/bare_unit_name.rs");
    let rule = spira_lint::all_rules()
        .into_iter()
        .find(|x| x.name() == "bare-unit-name")
        .unwrap();
    assert_eq!(r.allow_file.as_deref(), rule.allow_file());
    assert!(r.allow_file.is_some());
}

#[test_id(t6nc2xs)]
#[test]
fn a_record_with_an_extra_field_is_refused_naming_the_field() {
    let line = r#"{"schema":1,"rule":"r","path":"p","line":null,"message":"m","rule_source":"s","allow_file":null,"member":"sp-x"}"#;
    let err = FindingRecord::parse(line).unwrap_err();
    assert!(err.contains("member"), "{err}");
}

#[test_id(tyv4k7o)]
#[test]
fn a_record_of_another_schema_is_refused() {
    let line = r#"{"schema":2,"rule":"r","path":"p","line":null,"message":"m","rule_source":"s","allow_file":null}"#;
    assert!(FindingRecord::parse(line).unwrap_err().contains("schema"));
}
