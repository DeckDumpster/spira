use std::path::Path;
use std::process::Command;

const BARE: &str = "fn f() {\n    let _ = Command::new(\"bd\").output();\n}\n";

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn lint(dir: &Path, flags: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lint"))
        .args(["--root", dir.to_str().unwrap(), "--only", "call-deadline"])
        .args(flags)
        .env_remove("SPIRA_GATE_BASE")
        .output()
        .unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn fixture() -> testkit::TempDir {
    let d = testkit::TempDir::new("spira-lint-diff-mode");
    std::fs::create_dir_all(d.join("a/src")).unwrap();
    std::fs::create_dir_all(d.join("spira-lint")).unwrap();
    std::fs::write(d.join("a/src/lib.rs"), BARE).unwrap();
    std::fs::write(d.join("spira-lint/call-deadline-allow"), "").unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "base"]);
    git(&d, &["checkout", "-q", "-b", "branch"]);
    d
}

#[test]
fn a_hit_already_on_the_base_does_not_red_a_branch_that_touches_another_file() {
    let d = fixture();
    std::fs::write(d.join("notes.txt"), "x\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "other file"]);
    let (rc, out) = lint(&d, &["--diff", "main"]);
    assert_eq!((rc, out.as_str()), (0, ""));
    let (rc, out) = lint(&d, &["--base", "main"]);
    assert_eq!(rc, 1, "full-tree control must red the same tree");
    assert!(out.contains("a/src/lib.rs:2"), "{out}");
}

#[test]
fn a_new_hit_reds_and_names_its_line() {
    let d = fixture();
    std::fs::write(d.join("a/src/lib.rs"), format!("{BARE}{BARE}")).unwrap();
    git(&d, &["commit", "-qam", "add a hit"]);
    let (rc, out) = lint(&d, &["--diff", "main"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("a/src/lib.rs:5") && !out.contains("a/src/lib.rs:2:"), "{out}");
}

#[test]
fn an_uncommitted_new_file_is_judged() {
    let d = fixture();
    std::fs::write(d.join("a/src/new.rs"), BARE).unwrap();
    let (rc, out) = lint(&d, &["--diff", "main"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("a/src/new.rs:2"), "{out}");
}

fn lint_rule(dir: &Path, rule: &str, flags: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lint"))
        .args(["--root", dir.to_str().unwrap(), "--only", rule])
        .args(flags)
        .env_remove("SPIRA_GATE_BASE")
        .output()
        .unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn wait_fixture() -> testkit::TempDir {
    let d = testkit::TempDir::new("spira-lint-diff-stale-allow");
    std::fs::create_dir_all(d.join("spira")).unwrap();
    std::fs::create_dir_all(d.join("spira-lint")).unwrap();
    std::fs::write(d.join("spira/probe.sh"), "#!/bin/bash\nsleep 3\ntrue\n").unwrap();
    std::fs::write(d.join("spira/other.sh"), "#!/bin/bash\ntrue\n").unwrap();
    std::fs::write(d.join("spira-lint/time-only-wait-allow"), "# h\n1 spira/probe.sh sp-x\n").unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "base"]);
    git(&d, &["checkout", "-q", "-b", "branch"]);
    d
}

#[test]
fn a_member_that_drops_a_files_waits_below_its_allowance_reds_on_its_own_diff() {
    let d = wait_fixture();
    std::fs::write(d.join("spira/probe.sh"), "#!/bin/bash\ntrue\n").unwrap();
    git(&d, &["commit", "-qam", "convert the wait"]);
    let (rc, out) = lint_rule(&d, "time-only-wait", &["--diff", "main"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("spira/probe.sh") && out.contains("which now has 0"), "{out}");
}

#[test]
fn deleting_the_file_an_allowance_names_reds_the_member_that_deleted_it() {
    let d = wait_fixture();
    git(&d, &["rm", "-q", "spira/probe.sh"]);
    git(&d, &["commit", "-qm", "delete the file"]);
    let (rc, out) = lint_rule(&d, "time-only-wait", &["--diff", "main"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("which now has 0"), "{out}");
}

#[test]
fn an_edit_above_an_allowlisted_wait_does_not_move_the_finding_into_the_diff() {
    let d = wait_fixture();
    std::fs::write(d.join("spira-lint/time-only-wait-allow"), "# h\n# more\n1 spira/probe.sh sp-x\n").unwrap();
    git(&d, &["commit", "-qam", "reflow the header"]);
    let (rc, out) = lint_rule(&d, "time-only-wait", &["--diff", "main"]);
    assert_eq!((rc, out.as_str()), (0, ""));
}

#[test]
fn a_stale_journal_allow_line_reds_every_run_not_only_a_change_to_its_file() {
    let d = testkit::TempDir::new("spira-lint-diff-journal");
    std::fs::create_dir_all(d.join("spira")).unwrap();
    std::fs::create_dir_all(d.join("spira-lint")).unwrap();
    std::fs::write(d.join("spira/probe.sh"), "#!/bin/bash\ntrue\n").unwrap();
    std::fs::write(d.join("spira/other.sh"), "#!/bin/bash\ntrue\n").unwrap();
    std::fs::write(d.join("spira-lint/journal-readers-allow"), "# h\nspira/probe.sh sp-x\n").unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "base"]);
    git(&d, &["checkout", "-q", "-b", "branch"]);
    std::fs::write(d.join("spira/other.sh"), "#!/bin/bash\ntrue\ntrue\n").unwrap();
    git(&d, &["commit", "-qam", "unrelated edit"]);
    let (rc, out) = lint_rule(&d, "journal-reads", &["--diff", "main"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("no longer reads the journal"), "{out}");
}
