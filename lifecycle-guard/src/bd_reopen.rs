//! The Rust form of a bd reopen (sp-swh8b8): a `reopen` handed to bd as an argument vector,
//! or the claim-clearing `assign <id> ""`. A bead's return to rework is the machine's —
//! `spira-lc reopen <id> [cause] [actor]` records the event the row's own state implies — so
//! a reopen that only touches bd leaves the row where it was and the bead invisible to every
//! builder's ready set.
//!
//! The shell half is `direct-write` (shell.rs). Same shape as `bd_close.rs`: line-oriented over
//! the masked source with short literals restored, a gh line excluded, test code and the
//! machine's own door out of scope.

use crate::bd_close::{gh_close, in_scope};
use crate::bd_status::{is_test_file, restore_short_literals};
use crate::finding::{Class, Finding};
use crate::landstate::mask_rust;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn reopen_arg() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?:\[|\.args?\()\s*(?:[^\[\]();]*,\s*)?"reopen"(?:\.into\(\)|\.to_string\(\))?\s*[,\])]"#).unwrap())
}

fn clearing_assign() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""assign"\s*,[^;]*,\s*""\s*[,\])]"#).unwrap())
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
        let callee = if reopen_arg().is_match(n) && !gh_close().is_match(n) {
            "reopen"
        } else if clearing_assign().is_match(n) {
            "assign"
        } else {
            continue;
        };
        out.push(Finding {
            class: Class::BdReopenRust,
            file: rel.to_string(),
            line: idx + 1,
            function: None,
            callee: Some(callee.into()),
            detail: "a bd reopen from Rust bypasses the lifecycle machine; reopen through `spira-lc reopen <id> [cause] [actor]`".into(),
        });
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
    fn every_argument_vector_form_of_a_bd_reopen_is_found() {
        let src = r#"fn a() { self.bd_ok(&["reopen", id], None); }
fn b() { Command::new("bd").args(["reopen", id, "--reason", r]); }
fn c(db: &str) { run(db, &["reopen", id]); }
fn d() { self.bd(&["assign", id, ""]) }
fn e() { Command::new("bd").arg("reopen").arg(id); }
"#;
        assert_eq!(lines("x/src/a.rs", src), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn gh_prose_a_claim_assign_the_door_and_tests_are_not_findings() {
        let src = r#"fn a() { gh.call(Some(repo), &["issue", "reopen", n]); }
fn b() { eprintln!("never run bd reopen by hand"); }
fn c() { match verb { "reopen" => 1, _ => 0 } }
fn d() { bd(&["assign", id, actor]); }
fn e() { Some(("reopen", "rebase-conflict")) }
#[cfg(test)]
mod tests {
    fn t() { bd(&["reopen", "sp-1"]); }
}
"#;
        assert!(lines("x/src/a.rs", src).is_empty(), "{:?}", scan_text("x/src/a.rs", src));
        let door = r#"fn r(id: &str) { run(&["reopen", id, cause]) }"#;
        assert!(!in_scope("spira-config/src/lifecycle_row.rs"));
        assert_eq!(lines("spira-claim/src/store.rs", door), vec![1]);
    }
}
