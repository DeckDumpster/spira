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

macro_rules! scenario {
    ($($test:ident),* $(,)?) => {
        $( #[test] fn $test() {
            let slug = stringify!($test).strip_prefix("scenario_").expect("a scenario test is named scenario_<slug>").replace('_', "-");
            if let Err(e) = run_all(&text(&slug)) {
                panic!("scenario {slug}:\n{e}");
            }
        } )*
        const COVERED: &[&str] = &[$(stringify!($test)),*];
    };
}

scenario!(
    scenario_harness_smoke,
    scenario_hung_child,
    scenario_hung_runner,
    scenario_over_long_pass,
    scenario_prior_runner_alive,
    scenario_launch_fails,
    scenario_ec2_past_deadline,
    scenario_ledger_attempts,
    scenario_one_proxmox_slot,
    scenario_slot_reuse_needs_death,
    scenario_ec2_spill,
    scenario_pokebot_1031,
    scenario_flaky_rerun,
    scenario_base_runs_disagree,
    scenario_eject_refused_holds,
    scenario_judge_resumes,
    scenario_cut_needs_slot,
    scenario_dep_rides_stack,
    scenario_pokebot_image_fault,
    scenario_stack_supersede,
);

#[test]
fn every_scenario_file_is_its_own_test() {
    let mut files: Vec<String> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter_map(|n| n.strip_suffix(".toml").map(str::to_string))
            .filter(|n| n != "profiles")
            .map(|n| format!("scenario_{}", n.replace('-', "_")))
            .collect();
    files.sort();
    let mut covered: Vec<String> = COVERED.iter().map(|s| s.to_string()).collect();
    covered.sort();
    assert_eq!(files, covered);
}

#[test]
fn harness_smoke_passes_under_both_profiles_with_its_inverted_expectation_failing_by_name() {
    let reports = run_all(&text("harness-smoke")).unwrap();
    assert_eq!(
        reports
            .iter()
            .map(|r| r.profile.as_str())
            .collect::<Vec<_>>(),
        ["default", "reference-minimum"]
    );
    for r in &reports {
        assert_eq!(
            r.controls,
            [format!(
                "expectation inverted-no-repo-visited unmet under {}: counted 6, expected 0",
                r.profile
            )]
        );
        assert_eq!(r.lines.len(), 6);
    }
}

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
fn with_spill_off_the_ec2_spill_rounds_run_one_at_a_time() {
    let off = text("ec2-spill").replace("spill = \"ec2\"", "spill = \"\"");
    let e = run_all(&off).unwrap_err();
    for profile in ["default", "reference-minimum"] {
        for unmet in [
            "three-at-once unmet under {p}: counted 1, expected 3",
            "two-on-ec2 unmet under {p}: counted 0, expected 2",
        ] {
            let unmet = unmet.replace("{p}", profile);
            assert!(e.contains(&format!("expectation {unmet}")), "{e}");
        }
        assert!(
            e.contains(&format!(
                "inverted expectation inverted-one-at-a-time held under {profile}: counted 1"
            )),
            "{e}"
        );
    }
    assert!(
        !e.contains("every-round-ran"),
        "one at a time, every round still ran: {e}"
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
