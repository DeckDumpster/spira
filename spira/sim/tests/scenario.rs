use spira_sim::drive::{parse_scenario, scenario_path, scenario_repos, Scenario};
use spira_sim::summon::{claimable, routed_repo};
use spira_sim::world::{base_config, bead_db, config_settings, home_settings, registry_rows, repo_set, stub_gate_steps, RepoSpec, INERT_TOOLS, MODES, REPO_NAME};
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
    assert_eq!(registry_rows(Path::new("/w"), &repo_set(&[]).unwrap()), format!("{REPO_NAME}|/w/work|queue.local|local/main|||plan\n"));
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

fn seeded(extra: &str) -> Result<Scenario, String> {
    sc(&format!("{extra}\n"))
}

#[test]
fn a_seed_files_through_bead_sh_with_its_priority_labels_body_and_deps() {
    let s = seeded("[[seed]]\nhandle = \"a\"\ntitle = \"it's a\"\nfor = \"builder\"\npriority = 1\nlabels = [\"x\"]\nbody = \"b\"\n[[seed]]\nhandle = \"b\"\ntitle = \"b\"\nkind = \"event\"\ndeps = [\"a\"]\n").unwrap();
    let steps = spira_sim::drive::seed_steps(&s.seeds).unwrap();
    assert_eq!(steps.len(), 1, "seeds sharing a time are one step");
    let cmd = &steps[0].1;
    assert!(cmd.contains("bead.sh file 'it'\\''s a' --for 'builder' --repo 'sim' --priority 1 --body-file"), "{cmd}");
    assert!(cmd.contains("bead.sh label add \"$id\" 'x'"), "{cmd}");
    assert!(cmd.contains("bead.sh file 'b' --kind 'event'"), "{cmd}");
    assert!(cmd.contains("bead.sh dep add \"$id\" \"$(cat \"$SPIRA_RUN/seeds/a.id\")\""), "{cmd}");
    assert!(s.reads_bead_rows());
}

#[test]
fn a_command_step_reaches_a_seeds_id_by_handle() {
    let s = seeded("[[seed]]\nhandle = \"a\"\ntitle = \"a\"\nkind = \"event\"\n[[step]]\nat = 1\nname = \"c\"\ncommand = \"bead.sh label add {bead:a} x\"\n").unwrap();
    assert_eq!(s.steps[0].shell().unwrap(), "bead.sh label add \"$(cat \"$SPIRA_RUN/seeds/a.id\")\" x");
}

#[test]
fn a_bad_seed_or_a_handle_nothing_declares_is_refused() {
    for bad in [
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\n",
        "[[seed]]\nhandle = \"A b\"\ntitle = \"t\"\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"\"\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\nfor = \"builder\"\nkind = \"event\"\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\npriority = 5\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\n[[seed]]\nhandle = \"a\"\ntitle = \"u\"\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\n[[seed]]\nhandle = \"b\"\ntitle = \"t\"\n",
        "[[seed]]\nhandle = \"b\"\ntitle = \"t\"\ndeps = [\"a\"]\n",
        "[[seed]]\nhandle = \"a\"\ntitle = \"t\"\nat = 5\n[[seed]]\nhandle = \"b\"\ntitle = \"u\"\ndeps = [\"a\"]\n",
        "[[step]]\nat = 1\ncommand = \"echo {bead:ghost}\"\n",
        "[[step]]\nat = 1\nname = \"seed9\"\ncommand = \"true\"\n",
        "[[step]]\nat = 1\nname = \"n\"\ncommand = \"true\"\n[[step]]\nat = 2\nname = \"n\"\ncommand = \"true\"\n",
    ] {
        assert!(seeded(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_repo_spec_names_a_mode_and_a_trunk_and_the_ref_its_branches_land_on() {
    let spec = |s: &str| RepoSpec::parse(s).unwrap();
    assert_eq!((spec("docs:push").mode.as_str(), spec("docs:push").trunk.as_str(), spec("docs:push").base()), ("push", "main", "origin/main".to_string()));
    for mode in MODES {
        assert_eq!(spec(&format!("r:{mode}")).mode, *mode);
    }
    assert_eq!(spec("r:queue.local:master").base(), "local/master");
    assert_eq!(spec("r:pr:master").base(), "origin/master");
    for bad in ["", "docs", "docs:", ":push", "docs:merge", "Docs:push", "docs:push:", "a:push:b:c", "sim:push:master"] {
        assert!(RepoSpec::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_shipped_bead_contract_parses() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let s = parse_scenario(&std::fs::read_to_string(here.join("scenarios/bead-contract.toml")).unwrap()).unwrap();
    assert!(s.reads_bead_rows() && s.expects.len() >= 10);
}

#[test]
fn a_world_always_has_its_own_repository_first_and_registers_every_repository_it_was_given() {
    let spec = |s: &str| RepoSpec::parse(s).unwrap();
    let only = repo_set(&[spec("docs:hold:master")]).unwrap();
    assert_eq!(only.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), [REPO_NAME, "docs"]);
    assert_eq!(only[0].mode, "queue.local");
    let named = repo_set(&[spec("docs:pr"), spec("sim:push")]).unwrap();
    assert_eq!(named.iter().map(|r| (r.name.as_str(), r.mode.as_str())).collect::<Vec<_>>(), [(REPO_NAME, "push"), ("docs", "pr")]);
    assert!(repo_set(&[spec("docs:pr"), spec("docs:push")]).is_err());
    let w = Path::new("/w");
    assert_eq!(registry_rows(w, &named), "sim|/w/work|push|origin/main|||plan\ndocs|/w/repos/docs/work|pr|origin/main|||plan\n");
    let settings = config_settings(w, &only, Path::new("/w/bin/gh"));
    let section = |k: &str| settings.iter().find(|(n, _)| n == k).map(|(_, v)| serde_json::from_str::<serde_json::Value>(v).unwrap());
    assert_eq!(section("repo.docs").unwrap(), serde_json::json!({"path": "/w/repos/docs/work", "mode": "hold", "base": "origin/master"}));
    assert_eq!(section("repo.sim").unwrap()["mode"], "queue.local");
}

#[test]
fn a_scenario_declares_repositories_and_routes_its_beads_to_them() {
    let text = |extra: &str| format!("epoch = 1\nhorizon = 2\nrepos = [\"docs:push:master\"]\n{extra}");
    let ok = parse_scenario(&text("[[step]]\nat = 0\nfile = \"sp-a\"\nrepo = \"docs\"\n")).unwrap();
    assert_eq!(scenario_repos(&ok, &[]).unwrap().len(), 2);
    assert!(ok.steps[0].shell().unwrap().contains("-l repo:docs "));
    let claim = parse_scenario(&text("[[step]]\nat = 0\nclaim = \"sp-a\"\nrepo = \"docs\"\nscript = [\"submit\"]\n")).unwrap();
    assert!(claim.steps[0].shell().unwrap().contains("'sp-a repo docs' 'sp-a submit'"));
    for bad in ["[[step]]\nat = 0\nfile = \"sp-a\"\nrepo = \"nowhere\"\n", "[[step]]\nat = 0\ncommand = \"true\"\nrepo = \"docs\"\n"] {
        assert!(parse_scenario(&text(bad)).is_err(), "{bad}");
    }
    let over = scenario_repos(&ok, &["docs:hold".to_string(), "other:pr".to_string()]).unwrap();
    assert_eq!(over.iter().map(|r| (r.name.as_str(), r.mode.as_str(), r.trunk.as_str())).collect::<Vec<_>>(), [(REPO_NAME, "queue.local", "main"), ("docs", "hold", "main"), ("other", "pr", "main")]);
}

#[test]
fn the_summon_works_a_bead_in_the_repository_the_scenario_routes_it_to() {
    let scenario = "sp-a repo docs\nsp-a submit\nsp-b submit\n";
    assert_eq!(routed_repo(scenario, "sp-a").as_deref(), Some("docs"));
    assert_eq!(routed_repo(scenario, "sp-b"), None);
    assert!(spira_sim::agent::parse(scenario, "sp-a").is_ok());
}
