use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(dir: &str, rules: Option<&str>) -> Vec<Value> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lifecycle-guard"));
    cmd.arg("--json");
    if let Some(r) = rules {
        cmd.arg("--rules").arg(fixture(dir).join(r));
    }
    cmd.arg(fixture(dir));
    let out = cmd.output().expect("run lifecycle-guard");
    serde_json::from_slice(&out.stdout).expect("valid JSON findings")
}

fn classes(findings: &[Value]) -> Vec<&str> {
    findings
        .iter()
        .map(|f| f["class"].as_str().unwrap())
        .collect()
}

#[test]
fn clean_fixture_reports_nothing() {
    let findings = run("clean", None);
    assert!(findings.is_empty(), "expected no findings, got {findings:#?}");
}

#[test]
fn direct_write_is_reported() {
    let findings = run("direct_write", None);
    let cs = classes(&findings);
    assert!(cs.contains(&"direct-write"), "{cs:?}");
    assert_eq!(cs.iter().filter(|c| **c == "direct-write").count(), 2);
}

#[test]
fn wrapper_write_resolves_through_forwarding_and_fixed_verb_wrappers() {
    let findings = run("wrapper_write", None);
    // bead_reopen forwards "$@"; the verb only becomes known at the groomer's own call site.
    assert!(
        findings.iter().any(|f| f["class"] == "wrapper-write"
            && f["function"] == "do_reopen"
            && f["callee"] == "bead_reopen"),
        "expected a wrapper-write at do_reopen calling bead_reopen, got {findings:#?}"
    );
    // always_reopen bakes the verb in itself (a direct-write at its own definition) and
    // every caller of it — including through another function — is flagged too.
    assert!(
        findings.iter().any(|f| f["class"] == "direct-write" && f["function"] == "always_reopen"),
        "expected a direct-write inside always_reopen, got {findings:#?}"
    );
    assert!(
        findings.iter().any(|f| f["class"] == "wrapper-write"
            && f["function"] == "do_always"
            && f["callee"] == "always_reopen"),
        "expected a wrapper-write at do_always calling always_reopen, got {findings:#?}"
    );
}

#[test]
fn dynamic_verb_is_reported() {
    let findings = run("dynamic_verb", None);
    assert_eq!(classes(&findings), vec!["dynamic-verb"]);
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
fn retired_label_check_is_silent_without_a_rules_file() {
    // Same fixture, no --rules: nothing to compare against, so nothing to find. This is the
    // "empty allowlist" state this bead ships in — wiring real names in is the cutover bead's job.
    let findings = run("retired_label", None);
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn deleted_path_write_is_reported_when_configured() {
    let findings = run("deleted_path", Some("rules.json"));
    assert_eq!(classes(&findings), vec!["deleted-path"]);
}

#[test]
fn brief_naming_bd_is_reported() {
    let findings = run("brief_bd", None);
    assert_eq!(classes(&findings), vec!["brief-bd"]);
}

#[test]
fn fayth_naming_bd_is_reported() {
    let findings = run("fayth_bd", None);
    assert_eq!(classes(&findings), vec!["brief-bd"]);
}

#[test]
fn landstate_call_is_reported_in_shell() {
    let findings = run("landstate_call", None);
    let cs = classes(&findings);
    assert_eq!(cs, vec!["landstate-call", "landstate-call", "landstate-call"], "{findings:#?}");
    assert!(
        findings.iter().any(|f| f["callee"] == "landed" && f["function"] == "is_done"),
        "{findings:#?}"
    );
    assert!(
        findings.iter().any(|f| f["callee"] == "land_mark" && f["function"] == "mark_it"),
        "{findings:#?}"
    );
    assert!(
        findings.iter().any(|f| f["callee"] == "landed_sha" && f["function"] == "cite_it"),
        "{findings:#?}"
    );
}

#[test]
fn landstate_direct_path_read_is_reported_in_shell() {
    let findings = run("landstate_path", None);
    let cs = classes(&findings);
    assert_eq!(cs, vec!["landstate-path", "landstate-path", "landstate-path"], "{findings:#?}");
}

#[test]
fn landstate_call_is_reported_in_rust() {
    let findings = run("landstate_rust_call", None);
    assert_eq!(classes(&findings), vec!["landstate-call"], "{findings:#?}");
    assert_eq!(findings[0]["file"], "main.rs");
    assert_eq!(findings[0]["callee"], "land_mark");
}

#[test]
fn landstate_direct_path_read_is_reported_in_rust() {
    let findings = run("landstate_rust_path", None);
    assert_eq!(classes(&findings), vec!["landstate-path"], "{findings:#?}");
    assert_eq!(findings[0]["file"], "io.rs");
}

#[test]
fn landstate_findings_are_silent_inside_the_allow_listed_crates() {
    let findings = run("landstate_allowlist", None);
    assert!(findings.is_empty(), "expected no findings, got {findings:#?}");
}

#[test]
fn exit_code_is_nonzero_iff_findings_exist() {
    let clean = Command::new(env!("CARGO_BIN_EXE_lifecycle-guard"))
        .arg(fixture("clean"))
        .output()
        .unwrap();
    assert!(clean.status.success());

    let dirty = Command::new(env!("CARGO_BIN_EXE_lifecycle-guard"))
        .arg(fixture("direct_write"))
        .output()
        .unwrap();
    assert_eq!(dirty.status.code(), Some(1));
}

#[test]
fn bd_in_docs_and_disabled_personas_and_rust_comments_is_not_reported() {
    let findings = run("scoped", None);
    assert!(findings.is_empty(), "{findings:#?}");
}
