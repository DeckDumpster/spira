//! The Rust half of this bead's landstate barrier: a call to `land_mark`/`landed`/`landed_sha`
//! (however module-qualified) or an `fs` read of a path naming the landstate ledger, outside
//! the allow-listed lifecycle crate and spira-lc. Shell gets the same two checks in
//! `shell.rs`, over its own tree-sitter parse; Rust has no grammar in this crate, so this is a
//! line-oriented textual pass, the same shape already used here for credential/retired-label
//! tokens.

use crate::finding::{Class, Finding};
use crate::rules::landstate_path_allowed;
use std::path::{Path, PathBuf};

// land_state added (sp-cnnt6, "wave 4.16"): the read side moved from a lib.sh function no
// lint named into a Rust one (`landing_pass::landstate::land_state`, `aeon::conf::Conf::
// land_state`) that is just as much a direct reach into the ledger as the write.
const LANDSTATE_CALL_NAMES: &[&str] = &["land_mark", "land_state", "landed", "landed_sha"];
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
        let code = mask_rust(&text);
        for (idx, (line, code_line)) in text.lines().zip(code.lines()).enumerate() {
            let line_no = idx + 1;
            if code_line.trim().is_empty() {
                continue;
            }
            for name in LANDSTATE_CALL_NAMES {
                if is_call(code_line, name) {
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
            // The path half reads string literals (the ledger's directory is named in one) but
            // never a comment: `line` with its comments masked, its literals kept.
            if is_landstate_fs_read(&strip_comment_tail(line, code_line)) {
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

/// `text` with every comment and the contents of every string literal blanked to spaces,
/// line structure kept, so a call is never matched inside prose: `"never landed (gate-red)"`
/// or `// landed() used to ...` is not a call. Char literals and lifetimes are left alone
/// (neither can hold a call). Raw strings (`r"…"`, `r#"…"#`) and nested block comments are
/// handled; a quote inside a string is honoured through its escape.
pub fn mask_rust(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let blank = |c: u8| if c == b'\n' { b'\n' } else { b' ' };
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(b[i]));
                    i += 1;
                }
            }
            continue;
        }
        // Raw string: r"…" / r#"…"# / br"…", not part of a longer identifier.
        if c == b'r' && (i == 0 || !is_word_byte(b[i - 1]) || (b[i - 1] == b'b' && (i < 2 || !is_word_byte(b[i - 2])))) {
            let mut j = i + 1;
            while b.get(j) == Some(&b'#') {
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                let hashes = j - i - 1;
                out.extend_from_slice(&b[i..=j]);
                i = j + 1;
                while i < b.len() {
                    if b[i] == b'"' && b.len() >= i + 1 + hashes && b[i + 1..i + 1 + hashes].iter().all(|&h| h == b'#') {
                        out.extend_from_slice(&b[i..i + 1 + hashes]);
                        i += 1 + hashes;
                        break;
                    }
                    out.push(blank(b[i]));
                    i += 1;
                }
                continue;
            }
        }
        if c == b'"' {
            out.push(b'"');
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' && i + 1 < b.len() {
                    out.push(b' ');
                    out.push(blank(b[i + 1]));
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    out.push(b'"');
                    i += 1;
                    break;
                }
                out.push(blank(b[i]));
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `line` up to where its masked twin `code_line` starts a `//` comment's blanking — i.e.
/// the original text with a trailing comment cut off, string literals intact.
fn strip_comment_tail(line: &str, code_line: &str) -> String {
    // A position where the original has `//` but the masked line has spaces is a comment.
    let lb = line.as_bytes();
    let cb = code_line.as_bytes();
    let mut i = 0;
    while i + 1 < lb.len() && i + 1 < cb.len() {
        if lb[i] == b'/' && lb[i + 1] == b'/' && cb[i] == b' ' && cb[i + 1] == b' ' {
            // Inside a string literal the masked line is also blank; a string's `//` is only
            // a comment if no quote in the masked prefix is left open.
            let quotes = cb[..i].iter().filter(|&&q| q == b'"').count();
            if quotes % 2 == 0 {
                return line[..i].to_string();
            }
        }
        i += 1;
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_blanks_comments_and_string_contents_and_keeps_lines() {
        let src = "let a = landed(x); // landed(y)\nlet s = \"never landed (gate-red)\";\n/* land_mark(z)\n */ f(r#\"landed(\"q\")\"#);\n";
        let m = mask_rust(src);
        assert_eq!(m.lines().count(), src.lines().count());
        let l: Vec<&str> = m.lines().collect();
        assert!(is_call(l[0], "landed"), "{m}");
        assert!(!l[0][15..].contains("landed"), "{m}");
        assert!(!is_call(l[1], "landed"), "{m}");
        assert!(!is_call(l[2], "land_mark"), "{m}");
        assert!(!is_call(l[3], "landed"), "{m}");
    }

    #[test]
    fn a_string_does_not_make_a_call_and_a_comment_does_not_make_a_read() {
        let tmp = testkit::TempDir::new("lg-mask");
        let dir = tmp.path().to_path_buf();
        let f = dir.join("m.rs");
        std::fs::write(
            &f,
            "fn a() { eprintln!(\"never landed ({c})\"); }\n\
             fn b() { let _ = std::fs::read_to_string(p); } // the old landstate read\n\
             fn c() { let _ = std::fs::read_to_string(run.join(\"landstate\")); }\n\
             fn d() { landing_pass::landstate::land_state(&run, id); }\n",
        )
        .unwrap();
        let found = scan_rust(&[f], &dir);
        let got: Vec<(usize, &str)> = found.iter().map(|f| (f.line, f.class.as_str())).collect();
        assert_eq!(got, vec![(3, "landstate-path"), (4, "landstate-call")]);
    }
}
