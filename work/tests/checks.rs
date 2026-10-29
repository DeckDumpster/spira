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
    std::fs::create_dir_all(rel.join("spira")).unwrap();
    testkit::write_exe(rel.join("bin/nqmjx7-lint"), "#!/bin/sh\necho lint-ran\n");
    std::fs::copy(env!("CARGO_BIN_EXE_work"), rel.join("model-bin/work")).unwrap();
    // The binary was just copied by this process: another test thread's fork may still hold
    // the write descriptor, so the exec is retried past ETXTBSY.
    let out = testkit::retry_busy(|| Command::new(rel.join("model-bin/work")).arg("checks").current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin").output()).unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("lint-ran"), "{text}");
}

struct Fixture {
    dir: testkit::TempDir,
    rel: testkit::TempDir,
    stubs: testkit::TempDir,
    target: testkit::TempDir,
}

fn fixture(toml: &str, cargo_body: &str) -> Fixture {
    let dir = repo(Some(toml));
    let rel = testkit::TempDir::new("work-checks-release");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    testkit::write_exe(rel.join("bin/x-lint"), "#!/bin/sh\necho release-lint\n");
    let stubs = testkit::TempDir::new("work-checks-stubs");
    testkit::write_exe(stubs.join("cargo"), &format!("#!/bin/sh\necho \"$@\" >> \"{}/cargo.argv\"\n{cargo_body}", stubs.path().display()));
    let target = testkit::TempDir::new("work-checks-target");
    Fixture { dir, rel, stubs, target }
}

fn run_fixture(f: &Fixture, verb: &str) -> (i32, String) {
    let path = format!("{}:/usr/bin:/bin", f.stubs.path().display());
    let out = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg(verb)
        .current_dir(f.dir.path())
        .env_clear()
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", f.target.path())
        .env("SPIRA_RELEASE", f.rel.path())
        .output()
        .unwrap();
    (out.status.code().unwrap(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

const BUILD_ROW: &str = "[[check]]\nname = \"x-lint\"\nrun = \"x-lint\"\nbuild = \"x-lint\"\nbudget_ms = 5000\n";

#[test]
fn a_build_row_runs_the_worktree_binary_even_when_the_release_holds_one() {
    let f = fixture(BUILD_ROW, "mkdir -p \"$CARGO_TARGET_DIR/debug\"\nprintf '#!/bin/sh\\necho worktree-lint\\n' > \"$CARGO_TARGET_DIR/debug/x-lint\"\nchmod +x \"$CARGO_TARGET_DIR/debug/x-lint\"\n");
    let (code, text) = run_fixture(&f, "checks");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("worktree-lint") && !text.contains("release-lint"), "{text}");
    assert!(text.contains("built from worktree:"), "{text}");
    let argv = std::fs::read_to_string(f.stubs.join("cargo.argv")).unwrap();
    assert_eq!(argv.trim(), "build -p x-lint --bin x-lint");
}

#[test]
fn a_failed_worktree_build_fails_the_row_and_never_runs_the_release_binary() {
    let f = fixture(BUILD_ROW, "echo 'error: could not compile' >&2\nexit 101\n");
    let (code, text) = run_fixture(&f, "checks");
    assert_eq!(code, work::REFUSED, "{text}");
    assert!(text.contains("-- x-lint: FAIL (cannot build x-lint") && text.contains("could not compile"), "{text}");
    assert!(!text.contains("release-lint"), "{text}");
}

#[test]
fn a_build_row_whose_program_is_not_the_package_is_refused() {
    let f = fixture("[[check]]\nname = \"row\"\nrun = \"other\"\nbuild = \"x-lint\"\n", "exit 0\n");
    let (code, text) = run_fixture(&f, "checks");
    assert_eq!(code, work::REFUSED, "{text}");
    assert!(text.contains("\"row\"") && text.contains("\"other\"") && text.contains("\"x-lint\""), "{text}");
}

#[test]
fn prime_does_not_name_a_build_row_as_unresolved() {
    let f = fixture("[[check]]\nname = \"row\"\nrun = \"x-absent\"\nbuild = \"x-absent\"\n\n[[check]]\nname = \"plain\"\nrun = \"x-plain-absent\"\n", "exit 0\n");
    let (_, text) = run_fixture(&f, "prime");
    assert!(text.contains("\"x-plain-absent\", which resolves nowhere"), "{text}");
    assert!(!text.contains("\"x-absent\""), "{text}");
}

#[test]
fn a_row_without_build_never_builds() {
    let f = fixture("[[check]]\nname = \"own\"\nrun = \"x-own\"\nbudget_ms = 5000\n", "exit 0\n");
    std::fs::create_dir_all(f.dir.join("x-own")).unwrap();
    std::fs::write(f.dir.join("x-own/Cargo.toml"), "[package]\nname = \"x-own\"\n").unwrap();
    let (_, text) = run_fixture(&f, "prime");
    assert!(text.contains("\"x-own\", which resolves nowhere"), "{text}");
    let (code, text) = run_fixture(&f, "checks");
    assert_eq!(code, work::REFUSED, "{text}");
    assert!(text.contains("-- own: FAIL"), "{text}");
    assert!(!f.stubs.join("cargo.argv").exists());
}

#[test]
fn prime_names_a_reviewer_that_resolves_nowhere() {
    let dir = repo(None);
    let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_AGENT", "nqmjx7-no-claude")]);
    let out = Command::new(env!("CARGO_BIN_EXE_work")).arg("prime").current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin").env("SPIRA_TOML", toml).env("SPIRA_HOME", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira")).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("the reviewer \"nqmjx7-no-claude\""), "{}{}", String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or(""), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_row_without_build_reports_the_release_binary_it_resolved() {
    let f = fixture("[[check]]\nname = \"x-lint\"\nrun = \"x-lint\"\nbudget_ms = 5000\n", "exit 0\n");
    let (code, text) = run_fixture(&f, "checks");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("resolved: ") && text.contains("release-lint"), "{text}");
}
