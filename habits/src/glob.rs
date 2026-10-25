/// `**`, `*`, `?` and `{a,b}` over `/`-separated relative paths.
pub fn matches(pattern: &str, path: &str) -> bool {
    expand(pattern).iter().any(|p| walk(p.as_bytes(), path.as_bytes()))
}

fn expand(p: &str) -> Vec<String> {
    let Some(open) = p.find('{') else { return vec![p.to_string()] };
    let Some(close) = p[open..].find('}').map(|i| i + open) else { return vec![p.to_string()] };
    let (head, tail) = (&p[..open], &p[close + 1..]);
    p[open + 1..close].split(',').flat_map(|alt| expand(&format!("{head}{alt}{tail}"))).collect()
}

fn walk(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.starts_with(b"**/") => walk(&p[3..], s) || (!s.is_empty() && skip_segment(s).is_some_and(|r| walk(p, r))),
        Some(b'*') if p.starts_with(b"**") => (0..=s.len()).any(|i| walk(&p[2..], &s[i..])),
        Some(b'*') => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| walk(&p[1..], &s[i..])),
        Some(b'?') => s.first().is_some_and(|c| *c != b'/') && walk(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && walk(&p[1..], &s[1..]),
    }
}

fn skip_segment(s: &[u8]) -> Option<&[u8]> {
    s.iter().position(|c| *c == b'/').map(|i| &s[i + 1..])
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn globs() {
        assert!(matches("**/*.rs", "a.rs"));
        assert!(matches("**/*.rs", "a/b/c.rs"));
        assert!(!matches("**/*.rs", "a/b/c.sh"));
        assert!(matches("frontend/src/**/*.{ts,svelte}", "frontend/src/x/y.svelte"));
        assert!(matches("frontend/src/**/*.{ts,svelte}", "frontend/src/y.ts"));
        assert!(!matches("frontend/src/**/*.ts", "frontend/other/y.ts"));
        assert!(!matches("*.rs", "a/b.rs"));
        assert!(matches("spira/*.sh", "spira/x.sh"));
    }
}
