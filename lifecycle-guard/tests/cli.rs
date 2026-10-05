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

const SPIRA_LC_ONLY_CRATES: &[&str] = &["gate", "batcher-cut", "czar-pass", "gh-intake", "auron", "cockpit/ops"];

#[test]
fn spira_lc_only_crates_never_read_the_landstate_ledger() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut scanned = 0;
    for krate in SPIRA_LC_ONLY_CRATES {
        let out = Command::new(env!("CARGO_BIN_EXE_lifecycle-guard"))
            .arg("--json")
            .arg(root.join(krate).join("src"))
            .output()
            .expect("run lifecycle-guard");
        let findings: Vec<Value> = serde_json::from_slice(&out.stdout).expect("valid JSON findings");
        let reads: Vec<&Value> = findings
            .iter()
            .filter(|f| f["class"] == "landstate-path" || (f["class"] == "landstate-call" && f["callee"] != "land_mark"))
            .collect();
        assert!(reads.is_empty(), "{krate} reads the landstate ledger: {reads:#?}");
        scanned += 1;
    }
    assert_eq!(scanned, SPIRA_LC_ONLY_CRATES.len());
}

#[test]
fn the_reader_fence_sees_a_landstate_read_in_a_scoped_shape() {
    let findings = run("landstate_rust_path", None);
    assert!(findings.iter().any(|f| f["class"] == "landstate-path"), "{findings:#?}");
}

fn gate(dir: &std::path::Path) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_lifecycle-guard"))
        .arg("--gate")
        .arg(dir)
        .output()
        .expect("run lifecycle-guard --gate");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// sp-ts2qr: the landing gate's fence. A clean tree passes with the gate's fence line (a
/// count > 0, gate/DESIGN.md "Every fence proves it checked"); a test's planted violation
/// under tests/fixtures/ is data, not code; a legacy bd write — a class not yet refused at the
/// gate — is counted on one summary line and does not fail it; a comment or a string that
/// says "landed (" is not a call.
#[test]
fn gate_mode_passes_a_clean_tree_with_its_fence_line() {
    let (code, out, err) = gate(&fixture("gate_clean"));
    assert_eq!(code, Some(0), "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.contains("fence: lifecycle-guard checked 2 files"), "{out}");
    assert!(out.contains("not yet refused at the gate: credential=1"), "{out}");
    assert!(!out.contains("planted.sh") && !err.contains("REFUSED"), "{out}{err}");
}

/// The same tree without --gate scans everything it is pointed at, fixtures included.
#[test]
fn a_plain_run_still_scans_fixture_directories() {
    let findings = run("gate_clean", None);
    assert!(
        findings.iter().any(|f| f["file"] == "tests/fixtures/planted.sh" && f["class"] == "landstate-call"),
        "{findings:#?}"
    );
}

/// A reintroduced oracle — through the binary, the ledger's files, or the Rust API — is a
/// red, and the refusal names its exits (law-a-refusal-names-its-exit): the machine's route,
/// correcting the rule on the branch, and the operator's ungated landing.
#[test]
fn gate_mode_refuses_a_reintroduced_oracle_and_names_its_exits() {
    let (code, out, err) = gate(&fixture("gate_violation"));
    assert_eq!(code, Some(1), "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.contains("check.sh:4: [landstate-call]") && out.contains("landing-pass landed"), "{out}");
    assert!(out.contains("check.sh:5: [landstate-path]"), "{out}");
    assert!(out.contains("src/lib.rs:2: [landstate-call]") && out.contains("land_state"), "{out}");
    assert!(!out.contains("fence: lifecycle-guard"), "a refused run proves nothing: {out}");
    for exit in ["REFUSED", "3 finding(s)", "spira-lc", "lifecycle-guard/", "SPIRA_LAND_UNGATED=<reason>", "no allow-list"] {
        assert!(err.contains(exit), "the refusal does not name {exit:?}: {err}");
    }
}

/// A fence that checked nothing refuses instead of passing silent.
#[test]
fn gate_mode_refuses_a_tree_with_nothing_to_check() {
    let tmp = testkit::TempDir::new("lg-gate-empty");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("tests/fixtures")).unwrap();
    std::fs::write(dir.join("tests/fixtures/x.sh"), "#!/bin/bash\nlanded a b\n").unwrap();
    let (code, out, err) = gate(&dir);
    assert_eq!(code, Some(2), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("checked 0 files"), "{err}");
}

#[test]
fn landing_pass_oracle_subcommands_are_landstate_calls() {
    let findings = run("landstate_oracle_cmd", None);
    let calls: Vec<(u64, &str)> = findings
        .iter()
        .filter(|f| f["class"] == "landstate-call")
        .map(|f| (f["line"].as_u64().unwrap(), f["callee"].as_str().unwrap()))
        .collect();
    assert_eq!(
        calls,
        vec![
            (4, "landing-pass mark"),
            (5, "landing-pass cited-commit"),
            (6, "landing-pass close-on-land"),
        ],
        "{findings:#?}"
    );
}

/// The machine boundary is the lifecycle crate and spira-lc's one migration reader, not the
/// whole of spira-lc: a second, ledger-backed answer inside spira-lc is a finding too.
#[test]
fn spira_lc_beyond_its_migration_reader_is_held_to_the_rule() {
    let findings = run("spira_lc_beyond_classifier", None);
    assert_eq!(classes(&findings), vec!["landstate-path"], "{findings:#?}");
    assert_eq!(findings[0]["file"], "spira-lc/src/answer.rs");
}

/// sp-hyo5e: the gate refuses every way around the lifecycle machine, not only the ledger —
/// a direct bd write, a write through a wrapper, a verb it cannot resolve and a bd status read
/// feeding a decision. Each class is named in the refusal; none is merely counted.
#[test]
fn gate_mode_refuses_a_planted_write_around_the_machine() {
    let (code, out, err) = gate(&fixture("gate_bd_write"));
    assert_eq!(code, Some(1), "stdout:\n{out}\nstderr:\n{err}");
    for (line, class) in [
        ("aeon.sh:5: [direct-write]", "bdq update --status"),
        ("aeon.sh:7: [wrapper-write]", "release"),
        ("aeon.sh:7: [wrapper-write]", "bdq reopen"),
        ("aeon.sh:8: [direct-write]", "bd close"),
        ("aeon.sh:10: [dynamic-verb]", "cannot be resolved"),
        ("aeon.sh:11: [lifecycle-read]", "bd show"),
    ] {
        assert!(out.lines().any(|l| l.starts_with(line) && l.contains(class)), "no {line} … {class}:\n{out}");
    }
    assert!(!out.contains("not yet refused at the gate: direct-write"), "{out}");
    assert!(!out.contains("fence: lifecycle-guard"), "a refused run proves nothing: {out}");
    assert!(err.contains("REFUSED") && err.contains("no allow-list"), "{err}");
}

/// sp-hyo5e: a verb held in an array is the array's first word when every assignment agrees,
/// with its appends' flags still seen; a forwarder that pipes bd through a filter still leaves
/// the verb to its call site (and is not itself an unresolvable verb); an array filled at run
/// time stays dynamic.
#[test]
fn array_held_verbs_and_filtering_forwarders_are_resolved() {
    let findings = run("array_verb", None);
    let at = |line: u64| -> Vec<&str> {
        findings.iter().filter(|f| f["line"] == line).map(|f| f["class"].as_str().unwrap()).collect()
    };
    assert_eq!(at(4), Vec::<&str>::new(), "the forwarder's own \"$@\": {findings:#?}");
    assert_eq!(at(7), Vec::<&str>::new(), "READY resolves to `ready`: {findings:#?}");
    assert_eq!(at(10), vec!["direct-write"], "WRITE resolves to `update … --status`: {findings:#?}");
    assert_eq!(at(11), Vec::<&str>::new(), "bdjson show is a read: {findings:#?}");
    assert_eq!(at(12), vec!["wrapper-write"], "bdjson close: {findings:#?}");
    assert_eq!(at(15), vec!["dynamic-verb"], "a run-time fill stays dynamic: {findings:#?}");
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
    let (code, out, err) = gate(&dir);
    assert_eq!(code, Some(1), "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.lines().any(|l| l.starts_with("planted.sh:2: [direct-write]") && l.contains("bd close")), "{out}");
}
