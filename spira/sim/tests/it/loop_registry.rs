//! The test-registry flip path walked on the loop world (`scenarios/test-registry-flip.toml`): a
//! round finds a test red on its combined tree and green on every member alone, the flip handling
//! disables it for the registry's hold, the pane lists it, a landing clears it, an unattended row
//! expires and the test blocks again.

use lifecycle::bead::BeadState;
use spira_sim::loopworld::{self, Run, Script};

fn shipped() -> String {
    std::fs::read_to_string(testkit::manifest_dir().join("scenarios/test-registry-flip.toml")).expect("the shipped scenario")
}

fn walk(script: Script) -> Run {
    loopworld::run(7, script).unwrap_or_else(|e| panic!("the loop ran: {e}"))
}

/// The shipped scenario's run, walked once: it spans under three hours of virtual time.
fn shipped_run() -> &'static Run {
    static RUN: std::sync::OnceLock<Run> = std::sync::OnceLock::new();
    RUN.get_or_init(|| walk(shipped_script()))
}

fn shipped_script() -> Script {
    loopworld::parse(&shipped()).expect("it parses")
}

const CLEARED: &str = "unit:loom factory::tests::cleared_by_landing";
const EXPIRING: &str = "unit:loom factory::tests::left_to_expire";

#[test]
fn test_registry_flip_scenario_meets_every_expectation() {
    let script = shipped_script();
    let r = shipped_run();
    assert_eq!(r.missed(&script), None, "{}", r.dump());
    assert_eq!(r.rows["sp-tr02"].state, BeadState::Landed, "{}", r.dump());
}

#[test]
fn a_round_that_finds_a_flip_disables_it_for_its_hold_and_files_its_follow_on() {
    let r = shipped_run();
    let mut disabled: Vec<(&str, &str, i64)> = r.disabled.iter().map(|d| (d.test.as_str(), d.bead.as_str(), d.until - d.since)).collect();
    disabled.sort();
    assert_eq!(disabled, [(CLEARED, "sp-tr02", 7_200), (EXPIRING, "sp-tr03", 7_200)], "{}", r.dump());
    assert_eq!(r.opened[0], r.lands[0], "the round that found the flip landed its own members, re-certified without the tests");
    assert_eq!(r.lifecycle_of("sp-tr01").last().map(String::as_str), Some("LANDED"), "{}", r.dump());
}

#[test]
fn the_ops_pane_lists_each_disabled_test_with_its_bead_and_expiry() {
    let r = shipped_run();
    let (_, first) = r.looks.iter().find(|(i, _)| *i == 0).expect("the first look");
    for want in [CLEARED, EXPIRING, "sp-tr02", "sp-tr03", "until "] {
        assert!(first.pane.contains(want), "{want} missing from the pane:\n{}", first.pane);
    }
}

#[test]
fn the_follow_ons_landing_clears_its_row_and_only_that_row() {
    let r = shipped_run();
    let landed = r.landed_at("sp-tr02").expect("the follow-on landed");
    assert!(r.cleared.iter().any(|(at, rows)| *at >= landed && rows.iter().any(|t| t == CLEARED)), "{:?}", r.cleared);
    assert!(r.cleared.iter().all(|(_, rows)| !rows.iter().any(|t| t == EXPIRING)), "{:?}", r.cleared);
    let (_, second) = r.looks.iter().find(|(i, _)| *i == 1).expect("the second look");
    assert_eq!(second.held.iter().map(|(t, ..)| t.as_str()).collect::<Vec<_>>(), [EXPIRING]);
}

#[test]
fn an_unattended_row_expires_and_its_test_blocks_the_next_round() {
    let r = shipped_run();
    let (_, after) = r.looks.iter().find(|(i, _)| *i == 2).expect("the look after the expiry");
    assert!(after.held.is_empty(), "{}", after.pane);
    assert!(r.reds.iter().any(|(at, _, t)| t == EXPIRING && *at >= 8_500_000), "{:?}", r.reds);
    assert!(r.reds.iter().filter(|(_, _, t)| t == CLEARED).count() == 1, "the cleared test went red once, in the flip's own round: {:?}", r.reds);
}

/// The control: with no flip the corpus stays green, no row is written, and the scenario's own
/// expectations miss.
#[test]
fn the_scenario_fails_when_the_corpus_stays_green() {
    let mut script = shipped_script();
    script.flips.clear();
    script.goal = None;
    script.until = 3_000_000;
    script.looks.truncate(1);
    let r = walk(script.clone());
    assert!(r.disabled.is_empty() && r.reds.is_empty(), "{}", r.dump());
    assert!(r.missed(&script).is_some_and(|why| why.contains("ops pane")), "{:?}", r.missed(&script));
}

/// A row held for less time than the follow-on takes to land has expired by the first look, so
/// the pane is empty where the scenario expects both rows.
#[test]
fn the_scenario_fails_when_the_hold_is_shorter_than_the_landing() {
    let mut script = shipped_script();
    script.settings.insert("SPIRA_TEST_REGISTRY_HOLD_SECS".into(), "300".into());
    script.goal = None;
    script.until = 3_000_000;
    script.looks.truncate(1);
    script.flips.iter_mut().for_each(|f| f.blocks_again_after = None);
    script.beads.truncate(1);
    script.flips[1].members = vec!["sp-tr01".into()];
    let r = walk(script.clone());
    assert!(r.missed(&script).is_some_and(|why| why.contains("ops pane")), "{:?}", r.missed(&script));
}

/// A run of `sim run` on `scenario`, the way a pilgrim starts it.
fn sim_run(scenario: &std::path::Path) -> std::process::Output {
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    std::process::Command::new(testkit::bin_exe("sim"))
        .arg("run")
        .arg(scenario)
        .env_remove("SPIRA_LC_SOCKET")
        .env_remove("SPIRA_TOML")
        .current_dir(home)
        .output()
        .expect("sim runs")
}

#[test]
fn sim_run_walks_test_registry_flip_and_fails_when_no_flip_is_scripted() {
    let ok = sim_run(&testkit::manifest_dir().join("scenarios/test-registry-flip.toml"));
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    let dir = testkit::TempDir::new("loop-registry-miss");
    let miss = dir.join("miss.toml");
    let text = shipped();
    let flips = text.find("[[flip]]").unwrap()..text.find("# The round has landed").unwrap();
    let bare = text.replace(&text[flips], "").replace("until = 10000000", "until = 3000000").replace("goal = \"sp-tr02:LANDED\"\n", "");
    let bare = bare[..bare.find("# sp-tr02 has landed").unwrap()].to_string();
    std::fs::write(&miss, bare).unwrap();
    let bad = sim_run(&miss);
    assert!(!bad.status.success(), "{}", String::from_utf8_lossy(&bad.stdout));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("ops pane"), "{}", String::from_utf8_lossy(&bad.stderr));
}

/// The scenario's expectations bite each branch: the shared run, judged against a script that
/// expects what the world did not do, misses.
#[test]
fn the_scenario_fails_when_a_landing_does_not_clear_its_row_or_a_test_does_not_block_again() {
    let r = shipped_run();
    let mut kept = shipped_script();
    kept.looks[1].rows.push(loopworld::LookRow { test: CLEARED.into(), bead: "sp-tr02".into() });
    assert!(r.missed(&kept).is_some_and(|why| why.contains("ops pane")), "{:?}", r.missed(&kept));
    let mut never_red = shipped_script();
    never_red.flips[1].blocks_again_after = Some(r.reds.iter().map(|(at, ..)| *at).max().unwrap() + 1);
    assert!(r.missed(&never_red).is_some_and(|why| why.contains("block a round again")), "{:?}", r.missed(&never_red));
}

#[test]
fn the_registrys_default_hold_is_seven_days() {
    assert_eq!(spira_sim::registryworld::DEFAULT_HOLD_SECS, 604_800);
}
