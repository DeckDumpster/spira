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

/// A map and the config that declares it, holding `[repo.<name>]` for each of `names`.
fn fixture_of(names: &[&str]) -> (testkit::TempDir, std::path::PathBuf) {
    let d = testkit::TempDir::new("repo-set-cli");
    let map = d.path().join("repo-map");
    let rows: String = names
        .iter()
        .map(|n| format!("{n} | /srv/{n} | queue.local | local/main | fmt |  | plan\n"))
        .collect();
    std::fs::write(&map, rows).unwrap();
    let toml = spira_config::process::fixture_toml(d.path(), &[("SPIRA_REPO_MAP", map.to_str().unwrap())]);
    let mut text = std::fs::read_to_string(&toml).unwrap();
    for n in names {
        text.push_str(&format!(
            "\n[repo.{n}]\npath = \"/srv/{n}\"\nmode = \"queue.local\"\nbase = \"local/main\"\nformat = \"fmt\"\nlanes = [\"plan\"]\n"
        ));
    }
    std::fs::write(&toml, text).unwrap();
    (d, map)
}

fn fixture() -> (testkit::TempDir, std::path::PathBuf) {
    fixture_of(&["fx"])
}

fn config(map: &Path) -> std::path::PathBuf {
    map.parent().unwrap().join("spira.toml")
}

fn both_files(map: &Path) -> (String, String) {
    (std::fs::read_to_string(map).unwrap(), std::fs::read_to_string(config(map)).unwrap())
}

#[test_id(tefppb9)]
#[test]
fn a_generic_set_of_a_repo_lane_is_what_repo_field_reads() {
    let (_d, map) = fixture();
    let toml = config(&map);
    let o = run(&map, &["set", "repo.fx.lanes", r#"["plan","incident"]"#, toml.to_str().unwrap()]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(field(&map, "fx", "lanes"), "plan,incident");
}

#[test_id(t2l5l67)]
#[test]
fn a_generic_unset_of_a_repo_key_is_what_repo_field_reads() {
    let (_d, map) = fixture();
    let toml = config(&map);
    let o = run(&map, &["unset", "repo.fx.format", toml.to_str().unwrap()]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(field(&map, "fx", "format"), "");
}

#[test_id(tp79u9r)]
#[test]
fn repo_set_lanes_writes_the_typed_array_to_the_toml() {
    let (_d, map) = fixture();
    let o = run(&map, &["repo", "set", "fx", "lanes", "plan,spike"]);
    assert!(o.status.success(), "{o:?}");
    let doc = spira_config::load(&config(&map)).unwrap();
    assert_eq!(doc.repo["fx"].lanes, [spira_config::Lane::Plan, spira_config::Lane::Spike]);
    assert_eq!(field(&map, "fx", "lanes"), "plan,spike");
}

#[test_id(t5r4xza)]
#[test]
fn repo_set_gate_is_refused_as_a_retired_key() {
    let (_d, map) = fixture();
    let before = both_files(&map);
    let o = run(&map, &["repo", "set", "fx", "gate", ""]);
    assert!(!o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("[repo.*] retired the gate key"), "{o:?}");
    assert_eq!(both_files(&map), before);
}

#[test_id(t8pgn2q)]
#[test]
fn a_set_the_map_cannot_mirror_is_refused_whole_until_import_then_goes_through() {
    let (_d, map) = fixture();
    let mut rows = std::fs::read_to_string(&map).unwrap();
    rows.push_str("hand | /srv/hand | push | origin/main | | | plan\n");
    std::fs::write(&map, rows).unwrap();
    let before = both_files(&map);
    let toml = config(&map);
    let args = ["set", "repo.fx.base", "main", toml.to_str().unwrap()];
    let o = run(&map, &args);
    let said = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(!o.status.success() && said.contains("hand") && said.contains("spira-config repo import"), "{said}");
    assert_eq!(both_files(&map), before);

    let o = run(&map, &["repo", "import"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(field(&map, "hand", "path"), "/srv/hand");
    assert!(run(&map, &args).status.success());
    assert_eq!(field(&map, "fx", "base"), "main");
}

#[test_id(t4hw2lj)]
#[test]
fn a_hand_edited_row_is_reported_on_stderr_and_still_served() {
    let (_d, map) = fixture();
    assert!(run(&map, &["repo", "set", "fx", "lanes", "plan"]).status.success());
    run(&map, &["repo", "import"]);
    let quiet = run(&map, &["repo", "field", "fx", "lanes"]);
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("differs"), "{quiet:?}");
    let edited = std::fs::read_to_string(&map).unwrap().replace("| /srv/fx |", "| /srv/hand |");
    std::fs::write(&map, edited).unwrap();
    let o = run(&map, &["repo", "field", "fx", "path"]);
    let said = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(said.contains("differs from [repo.*]") && said.contains("for fx;") && said.contains("spira-config repo import"), "{said}");
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "/srv/hand");
}

#[test_id(tcnyva3)]
#[test]
fn set_with_an_unknown_field_refuses_and_changes_nothing() {
    let (_d, map) = fixture();
    let o = run(&map, &["repo", "set", "fx", "colour", "red"]);
    assert!(!o.status.success());
    assert_eq!(field(&map, "fx", "format"), "fmt");
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
    let (d, map) = fixture_of(&["fx", "other"]);
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
