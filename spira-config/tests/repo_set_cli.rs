//! `spira-config repo set` / `repo rm` through the binary a caller runs, against a throwaway repo-map.

#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::{Command, Output};

fn run(map: &Path, args: &[&str]) -> Output {
    let dir = map.parent().unwrap();
    let existing = dir.join("spira.toml");
    let toml = if existing.exists() {
        existing
    } else {
        spira_config::process::fixture_toml(dir, &[("SPIRA_REPO_MAP", map.to_str().unwrap())])
    };
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    Command::new(testkit::bin_exe("spira-config"))
        .env_clear()
        .env("SPIRA_TOML", toml)
        .env("SPIRA_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn field(map: &Path, name: &str, col: &str) -> String {
    String::from_utf8_lossy(&run(map, &["repo", "field", name, col]).stdout)
        .trim()
        .to_string()
}

fn fixture() -> (testkit::TempDir, std::path::PathBuf) {
    let d = testkit::TempDir::new("repo-set-cli");
    let map = d.path().join("repo-map");
    std::fs::write(
        &map,
        "fx | /srv/fx | queue.local | local/main | fmt | bash gate.sh | plan\n",
    )
    .unwrap();
    (d, map)
}

#[test_id(te0m3j7)]
#[test]
fn set_gate_to_empty_reads_back_empty_through_repo_field() {
    let (_d, map) = fixture();
    assert_eq!(field(&map, "fx", "gate"), "bash gate.sh");
    let o = run(&map, &["repo", "set", "fx", "gate", ""]);
    assert!(o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("bash gate.sh"));
    assert_eq!(field(&map, "fx", "gate"), "");
    assert_eq!(field(&map, "fx", "format"), "fmt");
}

#[test_id(tcnyva3)]
#[test]
fn set_with_an_unknown_field_refuses_and_changes_nothing() {
    let (_d, map) = fixture();
    let o = run(&map, &["repo", "set", "fx", "colour", "red"]);
    assert!(!o.status.success());
    assert_eq!(field(&map, "fx", "gate"), "bash gate.sh");
}

#[test_id(t93klmq)]
#[test]
fn rm_unregisters_the_repo() {
    let (_d, map) = fixture();
    assert!(run(&map, &["repo", "rm", "fx"]).status.success());
    assert_eq!(field(&map, "fx", "path"), "");
    assert!(!run(&map, &["repo", "rm", "fx"]).status.success());
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test_id(t9woxit)]
#[test]
fn publish_target_round_trips_through_set_publish_clear_and_rm() {
    let (d, map) = fixture();
    std::fs::write(
        &map,
        "fx | /srv/fx | queue.local | local/main | fmt | | plan\nother | /srv/o | queue.local | local/main | | | plan\n",
    )
    .unwrap();
    let o = run(&map, &["repo", "publish", "fx"]);
    assert!(o.status.success(), "{o:?}");
    let v: serde_json::Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(v, serde_json::json!({"repo": "fx", "remote": null, "branch": null, "mode": null}));

    for (f, val) in [("publish-remote", "origin"), ("publish-branch", "main"), ("publish-mode", "push")] {
        let o = run(&map, &["repo", "set", "fx", f, val]);
        assert!(o.status.success(), "{o:?}");
    }
    assert!(run(&map, &["repo", "set", "other", "publish-remote", "none"]).status.success());
    let v: serde_json::Value = serde_json::from_str(out(&run(&map, &["repo", "publish", "fx"])).trim()).unwrap();
    assert_eq!(v, serde_json::json!({"repo": "fx", "remote": "origin", "branch": "main", "mode": "push"}));
    let all = out(&run(&map, &["repo", "publish", "--all"]));
    let rows: Vec<serde_json::Value> = all.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for want in ["fx", "other"] {
        assert!(rows.iter().any(|r| r["repo"] == want), "{all}");
    }

    assert!(run(&map, &["repo", "set", "fx", "publish-mode", ""]).status.success());
    assert!(!run(&map, &["repo", "set", "fx", "publish-remote", ""]).status.success());
    assert!(!run(&map, &["repo", "set", "fx", "publish-mode", "merge"]).status.success());
    let v: serde_json::Value = serde_json::from_str(out(&run(&map, &["repo", "publish", "fx"])).trim()).unwrap();
    assert_eq!(v["mode"], serde_json::Value::Null);

    assert!(run(&map, &["repo", "rm", "fx"]).status.success());
    let toml = std::fs::read_to_string(d.path().join("spira.toml")).unwrap();
    assert!(!toml.contains("publish.fx"), "{toml}");
    assert!(toml.contains("publish.other"), "{toml}");
    let o = run(&map, &["repo", "publish", "fx"]);
    assert!(!o.status.success());
    assert!(out(&o).contains("\"error\""), "{o:?}");
}
