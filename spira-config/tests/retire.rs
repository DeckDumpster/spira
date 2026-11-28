//! `spira-config retire` on a fixture tree: the retirement leaves the three rules that read it satisfied.

#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::{Command, Output};

const BASE_DELTA: &str = "removed_tasks = [\"gone\"]\nremoved = [\"spira.gone\"] # keep\n[spira]\nadded = 1\n";
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
    w("spira/config-delta.toml", BASE_DELTA);
    testkit::git(t.path(), &["init", "-q", "-b", "main"]);
    testkit::git(t.path(), &["add", "-A"]);
    testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]);
    t
}

fn retire(root: &Path, args: &[&str]) -> Output {
    Command::new(testkit::bin_exe("spira-config")).env_clear().arg("retire").args(args).args(if args.contains(&"--bead") { &[][..] } else { &["--bead", "sp-x"][..] }).arg("--tree").arg(root).output().unwrap()
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

#[test_id(t6cix8g)]
#[test]
fn retiring_a_key_removes_its_registry_file_names_it_in_the_delta_and_keeps_the_fixture_line() {
    let t = tree();
    let out = retire(t.path(), &["spira.old_knob"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!t.path().join("spira/conf.d/SPIRA_OLD_KNOB").exists());
    assert!(t.path().join("spira/conf.d/SPIRA_KEPT").exists());
    assert_eq!(read(t.path(), "spira/config-delta.toml"), BASE_DELTA, "the folded file is never edited");
    let delta = spira_config::delta::delta_text(&t.path().join("spira")).unwrap().unwrap();
    assert!(read(t.path(), "spira/config-delta.d/sp-x.toml").contains("spira.old_knob"));
    let (added, removed) = spira_config::parse_config_delta(&delta).unwrap();
    assert!(removed.contains(&"spira.old_knob".to_string()) && removed.contains(&"spira.gone".to_string()), "{removed:?}");
    assert_eq!(added, ["spira.added"], "the delta's own additions survive");
    assert!(read(t.path(), "spira/testlib/complete.toml").contains("old_knob = \"x\""), "the landing ref still requires the line");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(!said.contains("follow-up"), "{said}");
    assert!(said.contains("complete.toml") && said.contains("later branch") && said.contains("deletes the line"), "{said}");
    assert!(said.contains("code that reads"), "{said}");
}

#[test_id(tu97ajm)]
#[test]
fn a_registry_key_declared_only_in_the_toml_directory_is_retired_too() {
    let t = tree();
    assert!(retire(t.path(), &["spira.only_toml"]).status.success());
    assert!(!t.path().join("spira/conf.toml.d/SPIRA_ONLY_TOML").exists());
}

#[test_id(tfd6uo8)]
#[test]
fn retiring_a_task_drops_it_from_the_default_and_names_it_in_removed_tasks() {
    let t = tree();
    let out = retire(t.path(), &["--task", "old.task"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let reg = read(t.path(), "spira/conf.d/SPIRA_TASK_MODES");
    assert!(reg.contains("${SPIRA_TASK_MODES:=alpha=act omega=act}"), "{reg}");
    assert_eq!(read(t.path(), "spira/config-delta.toml"), BASE_DELTA);
    assert!(read(t.path(), "spira/config-delta.d/sp-x.toml").contains("removed_tasks = [\"old.task\"]"));
    let delta = spira_config::delta::delta_text(&t.path().join("spira")).unwrap().unwrap();
    assert!(delta.contains("\"old.task\"") && delta.contains("\"gone\""), "{delta}");
    assert!(spira_config::parse_config_delta(&delta).is_ok());
    assert!(String::from_utf8_lossy(&out.stdout).contains("registers"), "names the code left to the builder");
}

#[test_id(txhlz5r)]
#[test]
fn an_unknown_key_or_task_is_refused_and_changes_nothing() {
    let t = tree();
    for args in [&["spira.nope"][..], &["--task", "nope"][..], &["not.spira.key"][..]] {
        let out = retire(t.path(), args);
        assert!(!out.status.success(), "{args:?}");
    }
    assert!(!retire(t.path(), &["spira.old_knob", "--tree"]).status.success(), "--tree needs a value");
    assert_eq!(read(t.path(), "spira/config-delta.toml"), BASE_DELTA);
    assert!(!t.path().join("spira/config-delta.d").exists());
}

#[test_id(t4ys8hm)]
#[test]
fn retiring_a_key_twice_is_refused() {
    let t = tree();
    assert!(retire(t.path(), &["spira.old_knob"]).status.success());
    assert!(!retire(t.path(), &["spira.old_knob"]).status.success());
}

#[test_id(t14as44)]
#[test]
fn retire_without_a_bead_exits_non_zero_and_prints_the_usage() {
    let t = tree();
    let out = Command::new(testkit::bin_exe("spira-config")).env_clear().args(["retire", "spira.old_knob", "--tree"]).arg(t.path()).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bead"));
    assert!(t.path().join("spira/conf.d/SPIRA_OLD_KNOB").exists());
}

#[test_id(tuv7vik)]
#[test]
fn two_branches_each_retiring_a_key_merge_clean_and_the_delta_lists_both() {
    let t = tree();
    std::fs::write(t.path().join(".gitattributes"), std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join(".gitattributes")).unwrap()).unwrap();
    testkit::git(t.path(), &["add", "-A"]);
    let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
    let commit = |msg: &str| testkit::git(t.path(), &[&id[..], &["commit", "-qm", msg]].concat());
    commit("attrs");
    for (branch, key, bead) in [("a", "spira.old_knob", "sp-a"), ("b", "spira.kept", "sp-b")] {
        testkit::git(t.path(), &["checkout", "-q", "-b", branch, "main"]);
        assert!(retire(t.path(), &[key, "--bead", bead]).status.success());
        testkit::git(t.path(), &["add", "-A"]);
        commit(branch);
    }
    testkit::git(t.path(), &["checkout", "-q", "a"]);
    testkit::git(t.path(), &[&id[..], &["merge", "-q", "--no-edit", "b"]].concat());
    let delta = spira_config::delta::delta_text(&t.path().join("spira")).unwrap().unwrap();
    let (_, removed) = spira_config::parse_config_delta(&delta).unwrap();
    assert!(removed.contains(&"spira.old_knob".to_string()) && removed.contains(&"spira.kept".to_string()), "{removed:?}");
}

#[test_id(tpockbx)]
#[test]
fn retiring_a_first_middle_and_last_task_of_a_multi_line_list_removes_just_that_line() {
    for (task, left) in [("a=act", "\nb=act\nc=act\n"), ("b=act", "\na=act\nc=act\n"), ("c=act", "\na=act\nb=act\n")] {
        let t = tree();
        let list = "\na=act\nb=act\nc=act\n";
        let file = t.path().join("spira/conf.d/SPIRA_TASK_MODES");
        std::fs::write(&file, TASKS.replace("alpha=act old.task=act omega=act", list)).unwrap();
        let out = retire(t.path(), &["--task", task.trim_end_matches("=act")]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let reg = read(t.path(), "spira/conf.d/SPIRA_TASK_MODES");
        assert!(reg.contains(&format!("${{SPIRA_TASK_MODES:={left}}}")), "{task}: {reg}");
    }
}

#[test_id(tjsr0rv)]
#[test]
fn two_branches_each_adding_a_task_in_the_same_gap_merge_clean_and_both_files_keep_both_tasks_once() {
    let t = testkit::TempDir::new("retire-add-task");
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let w = |rel: &str, body: &str| {
        let p = t.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    let real = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();
    w(".gitattributes", &real(".gitattributes"));
    w("spira/conf.d/SPIRA_TASK_MODES", &real("spira/conf.d/SPIRA_TASK_MODES"));
    w("spira/testlib/complete.toml", &real("spira/testlib/complete.toml"));
    let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
    let git = |args: &[&str]| testkit::git(t.path(), &[&id[..], args].concat());
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    for (branch, task) in [("a", "asks.new_a"), ("b", "asks.new_b")] {
        git(&["checkout", "-q", "-b", branch, "main"]);
        for f in ["spira/conf.d/SPIRA_TASK_MODES", "spira/testlib/complete.toml"] {
            let body = read(t.path(), f).replacen("asks.moot=act\n", &format!("{task}=act\nasks.moot=act\n"), 1);
            w(f, &body);
        }
        let toml = read(t.path(), "spira/testlib/complete.toml").replacen("seed = ", &format!("new_{branch} = \"x\"\nseed = "), 1);
        w("spira/testlib/complete.toml", &toml);
        git(&["commit", "-qam", branch]);
    }
    git(&["checkout", "-q", "a"]);
    git(&["merge", "-q", "--no-edit", "b"]);
    for f in ["spira/conf.d/SPIRA_TASK_MODES", "spira/testlib/complete.toml"] {
        let body = read(t.path(), f);
        for task in ["asks.new_a=act", "asks.new_b=act"] {
            assert_eq!(body.matches(task).count(), 1, "{f} {task}");
        }
    }
    let doc: toml::Table = read(t.path(), "spira/testlib/complete.toml").parse().expect("the merged fixture parses");
    assert!(doc["spira"].as_table().unwrap().contains_key("new_a") && doc["spira"].as_table().unwrap().contains_key("new_b"));
}

#[test_id(tbv6flj)]
#[test]
fn a_bead_that_is_not_an_id_is_refused_and_writes_nothing() {
    let t = tree();
    for bead in ["../x", "", ".."] {
        assert!(!retire(t.path(), &["spira.old_knob", "--bead", bead]).status.success(), "{bead:?}");
    }
    assert!(t.path().join("spira/conf.d/SPIRA_OLD_KNOB").exists());
    assert!(!t.path().join("spira/config-delta.d").exists());
}

#[test_id(t8khefn)]
#[test]
fn a_child_bead_id_names_its_fragment() {
    let t = tree();
    let o = retire(t.path(), &["spira.old_knob", "--bead", "sp-x.1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(t.path().join("spira/config-delta.d/sp-x.1.toml").exists());
}
