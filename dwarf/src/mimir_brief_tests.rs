//! The shipped Mimir brief, read as the dwarf is handed it.

use std::path::{Path, PathBuf};

fn spira_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira")
}

fn brief() -> String {
    std::fs::read_to_string(spira_dir().join("runes/mimir.md")).expect("mimir.md")
}

fn home_paths(text: &str) -> Vec<String> {
    const OPEN: &str = "{{SPIRA_HOME}}/";
    text.match_indices(OPEN)
        .map(|(i, _)| {
            let rest = &text[i + OPEN.len()..];
            let end = rest.find(|c: char| c.is_whitespace() || "\"') ".contains(c)).unwrap_or(rest.len());
            rest[..end].to_string()
        })
        .collect()
}

fn unresolved(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.contains("$SPIRA_") || l.contains("$BEAD_ID")).collect()
}

// covers: UC-ops-detection-remediation-17
#[test]
fn the_brief_encodes_the_five_step_pass_and_its_threshold() {
    let b = brief();
    for step in ["Census", "Select", "Diagnose", "Design", "Record"] {
        assert!(b.contains(step), "step {step}");
    }
    assert!(b.contains("three or more distinct causal events"));
    assert!(b.contains("coincidence"));
}

// covers: UC-ops-detection-remediation-17
#[test]
fn the_brief_encodes_the_output_bar_and_the_per_pass_bound() {
    let b = brief();
    for want in [
        "file:line",
        "command run and what it printed",
        "Acceptance criteria",
        "positive control",
        "failure class",
        "occurrence count",
        "{{MAX_BEADS}}",
        "flooded",
        "Silence is what",
    ] {
        assert!(b.contains(want), "output bar: {want}");
    }
}

// covers: UC-ops-detection-remediation-17
#[test]
fn the_brief_advances_the_watermark_only_by_compare_and_swap_on_a_successful_census() {
    let b = brief();
    for want in [
        "census_rc",
        "only when the census succeeded",
        "wm_now",
        "$wm_start",
        "leave the watermark where it is",
        "Watermark HELD at",
        "Watermark advanced to",
    ] {
        assert!(b.contains(want), "watermark advance: {want}");
    }
}

// covers: UC-ops-detection-remediation-17
#[test]
fn every_spira_home_path_in_the_brief_names_a_file_and_the_check_can_fail() {
    let planted = home_paths("run {{SPIRA_HOME}}/spira/census.sh now");
    assert_eq!(planted, ["spira/census.sh"]);
    assert!(!spira_dir().join("..").join(&planted[0]).exists(), "the planted doubled segment must not resolve");
    for rel in home_paths(&brief()) {
        assert!(spira_dir().join(&rel).exists(), "{{{{SPIRA_HOME}}}}/{rel} does not resolve");
    }
}

// covers: UC-ops-detection-remediation-17
#[test]
fn the_brief_names_no_shell_variable_the_renderer_would_leave_unexpanded() {
    assert_eq!(unresolved("a\n$SPIRA_FAKE_VAR x\nb"), ["$SPIRA_FAKE_VAR x"]);
    assert!(unresolved(&brief()).is_empty(), "{:?}", unresolved(&brief()));
}
