//! The Rust form of a bd state read (design §3.4: bd holds content, spira-lc holds state):
//! `ready` or `events` handed to bd as an argument vector — `.args(["ready", …])`,
//! `&["-C", db, "events", …]`, or a vector whose first element is `"ready"` on its own line.
//! bd's `ready` set is bd's status and assignee, which no claim writes; a bead's events are
//! the old attempt ledger. Both are the lifecycle row's now (`spira-lc list` / `show`).
//!
//! The `show` half of the same cutover — a bd row's `status` decided on — is `bd-status-read`
//! (bd_status.rs). Line-oriented over the masked source, same shape as `bd_close.rs`; test
//! code is out of scope.

use crate::bd_status::{is_test_file, restore_short_literals};
use crate::finding::{Class, Finding};
use crate::landstate::mask_rust;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn read_arg() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?:\[|\.args?\()\s*(?:[^\[\]();]*,\s*)?"(ready|events)"(?:\.into\(\)|\.to_string\(\))?\s*[,\])]"#).unwrap()
    })
}

fn opens_vector() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"^\s*"(ready|events)"(?:\.into\(\)|\.to_string\(\))?,\s*$"#).unwrap())
}

fn gh_call() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""(?:pr|issue|run)""#).unwrap())
}

/// The machine, this analyser, bdq's own verb classifier, and a predecessor install's seed.
fn in_scope(rel: &str) -> bool {
    !(rel.starts_with("lifecycle/")
        || rel.starts_with("lifecycle-guard/")
        || rel == "bead/src/bdq.rs"
        || rel.starts_with("release/src/acceptance/"))
}

pub fn scan_rust(files: &[PathBuf], root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        if is_test_file(&rel) || !in_scope(&rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        findings.extend(scan_text(&rel, &text));
    }
    findings
}

pub fn scan_text(rel: &str, text: &str) -> Vec<Finding> {
    let code = mask_rust(text);
    let lines: Vec<String> = text.lines().zip(code.lines()).map(|(o, c)| restore_short_literals(o, c)).collect();
    let mut out = Vec::new();
    for (idx, n) in lines.iter().enumerate() {
        if n.trim_start().starts_with("#[cfg(test)]")
            && lines.get(idx + 1).is_some_and(|l| l.trim_start().starts_with("mod ") || l.contains(" mod "))
        {
            break;
        }
        let verb = read_arg().captures(n).map(|c| c[1].to_string()).or_else(|| {
            let next_is_flag = lines.get(idx + 1).is_some_and(|l| l.trim_start().starts_with("\"--"));
            opens_vector().captures(n).filter(|_| next_is_flag).map(|c| c[1].to_string())
        });
        if let Some(verb) = verb {
            if gh_call().is_match(n) {
                continue;
            }
            out.push(Finding {
                class: Class::BdReadRust,
                file: rel.to_string(),
                line: idx + 1,
                function: None,
                callee: Some(verb.clone()),
                detail: format!("bd {verb} read decides on bd's state; read the lifecycle row (`spira_config::lc_state`)"),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rel: &str, src: &str) -> Vec<usize> {
        scan_text(rel, src).into_iter().map(|f| f.line).collect()
    }

    #[test]
    fn every_argument_vector_form_of_a_bd_ready_or_events_read_is_found() {
        let src = r#"fn a() { Command::new("bd").args(["ready", "--limit", "0"]); }
fn b(s: &Seam) { seam::bdq(s, &["events", id]); }
fn c(db: &str) { bd(&["-C", db, "ready", "--json"]); }
fn d() { Command::new("bd").arg("events").arg(id); }
fn e() -> Vec<String> {
    vec![
        "ready".into(),
        "--limit".into(),
    ]
}
"#;
        assert_eq!(lines("x/src/a.rs", src), vec![1, 2, 3, 4, 7]);
    }

    #[test]
    fn prose_gh_the_classifier_and_tests_are_not_findings() {
        let src = r#"fn a() { gh.call(Some(repo), &["run", "ready", n]); }
fn b() { eprintln!("bd ready is gone"); }
fn c() { let ready = true; log("ready", 1); }
fn d() -> &'static [&'static str] { &["show", "ready", "list"] }
#[cfg(test)]
mod tests {
    fn t() { bd(&["ready", "--json"]); }
}
"#;
        assert_eq!(lines("x/src/a.rs", src), vec![4]);
        let door = r#"const READ_VERBS: &[&str] = &["show", "ready"];"#;
        assert!(!in_scope("bead/src/bdq.rs") && lines("bead/src/bdq.rs", door).len() == 1);
    }
}
