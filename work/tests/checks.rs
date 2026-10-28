use std::path::Path;
use std::process::Command;

fn repo(checks: Option<&str>) -> testkit::TempDir {
    let dir = testkit::TempDir::new("work-checks");
    std::fs::create_dir_all(dir.join(".spira")).unwrap();
    Command::new("git").arg("-C").arg(dir.path()).args(["init", "-q"]).status().unwrap();
    if let Some(h) = checks {
        std::fs::write(dir.join(".spira/checks.toml"), h).unwrap();
    }
    dir
}

fn checks(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_work")).arg("checks").args(args).current_dir(dir).env_clear().env("PATH", std::env::var("PATH").unwrap()).output().unwrap();
    (out.status.code().unwrap(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn every_row_runs_and_the_failing_one_is_named() {
    let dir = repo(Some("[[check]]\nname = \"good\"\nrun = \"echo good-output\"\nbudget_ms = 5000\n\n[[check]]\nname = \"bad\"\nrun = \"sh -c 'echo bad-output; exit 1'\"\nbudget_ms = 5000\n"));
    let (code, out) = checks(dir.path(), &[]);
    assert_ne!(code, 0);
    assert!(out.contains("good-output"), "{out}");
    assert!(out.contains("failed: bad"), "{out}");
}

#[test]
fn a_repository_with_no_done_table_declares_none_and_passes() {
    let dir = repo(None);
    let (code, out) = checks(dir.path(), &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("declares no [[check]]"), "{out}");
}

#[test]
fn list_prints_rows_without_running_them() {
    let dir = repo(Some("[[check]]\nname = \"bad\"\nrun = \"false\"\n"));
    let (code, out) = checks(dir.path(), &["--list"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("bad: false"), "{out}");
}

#[test]
fn submit_is_refused_with_the_failing_check_output_before_any_socket_is_touched() {
    let dir = repo(Some("[[check]]\nname = \"bad\"\nrun = \"sh -c 'echo bad-output; exit 1'\"\nbudget_ms = 5000\n"));
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("submit")
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_WORK_BEAD_ID", "sp-x1")
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(work::REFUSED), "{text}");
    assert!(text.contains("bad-output") && text.contains("done checks failed"), "{text}");
}

#[test]
fn submit_with_passing_checks_proceeds_to_the_socket() {
    let dir = repo(Some("[[check]]\nname = \"good\"\nrun = \"true\"\nbudget_ms = 5000\n"));
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("submit")
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_WORK_BEAD_ID", "sp-x1")
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("done checks failed"), "{text}");
}
