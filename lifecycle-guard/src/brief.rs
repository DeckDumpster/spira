use crate::finding::{Class, Finding};
use std::path::{Path, PathBuf};

/// Only a file under a `chamber/` directory is a persona brief; `disabled/` personas are
/// never summoned. DESIGN notes, docs and wiki pages describe bd, they do not instruct an aeon.
pub fn is_persona_path(path: &Path) -> bool {
    let comps: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    comps.iter().any(|c| c == "chamber") && !comps.iter().any(|c| c == "disabled")
}

/// Personas are `.md` briefs and `.fayth` definitions. Naming `bd` in either teaches an aeon
/// to reach for the tool bd itself was told loses lifecycle authority, so every whole-word
/// mention is a finding — this file is not the one place that gets to decide it still knows
/// better.
pub fn scan(files: &[PathBuf], root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (idx, line) in text.lines().enumerate() {
            // One finding per line is enough to point a reader at it.
            if !whole_word_matches(line, "bd").is_empty() {
                findings.push(Finding {
                    class: Class::BriefBd,
                    file: rel_path.clone(),
                    line: idx + 1,
                    function: None,
                    callee: None,
                    detail: "names bd, which no longer holds lifecycle authority".to_string(),
                });
            }
        }
    }
    findings
}

fn whole_word_matches<'a>(line: &'a str, word: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let wlen = word.len();
    let mut i = 0;
    while let Some(found) = line[i..].find(word) {
        let start = i + found;
        let end = start + wlen;
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            out.push(&line[start..end]);
        }
        i = start + 1;
        if i >= line.len() {
            break;
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
