//! The call world (`spira_sim::callworld`) plays the scheduler's call pass over the checkout's own
//! runes; `bound-classes` is the shipped walk of the bound-resource scheduler.

use spira_sim::callworld;

fn root() -> std::path::PathBuf {
    spira_config::resolve::locate_home_for_process().unwrap()
}

fn shipped() -> String {
    std::fs::read_to_string(testkit::manifest_dir().join("scenarios/bound-classes.toml")).expect("the shipped scenario")
}

fn play(text: &str) -> (callworld::Script, Vec<callworld::Outcome>) {
    let script = callworld::parse(text).expect("it parses");
    let out = callworld::run(&root(), &script).expect("it runs");
    (script, out)
}

#[test]
fn bound_classes_scenario_meets_every_expectation_on_the_shipped_runes() {
    let (script, out) = play(&shipped());
    assert_eq!(callworld::missed(&script, &out), None, "{out:#?}");
}

#[test]
fn bound_classes_scenario_reads_the_cockpit_bound_at_the_spill() {
    let (_, out) = play(&shipped());
    let spill = out.iter().find(|o| o.name.contains("second-mender")).expect("the spill pass");
    assert_eq!(spill.calls, ["mender"]);
    assert_eq!(spill.sp_bound.split_whitespace().take(3).collect::<Vec<_>>(), ["build-disk", "9", "7"], "{spill:?}");
    assert!(spill.sp_bound.ends_with(" 1"), "over the limit: {spill:?}");
}

/// The control: the same walk on a world where a reactive session is held like a planned one
/// (mender not reactive) must miss, so a pass that spills is what the scenario proves.
#[test]
fn bound_classes_scenario_fails_when_the_reactive_spill_is_gone() {
    let text = format!("{}\n[rune.mender]\nreactive = false\n", shipped());
    let script = callworld::parse(&text).expect("it parses");
    let walked = callworld::run(&root(), &script).map(|out| callworld::missed(&script, &out));
    assert!(!matches!(walked, Ok(None)), "a mender held by build-disk must break the walk: {walked:?}");
}

#[test]
fn a_scenario_naming_a_rune_the_home_lacks_is_refused() {
    let text = "driver = \"calls\"\nmax_dwarves = 7\nmax_live = 40\n[[pass]]\nname = \"p\"\nstarts = { nobody = 1 }\nexpect_calls = []\n";
    let script = callworld::parse(text).expect("parses");
    assert!(callworld::run(&root(), &script).unwrap_err().contains("nobody"));
}
