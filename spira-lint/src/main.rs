//! spira-lint [--root <dir>] [--only <rule>] [--base <rev>]
//!
//! Exit: 0 clean, 1 any finding, 2 usage, 3 a rule refused to report clean.
//!
//! `--base` defaults to `SPIRA_GATE_BASE` (the gate sets it). On a clean run the positive
//! controls go to stderr: one `fence: <rule> checked <n> <unit>` per rule that reports one,
//! then `fence: spira-lint checked <n> files (<k> rules: …)` (sp-ufbkh).

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use spira_lint::{all_rules, run, Tree};

const USAGE: &str = "usage: spira-lint [--root <dir>] [--only <rule>] [--base <rev>]";

fn default_root() -> Option<PathBuf> {
    let out = Command::new("git").args(["rev-parse", "--show-toplevel"]).output().ok()?;
    out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn main() -> ExitCode {
    let mut root: Option<PathBuf> = None;
    let mut only: Option<String> = None;
    let mut base: Option<String> = std::env::var("SPIRA_GATE_BASE").ok();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => root = args.next().map(PathBuf::from),
            "--only" => only = args.next(),
            "--base" => base = args.next(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("spira-lint: unknown argument {a}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let mut rules = all_rules();
    if let Some(o) = &only {
        rules.retain(|r| r.name() == o);
        if rules.is_empty() {
            let names: Vec<&str> = all_rules().iter().map(|r| r.name()).collect();
            eprintln!("spira-lint: no rule named {o} (rules: {})", names.join(", "));
            return ExitCode::from(2);
        }
    }
    let Some(root) = root.or_else(default_root) else {
        eprintln!("spira-lint: not inside a git work tree and no --root given");
        return ExitCode::from(3);
    };
    let tree = match Tree::from_git(&root) {
        Ok(t) => t.with_base(base),
        Err(e) => {
            eprintln!("spira-lint: {e}");
            return ExitCode::from(3);
        }
    };
    let (mut findings, mut refused) = (0usize, false);
    let mut controls = Vec::new();
    for r in run(&tree, &rules) {
        if let Some((n, unit)) = &r.checked {
            controls.push(format!("fence: {} checked {n} {unit}", r.rule));
        }
        match r.outcome {
            Ok(v) if v.is_empty() => {}
            Ok(v) => {
                findings += v.len();
                for f in &v {
                    println!("{f}");
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
    if refused {
        return ExitCode::from(3);
    }
    if findings > 0 {
        return ExitCode::from(1);
    }
    eprintln!("spira-lint: clean — {} rule(s) over {} file(s)", rules.len(), tree.entries.len());
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
