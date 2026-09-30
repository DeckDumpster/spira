//! `case "$path" in $pat)` matching, as every bash selector did it: `*` matches any string,
//! `/` included; `?` one character; `[...]` a bracket expression (`!` or `^` negates, `a-z`
//! ranges); `\x` a literal `x`. Anything else matches itself.

/// Does `path` match the case pattern `pat`?
pub fn case_match(pat: &str, path: &str) -> bool {
    m(pat.as_bytes(), path.as_bytes())
}

fn m(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => {
            // Collapse runs of `*`; then try every split point.
            let rest = {
                let mut r = rest;
                while let Some((b'*', t)) = r.split_first() {
                    r = t;
                }
                r
            };
            if rest.is_empty() {
                return true;
            }
            (0..=s.len()).any(|i| m(rest, &s[i..]))
        }
        Some((b'?', rest)) => !s.is_empty() && m(rest, &s[1..]),
        Some((b'[', rest)) => {
            let Some(&c) = s.first() else { return false };
            let (neg, mut i) = match rest.first() {
                Some(b'!') | Some(b'^') => (true, 1),
                _ => (false, 0),
            };
            let start = i;
            let mut hit = false;
            while i < rest.len() {
                if rest[i] == b']' && i > start {
                    return hit != neg && m(&rest[i + 1..], &s[1..]);
                }
                if i + 2 < rest.len() && rest[i + 1] == b'-' && rest[i + 2] != b']' {
                    hit |= rest[i] <= c && c <= rest[i + 2];
                    i += 3;
                } else {
                    hit |= rest[i] == c;
                    i += 1;
                }
            }
            // No closing bracket: the `[` is literal.
            c == b'[' && m(rest, &s[1..])
        }
        Some((b'\\', rest)) if !rest.is_empty() => s.first() == Some(&rest[0]) && m(&rest[1..], &s[1..]),
        Some((&c, rest)) => s.first() == Some(&c) && m(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::case_match as g;

    #[test]
    fn star_crosses_slashes_like_case_does() {
        assert!(g("spira/*.sh", "spira/lib.sh"));
        assert!(g("spira/*.sh", "spira/testlib/x.sh"));
        assert!(g("*/Cargo.toml", "gate/Cargo.toml"));
        assert!(g("*/Cargo.toml", "cockpit/panel/Cargo.toml"));
        assert!(!g("*/Cargo.toml", "Cargo.toml"));
        assert!(g("*.md", "docs/x/README.md"));
        assert!(g("gate/src/*", "gate/src/a/b.rs"));
        assert!(g("**", ""));
    }

    #[test]
    fn question_brackets_and_escapes() {
        assert!(g("a?c", "abc"));
        assert!(!g("a?c", "ac"));
        assert!(g("t[0-3].sh", "t2.sh"));
        assert!(!g("t[!0-3].sh", "t2.sh"));
        assert!(g("t[!0-3].sh", "t9.sh"));
        assert!(g("a\\*b", "a*b"));
        assert!(!g("a\\*b", "axb"));
        assert!(g("a[b", "a[b"));
    }

    #[test]
    fn literal_patterns_are_whole_matches() {
        assert!(g("Makefile", "Makefile"));
        assert!(!g("Makefile", "gate/Makefile"));
        assert!(!g("spira/lib.sh", "spira/lib.sh.bak"));
    }
}
