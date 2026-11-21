//! A line-level reading of Rust source: enough to find top-level items, a file's public
//! surface and the identifiers it names, without parsing the language.

use std::collections::HashSet;

/// `text` with comments, string and char literals blanked to spaces, line structure kept.
pub fn code_lines(text: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum S {
        Code,
        Line,
        Block(u32),
        Str,
        Raw(usize),
    }
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut st = S::Code;
    let mut i = 0;
    let blank = |ch: char| if ch == '\n' { '\n' } else { ' ' };
    while i < c.len() {
        let ch = c[i];
        match st {
            S::Code => {
                if ch == '/' && c.get(i + 1) == Some(&'/') {
                    st = S::Line;
                    out.push_str("  ");
                    i += 2;
                } else if ch == '/' && c.get(i + 1) == Some(&'*') {
                    st = S::Block(1);
                    out.push_str("  ");
                    i += 2;
                } else if ch == '"' {
                    st = S::Str;
                    out.push(' ');
                    i += 1;
                } else if ch == 'r' && !prev_is_ident(&c, i) && raw_open(&c, i).is_some() {
                    let (hashes, len) = raw_open(&c, i).unwrap_or((0, 1));
                    st = S::Raw(hashes);
                    out.extend(std::iter::repeat_n(' ', len));
                    i += len;
                } else if ch == '\'' {
                    let end = if c.get(i + 1) == Some(&'\\') {
                        (i + 2..c.len().min(i + 12)).find(|j| c[*j] == '\'')
                    } else if c.get(i + 2) == Some(&'\'') {
                        Some(i + 2)
                    } else {
                        None
                    };
                    match end {
                        Some(e) => {
                            out.extend(std::iter::repeat_n(' ', e - i + 1));
                            i = e + 1;
                        }
                        None => {
                            out.push(ch);
                            i += 1;
                        }
                    }
                } else {
                    out.push(ch);
                    i += 1;
                }
            }
            S::Line => {
                if ch == '\n' {
                    st = S::Code;
                }
                out.push(blank(ch));
                i += 1;
            }
            S::Block(d) => {
                if ch == '/' && c.get(i + 1) == Some(&'*') {
                    st = S::Block(d + 1);
                    out.push_str("  ");
                    i += 2;
                } else if ch == '*' && c.get(i + 1) == Some(&'/') {
                    st = if d == 1 { S::Code } else { S::Block(d - 1) };
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(blank(ch));
                    i += 1;
                }
            }
            S::Str => {
                if ch == '\\' {
                    out.push(' ');
                    out.push(c.get(i + 1).map_or(' ', |n| blank(*n)));
                    i += 2;
                } else {
                    if ch == '"' {
                        st = S::Code;
                    }
                    out.push(blank(ch));
                    i += 1;
                }
            }
            S::Raw(h) => {
                if ch == '"' && (1..=h).all(|k| c.get(i + k) == Some(&'#')) {
                    st = S::Code;
                    out.extend(std::iter::repeat_n(' ', h + 1));
                    i += h + 1;
                } else {
                    out.push(blank(ch));
                    i += 1;
                }
            }
        }
    }
    out.split('\n').map(str::to_string).collect()
}

fn prev_is_ident(c: &[char], i: usize) -> bool {
    i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_')
}

fn raw_open(c: &[char], i: usize) -> Option<(usize, usize)> {
    let mut j = i + 1;
    while c.get(j) == Some(&'#') {
        j += 1;
    }
    (c.get(j) == Some(&'"')).then_some((j - i - 1, j - i + 1))
}

fn delta(code: &str) -> i32 {
    code.chars().map(|c| match c {
        '{' | '(' | '[' => 1,
        '}' | ')' | ']' => -1,
        _ => 0,
    }).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Impl,
    Fn,
    Trait,
    Other,
}

#[derive(Debug, Clone)]
pub struct Item {
    /// 0-based, inclusive: first doc or attribute line through the closing line.
    pub start: usize,
    pub head: usize,
    pub end: usize,
    pub kind: ItemKind,
    pub name: String,
    pub is_pub: bool,
    pub test_only: bool,
}

fn first_tokens(head: &str) -> (bool, ItemKind, String) {
    let mut toks = head.split_whitespace().peekable();
    let is_pub = toks.peek() == Some(&"pub");
    while let Some(t) = toks.peek() {
        if matches!(*t, "pub" | "unsafe" | "async" | "const" | "default") || t.starts_with("pub(") {
            toks.next();
        } else {
            break;
        }
    }
    let rest: Vec<&str> = toks.collect();
    match rest.first() {
        Some(t) if t.starts_with("impl") && (t.len() == 4 || t.starts_with("impl<")) => {
            let sig: String = head.trim().trim_start_matches("unsafe").trim().split(['{']).next().unwrap_or("").split(" where ").next().unwrap_or("").trim().to_string();
            (is_pub, ItemKind::Impl, sig)
        }
        Some(&"fn") => (is_pub, ItemKind::Fn, ident_after(rest.get(1).copied().unwrap_or(""))),
        Some(&"trait") => (is_pub, ItemKind::Trait, ident_after(rest.get(1).copied().unwrap_or(""))),
        _ => (is_pub, ItemKind::Other, String::new()),
    }
}

fn ident_after(s: &str) -> String {
    s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
}

pub fn items(raw: &[&str], code: &[String]) -> Vec<Item> {
    let n = code.len().min(raw.len());
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i < n {
        let t = code[i].trim();
        let r = raw[i].trim_start();
        if t.is_empty() {
            if r.starts_with("///") {
                start.get_or_insert(i);
            } else if r.is_empty() {
                start = None;
            }
            i += 1;
        } else if t.starts_with('#') {
            start.get_or_insert(i);
            let mut d = delta(&code[i]);
            while d > 0 && i + 1 < n {
                i += 1;
                d += delta(&code[i]);
            }
            i += 1;
        } else if t.starts_with('}') {
            i += 1;
        } else {
            let mut j = i;
            let mut d = 0;
            loop {
                d += delta(&code[j]);
                let tt = code[j].trim_end();
                if d <= 0 && (tt.ends_with('}') || tt.ends_with(';')) || j + 1 >= n {
                    break;
                }
                j += 1;
            }
            let s = start.take().unwrap_or(i);
            let (is_pub, kind, name) = first_tokens(t);
            let test_only = (s..i).any(|k| {
                let a = raw[k].replace(' ', "");
                a.contains("cfg(test)") || a.starts_with("#[test]")
            });
            out.push(Item { start: s, head: i, end: j, kind, name, is_pub, test_only });
            i = j + 1;
        }
    }
    out
}

fn sig_end(code: &[String], from: usize, limit: usize) -> usize {
    (from..=limit).find(|k| code[*k].contains('{') || code[*k].trim_end().ends_with(';')).unwrap_or(limit)
}

fn doc_start(raw: &[&str], mut at: usize) -> usize {
    while at > 0 && {
        let p = raw[at - 1].trim_start();
        p.starts_with("///") || p.starts_with("#[")
    } {
        at -= 1;
    }
    at
}

/// 1-based inclusive line ranges: each `pub` item's doc comment and signature, including the
/// `pub` methods of an inherent impl and the methods of a `pub trait`.
pub fn public_surface(raw: &[&str], code: &[String], items: &[Item]) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut depth_before = Vec::with_capacity(code.len());
    let mut d = 0;
    for l in code {
        depth_before.push(d);
        d += delta(l);
    }
    for it in items.iter().filter(|i| !i.test_only) {
        if it.is_pub {
            ranges.push((it.start, sig_end(code, it.head, it.end)));
        }
        let inherent_impl = it.kind == ItemKind::Impl && !it.name.contains(" for ");
        let pub_trait = it.kind == ItemKind::Trait && it.is_pub;
        if !(inherent_impl || pub_trait) {
            continue;
        }
        for k in it.head + 1..it.end {
            if depth_before[k] != 1 {
                continue;
            }
            let t = code[k].trim_start();
            let listed = if pub_trait {
                ["fn ", "async fn ", "type ", "const "].iter().any(|p| t.starts_with(p))
            } else {
                t.starts_with("pub ")
            };
            if listed {
                ranges.push((doc_start(raw, k), sig_end(code, k, it.end)));
            }
        }
    }
    ranges.into_iter().map(|(a, b)| (a as u32 + 1, b as u32 + 1)).collect()
}

/// Sorted, with overlapping and touching ranges joined.
pub fn merge(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The items one cohesive part may be: an impl block, or a function.
pub fn part_candidates(items: &[Item]) -> Vec<usize> {
    items.iter().enumerate().filter(|(_, i)| !i.test_only && matches!(i.kind, ItemKind::Impl | ItemKind::Fn)).map(|(k, _)| k).collect()
}

/// The chosen item's range, and for a function the private top-level functions it calls in
/// this file, transitively. Returns the ranges (1-based) and the helper names.
pub fn part_ranges(code: &[String], items: &[Item], chosen: usize) -> (Vec<(u32, u32)>, Vec<String>) {
    let span = |i: &Item| (i.start as u32 + 1, i.end as u32 + 1);
    let it = &items[chosen];
    let mut ranges = vec![span(it)];
    let mut helpers: Vec<String> = Vec::new();
    if it.kind == ItemKind::Fn {
        let mut seen: HashSet<usize> = HashSet::from([chosen]);
        let mut queue = vec![chosen];
        while let Some(cur) = queue.pop() {
            let body = code[items[cur].head..=items[cur].end].join("\n");
            for (k, other) in items.iter().enumerate() {
                if other.kind == ItemKind::Fn && !other.is_pub && !other.test_only && !seen.contains(&k) && calls(&body, &other.name) {
                    seen.insert(k);
                    queue.push(k);
                    ranges.push(span(other));
                    helpers.push(other.name.clone());
                }
            }
        }
    }
    (ranges, helpers)
}

pub(crate) fn calls(body: &str, name: &str) -> bool {
    let pat = format!("{name}(");
    body.match_indices(&pat).any(|(at, _)| !body[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
}

/// Names of the non-test `pub` top-level functions, traits, structs, enums and type aliases.
pub fn pub_names(raw: &[&str], code: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for it in items(raw, code).iter().filter(|i| i.is_pub && !i.test_only) {
        if !it.name.is_empty() {
            out.push(it.name.clone());
            continue;
        }
        let mut toks = code[it.head].split_whitespace().skip_while(|t| *t == "pub" || t.starts_with("pub("));
        if let (Some("struct" | "enum" | "type" | "union"), Some(n)) = (toks.next(), toks.next()) {
            out.push(ident_after(n));
        }
    }
    out
}

/// Each `#[test]` function in the file, top level or inside a module, with its body as code.
pub fn test_fns(raw: &[&str], code: &[String]) -> Vec<(String, String)> {
    let n = code.len().min(raw.len());
    let mut out = Vec::new();
    let mut pending = false;
    let mut i = 0;
    while i < n {
        let attr = raw[i].replace(' ', "");
        if attr.starts_with("#[test]") || attr.starts_with("#[tokio::test") {
            pending = true;
        } else if pending && !code[i].trim().is_empty() && !code[i].trim_start().starts_with('#') {
            pending = false;
            let (_, kind, name) = first_tokens(code[i].trim());
            if kind == ItemKind::Fn {
                let mut j = i;
                let mut d = 0;
                loop {
                    d += delta(&code[j]);
                    if d <= 0 && code[j].trim_end().ends_with('}') || j + 1 >= n {
                        break;
                    }
                    j += 1;
                }
                out.push((name, code[i..=j].join("\n")));
                i = j;
            }
        }
        i += 1;
    }
    out
}

#[derive(Debug, Default)]
pub struct Refs {
    pub idents: HashSet<String>,
    pub pairs: HashSet<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Id(String),
    Sep,
    Ch(char),
}

fn tokens(code: &str) -> Vec<Tok> {
    let c: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_alphabetic() || ch == '_' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[s..i].iter().collect()));
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
        } else if ch == ':' && c.get(i + 1) == Some(&':') {
            out.push(Tok::Sep);
            i += 2;
        } else {
            if !ch.is_whitespace() {
                out.push(Tok::Ch(ch));
            }
            i += 1;
        }
    }
    out
}

/// Identifiers a file names, and every `a::b` pair (including `a::{b, c}`).
pub fn refs(code_text: &str) -> Refs {
    let toks = tokens(code_text);
    let mut r = Refs::default();
    for (k, t) in toks.iter().enumerate() {
        let Tok::Id(a) = t else { continue };
        r.idents.insert(a.clone());
        if toks.get(k + 1) != Some(&Tok::Sep) {
            continue;
        }
        match toks.get(k + 2) {
            Some(Tok::Id(b)) => {
                r.pairs.insert((a.clone(), b.clone()));
            }
            Some(Tok::Ch('{')) => {
                let mut depth = 0;
                let mut j = k + 2;
                while j < toks.len() {
                    match &toks[j] {
                        Tok::Ch('{') => depth += 1,
                        Tok::Ch('}') => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Tok::Id(b) if depth == 1 && matches!(toks[j - 1], Tok::Ch('{') | Tok::Ch(',')) => {
                            r.pairs.insert((a.clone(), b.clone()));
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => {}
        }
    }
    r
}

/// Whether `line` is an impl of trait `name`: `impl Name for`, `impl<T> Name<T> for`, `unsafe impl`.
pub fn is_impl_of(line: &str, name: &str) -> bool {
    let t = line.trim_start().trim_start_matches("unsafe").trim_start();
    let Some(rest) = t.strip_prefix("impl") else { return false };
    let mut rest = rest.trim_start();
    if rest.starts_with('<') {
        let mut d = 0;
        let mut cut = None;
        for (k, ch) in rest.char_indices() {
            match ch {
                '<' => d += 1,
                '>' => {
                    d -= 1;
                    if d == 0 {
                        cut = Some(k + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(cut) = cut else { return false };
        rest = rest[cut..].trim_start();
    }
    let rest = rest.strip_prefix('!').unwrap_or(rest);
    let path = rest.split(" for ").next().unwrap_or("");
    rest.contains(" for ") && last_segment(path.split('<').next().unwrap_or("")) == name
}

fn last_segment(path: &str) -> &str {
    path.trim().rsplit("::").next().unwrap_or("")
}

/// Whether `code` names `name` as `dyn Name` or `impl Name` where it is not an impl block.
pub fn names_trait_object(code: &str, name: &str) -> bool {
    ["dyn", "impl"].iter().any(|kw| {
        let pat = format!("{kw} {name}");
        code.match_indices(&pat).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            let after = code[at + pat.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && !(*kw == "impl" && code[at..].lines().next().is_some_and(|l| is_impl_of(l, name)))
        })
    })
}

/// A `struct|enum|trait|type NAME` definition on this line.
pub fn defines_type(line: &str, name: &str, only_trait: bool) -> Option<&'static str> {
    let mut toks = line.split_whitespace().peekable();
    while let Some(t) = toks.peek() {
        if *t == "pub" || t.starts_with("pub(") || *t == "unsafe" {
            toks.next();
        } else {
            break;
        }
    }
    let kw = match toks.next()? {
        "struct" if !only_trait => "struct",
        "enum" if !only_trait => "enum",
        "trait" => "trait",
        "type" if !only_trait => "type",
        _ => return None,
    };
    (ident_after(toks.next()?) == name).then_some(kw)
}
