use spira_sim::drive::{parse_scenario, scenario_path, Scenario};
use spira_sim::summon::claimable;
use spira_sim::world::{base_config, bead_db, home_settings, registry_rows, stub_gate_steps, INERT_TOOLS, REPO_NAME};
use std::path::Path;

fn sc(steps: &str) -> Result<Scenario, String> {
    parse_scenario(&format!("epoch = 1\nhorizon = 10\n{steps}"))
}

fn shell(step: &str) -> Result<String, String> {
    let s = sc(&format!("[[step]]\nat = 0\n{step}\n"))?;
    s.steps[0].shell()
}

#[test]
fn a_file_step_creates_the_bead_in_both_stores_under_the_given_id() {
    let cmd = shell("file = \"sp-hp01\"").unwrap();
    assert!(cmd.contains("create 'sim sp-hp01' --id sp-hp01"), "{cmd}");
    assert!(cmd.contains("-l repo:sim") && cmd.contains("$SIM_BEADS_DB"), "{cmd}");
    assert!(cmd.ends_with("&& spira-lc create-bead sp-hp01"), "{cmd}");
}

#[test]
fn a_claim_step_appends_the_beads_script_for_the_stub_agent() {
    let cmd = shell("claim = \"sp-a\"\nscript = [\"commit a.txt\", \"submit\"]").unwrap();
    assert_eq!(cmd, "printf '%s\\n' 'sp-a commit a.txt' 'sp-a submit' >> \"$SPIRA_SIM_SCENARIO\"");
}

#[test]
fn a_command_step_is_the_command() {
    assert_eq!(shell("command = \"echo hi\"").unwrap(), "echo hi");
}

#[test]
fn a_step_must_be_exactly_one_kind_and_a_bead_id_is_not_shell() {
    for bad in [
        "at2 = 1",
        "file = \"sp-a\"\ncommand = \"x\"",
        "claim = \"sp-a\"",
        "script = [\"submit\"]",
        "file = \"sp-a; rm -rf /\"",
        "claim = \"$(x)\"\nscript = [\"submit\"]",
        "file = \"\"",
    ] {
        assert!(shell(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_goal_tag_needs_a_goal_and_real_actors_are_read_by_name() {
    assert!(sc("goal_tag = \"v\"\n").unwrap_err().contains("goal_tag needs a goal"));
    let s = sc("goal = \"sp-a:LANDED\"\ngoal_tag = \"v\"\nreal_actors = [\"rounds\"]\n").unwrap();
    assert_eq!((s.goal_tag.as_deref(), s.real_actors.as_slice()), (Some("v"), &["rounds".to_string()][..]));
}

#[test]
fn the_shipped_happy_path_parses_and_uses_actors_the_shipped_file_declares() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let s = parse_scenario(&std::fs::read_to_string(here.join("scenarios/happy-path.toml")).unwrap()).unwrap();
    let declared = std::fs::read_to_string(here.join("../../sim/actors.toml")).unwrap();
    for a in &s.real_actors {
        assert!(declared.contains(&format!("name = \"{a}\"")), "{a}");
    }
    assert!(s.steps.iter().any(|st| st.file.is_some()) && s.steps.iter().any(|st| st.claim.is_some()));
}

#[test]
fn the_summon_claims_only_ready_or_rework_beads_the_scenario_scripts_in_id_order() {
    let list = r#"[{"bead_id":"sp-b","state":"READY","version":"0"},{"bead_id":"sp-a","state":"READY","version":2},
        {"bead_id":"sp-c","state":"WORKING","version":"3"},{"bead_id":"sp-d","state":"READY","version":"0"}]"#;
    let scenario = "sp-b submit\nsp-a commit x\nsp-c submit\n";
    assert_eq!(claimable(list, scenario).unwrap(), vec![("sp-a".to_string(), "READY".to_string(), 2), ("sp-b".to_string(), "READY".to_string(), 0)]);
    assert_eq!(claimable(list, "").unwrap(), vec![]);
    assert!(claimable("not json", scenario).is_err());
}

#[test]
fn a_worlds_config_moves_every_fixture_path_inside_it_and_registers_only_its_repository() {
    let home = Path::new("/w/hm");
    let cfg = base_config(home);
    assert!(!cfg.contains("/fixture/userhome"), "a path still points outside the world");
    assert!(!cfg.contains("[repo.spira]"));
    assert!(cfg.contains("/w/hm/spira/run"), "run moved under the world's home");
    assert!(cfg.contains("/w/release/spira/chamber"), "the release paths follow the world's release link");
}

#[test]
fn a_world_registers_one_queue_local_row_and_names_its_tools_absolutely() {
    assert_eq!(registry_rows(Path::new("/w/work")), format!("{REPO_NAME}|/w/work|queue.local|local/main|||plan\n"));
    let s = home_settings(Path::new("/w"), "/fx/f1\n", Path::new("/usr/bin/bd"));
    let get = |k: &str| s.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("spira.bd"), Some("/usr/bin/bd"));
    assert_eq!(get("spira.db"), Some(bead_db("/fx/f1").to_str().unwrap()));
    assert_eq!(get("spira.home_repo"), Some(REPO_NAME));
    assert_eq!(get("spira.prod"), Some("/w/release/spira"));
}

#[test]
fn the_worlds_gate_is_the_stub_runner_alone_and_host_touching_tools_are_inert() {
    assert_eq!(stub_gate_steps(Path::new("/w/config/suite-runner")), "step \"/w/config/suite-runner\"\n");
    assert!(INERT_TOOLS.contains(&"unit-ensure") && INERT_TOOLS.contains(&"target-reap"));
}

#[test]
fn a_bare_scenario_name_resolves_to_the_shipped_scenario_from_anywhere_in_the_checkout() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let shipped = scenario_path(&root.join("spira/sim/src"), "happy-path");
    assert!(shipped.ends_with("spira/sim/scenarios/happy-path.toml") && shipped.is_file(), "{shipped:?}");
    assert_eq!(scenario_path(&root, "./happy-path.toml"), Path::new("./happy-path.toml"));
    assert_eq!(scenario_path(&root, "no-such-scenario"), Path::new("no-such-scenario"));
}

#[test]
fn a_returned_bead_is_claimed_again_from_rework() {
    let list = r#"[{"bead_id":"sp-a","state":"REWORK","version":"4"},{"bead_id":"sp-b","state":"SUBMITTED","version":"1"}]"#;
    assert_eq!(claimable(list, "sp-a drop x\nsp-b submit\n").unwrap(), vec![("sp-a".to_string(), "REWORK".to_string(), 4)]);
}

#[test]
fn a_file_step_carries_its_priority_into_the_bead_and_no_other_step_may() {
    assert!(shell("file = \"sp-a\"\npriority = 0").unwrap().contains("-t task -p 0 "));
    assert!(shell("file = \"sp-a\"").unwrap().contains("-t task -p 2 "));
    for bad in ["file = \"sp-a\"\npriority = 5", "command = \"true\"\npriority = 1"] {
        assert!(shell(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_scenarios_own_sql_is_one_select_under_a_plain_name() {
    let own = |kind: &str, name: &str, sql: &str| sc(&format!("[[{kind}]]\nname = \"{name}\"\nsql = \"{sql}\"\n")).map(|_| ());
    assert!(own("invariant", "a_b1", "SELECT seq FROM events").is_ok());
    assert!(own("expect", "a_b1", "select 1 from events").is_ok());
    for (name, sql) in [("A", "SELECT 1"), ("", "SELECT 1"), ("x y", "SELECT 1"), ("x", "DROP TABLE events"), ("x", "SELECT 1; SELECT 2")] {
        assert!(own("invariant", name, sql).is_err(), "{name} {sql}");
    }
}
