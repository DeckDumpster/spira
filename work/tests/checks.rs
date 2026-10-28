use std::path::Path;
use std::process::Command;

fn repo(habits: Option<&str>) -> testkit::TempDir {
    let dir = testkit::TempDir::new("work-checks");
    std::fs::create_dir_all(dir.join(".spira")).unwrap();
    Command::new("git").arg("-C").arg(dir.path()).args(["init", "-q"]).status().unwrap();
    if let Some(h) = habits {
        std::fs::write(dir.join(".spira/habits.toml"), h).unwrap();
    }
    dir
}

fn checks(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_work")).arg("checks").args(args).current_dir(dir).env_clear().env("PATH", std::env::var("PATH").unwrap()).output().unwrap();
    (out.status.code().unwrap(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn every_row_runs_and_the_failing_one_is_named() {
    let dir = repo(Some("[[done]]\nname = \"good\"\nrun = \"echo good-output\"\nbudget_ms = 5000\n\n[[done]]\nname = \"bad\"\nrun = \"sh -c 'echo bad-output; exit 1'\"\nbudget_ms = 5000\n"));
    let (code, out) = checks(dir.path(), &[]);
    assert_ne!(code, 0);
    assert!(out.contains("good-output"), "{out}");
    assert!(out.contains("failed: bad"), "{out}");
}

#[test]
fn a_repository_with_no_done_table_declares_none_and_passes() {
    let dir = repo(Some("[[habit]]\nname = \"x\"\nglob = \"*\"\n"));
    let (code, out) = checks(dir.path(), &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("declares no [[done]]"), "{out}");
}

#[test]
fn list_prints_rows_without_running_them() {
    let dir = repo(Some("[[done]]\nname = \"bad\"\nrun = \"false\"\n"));
    let (code, out) = checks(dir.path(), &["--list"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("bad: false"), "{out}");
}
