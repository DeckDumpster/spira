//! `spira-config set|unset --release <home>` types a write against a staged release's registry
//! while an older release is the one in force.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn key(home: &Path, name: &str, ty: &str) {
    let dir = home.join("conf.d");
    std::fs::create_dir_all(home.join("conf.toml.d")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(name),
        format!("TYPE={ty}\nDOC=d\nSCHEMA=REQUIRED: d\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT\nSPIRA_CONF_DEFAULT_EOF\n"),
    )
    .unwrap();
}

struct Box_ {
    _d: testkit::TempDir,
    installed: PathBuf,
    staged: PathBuf,
    toml: PathBuf,
}

fn setup(name: &str) -> Box_ {
    let d = testkit::TempDir::new(name);
    let installed = d.path().join("installed");
    let staged = d.path().join("staged");
    key(&installed, "SPIRA_OLD_KEY", "string");
    key(&installed, "SPIRA_RETIRING_KEY", "string");
    key(&staged, "SPIRA_OLD_KEY", "string");
    key(&staged, "SPIRA_NEW_BOX_PATH", "string");
    key(&staged, "SPIRA_NEW_COUNT", "u32");
    let toml = d.path().join("spira.toml");
    std::fs::write(&toml, "[spira]\nold_key = \"x\"\nretiring_key = \"y\"\n").unwrap();
    Box_ { _d: d, installed, staged, toml }
}

fn run(b: &Box_, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .env_clear()
        .env("SPIRA_HOME", &b.installed)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &Box_) -> String {
    std::fs::read_to_string(&b.toml).unwrap()
}

#[test]
fn a_key_only_the_staged_release_declares_is_written_through_set_release() {
    let b = setup("set-release-new");
    let (staged, toml) = (b.staged.to_str().unwrap(), b.toml.to_str().unwrap());
    let refused = run(&b, &["set", "spira.new_box_path", "/box/p", toml]);
    assert!(!refused.status.success(), "control: the installed registry does not declare it");
    assert!(!text(&b).contains("new_box_path"));

    let ok = run(&b, &["set", "--release", staged, "spira.new_box_path", "/box/p", toml]);
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    assert!(text(&b).contains("new_box_path = \"/box/p\""), "{}", text(&b));
    assert!(text(&b).contains("old_key = \"x\""), "other lines survive");
    assert!(std::fs::read_dir(b.toml.parent().unwrap()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("spira.toml.bak")), "a backup is kept");

    let again = run(&b, &["set", "--release", staged, "spira.new_count", "7", toml]);
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    assert!(text(&b).contains("new_box_path = \"/box/p\"") && text(&b).contains("new_count = 7"), "{}", text(&b));
}

#[test]
fn set_release_types_the_value_against_the_staged_registry() {
    let b = setup("set-release-type");
    let out = run(&b, &["set", "--release", b.staged.to_str().unwrap(), "spira.new_count", "many", b.toml.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!text(&b).contains("new_count"));
}

#[test]
fn set_release_refuses_a_key_the_staged_release_does_not_declare() {
    let b = setup("set-release-undeclared");
    let (staged, toml) = (b.staged.to_str().unwrap(), b.toml.to_str().unwrap());
    let before = text(&b);
    let out = run(&b, &["set", "--release", staged, "spira.nowhere", "v", toml]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nowhere"), "{}", String::from_utf8_lossy(&out.stderr));
    let retired = run(&b, &["set", "--release", staged, "spira.retiring_key", "v", toml]);
    assert!(!retired.status.success(), "a key the staged release drops is not writable through it");
    assert_eq!(text(&b), before);
}

#[test]
fn a_key_the_staged_release_retires_is_unset_by_the_installed_registry_that_still_declares_it() {
    let b = setup("unset-retired");
    let toml = b.toml.to_str().unwrap();
    let out = run(&b, &["unset", "spira.retiring_key", toml]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!text(&b).contains("retiring_key") && text(&b).contains("old_key"), "{}", text(&b));
}

#[test]
fn unset_release_removes_a_key_only_the_staged_release_declares() {
    let b = setup("unset-release");
    let (staged, toml) = (b.staged.to_str().unwrap(), b.toml.to_str().unwrap());
    assert!(run(&b, &["set", "--release", staged, "spira.new_box_path", "/box/p", toml]).status.success());
    let out = run(&b, &["unset", "--release", staged, "spira.new_box_path", toml]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!text(&b).contains("new_box_path"), "{}", text(&b));
}
