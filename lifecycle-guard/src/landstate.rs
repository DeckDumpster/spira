//! The Rust half of this bead's landstate barrier: a call to `land_mark`/`landed`/`landed_sha`
//! (however module-qualified) or an `fs` read of a path naming the landstate ledger, outside
//! the allow-listed lifecycle crate and spira-lc. Shell gets the same two checks in
//! `shell.rs`, over its own tree-sitter parse; Rust has no grammar in this crate, so this is a
//! line-oriented textual pass, the same shape already used here for credential/retired-label
//! tokens.

use crate::finding::{Class, Finding};
use crate::rules::landstate_path_allowed;
use std::path::{Path, PathBuf};

const LANDSTATE_CALL_NAMES: &[&str] = &["land_mark", "landed", "landed_sha"];
const FS_READ_TOKENS: &[&str] = &["fs::read_dir", "fs::read_to_string", "fs::read(", "file::open"];

pub fn scan_rust(files: &[PathBuf], root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in files {
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if landstate_path_allowed(&rel_path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            for name in LANDSTATE_CALL_NAMES {
                if is_call(line, name) {
                    findings.push(Finding {
                        class: Class::LandstateCall,
                        file: rel_path.clone(),
                        line: line_no,
                        function: None,
                        callee: Some((*name).to_string()),
                        detail: format!(
                            "{name} is a direct call into the landstate ledger; read it through spira-lc show/list instead"
                        ),
                    });
                }
            }
            if is_landstate_fs_read(line) {
                findings.push(Finding {
                    class: Class::LandstatePath,
                    file: rel_path.clone(),
                    line: line_no,
                    function: None,
                    callee: None,
                    detail: "fs read reaches the landstate ledger directly; read it through spira-lc show/list instead".to_string(),
                });
            }
        }
    }
    findings
}

/// True when `word` appears in `line` as a call: a whole identifier immediately followed
/// (module path prefixes like `io::` count as a boundary already) by `(`, and not the `fn`
/// keyword that introduces its own definition.
fn is_call(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    let wlen = word.len();
    let mut i = 0;
    while let Some(found) = line[i..].find(word) {
        let start = i + found;
        let end = start + wlen;
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok && line[end..].trim_start().starts_with('(') {
            let before = line[..start].trim_end();
            if !before.ends_with("fn") {
                return true;
            }
        }
        i = start + 1;
        if i >= line.len() {
            break;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_landstate_fs_read(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("landstate") && FS_READ_TOKENS.iter().any(|t| lower.contains(t))
}
