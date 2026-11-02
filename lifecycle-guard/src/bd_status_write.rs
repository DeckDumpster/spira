//! bd's status for a bead the control plane owns has one writer: reconcile-content
//! (`spira-lc/src/reconcile_content.rs`), which writes it from the lifecycle record or the ask.
//! The `bd-status-read` rule's mirror: inside spira-lc, a call of the `Bd` port's status writers
//! (`bd.close(…)`, `bd.reopen(…)`, `Bd::close(…)`) anywhere else is a finding, so a verb that ends or hands back
//! a bead writes the control plane and leaves bd to reconcile-content. `spira-lc/src/bd.rs` is
//! the port's implementation; other crates reach bd's status only through spira-lc's verbs
//! (`bd-close-rust`, `bd-reopen-rust`).

use crate::bd_status::{is_test_file, restore_short_literals};
use crate::finding::{Class, Finding};
use crate::landstate::mask_rust;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The one file allowed to write bd's status through the port.
pub const WRITER: &str = "spira-lc/src/reconcile_content.rs";

fn status_write() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:\b(?:bd|LiveBd)\s*\.|\bBd\s*::)\s*(close|reopen)\s*\(").unwrap())
}

pub(crate) fn in_scope(rel: &str) -> bool {
    rel.starts_with("spira-lc/src/") && rel != WRITER && rel != "spira-lc/src/bd.rs"
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
        if let Some(c) = status_write().captures(n) {
            out.push(Finding {
                class: Class::BdStatusWrite,
                file: rel.to_string(),
                line: idx + 1,
                function: None,
                callee: Some(c[1].to_string()),
                detail: format!("a bd status write outside reconcile-content; write the control plane and let {WRITER} make bd agree"),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(rel: &str, src: &str) -> Vec<usize> {
        if is_test_file(rel) || !in_scope(rel) {
            return Vec::new();
        }
        scan_text(rel, src).into_iter().map(|f| f.line).collect()
    }

    #[test]
    fn a_port_status_write_outside_reconcile_content_is_found_and_its_writer_and_tests_are_not() {
        let src = "fn a() { bd.close(&id, &reason) }\nfn b() { let _ = bd . reopen(id); }\nfn e() { crate::callers::Bd::close(&mut bd::LiveBd, id, r) }\nfn c() { bd.closed(&ids) }\nfn d() { // bd.close(x)\n}\n#[cfg(test)]\nmod tests {\n    fn t() { bd.close(\"sp-1\", \"r\"); }\n}\n";
        assert_eq!(found("spira-lc/src/callers.rs", src), vec![1, 2, 3]);
        assert!(found(WRITER, src).is_empty());
        assert!(found("spira-lc/src/bd.rs", src).is_empty());
        assert!(found("spira-lc/src/callers/tests.rs", src).is_empty());
        assert!(found("heimdall/src/alerts.rs", src).is_empty(), "outside spira-lc, the bd-close-rust rule judges");
    }
}
