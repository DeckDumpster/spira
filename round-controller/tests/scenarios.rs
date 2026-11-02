//! Every file under `scenarios/` is one test, run under every profile.

use std::path::Path;

use round_controller::sim::{self, harness_seed, parse, profiles, Report};

/// The seed every scenario here runs on: `SPIRA_SEED`, as the simulator sets it, else the default.
fn seed() -> u64 {
    harness_seed(std::env::var("SPIRA_SEED").ok().as_deref())
}

fn run_all(text: &str) -> Result<Vec<Report>, String> {
    sim::run_all(text, seed())
}

fn text(slug: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scenarios")
        .join(format!("{slug}.toml"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A replay's virtual walls and Judge passes are reported on stdout, never asserted.
fn reported(reports: &[Report]) {
    for line in reports.iter().flat_map(|r| &r.lines) {
        if ["virtual-wall ", "judge-passes ", "redrives ", "replay-passes "].iter().any(|p| line.starts_with(p)) {
            println!("{line}");
        }
    }
}

macro_rules! scenario {
    ($($test:ident),* $(,)?) => {
        $( #[test] fn $test() {
            let slug = stringify!($test).strip_prefix("scenario_").expect("a scenario test is named scenario_<slug>").replace('_', "-");
            match run_all(&text(&slug)) {
                Err(e) => panic!("scenario {slug}:\n{e}"),
                Ok(reports) => reported(&reports),
            }
        } )*
    };
}

scenario!(
    scenario_harness_smoke,
    scenario_hung_child,
    scenario_hung_runner,
    scenario_over_long_pass,
    scenario_prior_runner_alive,
    scenario_launch_fails,
    scenario_launch_collision,
    scenario_ec2_past_deadline,
    scenario_one_proxmox_slot,
    scenario_slot_reuse_needs_death,
    scenario_pokebot_1031,
    scenario_flaky_rerun,
    scenario_judge_resumes,
    scenario_stack_supersede,
    scenario_lease_lost_kills_vm,
    scenario_superseded_mid_pass,
    scenario_resume_from_progress,
    scenario_halt_mid_pass,
    scenario_landed_from_git,
    scenario_intent_apply_once,
    scenario_switch_halted,
    scenario_switch_scoped_stop,
    scenario_switch_andon_pulled,
    scenario_switch_andon_absent,
    scenario_launch_fail_releases,
);

#[test]
fn the_inverted_expectation_fails_the_scenario_naming_itself_once_it_is_no_longer_inverted() {
    let flipped = text("harness-smoke").replace("inverted = true", "inverted = false");
    let e = run_all(&flipped).unwrap_err();
    assert!(
        e.contains("expectation inverted-no-repo-visited unmet under default"),
        "{e}"
    );
    assert!(
        e.contains("expectation inverted-no-repo-visited unmet under reference-minimum"),
        "{e}"
    );
}

#[test]
fn the_loader_refuses_an_unknown_key_by_name() {
    let e = parse(&text("harness-smoke").replace("every_secs", "evry_secs")).unwrap_err();
    assert!(e.contains("evry_secs"), "{e}");
}

#[test]
fn the_virtual_clock_is_deterministic_under_a_fixed_spira_seed() {
    let sc = parse(&text("harness-smoke")).unwrap();
    let p = &profiles().unwrap()[0];
    let lines = |seed: &str| sim::run(&sc, p, harness_seed(Some(seed))).unwrap().lines;
    assert_eq!(lines("7"), lines("7"));
    assert_ne!(lines("7"), lines("8"), "the seed draws each pass's jitter");
    assert!(
        lines("7")[0].starts_with("2027-01-15T08:00:00Z visited steps repo=home"),
        "{:?}",
        lines("7")
    );
}
