//! Install's one-time lifecycle population (sp-k62xz8): Spira installed on top of an existing
//! beads database gives every bead in it a lifecycle row, in the state its bd status and
//! landing evidence imply. Without it a reinstall over a populated store came up with every
//! existing bead invisible to the lifecycle machine.
//!
//! THE CLASSIFIER IS spira-lc's OWN — `spira-lc classify --every-bead`, the migration
//! classifier `spira/cutover-deploy.sh` runs per repository — never a second implementation
//! here. This module builds its argv and judges its report; nothing else.
//!
//! Idempotent and non-destructive by the classifier's own contract: a bead that already has a
//! row is counted as present and left untouched, bd is only read. Loud: a bead the classifier
//! could not classify fails the phase, named — the report's `errors` are each `<id>: <why>`.
//! A bead labelled with a repository this config does not have (a beads DB from somewhere
//! else) is not an error: the classifier judges it on bd and the ledger alone, and install
//! warns with the list (`unknown-repo: N (...)`).

use serde_json::Value;

/// What the population reads, every path resolved by the caller from the config in force.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub bd_bin: String,
    pub bd_db: String,
    pub landstate_dir: String,
    pub queue_dir: String,
    pub ask_label: Option<String>,
}

/// The counts install reports: beads in the database, rows this run created, rows already
/// there — and the beads classified without git evidence because their `repo:` label names a
/// repository this config does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counts {
    pub beads: u64,
    pub created: u64,
    pub present: u64,
    pub unknown_repo: Vec<String>,
}

/// How many unknown-repo bead ids the warning names before it says "and N more".
pub const UNKNOWN_REPO_SHOWN: usize = 10;

/// The warning line for beads whose `repo:` label names no configured repository, or `None`
/// when there are none: `unknown-repo: N (sp-a, sp-b, ... and M more)`.
pub fn unknown_repo_line(ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let shown = ids.iter().take(UNKNOWN_REPO_SHOWN).cloned().collect::<Vec<_>>().join(", ");
    let more = ids.len().saturating_sub(UNKNOWN_REPO_SHOWN);
    let tail = if more > 0 { format!(", and {more} more") } else { String::new() };
    Some(format!("unknown-repo: {} ({shown}{tail}) — labelled with a repository this config does not have; classified on bd status and the ledger alone", ids.len()))
}

/// `spira-lc` argv (after the program name) for one population run.
pub fn args(i: &Inputs) -> Vec<String> {
    let mut a: Vec<String> = ["classify", "--every-bead", "--bd-bin", &i.bd_bin, "--bd-db", &i.bd_db, "--landstate-dir", &i.landstate_dir, "--queue-dir", &i.queue_dir]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(l) = &i.ask_label {
        a.push("--ask-label".into());
        a.push(l.clone());
    }
    a
}

/// Judge one classify run from its exit code and output (stdout, possibly with stderr beside
/// it). `Err` names every bead that could not be classified, or says why the report itself
/// cannot be trusted; never a partial `Ok`.
pub fn interpret(rc: i32, out: &str) -> Result<Counts, String> {
    let report = out
        .find('{')
        .and_then(|i| serde_json::Deserializer::from_str(&out[i..]).into_iter::<Value>().next())
        .and_then(Result::ok)
        .ok_or_else(|| format!("spira-lc classify exited {rc} without a report: {}", out.trim()))?;
    let errors: Vec<String> = report.get("errors").and_then(Value::as_array).map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!(
            "{} bead(s) could not be classified — no bead is skipped; fix these and re-run install (rows already written are kept):\n  {}",
            errors.len(),
            errors.join("\n  ")
        ));
    }
    if rc != 0 {
        return Err(format!("spira-lc classify exited {rc}: {}", out.trim()));
    }
    let n = |k: &str| report.get(k).and_then(Value::as_u64).ok_or_else(|| format!("spira-lc classify's report has no {k:?} count"));
    let unknown_repo = report.get("unknown_repo").and_then(Value::as_array).map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let c = Counts { beads: n("beads")?, created: n("classified")?, present: n("skipped_already_classified")?, unknown_repo };
    if c.created + c.present != c.beads {
        return Err(format!("spira-lc classify saw {} bead(s) but accounts for {} created + {} present — refusing to call that complete", c.beads, c.created, c.present));
    }
    Ok(c)
}

/// One population run: `run(args)` invokes `spira-lc <args>` and returns (exit code, output).
pub fn run(i: &Inputs, run: impl FnOnce(&[String]) -> (i32, String)) -> Result<Counts, String> {
    let (rc, out) = run(&args(i));
    interpret(rc, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs { bd_bin: "bd".into(), bd_db: "/db".into(), landstate_dir: "/run/landstate".into(), queue_dir: "/run/queue".into(), ask_label: Some("ask-x".into()) }
    }

    #[test]
    fn the_argv_is_the_classifiers_every_bead_mode_with_every_path() {
        let a = args(&inputs());
        assert_eq!(a[..2], ["classify".to_string(), "--every-bead".to_string()]);
        for (flag, v) in [("--bd-db", "/db"), ("--landstate-dir", "/run/landstate"), ("--queue-dir", "/run/queue"), ("--ask-label", "ask-x"), ("--bd-bin", "bd")] {
            let at = a.iter().position(|x| x == flag).unwrap_or_else(|| panic!("{flag} missing"));
            assert_eq!(a[at + 1], v);
        }
        assert!(!a.iter().any(|x| x == "--repo" || x == "--dry-run" || x == "--reclassify"), "{a:?}");
    }

    #[test]
    fn no_ask_label_passes_none() {
        let a = args(&Inputs { ask_label: None, ..inputs() });
        assert!(!a.iter().any(|x| x == "--ask-label"));
    }

    #[test]
    fn a_clean_report_gives_the_three_counts() {
        let out = r#"{"beads": 4, "classified": 3, "skipped_already_classified": 1, "errors": []}"#;
        assert_eq!(interpret(0, out), Ok(Counts { beads: 4, created: 3, present: 1, unknown_repo: vec![] }));
    }

    #[test]
    fn a_second_run_reports_nothing_created() {
        let out = r#"{"beads": 4, "classified": 0, "skipped_already_classified": 4, "errors": []}"#;
        assert_eq!(interpret(0, out), Ok(Counts { beads: 4, created: 0, present: 4, unknown_repo: vec![] }));
    }

    #[test]
    fn a_report_after_a_stderr_line_still_parses() {
        let out = "spira: a warning\n{\"beads\": 1, \"classified\": 1, \"skipped_already_classified\": 0, \"errors\": []}\n";
        assert_eq!(interpret(0, out).unwrap().created, 1);
    }

    #[test]
    fn unknown_repo_beads_are_counts_not_errors() {
        let out = r#"{"beads": 2, "classified": 2, "skipped_already_classified": 0, "unknown_repo": ["sp-a", "sp-b"], "errors": []}"#;
        let c = interpret(0, out).unwrap();
        assert_eq!(c.unknown_repo, vec!["sp-a".to_string(), "sp-b".to_string()]);
        assert!(unknown_repo_line(&c.unknown_repo).unwrap().starts_with("unknown-repo: 2 (sp-a, sp-b)"));
    }

    #[test]
    fn no_unknown_repo_beads_means_no_warning() {
        assert_eq!(unknown_repo_line(&[]), None);
    }

    #[test]
    fn the_unknown_repo_list_is_capped() {
        let ids: Vec<String> = (0..13).map(|i| format!("sp-{i}")).collect();
        let l = unknown_repo_line(&ids).unwrap();
        assert!(l.starts_with("unknown-repo: 13 (sp-0,") && l.contains("sp-9, and 3 more)") && !l.contains("sp-10"), "{l}");
    }

    #[test]
    fn an_unclassifiable_bead_fails_naming_it() {
        let out = r#"{"beads": 2, "classified": 1, "skipped_already_classified": 0, "errors": ["sp-x: carries more than one repo: label (a, b)"]}"#;
        let e = interpret(2, out).unwrap_err();
        assert!(e.contains("sp-x") && e.contains("1 bead(s)"), "{e}");
    }

    #[test]
    fn a_count_that_does_not_add_up_is_refused() {
        let out = r#"{"beads": 3, "classified": 1, "skipped_already_classified": 1, "errors": []}"#;
        assert!(interpret(0, out).unwrap_err().contains("refusing"));
    }

    #[test]
    fn no_report_is_a_failure_carrying_the_output() {
        let e = interpret(2, "cannot tell: connection refused").unwrap_err();
        assert!(e.contains("connection refused"), "{e}");
    }

    #[test]
    fn a_nonzero_exit_with_an_empty_error_list_is_still_a_failure() {
        let out = r#"{"beads": 0, "classified": 0, "skipped_already_classified": 0, "errors": []}"#;
        assert!(interpret(2, out).is_err());
    }
}
