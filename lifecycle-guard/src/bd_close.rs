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
//! may name a bd `close`. `spira-store/src/lifecycle_row.rs` (`close`) is the one client that
//! hands spira-lc one.
//!
//! Line-oriented over the masked source (`landstate::mask_rust`) with short literals restored,
//! the same shape as `bd_status.rs`. A line that also names `"pr"`/`"issue"`/`"run"` is gh's
//! close, not bd's. Test code is out of scope (a `tests/` directory, `tests.rs`, or what
//! follows a `#[cfg(test)] mod`).
//!
//! An argument vector handed to the lifecycle machine itself is not bd's, and is not a
//! finding ([`machine_door`]): the argv of `LcClient::call_stdin` (spira-config's
//! `lc_call.rs`, the only `call_stdin` in the tree — the in-process client of lc-serve, or
//! the `spira-lc` program in direct mode), the store's content door
//! (`["content", "close", …]`, which runs bdq's write guards and writes the terminal row
//! after bd's close: `spira-lc/src/content.rs`), and a vector handed to the `spira-lc` or
//! `spira-claim` program by name (`spira-claim reopen` is `spira-lc reopen` plus its
//! bookkeeping: `spira-claim/src/reopen.rs`). Only the door's own span is passed over, so a
//! line that also hands bd a close is still a finding.

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

/// The lifecycle machine's doors a `close`/`reopen` vector may be handed to (module doc):
/// `.call_stdin(&["close"…` / `["reopen"…`, `["content", "close"…`, and
/// `"spira-lc"`/`"spira-claim"` named as the program just before the vector.
pub(crate) fn machine_door() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?:\.call_stdin\(\s*&?\s*\[\s*"(?:close|reopen)"|"spira-(?:lc|claim)"\s*,\s*&?\s*(?:vec!)?\[\s*"(?:close|reopen)"|\[\s*"content"\s*,\s*"close")"#).unwrap()
    })
}

/// `line` with every [`machine_door`] span blanked, so what remains is judged as bd's.
pub(crate) fn without_doors(line: &str) -> String {
    machine_door().replace_all(line, "").into_owned()
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
        || rel == "spira-store/src/lifecycle_row.rs"
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
        if close_arg().is_match(&without_doors(n)) && !gh_close().is_match(n) {
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

    fn lines(src: &str) -> Vec<usize> {
        scan_text("x/src/a.rs", src).into_iter().map(|f| f.line).collect()
    }

    /// A bd close is still refused in every shape; the lifecycle machine's own doors — the
    /// lc-serve client's `call_stdin`, the content door, the spira-lc program — are not bd.
    #[test]
    fn a_bd_close_is_refused_and_the_lifecycle_machine_is_not() {
        let bd = r#"fn a() { bdq(seam, &["close", id, "--reason", r]); }
fn b() { Command::new("bd").args(["-C", db, "close", id]); }
fn c() { cmd.arg("close"); }
fn d(lc: &LcClient) { lc.call_stdin(&["show", id], ""); bd(&["close", id]); }
"#;
        assert_eq!(lines(bd), vec![1, 2, 3, 4]);
        let machine = r#"fn a(lc: &LcClient) { let r = lc.call_stdin(&["close", id, "--reason-file", "-", "--actor", "sift"], &reason)?; }
fn b(e: &Effects) { let r = super::lc(e).call_stdin(&["close", j.id.as_str(), "--reason-file", "-"], &j.text); }
fn c(e: &Effects) { answer(&client(e, ms), &["content", "close", &w.id, "--reason", &w.text]) }
fn d() { run("spira-lc", &["close", id, "--reason-file", "-"]); }
"#;
        assert_eq!(lines(machine), Vec::<usize>::new(), "{:?}", scan_text("x/src/a.rs", machine));
        assert!(without_doors(r#"x.call(&["close", id])"#).contains("close"), "only call_stdin is the client's");
    }
}
