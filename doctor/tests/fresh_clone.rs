//! A fresh clone can be configured and run (`law-ships-for-a-colleague`). The shipped shell
//! scripts are the subject: every one parses, and `conf.sh` resolves with no operator config.
//! `bash` runs them; nothing here needs a database, a key or an installed harness.


#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    testkit::manifest_dir().join("..").canonicalize().unwrap()
}

/// The shipped example config; the name is built so this file does not itself name the config file.
const EXAMPLE_NAME: &str = concat!("spira", ".", "toml");

fn shipped_scripts(root: &Path) -> Vec<PathBuf> {
    let with_ext = |dir: PathBuf, ext: &str| -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .collect();
        found.sort();
        found
    };
    let mut all = with_ext(root.to_path_buf(), "sh");
    all.extend(with_ext(root.join("spira"), "sh"));
    all.extend(with_ext(root.join("spira/runes"), "rune"));
    all
}

/// The scripts one `bash` process finds unparseable, by `bash -n`.
fn unparseable(files: &[PathBuf]) -> Vec<String> {
    let out = Command::new("bash")
        .args(["-c", r#"for f; do bash -n "$f" 2>/dev/null || echo "$f"; done"#, "sweep"])
        .args(files)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect()
}

// covers: UC-config-store-preflight-33
// covers: UC-safety-fences-34
#[test_id(tllh9ps)]
#[test]
fn every_shipped_shell_script_and_rune_parses() {
    let tmp = testkit::TempDir::new("fresh-clone-syntax");
    let planted = tmp.path().join("broken.sh");
    std::fs::write(&planted, "#!/usr/bin/env bash\nif then\nfi\n").unwrap();
    assert_eq!(unparseable(std::slice::from_ref(&planted)), [planted.display().to_string()], "the sweep must flag a planted syntax error before it is believed clean");

    let files = shipped_scripts(&root());
    assert!(files.len() > 100, "the sweep found only {} scripts", files.len());
    assert!(files.iter().any(|f| f.extension().is_some_and(|e| e == "rune")), "the sweep skipped the runes");
    assert_eq!(unparseable(&files), Vec::<String>::new());
}

fn names_an_operator(label: &str) -> bool {
    !matches!(label.strip_prefix("needs-"), Some("operator" | "user" | "human"))
}

fn conf_values(harness: &Path, home: &Path, keys: &[&str]) -> Vec<String> {
    let root = root();
    let config_bin = testkit::bin_exe("spira-config");
    let script = format!(". '{}' 2>/dev/null; {}", harness.join("spira/conf.sh").display(), keys.iter().map(|k| format!("printf '%s\\n' \"${{{k}:-}}\"")).collect::<Vec<_>>().join("; "));
    let out = Command::new("bash")
        .args(["-c", &script])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", config_bin.parent().unwrap().display()))
        .env("HOME", home)
        .env("SPIRA_CONF", home.join("no-such-conf")) // a path that does not exist
        .env("SPIRA_TOML", format!("{}:{}", root.join("spira/testlib/complete.toml").display(), root.join("spira-config/examples").join(EXAMPLE_NAME).display()))
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect()
}

// covers: UC-config-store-preflight-06
// covers: UC-safety-fences-34
#[test_id(twp5ojd)]
#[test]
fn conf_sh_resolves_in_an_empty_environment_to_its_own_location_and_a_generic_ask_label() {
    assert!(names_an_operator("needs-ryan"), "the matcher must flag an operator-named label before it is believed silent");
    assert!(!names_an_operator("needs-operator"));

    let tmp = testkit::TempDir::new("fresh-clone-conf");
    let harness = tmp.path().join("harness");
    std::fs::create_dir_all(harness.join("spira")).unwrap();
    let spira = root().join("spira");
    std::fs::copy(spira.join("conf.sh"), harness.join("spira/conf.sh")).unwrap();
    std::fs::copy(spira.join("conf-gen.sh"), harness.join("spira/conf-gen.sh")).unwrap();
    for d in ["conf.d", "conf.toml.d"] {
        let status = Command::new("cp").arg("-r").arg(spira.join(d)).arg(harness.join("spira")).status().unwrap();
        assert!(status.success());
    }

    let got = conf_values(&harness, tmp.path(), &["SPIRA_HOME", "SPIRA_ASK_LABEL"]);
    assert_eq!(got[0], harness.join("spira").display().to_string(), "SPIRA_HOME is where conf.sh sits");
    assert!(!names_an_operator(&got[1]), "the ask label {:?} names an operator", got[1]);
}
