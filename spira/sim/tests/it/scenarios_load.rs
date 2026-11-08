//! Every scenario shipped under `spira/sim/scenarios/` loads: it parses, and every `real_actors`
//! name is a sim-only actor of the tree's `sim/actors.toml`. The loop's units (sentinel-call,
//! sentinel-landing, sift, rounds, gate-worker, publish, supersede-duty) are tasks of
//! `spira-runtime serve` now and the simulator drives them in-process (ruling D6: the loop world,
//! `spira/sim/src/loopworld.rs`, and `sim/tests/it/whole_loop.rs`), so a scenario that still names
//! one, or runs one as an inline actor, is refused here. The scenarios that did were ported to
//! loop worlds and `spira/sim/tests/it/loop_*.rs`, kept as world scenarios with the call's effect as
//! their actor, or retired with their cover (`spira/sim/MIGRATION.md`); [`BLOCKED`] lists any not
//! yet done, and says why.

use spira_sim::call::{dwarf_command, rune_of};
use spira_sim::drive::{parse_scenario, Scenario, SCENARIOS_DIR};
use spira_sim::loopworld;
use std::path::{Path, PathBuf};

/// Scenarios that still name a migrated actor, each with what it proves and why it is not yet
/// ported. None: every one was ported or retired (`spira/sim/MIGRATION.md`). A row here must
/// name what the scenario proves and its partial cover, so a property is reported, never dropped.
const BLOCKED: &[(&str, &str)] = &[];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shipped() -> Vec<(String, Scenario)> {
    let dir = root().join(SCENARIOS_DIR);
    let mut out: Vec<(String, Scenario)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter_map(|p| {
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            if loopworld::is_loop(&text) {
                loopworld::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                return None;
            }
            let sc = parse_scenario(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            Some((name, sc))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_shipped_scenario_names_only_sim_only_actors_or_is_listed_as_blocked() {
    let actors = sim::actors::parse_specs(&std::fs::read_to_string(root().join("sim/actors.toml")).expect("sim/actors.toml")).expect("the tree's actors parse");
    let known: Vec<&str> = actors.iter().map(|a| a.name.as_str()).collect();
    let mut unloadable: Vec<String> = vec![];
    for (name, sc) in shipped() {
        let missing: Vec<&String> = sc.real_actors.iter().filter(|a| !known.contains(&a.as_str())).collect();
        if !missing.is_empty() {
            unloadable.push(name.clone());
            assert!(BLOCKED.iter().any(|(b, _)| *b == name), "{name} names {missing:?}, which sim/actors.toml does not have: port or retire it (ruling D6)");
        }
    }
    let present: Vec<String> = shipped_names();
    let mut blocked: Vec<String> = BLOCKED.iter().map(|(b, _)| b.to_string()).filter(|b| present.contains(b)).collect();
    blocked.sort();
    assert_eq!(unloadable, blocked, "every blocked scenario still shipped still fails at load, and only those: one that loads now comes off the list");
}

/// The call the ported scenarios run in place of the sentinel-call actor: the world's
/// `SPIRA_CALL` seam given the argv sentinel-call's `unit_start` effect starts, `dwarf --home
/// <home> builder` under `call_argv`'s properties and environment. Every scenario's call is the
/// same command, and the seam reads it as a builder call of the release's dwarf.
#[test]
fn the_ported_scenarios_call_through_the_seam_with_the_call_effects_argv() {
    let calls: Vec<(String, String)> = shipped().into_iter().flat_map(|(n, sc)| sc.actors.into_iter().filter(|a| a.name == "call").map(move |a| (n.clone(), a.command))).collect();
    let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["builder-excludes-qa-proposed", "claim-rank", "dwarf-brief", "dwarf-slain", "rc-cutover-readiness", "rebase-loop-note", "resubmit-moved-tip", "submit-from-other-cwd"]);
    let one = &calls[0].1;
    assert!(calls.iter().all(|(_, c)| c == one), "one call command: {calls:?}");
    let shell = one.strip_prefix("\"$SPIRA_CALL\"").expect("the call runs the world's SPIRA_CALL seam");
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s\\n'{shell}"))
        .env_clear()
        .envs([("SPIRA_RELEASE", "/r"), ("SPIRA_HOME", "/h"), ("HOME", "/home/u"), ("SPIRA_TOML", "/c/sim.toml")])
        .output()
        .expect("sh");
    assert!(out.status.success(), "{out:?}");
    let argv: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(rune_of(&argv), Ok("builder"), "{argv:?}");
    let (env, command) = dwarf_command(&argv).expect("a dwarf call");
    assert_eq!(command, ["/r/bin/dwarf", "--home", "/h", "builder"]);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(env, [("SPIRA_RELEASE", "/r"), ("PATH", "/r/bin:/r/spira:/usr/local/bin:/usr/bin:/bin"), ("HOME", "/home/u"), ("SPIRA_TOML", "/c/sim.toml")]);
    for p in ["--property=TimeoutStartSec=3600", "--property=IOSchedulingClass=idle", "--property=IOWeight=10"] {
        assert!(argv.iter().any(|a| a == p), "{p} (sentinel_call::dwarf_props): {argv:?}");
    }
}

/// The loop worlds shipped beside the world scenarios: each parses as one (`driver = "loop"`).
#[test]
fn the_shipped_loop_worlds_parse() {
    let dir = root().join(SCENARIOS_DIR);
    let mut loops: Vec<String> = std::fs::read_dir(&dir)
        .expect("scenarios")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| loopworld::is_loop(&t)))
        .map(|p| p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string())
        .collect();
    loops.sort();
    assert!(loops.contains(&"happy-path".to_string()), "watch.perf_happy's scenario is a loop world: {loops:?}");
    for name in loops {
        let text = std::fs::read_to_string(dir.join(format!("{name}.toml"))).expect("readable");
        loopworld::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Every scenario file's name, world or loop.
fn shipped_names() -> Vec<String> {
    let dir = root().join(SCENARIOS_DIR);
    std::fs::read_dir(&dir).expect("scenarios").filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "toml")).filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string)).collect()
}

/// What the loop's migrated units ran as processes. A world scenario's inline actor running one
/// would bring the unit back as an actor process under another name (ruling D6).
const MIGRATED_COMMANDS: &[&str] = &["sentinel --call-only", "landing-pass land", "landing-pass --pass", "round-controller sifts", "batcher rounds", "gate-worker run", "queue publish-settle", "supersede-duty.sh", "reconciler goodput-pass"];

#[test]
fn no_shipped_scenario_runs_a_migrated_unit_as_an_inline_actor() {
    let found: Vec<(String, String)> = shipped()
        .into_iter()
        .flat_map(|(n, sc)| sc.actors.into_iter().filter(|a| MIGRATED_COMMANDS.iter().any(|m| a.command.contains(m))).map(move |a| (n.clone(), a.command)))
        .collect();
    assert!(found.is_empty(), "these inline actors run a unit that is a task of spira-runtime serve: {found:?}");
}
