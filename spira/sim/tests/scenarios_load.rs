//! Every scenario shipped under `spira/sim/scenarios/` loads: it parses, and every `real_actors`
//! name is a sim-only actor of the tree's `sim/actors.toml`. The loop's units (sentinel-call,
//! sentinel-landing, sift, rounds, gate-worker, publish) are tasks of `spira-runtime serve` now
//! and the simulator drives them in-process (ruling D6, `sim/tests/whole_loop.rs` and
//! `sim/tests/loop_ports.rs`), so a scenario that still names one fails at load. The scenarios
//! that named one were ported (the loop's half to `loop_ports.rs`, the effect's half kept here
//! with the call's effect as an actor) or retired with their cover; the ones in [`BLOCKED`] are
//! neither yet, and say why.

use spira_sim::call::{dwarf_command, rune_of};
use spira_sim::drive::{parse_scenario, Scenario, SCENARIOS_DIR};
use std::path::{Path, PathBuf};

/// Scenarios that still name a migrated actor, each with what it proves and why it is not
/// ported: each needs a bead landed through the real `landing-pass land`, round VM or release
/// tools, which only the loop's tasks drive now, and a world does not run the service. Their
/// property is reported, not dropped; the cover named is partial.
const BLOCKED: &[(&str, &str)] = &[
    ("happy-path", "filed to LANDED, published and release-tagged; loop half covered by whole_loop.rs, the publish PR and tag half by nothing end to end; watch.perf_happy runs it in production"),
    ("cut-in-bare-env", "gate.yml's cut job (steps/cut.sh) succeeds in a bare env after a landing; no cover"),
    ("publish-then-cut", "the cut job with the release on PATH after a landing; no cover"),
    ("push-lands", "a push-mode repo lands on its origin trunk; landing-pass tests cover it on fakes only"),
    ("repo-modes", "push to a master trunk creates no main and a hold repo never lands; push to landing:master untested"),
    ("run-state-trail", "ROUND-LAND logged with its member, certificate on disk before it; the members= log line untested"),
    ("rc-stale-round-pointer", "a round lands past an abandoned round's record; queue round tests cover land/certify, not round-recert --drive"),
    ("rc-attributing-redrive", "a re-drive from ATTRIBUTING lands and never returns the member; queue covers the re-drive, not the land"),
    ("rc-fences-in-vm", "fences run in the round VM, never on the host; no negative check of a host gate call"),
    ("supersede", "only a LANDED successor supersedes, with its bd note, close and inbox line; decide covered, the writes not"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shipped() -> Vec<(String, Scenario)> {
    let dir = root().join(SCENARIOS_DIR);
    let mut out: Vec<(String, Scenario)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let sc = parse_scenario(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, sc)
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
    let mut blocked: Vec<String> = BLOCKED.iter().map(|(b, _)| b.to_string()).collect();
    blocked.sort();
    assert_eq!(unloadable, blocked, "every blocked scenario still fails at load, and only those: one that loads now comes off the list");
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
