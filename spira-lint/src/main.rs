//! spira-lint [--root <dir>] [--only <rule>] [--base <rev>] [--findings <file>] [--deny-list <file>] [--gate-bead <id>]
//! spira-lint --only inventory --scan <file>
//!
//! Exit: 0 clean, 1 any finding, 2 usage, 3 a rule refused to report clean,
//! 4 the tree could not be walked (git failed) — the tool failed, nothing was linted.
//!
//! `--diff <rev>` is `--base <rev>` plus a filter: a finding is reported only when it is on a
//! line the working tree adds or changes relative to `git merge-base <rev> HEAD`, and a
//! whole-file finding only for a file the diff touches. A finding is also kept when its message
//! names a touched file as an exact path. Rules that already judge against the
//! base (`BASE_RELATIVE`) are left whole. Without it, every finding in the tree is reported.
//!
//! `--findings <file>` also writes each reported finding as one `record::FindingRecord` JSON
//! line, for a reader that attributes a red without parsing the text on stdout.
//!
//! `--deny-list <file>` is the registry `test-id-row-orphan` reads (the gate passes its `SPIRA_TEST_DENY_LIST`); an empty
//! or missing value exits 2. Without the flag the rule reads nothing and reports `none configured`.
//!
//! `--gate-bead <id>` is the item the gate is judging (it passes its `SPIRA_GATE_BEAD`); `test-id-row-orphan` lets that
//! item delete its own disabled test. An empty value names no item (a gate run by hand); a missing value exits 2.
//!
//! `--base` defaults to `SPIRA_GATE_BASE` (the gate sets it). On a clean run the positive
//! controls go to stderr: one `fence: <rule> checked <n> <unit>` per rule that reports one,
//! then `fence: spira-lint checked <n> files (<k> rules: …)` (sp-ufbkh).
//!
//! `--scan <file>` is the one standalone, tree-free mode: it scans one file's content with
//! `--only`'s rule instead of walking the repository, for a caller that has text to check but
//! no commit to check it against (`sop write`, validating a runbook body before it is
//! staged). Only `inventory` supports it today — the rule `spira/inventory.sh --scan` carried
//! and the only one with a real caller outside its own tests. It prints one offending token
//! per line and exits 0 either way; the caller decides what a non-empty result means. Unlike
//! the bash original, a malformed `spira/inventory-deny` entry exits 3 rather than silently
//! scanning with no patterns at all.

use std::path::PathBuf;
use std::process::ExitCode;

use spira_lint::args::{parse_args, Args, Parsed, USAGE};
use spira_lint::{all_rules, run, Tree};

fn default_root() -> Option<PathBuf> {
    let out = spira_config::bounded::bounded("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn main() -> ExitCode {
    let meter = spira_otel::metrics::Run::begin(
        spira_config::process::cfg("SPIRA_METRICS_ENDPOINT")
            .ok()
            .as_deref(),
        "spira-lint",
        "lint",
    );
    let code = real_main();
    meter.end(spira_otel::metrics::outcome_of_exit(code));
    code
}

fn real_main() -> ExitCode {
    let parsed = match parse_args(
        std::env::args().skip(1),
        std::env::var("SPIRA_GATE_BASE").ok(),
    ) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let Args {
        root,
        only,
        base,
        scan,
        diff,
        emit_allow,
        findings_file,
        deny_list,
        gate_bead,
    } = parsed;
    if let Some(file) = scan {
        if only.as_deref() != Some("inventory") {
            eprintln!("spira-lint: --scan is only supported with --only inventory\n{USAGE}");
            return ExitCode::from(2);
        }
        let content = match std::fs::read(&file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("spira-lint: {}: {e}", file.display());
                return ExitCode::from(2);
            }
        };
        let root = root
            .or_else(default_root)
            .unwrap_or_else(|| PathBuf::from("."));
        let deny_text =
            std::fs::read_to_string(root.join("spira/inventory-deny")).unwrap_or_default();
        let deny = spira_lint::rules::inventory::deny_fragments(&deny_text);
        let pat = match spira_lint::rules::inventory::pattern_re(&deny) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("spira-lint: spira/inventory-deny: {e}");
                return ExitCode::from(3);
            }
        };
        for hit in spira_lint::rules::inventory::scan(&content, &pat) {
            println!("{hit}");
        }
        return ExitCode::SUCCESS;
    }
    let mut rules = all_rules();
    let sources = spira_lint::all_rules_with_source();
    if let Some(o) = &only {
        rules.retain(|r| r.name() == o);
        if rules.is_empty() {
            let names: Vec<&str> = all_rules().iter().map(|r| r.name()).collect();
            eprintln!(
                "spira-lint: no rule named {o} (rules: {})",
                names.join(", ")
            );
            return ExitCode::from(2);
        }
    }
    let Some(root) = root.or_else(default_root) else {
        eprintln!("spira-lint: not inside a git work tree and no --root given");
        return ExitCode::from(3);
    };
    let tree = match Tree::from_git(&root) {
        Ok(t) => t
            .with_base(base)
            .with_deny_list(deny_list)
            .with_gate_bead(gate_bead),
        Err(e) => {
            eprintln!("spira-lint: {e}");
            return ExitCode::from(4);
        }
    };
    if emit_allow {
        #[allow(clippy::type_complexity)]
        let (name, render): (&str, fn(&Tree) -> Result<String, spira_lint::LintError>) = match only
            .as_deref()
        {
            Some("call-deadline") => (
                "call-deadline",
                spira_lint::rules::call_deadline::render_allow,
            ),
            Some("bdq-read") => ("bdq-read", spira_lint::rules::bdq_read::render_allow),
            Some("store-door") => ("store-door", spira_lint::rules::store_door::render_allow),
            Some("hash-iter-output") => (
                "hash-iter-output",
                spira_lint::rules::hash_iter_output::render_allow,
            ),
            Some("config-key-registered") => (
                "config-key-registered",
                spira_lint::rules::config_key_registered::render_allow,
            ),
            Some("state-from-text") => (
                "state-from-text",
                spira_lint::rules::state_from_text::render_allow,
            ),
            Some("loop-blocking") => {
                print!(
                    "{}",
                    spira_lint::rules::loop_blocking::render_allow(&tree, "sp-<bead>")
                );
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("spira-lint: --emit-allow is only supported with --only call-deadline, hash-iter-output, config-key-registered, state-from-text, bdq-read, store-door or loop-blocking\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        return match render(&tree) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{name}: error: {e}");
                ExitCode::from(3)
            }
        };
    }
    let scope = if diff {
        match spira_lint::DiffScope::from_tree(&tree) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("spira-lint: {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        None
    };
    let (mut findings, mut refused) = (0usize, false);
    let mut controls = Vec::new();
    let mut records = String::new();
    let results = run(&tree, &rules);
    for r in &results {
        if let Some((n, unit)) = &r.checked {
            controls.push(format!("fence: {} checked {n} {unit}", r.rule));
        }
        if let Some(text) = &r.report {
            print!("{text}");
        }
        match &r.outcome {
            Ok(v) => {
                let mut v = v.clone();
                v.retain(|f| spira_lint::kept_by(scope.as_ref(), r.rule, f));
                if v.is_empty() {
                    continue;
                }
                findings += v.len();
                for f in &v {
                    println!("{f}");
                    if findings_file.is_some() {
                        let rule = sources.iter().find(|(_, x)| x.name() == f.rule);
                        records.push_str(
                            &spira_lint::record::FindingRecord::of(
                                f,
                                rule.map(|(s, x)| (*s, x.allow_file())),
                            )
                            .to_line(),
                        );
                        records.push('\n');
                    }
                }
                if !r.hint.is_empty() {
                    eprintln!("{}: {}", r.rule, r.hint);
                }
            }
            Err(e) => {
                refused = true;
                eprintln!("{}: error: {e}", r.rule);
            }
        }
    }
    if let Some(file) = &findings_file {
        if let Err(e) = std::fs::write(file, &records) {
            eprintln!("spira-lint: {}: {e}", file.display());
            return ExitCode::from(3);
        }
    }
    if refused || findings > 0 {
        if let Some(line) = spira_lint::red_summary_of(&results, scope.as_ref()) {
            eprintln!("{line}");
        }
    }
    if refused {
        return ExitCode::from(3);
    }
    if findings > 0 {
        return ExitCode::from(1);
    }
    eprintln!(
        "spira-lint: clean — {} rule(s) over {} file(s)",
        rules.len(),
        tree.entries.len()
    );
    for c in &controls {
        eprintln!("{c}");
    }
    let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
    eprintln!(
        "fence: spira-lint checked {} files ({} rules: {})",
        tree.entries.len(),
        rules.len(),
        names.join(", ")
    );
    ExitCode::SUCCESS
}
