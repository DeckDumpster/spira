//! CLI-level tests: the contract at the interface (exit codes, stdout shapes), with every
//! store read replaced by a file (`--events`, `--ready`, `--epics`, `--lifecycle`,
//! `--blocker-records`) or by a fake `bdq` script driven through [`Store`] directly.

use super::*;

// ---- select --------------------------------------------------------------------------

// ---- select --blockers machine: the stacked-dependents parity fixture ------------------

// ---- the lifecycle switch (DESIGN.md §6a) ------------------------------------------------

// ---- stack: the dwarf's own claim-time proposal ----------------------------------------

// ---- the store, through a fake bdq ----------------------------------------------------

// ---- ready / claim CLI (wave 4.25, sp-obhv6) -------------------------------------------
//
// `rune-ready`/`rune-exclude`/`bulk-ready-by-rune`/`claim-retry` read real process
// environment (`SPIRA_HOME`, `SPIRA_TOML`, ...) — unlike the rest of this
// file's tests, which thread every input through a file or a hand-built `Store`. These
// edit the environment only through `testkit::env`, which serializes them against every
// other test in this binary and restores on drop; `run` takes the same lock to read.

/// `rune_home` is the declared home: `SPIRA_HOME` wins, else `$SPIRA_RELEASE/spira`, and
/// neither set is a refusal naming both — never a search from this executable.
#[test]
fn rune_home_is_spira_home_then_the_declared_release_and_refuses_naming_both() {
    let pinned = testkit::env(&[("SPIRA_HOME", Some("/some/pinned/tree"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("/some/pinned/tree")));
    drop(pinned);
    let rel = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", Some("/r"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("/r/spira")));
    drop(rel);
    let _env = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", None)]);
    let err = super::rune_home().unwrap_err();
    assert!(err.contains("SPIRA_HOME") && err.contains("SPIRA_RELEASE"), "{err}");
}

// ---- rune-ready / bulk-ready-by-rune: the machine's ready set --------------------------------
// ---- the lifecycle row is the claim (sp-860zj) -------------------------------------------
//
// bd's status and assignee are content nobody reads for claimability: the candidate set is
// the lifecycle machine's READY/REWORK rows. bd below answers its own `list --status open
// --no-assignee` with W alone (open, unassigned) and holds P in_progress under a dead
// dwarf's name; the machine says the opposite.
// ---- ready-count --json: the ready set a display reads (sp-7g5q6) ------------------------
//
// The cockpit's NEXT rows asked bd ready itself; under the machine that set is bd's status
// and assignee, which nobody claims by any more. `ready-count --json` is the same set the
// count is, so a display and the caller cannot disagree.

// ---- one resolution path: the label keys come from the config file, never the caller's env ----

#[test]
fn no_call_site_reads_the_unfiltered_store() {
    let src = include_str!("store.rs");
    assert_eq!(src.matches("c.arg(\"list\")").count(), 1, "one spira-lc list site");
    assert!(src.contains("c.arg(\"list\").args(filter)"));
}

#[test]
fn express_is_a_bool_flag_and_never_swallows_what_follows() {
    let raw: Vec<String> = ["labels", "", "--express"].iter().map(|s| s.to_string()).collect();
    let a = Args::parse(&raw).expect("--express takes no value");
    assert!(a.has("--express"));
    assert_eq!(a.pos, vec!["labels".to_string(), String::new()]);
}

// ---- ready-count shares one computation per ready_ttl_s (sp-su318h) ----------------------
