use std::path::Path;
use std::process::Command;

fn file(args: &[&str]) -> (i32, String) {
    let t = testkit::TempDir::new("bead-proposal-part");
    let toml = spira_config::process::fixture_toml(t.path(), &[("SPIRA_DB", &t.path().to_string_lossy())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
    let out = Command::new(env!("CARGO_BIN_EXE_bead"))
        .arg("file")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("SPIRA_REPO", Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_proposal_without_a_part_is_refused_and_the_parts_are_printed() {
    let (code, err) = file(&["law-x: Do x.", "--kind", "proposal"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--part"), "{err}");
    assert!(err.contains("Part 1"), "the table of contents must be printed: {err}");
}

#[test]
fn a_part_on_anything_but_a_proposal_is_refused() {
    let (code, err) = file(&["t", "--kind", "insight", "--part", "2"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("only to --kind proposal"), "{err}");
}
