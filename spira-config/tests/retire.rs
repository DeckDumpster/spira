//! `spira-config retire` on a fixture tree: the retirement leaves the three rules that read it satisfied.

use std::path::Path;
use std::process::{Command, Output};

const KEY: &str = "TYPE=string\nGROUP=g\nDOC=d\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_OLD_KNOB:=x}\"\nSPIRA_CONF_DEFAULT_EOF\n";
const TASKS: &str = "TYPE=string\nGROUP=g\nDOC=d\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_TASK_MODES:=alpha=act old.task=act omega=act}\"\nSPIRA_CONF_DEFAULT_EOF\n";

fn tree() -> testkit::TempDir {
    let t = testkit::TempDir::new("retire");
    let w = |rel: &str, body: &str| {
        let p = t.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w("spira/conf.d/SPIRA_OLD_KNOB", KEY);
    w("spira/conf.d/SPIRA_KEPT", &KEY.replace("OLD_KNOB", "KEPT"));
    w("spira/conf.d/SPIRA_TASK_MODES", TASKS);
    w("spira/conf.toml.d/SPIRA_ONLY_TOML", &KEY.replace("OLD_KNOB", "ONLY_TOML"));
    w("spira/testlib/complete.toml", "[spira]\nold_knob = \"x\"\nkept = \"x\"\ntask_modes = \"x\"\nonly_toml = \"x\"\n");
    w("spira/config-delta.toml", "removed_tasks = [\"gone\"]\nremoved = [\"spira.gone\"] # keep\n[spira]\nadded = 1\n");
    testkit::git(t.path(), &["init", "-q", "-b", "main"]);
    testkit::git(t.path(), &["add", "-A"]);
    testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]);
    t
}

fn retire(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config")).env_clear().arg("retire").args(args).arg("--tree").arg(root).output().unwrap()
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

#[test]
fn retiring_a_key_removes_its_registry_file_names_it_in_the_delta_and_keeps_the_fixture_line() {
    let t = tree();
    let out = retire(t.path(), &["spira.old_knob"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!t.path().join("spira/conf.d/SPIRA_OLD_KNOB").exists());
    assert!(t.path().join("spira/conf.d/SPIRA_KEPT").exists());
    let delta = read(t.path(), "spira/config-delta.toml");
    let (added, removed) = spira_config::parse_config_delta(&delta).unwrap();
    assert!(removed.contains(&"spira.old_knob".to_string()) && removed.contains(&"spira.gone".to_string()), "{removed:?}");
    assert_eq!(added, ["spira.added"], "the delta's own additions survive");
    assert!(delta.contains("# keep"), "the delta's comments survive:\n{delta}");
    assert!(read(t.path(), "spira/testlib/complete.toml").contains("old_knob = \"x\""), "the landing ref still requires the line");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("complete.toml") && said.contains("follow-up"), "{said}");
    assert!(said.contains("code that reads"), "{said}");
}

#[test]
fn a_registry_key_declared_only_in_the_toml_directory_is_retired_too() {
    let t = tree();
    assert!(retire(t.path(), &["spira.only_toml"]).status.success());
    assert!(!t.path().join("spira/conf.toml.d/SPIRA_ONLY_TOML").exists());
}

#[test]
fn retiring_a_task_drops_it_from_the_default_and_names_it_in_removed_tasks() {
    let t = tree();
    let out = retire(t.path(), &["--task", "old.task"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let reg = read(t.path(), "spira/conf.d/SPIRA_TASK_MODES");
    assert!(reg.contains("${SPIRA_TASK_MODES:=alpha=act omega=act}"), "{reg}");
    let delta = read(t.path(), "spira/config-delta.toml");
    assert!(delta.contains("\"old.task\"") && delta.contains("\"gone\""), "{delta}");
    assert!(spira_config::parse_config_delta(&delta).is_ok());
    assert!(String::from_utf8_lossy(&out.stdout).contains("registers"), "names the code left to the builder");
}

#[test]
fn an_unknown_key_or_task_is_refused_and_changes_nothing() {
    let t = tree();
    for args in [&["spira.nope"][..], &["--task", "nope"][..], &["not.spira.key"][..]] {
        let out = retire(t.path(), args);
        assert!(!out.status.success(), "{args:?}");
    }
    assert!(!retire(t.path(), &["spira.old_knob", "--tree"]).status.success(), "--tree needs a value");
    assert_eq!(read(t.path(), "spira/config-delta.toml"), "removed_tasks = [\"gone\"]\nremoved = [\"spira.gone\"] # keep\n[spira]\nadded = 1\n");
}

#[test]
fn retiring_a_key_twice_is_refused() {
    let t = tree();
    assert!(retire(t.path(), &["spira.old_knob"]).status.success());
    assert!(!retire(t.path(), &["spira.old_knob"]).status.success());
}
