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
    lined_tokens(code).into_iter().map(|(t, _)| t).collect()
}

/// Each token with the 1-based line it starts on.
fn lined_tokens(code: &str) -> Vec<(Tok, u32)> {
    let c: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_alphabetic() || ch == '_' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push((Tok::Id(c[s..i].iter().collect()), line));
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
        } else if ch == ':' && c.get(i + 1) == Some(&':') {
            out.push((Tok::Sep, line));
            i += 2;
        } else {
            if ch == '\n' {
                line += 1;
            } else if !ch.is_whitespace() {
                out.push((Tok::Ch(ch), line));
            }
            i += 1;
        }
    }
    out
}

/// Every `a::b` pair (including `a::{b, c}`) with the index of `b`'s token.
fn pair_hits(toks: &[Tok]) -> Vec<(String, String, usize)> {
    let mut hits = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        let Tok::Id(a) = t else { continue };
        if toks.get(k + 1) != Some(&Tok::Sep) {
            continue;
        }
        match toks.get(k + 2) {
            Some(Tok::Id(b)) => hits.push((a.clone(), b.clone(), k + 2)),
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
                        Tok::Id(b) if depth == 1 && matches!(toks[j - 1], Tok::Ch('{') | Tok::Ch(',')) => hits.push((a.clone(), b.clone(), j)),
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => {}
        }
    }
    hits
}

/// Identifiers a file names, and every `a::b` pair (including `a::{b, c}`).
pub fn refs(code_text: &str) -> Refs {
    let toks = tokens(code_text);
    let mut r = Refs::default();
    for t in &toks {
        if let Tok::Id(a) = t {
            r.idents.insert(a.clone());
        }
    }
    r.pairs = pair_hits(&toks).into_iter().map(|(a, b, _)| (a, b)).collect();
    r
}

/// The first line of `code_text` that names one of `pairs` as `a::b` or one of `idents`.
pub fn first_line_naming(code_text: &str, pairs: &[(String, String)], idents: &[String]) -> Option<u32> {
    let lined = lined_tokens(code_text);
    let toks: Vec<Tok> = lined.iter().map(|(t, _)| t.clone()).collect();
    let pair_line = pair_hits(&toks).into_iter().filter(|(a, b, _)| pairs.iter().any(|(pa, pb)| pa == a && pb == b)).map(|(_, _, at)| lined[at].1);
    let ident_line = lined.iter().filter(|(t, _)| matches!(t, Tok::Id(x) if idents.contains(x))).map(|(_, l)| *l);
    pair_line.chain(ident_line).min()
}

/// One name a `use` declaration brings in: `use a::{b as c, d::*}` is `a::b` as `c` and glob `a::d`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub path: Vec<String>,
    pub alias: String,
    pub glob: bool,
    pub public: bool,
}

fn use_tree(toks: &[Tok], j: &mut usize, prefix: &mut Vec<String>, public: bool, out: &mut Vec<Use>) {
    let base = prefix.len();
    loop {
        match toks.get(*j) {
            Some(Tok::Sep) => *j += 1,
            Some(Tok::Id(x)) if toks.get(*j + 1) == Some(&Tok::Sep) => {
                prefix.push(x.clone());
                *j += 2;
            }
            Some(Tok::Id(x)) => {
                *j += 1;
                let mut path = prefix.clone();
                if x != "self" {
                    path.push(x.clone());
                }
                let mut alias = path.last().cloned().unwrap_or_default();
                if let (Some(Tok::Id(kw)), Some(Tok::Id(a))) = (toks.get(*j), toks.get(*j + 1)) {
                    if kw == "as" {
                        alias = a.clone();
                        *j += 2;
                    }
                }
                out.push(Use { path, alias, glob: false, public });
                break;
            }
            Some(Tok::Ch('*')) => {
                *j += 1;
                out.push(Use { path: prefix.clone(), alias: String::new(), glob: true, public });
                break;
            }
            Some(Tok::Ch('{')) => {
                *j += 1;
                loop {
                    use_tree(toks, j, prefix, public, out);
                    match toks.get(*j) {
                        Some(Tok::Ch(',')) => *j += 1,
                        Some(Tok::Ch('}')) => {
                            *j += 1;
                            break;
                        }
                        _ => break,
                    }
                }
                break;
            }
            _ => break,
        }
    }
    prefix.truncate(base);
}

/// Every name the `use` declarations in `code_text` bring in, groups expanded and renames kept.
pub fn uses(code_text: &str) -> Vec<Use> {
    let toks = tokens(code_text);
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        if *t != Tok::Id("use".to_string()) || toks.get(k + 1) == Some(&Tok::Ch('<')) {
            continue;
        }
        let is_pub = |end: usize| match toks[..end].last() {
            Some(Tok::Id(p)) => p == "pub",
            Some(Tok::Ch(')')) => toks[..end].iter().rposition(|t| *t == Tok::Ch('(')).is_some_and(|o| o > 0 && toks[o - 1] == Tok::Id("pub".to_string())),
            _ => false,
        };
        let public = is_pub(k);
        let mut j = k + 1;
        use_tree(&toks, &mut j, &mut Vec::new(), public, &mut out);
    }
    out
}

fn path_segments(path: &str) -> Vec<String> {
    path.split("::").map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Whether `line` is an impl whose trait path `resolves`: `impl Name for`, `impl<T> a::Name<T> for`, `unsafe impl`.
pub fn is_impl_of(line: &str, resolves: &dyn Fn(&[String]) -> bool) -> bool {
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
    rest.contains(" for ") && resolves(&path_segments(path.split('<').next().unwrap_or("")))
}

/// Whether `code` names a trait whose path `resolves` as `dyn Path` or `impl Path`, outside an impl block header.
pub fn names_trait_object(code: &str, resolves: &dyn Fn(&[String]) -> bool) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    ["dyn", "impl"].iter().any(|kw| {
        code.match_indices(kw).any(|(at, _)| {
            let after = &code[at + kw.len()..];
            let path: String = after.trim_start().chars().take_while(|c| ident(*c) || *c == ':').collect();
            !code[..at].chars().next_back().is_some_and(ident)
                && after.starts_with(char::is_whitespace)
                && resolves(&path_segments(&path))
                && !(*kw == "impl" && code[at..].lines().next().is_some_and(|l| is_impl_of(l, resolves)))
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
