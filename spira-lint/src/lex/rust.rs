//! Classify every byte of a Rust source as code, comment or literal.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Code,
    Comment,
    Literal,
}

/// A source with its per-byte classification.
pub struct Classified<'a> {
    pub src: &'a [u8],
    pub class: Vec<Class>,
}

impl<'a> Classified<'a> {
    /// The source with every non-code byte blanked (newlines kept, so offsets and line
    /// numbers survive).
    pub fn code_only(&self) -> Vec<u8> {
        self.mask(|c| c == Class::Code)
    }

    /// The source with comments blanked and literals kept.
    pub fn without_comments(&self) -> Vec<u8> {
        self.mask(|c| c != Class::Comment)
    }

    fn mask(&self, keep: impl Fn(Class) -> bool) -> Vec<u8> {
        self.src
            .iter()
            .zip(&self.class)
            .map(|(&b, &c)| if keep(c) || b == b'\n' { b } else { b' ' })
            .collect()
    }

    /// 1-based line of byte offset `at`.
    pub fn line_of(&self, at: usize) -> usize {
        1 + self.src[..at.min(self.src.len())].iter().filter(|&&b| b == b'\n').count()
    }
}

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub fn classify(src: &[u8]) -> Classified<'_> {
    let n = src.len();
    let mut class = vec![Class::Code; n];
    let at = |i: usize| -> u8 { *src.get(i).unwrap_or(&0) };
    let mut i = 0;
    while i < n {
        let start = i;
        let c = src[i];
        let prev_ident = i > 0 && ident(src[i - 1]);
        if c == b'/' && at(i + 1) == b'/' {
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            class[start..i].fill(Class::Comment);
            continue;
        }
        if c == b'/' && at(i + 1) == b'*' {
            let mut depth = 0usize;
            while i < n {
                if src[i] == b'/' && at(i + 1) == b'*' {
                    depth += 1;
                    i += 2;
                } else if src[i] == b'*' && at(i + 1) == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            class[start..i.min(n)].fill(Class::Comment);
            continue;
        }
        if !prev_ident {
            // raw strings: r"…", r#"…"#, br"…", cr#"…"#
            let mut j = i;
            if matches!(at(j), b'b' | b'c') && at(j + 1) == b'r' {
                j += 1;
            }
            if at(j) == b'r' && matches!(at(j + 1), b'"' | b'#') {
                let mut k = j + 1;
                let mut hashes = 0;
                while at(k) == b'#' {
                    hashes += 1;
                    k += 1;
                }
                if at(k) == b'"' {
                    k += 1;
                    'scan: while k < n {
                        if src[k] == b'"' && (0..hashes).all(|h| at(k + 1 + h) == b'#') {
                            k += 1 + hashes;
                            break 'scan;
                        }
                        k += 1;
                    }
                    i = k.min(n);
                    class[start..i].fill(Class::Literal);
                    continue;
                }
            }
            // byte / C string prefixes: b"…", c"…", b'…'
            if matches!(c, b'b' | b'c') && matches!(at(i + 1), b'"' | b'\'') {
                i += 1;
            }
        }
        if at(i) == b'"' {
            i += 1;
            while i < n && src[i] != b'"' {
                i += if src[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(n);
            class[start..i].fill(Class::Literal);
            continue;
        }
        if at(i) == b'\'' {
            // a char literal, or a lifetime / label
            if at(i + 1) == b'\\' {
                let mut k = i + 3;
                while k < n && src[k] != b'\'' {
                    k += 1;
                }
                i = (k + 1).min(n);
                class[start..i].fill(Class::Literal);
                continue;
            }
            let len = utf8_len(at(i + 1));
            if at(i + 1) != 0 && at(i + 1 + len) == b'\'' {
                i += len + 2;
                class[start..i].fill(Class::Literal);
                continue;
            }
            i += 1;
            continue;
        }
        i = if i == start { i + 1 } else { i };
    }
    Classified { src, class }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> String {
        String::from_utf8(classify(s.as_bytes()).code_only()).unwrap()
    }

    #[test]
    fn comments_and_strings_are_not_code() {
        let c = code("let a = \"x::y\"; // z::w\n/* q::r /* nested */ s::t */ u::v");
        assert!(!c.contains("x::y") && !c.contains("z::w") && !c.contains("q::r") && !c.contains("s::t"));
        assert!(c.contains("u::v") && c.contains("let a ="));
        assert_eq!(c.matches('\n').count(), 1);
    }

    #[test]
    fn raw_strings_chars_and_lifetimes() {
        let c = code("r#\"a \" b::c\"# fn f<'a>(x: &'a str) -> char { '\\'' } br\"d::e\" b'x' g::h");
        assert!(!c.contains("b::c") && !c.contains("d::e"));
        assert!(c.contains("fn f<'a>(x: &'a str)"), "{c}");
        assert!(c.contains("g::h"));
    }

    #[test]
    fn escaped_quote_in_string() {
        let c = code("\"a \\\" toml::x\" toml::y");
        assert!(!c.contains("toml::x") && c.contains("toml::y"));
    }
}
