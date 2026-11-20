//! Path globs for `SPIRA_CR_EXCLUDE`: `*` and `?` within a segment, `**` across segments; a
//! pattern with no `/` matches the file name alone.

pub fn matches(pattern: &str, path: &str) -> bool {
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return seg_match(&pattern.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>());
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs(&p, &s)
}

pub fn any(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| matches(p, path))
}

fn segs(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => segs(&p[1..], s) || (!s.is_empty() && segs(p, &s[1..])),
        Some(x) => {
            !s.is_empty()
                && seg_match(&x.chars().collect::<Vec<_>>(), &s[0].chars().collect::<Vec<_>>())
                && segs(&p[1..], &s[1..])
        }
    }
}

fn seg_match(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => seg_match(&p[1..], s) || (!s.is_empty() && seg_match(p, &s[1..])),
        Some('?') => !s.is_empty() && seg_match(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && seg_match(&p[1..], &s[1..]),
    }
}
