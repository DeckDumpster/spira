//! The Rust form of a bd lifecycle write (sp-3fue0j): a `close` handed to bd as an argument
//! vector — `.args(["close", id, …])`, `bdq(seam, &["close", …])`, `&["-C", db, "close", …]`,
//! `.arg("close")`. The shell half is `direct-write` (shell.rs), which parses a `bd close`
//! command line; a Rust caller never writes one, so before this rule every Rust close walked
//! past the gate: on 2026-10-06 ten production paths closed beads this way and never told
//! the lifecycle machine, leaving 237 closed beads READY on their rows.
//!
//! A bead's close is the machine's: `spira-lc close <id> --reason-file -` records the terminal
//! event (DROPPED, or SUPERSEDED when the reason names a successor) and then closes the store
//! — `spira-lc/src/bd.rs` is that door, and `spira-lc/src/content.rs` the cockpit's allowlisted
//! pass-through to it (a `close` only with `--reason --force`); the only files in scope that
//! may name a bd `close`. `spira-config/src/lifecycle_row.rs` (`close`) is the one client that
//! hands spira-lc one.
//!
//! Line-oriented over the masked source (`landstate::mask_rust`) with short literals restored,
//! the same shape as `bd_status.rs`. A line that also names `"pr"`/`"issue"`/`"run"` is gh's
//! close, not bd's. Test code is out of scope (a `tests/` directory, `tests.rs`, or what
//! follows a `#[cfg(test)] mod`).

use crate::bd_status::{is_test_file, restore_short_literals};
use crate::finding::{Class, Finding};
use crate::landstate::mask_rust;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn close_arg() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // "close" as an element of an argument vector, or the sole argument of `.arg(`: an
        // opening `[`/`(`, then any run of earlier elements, then the literal.
        Regex::new(r#"(?:\[|\.args?\()\s*(?:[^\[\]();]*,\s*)?"close"(?:\.into\(\)|\.to_string\(\))?\s*[,\])]"#).unwrap()
    })
}

pub(crate) fn gh_close() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""(?:pr|issue|run)""#).unwrap())
}

/// The machine, this analyser, and the doors: `bd.rs`, and `content.rs`, the cockpit's
/// allowlisted passthrough, which names `close` only as a match pattern for its own check.
pub(crate) fn in_scope(rel: &str) -> bool {
    !(rel.starts_with("lifecycle/")
        || rel.starts_with("lifecycle-guard/")
        || rel == "spira-lc/src/bd.rs"
        || rel == "spira-lc/src/content.rs"
        || rel == "spira-config/src/lifecycle_row.rs"
        // Acceptance seeds a PREDECESSOR install as it was before an upgrade — an aged store
        // whose closed bead is closed the way that release closed it. It is a fixture for a
        // scratch install, never this box's store.
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

/// A match arm whose pattern is a slice (`["close", id, ..] => …`) matches a verb; it hands bd nothing.
fn is_slice_pattern_arm(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('[') && t.contains("=>")
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
        if is_slice_pattern_arm(n) {
            continue;
        }
        if close_arg().is_match(n) && !gh_close().is_match(n) {
            out.push(Finding {
                class: Class::BdCloseRust,
                file: rel.to_string(),
                line: idx + 1,
                function: None,
                callee: Some("close".into()),
                detail: "bd close from Rust bypasses the lifecycle machine; close through `spira-lc close <id> --reason-file -`".into(),
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
    fn every_argument_vector_form_of_a_bd_close_is_found() {
        let src = r#"fn a() { Command::new("bd").args(["close", id, "--force", "--reason", reason]); }
fn b(s: &Seam) { seam::bdq(s, &["close", id, "--reason", reason]); }
fn c(db: &str) { bd(&["-C", db, "close", id, "--reason", reason, "--force"]); }
fn d() { Command::new("bd").arg("close").arg(id); }
fn e() { self.run_stdin(&["close", id, "--reason-file", "-"], reason) }
fn f() { Command::new("spira").args(["bdq", "close", id, "--reason-file", "-"]); }
fn g() { let v = vec!["close".to_string(), id.clone()]; }
"#;
        assert_eq!(lines("x/src/a.rs", src), vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn gh_closes_prose_the_door_and_tests_are_not_findings() {
        let src = r#"fn a() { gh.call(Some(repo), &["pr", "close", n]); }
fn b() { vec!["issue".into(), "close".into(), n.into()] }
fn c() { eprintln!("never run bd close by hand"); }
fn d() { fs::write(spool.join("close"), ""); }
fn h(a: &[&str]) -> R { match a {
    ["close", id, rest @ ..] if ok(rest) => Ok(()),
    _ => Err(()) } }
fn e(v: View) -> &'static str { match v { View::Decisions => "close", _ => "" } }
#[cfg(test)]
mod tests {
    fn t() { bd(&["close", "sp-1"]); }
}
"#;
        assert!(lines("x/src/a.rs", src).is_empty(), "{:?}", scan_text("x/src/a.rs", src));
        let door = r#"fn close(&mut self, id: &str, r: &str) { run(&["close", id, "--reason", r]) }"#;
        assert!(scan_rust_rel("spira-lc/src/bd.rs", door).is_empty());
        let pass = r#"fn check(a: &[&str]) { match a { ["close", id, rest @ ..] => {} _ => {} } }"#;
        assert_eq!(lines("spira-lc/src/content.rs", pass), vec![1]);
        assert!(scan_rust_rel("spira-lc/src/content.rs", pass).is_empty());
        assert_eq!(scan_rust_rel("spira-lc/src/callers.rs", door), vec![1]);
        assert!(scan_rust_rel("spira-lc/src/content.rs", door).is_empty());
    }

    fn scan_rust_rel(rel: &str, src: &str) -> Vec<usize> {
        if is_test_file(rel) || !in_scope(rel) {
            return Vec::new();
        }
        lines(rel, src)
    }
}
