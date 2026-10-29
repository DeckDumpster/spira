//! Which registry a process reads: the one `SPIRA_HOME` names, remembered once it loads, and the
//! tree under test's own for a tool a gate trial runs against it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn real_home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira")
}

fn home_with_extra_key(dir: &Path) -> PathBuf {
    let home = dir.join("tree/spira");
    std::fs::create_dir_all(home.join("conf.d")).unwrap();
    std::fs::create_dir_all(home.join("conf.toml.d")).unwrap();
    for sub in ["conf.d", "conf.toml.d"] {
        for e in std::fs::read_dir(real_home().join(sub)).unwrap().flatten() {
            std::fs::copy(e.path(), home.join(sub).join(e.file_name())).unwrap();
        }
    }
    std::fs::write(
        home.join("conf.d/SPIRA_BRANCH_ADDED_KEY"),
        "TYPE=string\nGROUP=test\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT.\nSPIRA_CONF_DEFAULT_EOF\n",
    )
    .unwrap();
    home
}

#[test]
fn a_refusal_is_not_remembered_and_the_first_loaded_registry_is_kept() {
    let dir = testkit::TempDir::new("spira-config-registry-home");
    let missing = dir.join("nowhere");
    {
        let _env = testkit::env(&[("SPIRA_HOME", missing.to_str())]);
        let err = spira_config::registry::for_process().unwrap_err();
        assert_eq!(err, format!("no config registry at {}", missing.join("conf.d").display()));
    }
    {
        let _env = testkit::env(&[("SPIRA_HOME", real_home().to_str())]);
        assert!(spira_config::registry::for_process().unwrap().get("SPIRA_RUN").is_some(), "the next call looks again");
    }
    let other = home_with_extra_key(dir.path());
    let _env = testkit::env(&[("SPIRA_HOME", other.to_str())]);
    assert!(spira_config::registry::for_process().unwrap().get("SPIRA_BRANCH_ADDED_KEY").is_none(), "a loaded registry is read once");
}

#[test]
fn a_tool_run_against_the_tree_that_adds_a_key_reads_it_and_one_run_against_the_release_does_not() {
    let dir = testkit::TempDir::new("spira-config-registry-tree");
    let toml = dir.join("spira.toml");
    std::fs::write(&toml, "[spira]\nid_prefix = \"sp\"\nbranch_added_key = \"x\"\n").unwrap();
    let validate = |home: &Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_spira-config"))
            .args(["validate"])
            .arg(&toml)
            .env_clear()
            .env("SPIRA_HOME", home)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    let tree = validate(&home_with_extra_key(dir.path()));
    assert!(!tree.contains("branch_added_key"), "the tree's own registry knows its key: {tree}");
    let release = validate(&real_home());
    assert!(release.contains("spira.branch_added_key is not known to this release"), "positive control: {release}");
}
