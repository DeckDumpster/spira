//! sp-upubz / sp-iku03: with SPIRA_HOME unset, `sop` must find lib.sh by walking up from
//! its own executable, and a failure to source it must be loud, never an empty shelf.
use std::path::Path;
use std::process::Command;

fn run_list(root: &Path, with_lib: bool) -> (String, String) {
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe = bin_dir.join("sop");
    std::fs::copy(env!("CARGO_BIN_EXE_sop"), &exe).unwrap();
    let home = root.join("spira");
    std::fs::create_dir_all(&home).unwrap();
    if with_lib {
        std::fs::write(
            home.join("lib.sh"),
            "bdjson() { echo '{\"sop-fake\":\"SYMPTOM: positive control\"}'; }\n",
        )
        .unwrap();
    }
    let out = Command::new(&exe)
        .arg("list")
        .env_remove("SPIRA_HOME")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("sop-home-unset-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn list_non_empty_with_spira_home_unset() {
    let root = tmp("pos");
    let (out, _) = run_list(&root, true);
    assert!(out.contains("sop-fake"), "shelf must be non-empty (positive control): {out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unsourceable_lib_is_loud_not_empty() {
    let root = tmp("neg");
    let (_, err) = run_list(&root, false);
    assert!(err.contains("cannot source"), "exit-96 must be loud: {err}");
    let _ = std::fs::remove_dir_all(&root);
}
