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

#[test]
fn submit_in_a_repository_declaring_no_checks_is_not_refused_by_them() {
    let dir = repo(None);
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("submit")
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("SPIRA_WORK_BEAD_ID", "sp-x1")
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("done checks failed") && !text.contains("refused"), "{text}");
}

#[test]
fn a_check_program_in_the_release_bin_resolves_under_the_models_restricted_path() {
    let dir = repo(Some("[[check]]\nname = \"lint\"\nrun = \"nqmjx7-lint\"\nbudget_ms = 5000\n"));
    let rel = testkit::TempDir::new("work-checks-release");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    testkit::write_exe(rel.join("bin/nqmjx7-lint"), "#!/bin/sh\necho lint-ran\n");
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("checks")
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_RELEASE", rel.path())
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("lint-ran"), "{text}");
}

#[test]
fn prime_names_a_check_program_that_resolves_nowhere() {
    let dir = repo(Some("[[check]]\nname = \"lint\"\nrun = \"nqmjx7-absent\"\nbudget_ms = 5000\n"));
    let out = Command::new(env!("CARGO_BIN_EXE_work")).arg("prime").current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"nqmjx7-absent\", which resolves nowhere"));
}

#[test]
fn the_release_is_found_from_the_executable_when_the_environment_names_none() {
    let dir = repo(Some("[[check]]\nname = \"lint\"\nrun = \"nqmjx7-lint\"\nbudget_ms = 5000\n"));
    let rel = testkit::TempDir::new("work-checks-exe-release");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    std::fs::create_dir_all(rel.join("model-bin")).unwrap();
    testkit::write_exe(rel.join("bin/nqmjx7-lint"), "#!/bin/sh\necho lint-ran\n");
    std::fs::copy(env!("CARGO_BIN_EXE_work"), rel.join("model-bin/work")).unwrap();
    let out = Command::new(rel.join("model-bin/work")).arg("checks").current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("lint-ran"), "{text}");
}

#[test]
fn a_check_that_is_a_crate_of_the_repository_is_built_when_no_release_holds_it() {
    let dir = repo(Some("[[check]]\nname = \"own\"\nrun = \"nqmjx7-own\"\nbudget_ms = 60000\n"));
    std::fs::create_dir_all(dir.join("nqmjx7-own/src")).unwrap();
    std::fs::write(dir.join("nqmjx7-own/src/main.rs"), "fn main() { println!(\"own-ran\"); }\n").unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"nqmjx7-own\"]\n").unwrap();
    std::fs::write(dir.join("nqmjx7-own/Cargo.toml"), "[package]\nname = \"nqmjx7-own\"\nversion = \"0.0.0\"\nedition = \"2021\"\n").unwrap();
    let target = testkit::TempDir::new("work-checks-own-target");
    let rel = testkit::TempDir::new("work-checks-empty-release");
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("checks")
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", std::env::var("HOME").unwrap())
        .env("CARGO_TARGET_DIR", target.path())
        .env("SPIRA_RELEASE", rel.path())
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("own-ran"), "{text}");
}

#[test]
fn prime_names_a_reviewer_that_resolves_nowhere() {
    let dir = repo(None);
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_AGENT", "nqmjx7-no-claude")]);
    let out = Command::new(env!("CARGO_BIN_EXE_work")).arg("prime").current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin").env("SPIRA_TOML", toml).env("SPIRA_HOME", dir.path()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("the reviewer \"nqmjx7-no-claude\""), "{}{}", String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or(""), String::from_utf8_lossy(&out.stderr));
}
