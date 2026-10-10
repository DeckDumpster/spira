//! A hold kept as a bd label (sp-psztcc): poison and ask are holds on the bead's lifecycle row,
//! which the claim already refuses. A label standing in for one goes stale the moment the hold
//! is withdrawn — on 2026-10-06 twenty claimable beads, the head of a release's critical path
//! among them, sat invisible to every builder because each still carried `needs-ryan` after its
//! ask was withdrawn. Two shapes are a finding:
//!
//! 1. **predicate** — a fayth's `FAYTH_EXCLUDE_LABELS` naming `spira-poison` or
//!    `$SPIRA_ASK_LABEL`: claimability read off a label instead of the row.
//! 2. **write** — code adding either label: Rust handing bd `"label", "add", …, "spira-poison"`
//!    (or the ask label through a variable named `ask`), shell `label add … spira-poison` /
//!    `"$SPIRA_ASK_LABEL"`.
//!
//! The ask label is still how an ASK bead (the question itself, a non-work bead) is marked;
//! filing one goes through `bead.sh`/`mail`, which name it in a `create`, never a `label add`.
//! Test code is out of scope, as in `bd_status.rs`.

use crate::bd_status::is_test_file;
use crate::finding::{Class, Finding};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Res {
    predicate: Regex,
    rust_add: Regex,
    sh_add: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        predicate: Regex::new(r#"^\s*FAYTH_EXCLUDE_LABELS=.*(spira-poison|\$\{?SPIRA_ASK_LABEL)"#).unwrap(),
        rust_add: Regex::new(r#""label"\s*,\s*"add"\s*,[^\]]*(?:"spira-poison"|&\s*ask\b|&\s*self\.cfg\.ask\b|ask_label\(\))"#).unwrap(),
        sh_add: Regex::new(r#"\blabel\s+add\s+\S+\s+["']?(?:spira-poison|\$\{?SPIRA_ASK_LABEL)"#).unwrap(),
    })
}

fn finding(rel: &str, line: usize, shape: &str, detail: &str) -> Finding {
    Finding {
        class: Class::HoldLabel,
        file: rel.to_string(),
        line,
        function: None,
        callee: Some(shape.to_string()),
        detail: detail.to_string(),
    }
}

pub fn scan_text(rel: &str, text: &str) -> Vec<Finding> {
    let r = res();
    let mut out = Vec::new();
    let is_fayth = rel.ends_with(".fayth");
    let is_rust = rel.ends_with(".rs");
    for (idx, line) in text.lines().enumerate() {
        if is_rust && line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let code = line.trim_start();
        if code.starts_with('#') && !is_rust || code.starts_with("//") {
            continue;
        }
        if is_fayth && r.predicate.is_match(line) {
            out.push(finding(rel, idx + 1, "predicate", "a fayth excludes by a hold label; poison and ask are lifecycle holds the claim already refuses — drop the label from FAYTH_EXCLUDE_LABELS"));
        } else if is_rust && r.rust_add.is_match(line) {
            out.push(finding(rel, idx + 1, "write", "a hold written as a bd label; hold it on the row (spira-lc hold <id> poison|ask) and write no label"));
        } else if !is_rust && !is_fayth && r.sh_add.is_match(line) {
            out.push(finding(rel, idx + 1, "write", "a hold written as a bd label; hold it on the row (spira-lc hold <id> poison|ask) and write no label"));
        }
    }
    out
}

pub fn scan(files: &[PathBuf], root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        if is_test_file(&rel) || name.starts_with("test-") || name == "testlib.sh" || rel.starts_with("lifecycle-guard/") || rel.contains("/disabled/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        findings.extend(scan_text(&rel, &text));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rel: &str, src: &str) -> Vec<usize> {
        scan_text(rel, src).into_iter().map(|f| f.line).collect()
    }

    #[test]
    fn every_hold_label_shape_is_found() {
        assert_eq!(lines("spira/chamber/builder.fayth", "FAYTH_EXCLUDE_LABELS=\"spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL\"\n"), vec![1]);
        assert_eq!(lines("spira/chamber/ops.fayth", "FAYTH_EXCLUDE_LABELS=\"$SPIRA_ASK_LABEL\"\n"), vec![1]);
        let rs = "fn a() { let _ = self.d.bd.bd(&s(&[\"label\", \"add\", &id, \"spira-poison\"])); }\nfn b() { let _ = self.d.bd.bd(&s(&[\"label\", \"add\", &id, &ask])); }\nfn c() { self.bd().quiet(self.h, &[\"label\", \"add\", &c.id, &self.cfg.ask], None); }\n";
        assert_eq!(lines("aeon/src/run.rs", rs), vec![1, 2, 3]);
        let sh = "park() {\n    bdq label add \"$id\" \"$SPIRA_ASK_LABEL\" >/dev/null\n    bdq label add \"$id\" spira-poison\n}\n";
        assert_eq!(lines("spira/lib.sh", sh), vec![2, 3]);
    }

    #[test]
    fn the_non_hold_labels_comments_and_tests_are_not_findings() {
        assert!(lines("spira/chamber/builder.fayth", "FAYTH_EXCLUDE_LABELS=\"$SPIRA_CI_LABEL,qa-proposed\"\n# FAYTH_EXCLUDE_LABELS=\"spira-poison\"\n").is_empty());
        let rs = "fn a() { bd(&[\"label\", \"add\", &id, \"overseer\"]); }\n// bd(&[\"label\", \"add\", &id, \"spira-poison\"])\n#[cfg(test)]\nmod tests {\n    fn t() { bd(&[\"label\", \"add\", \"sp-1\", \"spira-poison\"]); }\n}\n";
        assert!(lines("aeon/src/run.rs", rs).is_empty());
        assert!(lines("spira/lib.sh", "bdq label add \"$id\" overseer\n# bdq label add \"$id\" spira-poison\n").is_empty());
    }
}
