//! Identifier validation (DESIGN.md §5). An identifier is the only kind of value queue ever
//! puts in a subprocess's argv, and only after it passes here — so argv is bounded by
//! construction, never by how large the backlog has grown (law-payloads-go-on-stdin).

/// The longest identifier queue will pass in argv. Bead ids, refs, SHAs, repo names and
/// paths are all far shorter; anything longer is not an identifier.
pub const MAX_IDENT: usize = 255;

/// The longest free-text value queue will put in argv, for the two interfaces that accept
/// nothing else (forge.sh pr-comment, spira-lc --reason). DESIGN.md §5.
pub const MAX_ARGV_TEXT: usize = 200;

/// `Ok(s)` when `s` is a plain identifier: non-empty, at most [`MAX_IDENT`] bytes, not
/// starting with `-` (so it can never be read as an option), and made only of
/// `[A-Za-z0-9._/:@+~^-]` (refs and `id:tip` pairs; braces, so `rev^{tree}`, are not).
pub fn check(kind: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{kind} is empty"));
    }
    if s.len() > MAX_IDENT {
        return Err(format!("{kind} is longer than {MAX_IDENT} bytes"));
    }
    if s.starts_with('-') {
        return Err(format!("{kind} '{s}' starts with '-'"));
    }
    if let Some(c) = s.chars().find(|c| !(c.is_ascii_alphanumeric() || "._/:@+~^-".contains(*c))) {
        return Err(format!("{kind} '{s}' contains {c:?}"));
    }
    Ok(())
}

/// A filesystem path identifier: same rule, but spaces are not allowed either and the
/// length cap is PATH_MAX-ish rather than a ref's.
pub fn check_path(kind: &str, s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 4096 {
        return Err(format!("{kind} is empty or too long"));
    }
    if s.contains('\0') || s.contains('\n') {
        return Err(format!("{kind} contains a NUL or newline"));
    }
    Ok(())
}

/// Flatten `text` to one line and cut it to [`MAX_ARGV_TEXT`] bytes on a char boundary —
/// the bounded form of a reason for an argv-only interface.
pub fn bounded_text(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.len() <= MAX_ARGV_TEXT {
        return one;
    }
    let mut end = MAX_ARGV_TEXT;
    while !one.is_char_boundary(end) {
        end -= 1;
    }
    one[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_identifiers_pass() {
        for s in ["sp-abc12", "sp-s088v.5", "spira/sp-x", "origin/main", "local/main", "refs/archive/rounds/12", "abc123:def456", "HEAD^"] {
            assert!(check("x", s).is_ok(), "{s}");
        }
    }

    #[test]
    fn option_like_space_and_long_values_are_refused() {
        assert!(check("x", "--force").is_err());
        assert!(check("x", "a b").is_err());
        assert!(check("x", "a;b").is_err());
        assert!(check("x", "").is_err());
        assert!(check("x", &"a".repeat(MAX_IDENT + 1)).is_err());
    }

    #[test]
    fn bounded_text_is_one_line_and_capped() {
        assert_eq!(bounded_text("a\nb   c"), "a b c");
        let long = "é".repeat(300);
        let b = bounded_text(&long);
        assert!(b.len() <= MAX_ARGV_TEXT);
        assert!(b.is_char_boundary(b.len()));
    }
}
