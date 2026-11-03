//! The scanner against its fixtures, in process: each planted shape is a finding of its class,
//! and `--gate` refuses only the classes it enforces (sp-ao9dn2: once tests/cli.rs, through the
//! binary).

use super::*;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn as_values(findings: &[Finding]) -> Vec<Value> {
    match serde_json::to_value(findings).expect("findings serialize") {
        Value::Array(v) => v,
        other => panic!("findings are not an array: {other}"),
    }
}

/// The findings `lifecycle-guard --json [--rules <dir>/<rules>] <dir>` prints.
fn run(dir: &str, rules: Option<&str>) -> Vec<Value> {
    let root = fixture(dir);
    let rules = match rules {
        Some(r) => Rules::load(&root.join(r)).expect("rules load"),
        None => Rules::default(),
    };
    let (_, findings) = run_one(&root, &rules, false).expect("scan");
    as_values(&findings)
}

fn classes(findings: &[Value]) -> Vec<&str> {
    findings.iter().map(|f| f["class"].as_str().unwrap()).collect()
}

#[test]
fn lifecycle_read_in_a_conditional_is_reported() {
    let findings = run("lifecycle_read", None);
    assert_eq!(classes(&findings), vec!["lifecycle-read"]);
}

#[test]
fn credential_reference_is_reported() {
    let findings = run("credential", None);
    assert_eq!(classes(&findings), vec!["credential"]);
}

#[test]
fn retired_label_is_reported_when_configured() {
    let findings = run("retired_label", Some("rules.json"));
    assert_eq!(classes(&findings), vec!["retired-label"]);
}

#[test]
fn landstate_direct_path_read_is_reported_in_shell() {
    let findings = run("landstate_path", None);
    let cs = classes(&findings);
    assert_eq!(cs, vec!["landstate-path", "landstate-path", "landstate-path"], "{findings:#?}");
}

#[test]
fn the_reader_fence_sees_a_landstate_read_in_a_scoped_shape() {
    let findings = run("landstate_rust_path", None);
    assert!(findings.iter().any(|f| f["class"] == "landstate-path"), "{findings:#?}");
}

/// The `--gate` run on `dir`: its exit code, how many files it checked, and its findings.
fn gate(dir: &std::path::Path) -> (u8, usize, Vec<Value>) {
    match run_one(dir, &Rules::default(), true) {
        Ok((n, f)) => (gate_verdict(n, &f), n, as_values(&f)),
        Err(_) => (2, 0, vec![]),
    }
}

/// A fence that checked nothing refuses instead of passing silent.
#[test]
fn gate_mode_refuses_a_tree_with_nothing_to_check() {
    let tmp = testkit::TempDir::new("lg-gate-empty");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("tests/fixtures")).unwrap();
    std::fs::write(dir.join("tests/fixtures/x.sh"), "#!/bin/bash\nlanded a b\n").unwrap();
    let (code, checked, findings) = gate(&dir);
    assert_eq!(code, 2, "{findings:#?}");
    assert_eq!(checked, 0);
}

/// No crate is exempt: a ledger-backed answer inside spira-lc is a finding like any other.
#[test]
fn spira_lc_is_held_to_the_rule() {
    let findings = run("spira_lc_beyond_classifier", None);
    assert_eq!(classes(&findings), vec!["landstate-path"], "{findings:#?}");
    assert_eq!(findings[0]["file"], "spira-lc/src/answer.rs");
}

/// sp-voip5: the verb follows bd's leading global flags. `bd -C x close y` was read as verb
/// `-C` and went unseen; `-C`/`--db`/`--actor` take a value word, `--json`/`--actor=…` do
/// not, an array of flags is looked through, and `ready --claim` is a claim.
#[test]
fn verbs_after_leading_global_flags_are_judged() {
    let findings = run("global_flags", None);
    let at = |line: u64| -> Vec<&str> {
        findings.iter().filter(|f| f["line"] == line).map(|f| f["class"].as_str().unwrap()).collect()
    };
    let detail = |line: u64| -> String {
        findings.iter().filter(|f| f["line"] == line).map(|f| f["detail"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(at(3), vec!["direct-write"], "bd -C x close y: {findings:#?}");
    assert!(detail(3).contains("bd close"), "{findings:#?}");
    assert_eq!(at(4), vec!["direct-write"], "bdq --db … update --status: {findings:#?}");
    assert_eq!(at(5), vec!["direct-write"], "bd --actor=me --json reopen: {findings:#?}");
    assert_eq!(at(6), vec!["direct-write"], "bd ready --claim: {findings:#?}");
    assert!(detail(6).contains("ready --claim"), "{findings:#?}");
    assert_eq!(at(7), Vec::<&str>::new(), "a plain ready is a read: {findings:#?}");
    assert_eq!(at(8), vec!["dynamic-verb"], "a dynamic verb after -C: {findings:#?}");
    assert_eq!(at(9), Vec::<&str>::new(), "update --title is metadata: {findings:#?}");
    assert_eq!(at(10), vec!["lifecycle-read"], "bd -C x show in a test: {findings:#?}");
    assert_eq!(at(12), vec!["direct-write"], "flags held in an array: {findings:#?}");
    assert_eq!(at(13), Vec::<&str>::new(), "no verb at all: {findings:#?}");
}

/// sp-voip5: the gate refuses a planted `bd -C x close y` — the shape the old analyser,
/// reading argv[1] as the verb, let through.
#[test]
fn gate_mode_refuses_a_close_behind_a_directory_flag() {
    let tmp = testkit::TempDir::new("lg-voip5");
    let dir = tmp.path().to_path_buf();
    std::fs::write(dir.join("planted.sh"), "#!/bin/bash\nbd -C x close y\n").unwrap();
    let (code, _, findings) = gate(&dir);
    assert_eq!(code, 1, "{findings:#?}");
    assert!(
        findings.iter().any(|f| f["file"] == "planted.sh"
            && f["line"] == 2
            && f["class"] == "direct-write"
            && f["detail"].as_str().unwrap().contains("bd close")),
        "{findings:#?}"
    );
}

