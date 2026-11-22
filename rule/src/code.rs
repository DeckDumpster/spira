//! The statute code: `law/toc.toml` plus the statute texts generate a tree of Parts and
//! numbered sections. A slug is a statute's identity; its §N.M number is presentation, derived
//! from `toc.toml`. A change to the book leaves a pointer in `law/pointers.toml`, never a gap.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub number: u32,
    pub name: String,
    pub slug: String,
    pub scope: String,
    pub slugs: Vec<String>,
}

pub fn parse_toc(text: &str) -> Result<Vec<Part>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("law/toc.toml: {e}"))?;
    let rows = match table.get("part") {
        Some(toml::Value::Array(a)) => a.clone(),
        Some(_) => return Err("law/toc.toml: `part` must be an array of tables".into()),
        None => Vec::new(),
    };
    let mut parts: Vec<Part> = Vec::new();
    for row in &rows {
        let str_of = |k: &str| row.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| format!("law/toc.toml: a part has no string `{k}`"));
        let number = row
            .get("number")
            .and_then(|v| v.as_integer())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or("law/toc.toml: a part has no positive integer `number`")?;
        let slugs = match row.get("slugs") {
            Some(toml::Value::Array(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or("law/toc.toml: `slugs` must be strings")).collect::<Result<Vec<_>, _>>()?,
            _ => return Err(format!("law/toc.toml: part {number} has no `slugs` array")),
        };
        let part = Part { number, name: str_of("name")?, slug: str_of("slug")?, scope: str_of("scope")?, slugs };
        if parts.iter().any(|p| p.number == number) {
            return Err(format!("law/toc.toml: part {number} appears twice"));
        }
        parts.push(part);
    }
    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    for p in &parts {
        for s in &p.slugs {
            if let Some(other) = seen.insert(s, p.number) {
                return Err(format!("law/toc.toml: {s} is in part {other} and part {}", p.number));
            }
        }
    }
    Ok(parts)
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

pub fn render_toc(parts: &[Part]) -> String {
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("[[part]]\nnumber = {}\nname = {}\nslug = {}\nscope = {}\nslugs = [\n", p.number, quote(&p.name), quote(&p.slug), quote(&p.scope)));
        for s in &p.slugs {
            out.push_str(&format!("  {},\n", quote(s)));
        }
        out.push_str("]\n");
    }
    out
}

pub fn parts_list(parts: &[Part]) -> String {
    if parts.is_empty() {
        return "  (no Parts in law/toc.toml)\n".to_string();
    }
    parts.iter().map(|p| format!("  {} — {}: {}\n", p.number, p.name, p.scope)).collect()
}

pub const POINTERS_FILE: &str = "pointers.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub section: String,
    pub slug: String,
    pub on: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub from: String,
    pub into: String,
    pub reason: String,
    pub on: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub slug: String,
    pub reason: String,
    pub replaced_by: Option<String>,
    pub on: String,
}

/// `law/pointers.toml`: where a statute's old number, or the statute itself, went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pointers {
    pub moved: Vec<Moved>,
    pub merged: Vec<Merged>,
    pub retired: Vec<Retired>,
}

fn pointer_rows(table: &toml::Table, kind: &str, keys: &[&str], optional: &[&str]) -> Result<Vec<BTreeMap<String, String>>, String> {
    let file = "law/pointers.toml";
    let rows = match table.get(kind) {
        Some(toml::Value::Array(a)) => a.clone(),
        Some(_) => return Err(format!("{file}: `{kind}` must be an array of tables")),
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for row in &rows {
        let Some(t) = row.as_table() else { return Err(format!("{file}: `{kind}` must be an array of tables")) };
        let mut m = BTreeMap::new();
        for (k, v) in t {
            if !keys.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
                return Err(format!("{file}: [[{kind}]] has unknown field `{k}`"));
            }
            let v = v.as_str().ok_or_else(|| format!("{file}: [[{kind}]] `{k}` must be a string"))?;
            m.insert(k.clone(), v.to_string());
        }
        if let Some(k) = keys.iter().find(|k| !m.contains_key(**k)) {
            return Err(format!("{file}: a [[{kind}]] has no `{k}`"));
        }
        out.push(m);
    }
    Ok(out)
}

pub fn parse_pointers(text: &str) -> Result<Pointers, String> {
    let file = "law/pointers.toml";
    let table: toml::Table = text.parse().map_err(|e| format!("{file}: {e}"))?;
    match table.get("version").and_then(|v| v.as_integer()) {
        Some(1) => {}
        Some(n) => return Err(format!("{file}: version {n} is not understood (this build reads 1)")),
        None => return Err(format!("{file}: no integer `version`")),
    }
    if let Some(k) = table.keys().find(|k| !["version", "moved", "merged", "retired"].contains(&k.as_str())) {
        return Err(format!("{file}: unknown field `{k}`"));
    }
    let moved = pointer_rows(&table, "moved", &["section", "slug", "on"], &[])?
        .into_iter()
        .map(|mut m| Moved { section: m.remove("section").unwrap_or_default(), slug: m.remove("slug").unwrap_or_default(), on: m.remove("on").unwrap_or_default() })
        .collect();
    let merged = pointer_rows(&table, "merged", &["from", "into", "reason", "on"], &[])?
        .into_iter()
        .map(|mut m| Merged { from: m.remove("from").unwrap_or_default(), into: m.remove("into").unwrap_or_default(), reason: m.remove("reason").unwrap_or_default(), on: m.remove("on").unwrap_or_default() })
        .collect();
    let retired = pointer_rows(&table, "retired", &["slug", "reason", "on"], &["replaced_by"])?
        .into_iter()
        .map(|mut m| Retired { slug: m.remove("slug").unwrap_or_default(), reason: m.remove("reason").unwrap_or_default(), replaced_by: m.remove("replaced_by"), on: m.remove("on").unwrap_or_default() })
        .collect();
    Ok(Pointers { moved, merged, retired })
}

pub fn render_pointers(p: &Pointers) -> String {
    let mut out = String::from("version = 1\n");
    for m in &p.moved {
        out.push_str(&format!("\n[[moved]]\nsection = {}\nslug = {}\non = {}\n", quote(&m.section), quote(&m.slug), quote(&m.on)));
    }
    for m in &p.merged {
        out.push_str(&format!("\n[[merged]]\nfrom = {}\ninto = {}\nreason = {}\non = {}\n", quote(&m.from), quote(&m.into), quote(&m.reason), quote(&m.on)));
    }
    for r in &p.retired {
        out.push_str(&format!("\n[[retired]]\nslug = {}\nreason = {}\n", quote(&r.slug), quote(&r.reason)));
        if let Some(b) = &r.replaced_by {
            out.push_str(&format!("replaced_by = {}\n", quote(b)));
        }
        out.push_str(&format!("on = {}\n", quote(&r.on)));
    }
    out
}

pub fn section_of(toc: &[Part], slug: &str) -> Option<String> {
    toc.iter().find_map(|p| p.slugs.iter().position(|s| s == slug).map(|i| format!("{}.{}", p.number, i + 1)))
}

fn slug_at<'a>(toc: &'a [Part], section: &str) -> Option<&'a str> {
    let (part, n) = section.split_once('.')?;
    let p = toc.iter().find(|p| p.number.to_string() == part)?;
    p.slugs.get(n.parse::<usize>().ok()?.checked_sub(1)?).map(String::as_str)
}

/// Slug -> "N.M" read from the generated `N-part/N.M-slug.md` file names under `law`.
pub fn numbers_on_disk(law: &Path) -> BTreeMap<String, String> {
    let mut files = Vec::new();
    walk(law, law, &mut files);
    numbers_of(&files)
}

pub fn numbers_of(files: &[String]) -> BTreeMap<String, String> {
    files
        .iter()
        .filter_map(|f| {
            let name = Path::new(f).file_name()?.to_str()?;
            let (sec, rest) = name.strip_suffix(".md")?.split_once('-')?;
            let (a, b) = sec.split_once('.')?;
            (a.chars().chain(b.chars()).all(|c| c.is_ascii_digit()) && !a.is_empty() && !b.is_empty()).then(|| (rest.to_string(), sec.to_string()))
        })
        .collect()
}

/// `pointers` plus a `moved` entry for every slug of `old` whose section differs in `new_toc`.
/// A slug the store still holds stays in the toc, so a dropped one is refused; one the store
/// lacks has left the book and records no move.
pub fn record_moves(old: &BTreeMap<String, String>, new_toc: &[Part], pointers: &Pointers, today: &str, book: &BTreeMap<String, String>) -> Result<Pointers, String> {
    if let Some(gone) = old.keys().find(|s| section_of(new_toc, s).is_none() && book.contains_key(*s)) {
        return Err(format!("law/toc.toml no longer lists {gone}, which holds §{} and is still in the store — a statute in force stays in the toc; retire it with `rule.sh retire` instead", old[gone]));
    }
    let mut out = pointers.clone();
    for (slug, was) in old {
        let Some(now) = section_of(new_toc, slug) else { continue };
        if now != *was && !out.moved.iter().any(|m| m.section == *was && m.slug == *slug) {
            out.moved.push(Moved { section: was.clone(), slug: slug.clone(), on: today.to_string() });
        }
    }
    Ok(out)
}

/// The memory key holding the retirement record of statute `slug`.
pub fn retired_key(slug: &str) -> String {
    format!("retired-{slug}")
}

/// One `Retired` row per `retired-law-*` record in `book`; `on` is the date part of its `at`.
pub fn store_pointers(book: &BTreeMap<String, String>) -> Vec<Retired> {
    book.iter()
        .filter_map(|(key, value)| {
            let slug = key.strip_prefix("retired-").filter(|s| s.starts_with("law-"))?;
            let r = sop::shelf::Retired::from_json(value)?;
            Some(Retired { slug: slug.to_string(), reason: r.reason, replaced_by: r.replaced_by, on: r.at.split(['T', ' ']).next().unwrap_or_default().to_string() })
        })
        .collect()
}

/// `pointers` plus the store's retirement records, for each slug no pointer row already names and the store does not hold again.
pub fn with_store_pointers(mut pointers: Pointers, book: &BTreeMap<String, String>) -> Pointers {
    for r in store_pointers(book) {
        let named = book.contains_key(&r.slug) || pointers.retired.iter().any(|p| p.slug == r.slug) || pointers.merged.iter().any(|m| m.from == r.slug);
        if !named {
            pointers.retired.push(r);
        }
    }
    pointers
}

fn successor<'a>(pointers: &'a Pointers, slug: &str) -> Option<&'a str> {
    if let Some(m) = pointers.merged.iter().rev().find(|m| m.from == slug) {
        return Some(&m.into);
    }
    pointers.retired.iter().rev().find(|r| r.slug == slug).and_then(|r| r.replaced_by.as_deref())
}

/// `toc` with every slug the store lacks taken out: a successor the store holds and the toc
/// does not list yet takes its place, otherwise the slug leaves.
pub fn without_missing(toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Vec<Part> {
    let mut out = toc.to_vec();
    let missing: Vec<String> = toc.iter().flat_map(|p| p.slugs.iter()).filter(|s| !book.contains_key(*s)).cloned().collect();
    for slug in missing {
        let next = successor(pointers, &slug).filter(|n| book.contains_key(*n) && !out.iter().any(|p| p.slugs.iter().any(|s| s == n))).map(str::to_string);
        for p in &mut out {
            if let Some(i) = p.slugs.iter().position(|s| *s == slug) {
                match &next {
                    Some(n) => p.slugs[i] = n.clone(),
                    None => {
                        p.slugs.remove(i);
                    }
                }
            }
        }
    }
    out
}

/// The book as the store holds it: `toc` without the slugs the store lacks, and a `moved` row
/// for every slug of `landed` (slug -> section on the landing ref) whose section changed.
pub fn in_force(toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>, landed: &BTreeMap<String, String>, today: &str) -> Result<(Vec<Part>, Pointers), String> {
    let toc = without_missing(toc, pointers, book);
    let pointers = record_moves(landed, &toc, pointers, today, book)?;
    Ok((toc, pointers))
}

/// Problems with `pointers` against `toc`: every slug a pointer leads to must be in the toc.
/// The slug a retirement or merge leaves behind need not be.
pub fn check_pointers(toc: &[Part], pointers: &Pointers) -> Vec<String> {
    let named = pointers.moved.iter().map(|m| &m.slug).chain(pointers.merged.iter().map(|m| &m.into)).chain(pointers.retired.iter().filter_map(|r| r.replaced_by.as_ref()));
    let mut out: Vec<String> = named.filter(|s| section_of(toc, s).is_none()).map(|s| format!("{POINTERS_FILE}: names {s}, which is not in toc.toml")).collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub hops: Vec<String>,
    pub slug: String,
    pub section: String,
    pub retired: Option<Retired>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
    Unknown,
    Cycle(String),
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::Unknown => write!(f, "nothing ever held that number or slug"),
            Miss::Cycle(c) => write!(f, "pointers form a cycle: {c}"),
        }
    }
}

pub fn is_section(s: &str) -> bool {
    s.split_once('.').is_some_and(|(a, b)| [a, b].iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit())))
}

/// Follows `arg` (`law-x`, `x`, `N.M`, `§N.M`) through moved, merged and retired-with-
/// replacement pointers to the statute now in force, or to a retired one with no successor.
pub fn resolve(toc: &[Part], pointers: &Pointers, arg: &str) -> Result<Resolution, Miss> {
    let arg = arg.strip_prefix('§').unwrap_or(arg);
    let now_of = |s: &str| section_of(toc, s).map_or_else(|| "?".to_string(), |n| format!("§{n}"));
    let mut hops = Vec::new();
    let mut slug = if is_section(arg) {
        match slug_at(toc, arg) {
            Some(s) => s.to_string(),
            None => {
                let m = pointers.moved.iter().rev().find(|m| m.section == arg).ok_or(Miss::Unknown)?;
                hops.push(format!("§{arg} moved: {} is now {}", m.slug, now_of(&m.slug)));
                m.slug.clone()
            }
        }
    } else {
        let s = format!("law-{}", arg.strip_prefix("law-").unwrap_or(arg));
        let named = pointers.merged.iter().any(|m| m.from == s) || pointers.retired.iter().any(|r| r.slug == s);
        if section_of(toc, &s).is_none() && !named {
            return Err(Miss::Unknown);
        }
        s
    };
    let mut seen = vec![slug.clone()];
    loop {
        let next = if let Some(m) = pointers.merged.iter().rev().find(|m| m.from == slug) {
            hops.push(format!("{} merged into {} ({}): {}", m.from, m.into, now_of(&m.into), m.reason));
            m.into.clone()
        } else if let Some(r) = pointers.retired.iter().rev().find(|r| r.slug == slug) {
            let Some(rep) = &r.replaced_by else {
                return Ok(Resolution { hops, section: section_of(toc, &slug).unwrap_or_default(), slug, retired: Some(r.clone()) });
            };
            hops.push(format!("{} retired: {}; replaced by {} ({})", r.slug, r.reason, rep, now_of(rep)));
            rep.clone()
        } else {
            return Ok(Resolution { hops, section: section_of(toc, &slug).unwrap_or_default(), slug, retired: None });
        };
        if seen.contains(&next) {
            seen.push(next);
            return Err(Miss::Cycle(seen.join(" -> ")));
        }
        seen.push(next.clone());
        slug = next;
    }
}

/// How a retired statute's section reads.
pub fn retired_body(r: &Retired, toc: &[Part]) -> String {
    let mut out = format!("[Retired {}] {}", r.on, r.reason);
    if let Some(rep) = &r.replaced_by {
        out.push_str(&format!("\n\nReplaced by {} {rep}.", section_of(toc, rep).map_or_else(String::new, |n| format!("§{n}"))));
    }
    out
}

/// Appends `slug` to Part `number`. A slug already in that Part is left where it is (an
/// amendment); one already in another Part is refused — a move is made by editing `toc.toml`
/// and running `rule.sh code`, which records the old number.
pub fn append(parts: &[Part], number: u32, slug: &str) -> Result<Vec<Part>, String> {
    let mut out = parts.to_vec();
    if let Some(home) = out.iter().find(|p| p.slugs.iter().any(|s| s == slug)) {
        if home.number != number {
            return Err(format!("{slug} is already section of Part {} — to move it, edit law/toc.toml and run `rule.sh code`", home.number));
        }
        return Ok(out);
    }
    let Some(part) = out.iter_mut().find(|p| p.number == number) else {
        return Err(format!("there is no Part {number}"));
    };
    part.slugs.push(slug.to_string());
    Ok(out)
}

/// A statute's trailing parenthetical citing the operator (`(per ...)`) or a bead
/// (`(sp-...: ...)`) names the case that produced it; the shipped projection carries the rule,
/// the store and the wiki keep the history.
fn without_citation(text: &str) -> std::borrow::Cow<'_, str> {
    let mut rest = text.trim_end();
    let mut punct = "";
    let mut trimmed = false;
    loop {
        let t = rest.trim_end();
        let (core, p) = match t.strip_suffix(['.', ';']) {
            Some(c) if c.ends_with(')') => (c, &t[c.len()..]),
            _ => (t, ""),
        };
        let Some(i) = core.strip_suffix(')').and_then(|_| core.rfind('(')).filter(|&i| is_citation(&core[i..])) else { break };
        if !trimmed {
            punct = p;
        }
        trimmed = true;
        rest = core[..i].trim_end();
    }
    let head = if trimmed { format!("{rest}{punct}") } else { text.to_string() };
    let cleaned = without_dated_provenance(&head);
    if cleaned == text {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(cleaned)
    }
}

/// Drops every `(per ...)` parenthetical holding a `YYYY-MM-DD` date, with the space before it.
fn without_dated_provenance(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("(per ") {
        let Some(len) = rest[i..].find(')') else { break };
        if has_date(&rest[i..i + len]) {
            out.push_str(rest[..i].trim_end_matches([' ', '\t']));
        } else {
            out.push_str(&rest[..i + len + 1]);
        }
        rest = &rest[i + len + 1..];
    }
    out.push_str(rest);
    out
}

fn has_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.windows(10).any(|w| w.iter().enumerate().all(|(k, c)| if k == 4 || k == 7 { *c == b'-' } else { c.is_ascii_digit() }))
}

fn is_citation(paren: &str) -> bool {
    paren.starts_with("(per ") || paren.strip_prefix("(sp-").is_some_and(|r| r.split_once(':').is_some_and(|(id, _)| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')))
}

pub fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(i) => flat[..=i].to_string(),
        None => flat,
    }
}

pub fn part_dir(p: &Part) -> String {
    format!("{}-{}", p.number, p.slug)
}

/// Every file of the tree, keyed by path relative to `law/`. `toc.toml` and `pointers.toml` are
/// inputs, not output. A slug the book no longer holds renders its pointer record, or as a
/// reserved section when it has none.
pub fn generate(parts: &[Part], pointers: &Pointers, text_of: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut top = String::from("# Statute code\n\n");
    for p in parts {
        top.push_str(&format!("- Part {} — {} — {}\n", p.number, p.name, p.scope));
        let mut idx = format!("# Part {} — {}\n\n{}\n\n", p.number, p.name, p.scope);
        for (i, slug) in p.slugs.iter().enumerate() {
            let sec = format!("{}.{}", p.number, i + 1);
            let text = text_of(slug);
            let at = |s: &str| section_of(parts, s).map_or_else(String::new, |n| format!("§{n} "));
            let (summary, body) = if let Some(m) = pointers.merged.iter().rev().find(|m| m.from == *slug) {
                let line = format!("[Merged into {}{}] {}", at(&m.into), m.into, m.reason);
                (line.clone(), line)
            } else if let Some(r) = pointers.retired.iter().rev().find(|r| r.slug == *slug) {
                (format!("[Retired {}] {}", r.on, r.reason), retired_body(r, parts))
            } else {
                match &text {
                    Some(t) => {
                        let t = without_citation(t);
                        (first_sentence(&t), t.trim_end_matches('\n').to_string())
                    }
                    None => ("[Reserved]".to_string(), "[Reserved] — no longer in force.".to_string()),
                }
            };
            idx.push_str(&format!("- §{sec} {slug} — {summary}\n"));
            files.insert(format!("{}/{sec}-{slug}.md", part_dir(p)), format!("# §{sec} {slug}\n\n{body}\n"));
        }
        files.insert(format!("{}/index.md", part_dir(p)), idx);
    }
    files.insert("index.md".to_string(), top);
    files
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
}

/// Differences between `expected` and the files under `dir` (`toc.toml` and `pointers.toml` excluded).
pub fn drift(dir: &Path, expected: &BTreeMap<String, String>) -> Vec<String> {
    let mut on_disk = Vec::new();
    walk(dir, dir, &mut on_disk);
    on_disk.retain(|f| f != "toc.toml" && f != POINTERS_FILE);
    let mut out = Vec::new();
    for (f, want) in expected {
        match std::fs::read_to_string(dir.join(f)) {
            Ok(have) if &have == want => {}
            Ok(_) => out.push(format!("{f}: differs from what toc.toml generates")),
            Err(_) => out.push(format!("{f}: missing")),
        }
    }
    for f in on_disk {
        if !expected.contains_key(&f) {
            out.push(format!("{f}: not generated by toc.toml"));
        }
    }
    out.sort();
    out
}

/// File names under `law/` at `base` of the repository at `repo_path`.
pub fn law_files_at(repo_path: &Path, base: &str) -> Result<Vec<String>, String> {
    let out = git_in(repo_path, &["ls-tree", "-r", "--name-only", base, "law/"])?;
    Ok(out.lines().filter_map(|l| l.strip_prefix("law/")).map(str::to_string).collect())
}

/// `drift` for the `law/` tree at `base`, read from git rather than a directory.
pub fn drift_at(repo_path: &Path, base: &str, expected: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let mut on_ref = law_files_at(repo_path, base)?;
    on_ref.retain(|f| f != "toc.toml" && f != POINTERS_FILE);
    let mut out = Vec::new();
    for (f, want) in expected {
        match git_blob(repo_path, &format!("{base}:law/{f}")) {
            Some(have) if &have == want => {}
            Some(_) => out.push(format!("{f}: differs from what toc.toml generates")),
            None => out.push(format!("{f}: missing")),
        }
    }
    out.extend(on_ref.into_iter().filter(|f| !expected.contains_key(f)).map(|f| format!("{f}: not generated by toc.toml")));
    out.sort();
    Ok(out)
}

fn git_blob(repo_path: &Path, spec: &str) -> Option<String> {
    let out = spira_config::bounded::bounded("git").arg("-C").arg(repo_path).args(["cat-file", "blob", spec]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Makes `dir` hold exactly `expected` (plus `toc.toml`, which it writes from `toc`, and any
/// `pointers.toml`, which it leaves alone).
pub fn write_tree(dir: &Path, toc: &str, expected: &BTreeMap<String, String>) -> std::io::Result<()> {
    let mut stale = Vec::new();
    walk(dir, dir, &mut stale);
    for f in stale.into_iter().filter(|f| f != "toc.toml" && f != POINTERS_FILE) {
        std::fs::remove_file(dir.join(f))?;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                let _ = std::fs::remove_dir(e.path());
            }
        }
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("toc.toml"), toc)?;
    for (f, body) in expected {
        let path = dir.join(f);
        std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
        std::fs::write(path, body)?;
    }
    Ok(())
}

pub fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = spira_config::bounded::bounded("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("rule: git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("rule: git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Where each statute's seed text lives in the tree: what `the_committed_tree_is_what_toc_toml_generates`
/// generates law/ from.
pub const SEEDS_DIR: &str = "spira/statutes";

/// Writes the seed text of every statute the table of contents files, from the book, so the
/// committed law/ is reproducible from the tree alone. The trailing citation is dropped, as
/// law/ drops it: the case history stays in the store, and the shipped tree carries mechanism.
pub fn write_seeds(wt: &Path, toc: &[Part], book: &BTreeMap<String, String>) -> Result<(), String> {
    let dir = wt.join(SEEDS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("rule: {}: {e}", dir.display()))?;
    for slug in toc.iter().flat_map(|p| p.slugs.iter()) {
        let f = dir.join(format!("{slug}.txt"));
        if let Some(text) = book.get(slug) {
            std::fs::write(&f, without_citation(text).as_ref()).map_err(|e| format!("rule: {}: {e}", f.display()))?;
        }
    }
    let kept: std::collections::BTreeSet<&str> = toc.iter().flat_map(|p| p.slugs.iter()).filter(|s| book.contains_key(*s)).map(String::as_str).collect();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("rule: {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("rule: {}: {e}", dir.display()))?.path();
        let stale = path.extension().is_some_and(|x| x == "txt") && !path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| kept.contains(s));
        if stale {
            std::fs::remove_file(&path).map_err(|e| format!("rule: {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// law/ in this worktree against what toc.toml generates from its seeds and `pointers`: what
/// the committed tree must reproduce.
pub fn seed_drift(wt: &Path, toc: &[Part], pointers: &Pointers) -> Vec<String> {
    let seeds = wt.join(SEEDS_DIR);
    let text = |slug: &str| std::fs::read_to_string(seeds.join(format!("{slug}.txt"))).ok();
    drift(&wt.join("law"), &generate(toc, pointers, text))
}

/// A checkout outruns the 5s default on a loaded disk; the kill leaves stderr empty, so the status is reported.
const WORKTREE_ADD_SECS: &str = "120";

fn add_worktree(repo_path: &Path, branch: &str, wt: &str, base: &str) -> Result<(), String> {
    let args = ["worktree", "add", "-q", "-b", branch, wt, base];
    let out = spira_config::bounded::bounded_for("git", WORKTREE_ADD_SECS).arg("-C").arg(repo_path).args(args).output().map_err(|e| format!("rule: git: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let code = out.status.code().map_or("killed by a signal".to_string(), |c| format!("exit {c}"));
    Err(format!("rule: git {} ({code}): {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
}

/// Regenerates `law/` in a short-lived worktree of `repo`, commits it on a branch of its own and
/// removes the worktree. The caller's directory and the repo's own checkout are never touched.
#[allow(clippy::too_many_arguments)]
pub fn commit_law(repo_path: &Path, base: Option<&str>, wt_root: &Path, verb: &str, key: &str, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<String, String> {
    commit_law_cut(repo_path, base, wt_root, verb, key, toc, pointers, book).map(|(branch, _)| branch)
}

/// [`commit_law`], also naming the ref the commit was cut from: the newest book-only unlanded
/// tip (`refs/heads/...`), or `None` when it was cut from the base.
#[allow(clippy::too_many_arguments)]
pub fn commit_law_cut(repo_path: &Path, base: Option<&str>, wt_root: &Path, verb: &str, key: &str, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<(String, Option<String>), String> {
    std::fs::create_dir_all(wt_root).map_err(|e| format!("rule: {}: {e}", wt_root.display()))?;
    let stamp = format!("{}-{}", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0), std::process::id());
    let branch = format!("law/{}-{stamp}", key.trim_start_matches("law-"));
    let wt = wt_root.join(format!("rule-{verb}-{}-{stamp}", key.trim_start_matches("law-")));
    let wt_s = wt.display().to_string();
    let base = base.map(str::to_string).or_else(|| {
        let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), repo_path);
        spira_config::repos::landref(&reg, &repo_path.display().to_string())
    });
    let base = base.ok_or_else(|| format!("rule: no landing ref resolves for {} — refusing to cut law/ from its checkout's HEAD", repo_path.display()))?;
    let tip = newest_book_only_tip(repo_path, &base);
    let cut_from = tip.clone().unwrap_or(base);
    if let Err(e) = add_worktree(repo_path, &branch, &wt_s, &cut_from) {
        let _ = git_in(repo_path, &["worktree", "remove", "--force", &wt_s]);
        let _ = git_in(repo_path, &["branch", "-D", &branch]);
        return Err(e);
    }
    let done = (|| {
        write_law(&wt.join("law"), toc, pointers, book)?;
        write_seeds(&wt, toc, book)?;
        let drifted = seed_drift(&wt, toc, pointers);
        if !drifted.is_empty() {
            return Err(format!("rule: law/ would not be what toc.toml generates from spira/statutes: {drifted:?}"));
        }
        git_in(&wt, &["add", "-A", "--", "law", SEEDS_DIR])?;
        let mut child = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&wt)
            .args(["commit", "-q", "-F", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("rule: git commit: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "law: {verb} {key}");
        }
        let out = child.wait_with_output().map_err(|e| format!("rule: git commit: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("rule: git commit: {} {}", String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim()).trim_end().to_string())
        }
    })();
    let _ = git_in(repo_path, &["worktree", "remove", "--force", &wt_s]);
    if done.is_err() {
        let _ = git_in(repo_path, &["branch", "-D", &branch]);
    }
    done.map(|()| (branch, tip))
}

/// The home repository: its checkout, the ref `law/` is cut from, and its name in the repo map.
pub struct LawRepo<'a> {
    pub path: &'a Path,
    pub base: Option<&'a str>,
    pub name: &'a str,
}

/// Commits the `law/` projection on a branch of its own and hands it to the queue under a
/// bead, so the next round lands it. Returns the bead id and the branch the queue holds.
pub fn land_law(repo: &LawRepo, wt_root: &Path, verb: &str, key: &str, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<(String, String), String> {
    let (repo_path, repo_name) = (repo.path, repo.name);
    let (law_branch, cut_from) = commit_law_cut(repo_path, repo.base, wt_root, verb, key, toc, pointers, book)?;
    let carrier = carrier_of(cut_from.as_deref(), bead_state);
    match carrier {
        Some(id) => carry_on(repo_path, repo_name, &law_branch, &id).map(|branch| (id, branch)),
        None => submit_law(repo_path, repo_name, wt_root, &law_branch, verb, key),
    }
}

/// The bead an unlanded law/ stack already rides: the id of the `spira/<id>` ref a commit was
/// cut from when the store holds that bead SUBMITTED. A base cut, a `law/*` cut, or a bead in
/// any other state files a new item.
pub fn carrier_of(cut_from: Option<&str>, state_of: impl Fn(&str) -> Option<String>) -> Option<String> {
    let id = cut_from?.strip_prefix("refs/heads/spira/")?;
    (state_of(id).as_deref() == Some("SUBMITTED")).then(|| id.to_string())
}

/// The lifecycle state of bead `id` through the store's own client; `None` when unreadable.
fn bead_state(id: &str) -> Option<String> {
    let lc = spira_store::lc_call::LcClient::configured().ok()?;
    let r = lc.call(&["show", id]).ok().filter(|r| r.code == 0)?;
    state_in_show(&r.out)
}

/// The `bead.state` of a `spira-lc show` reply.
pub fn state_in_show(out: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(out).ok()?;
    v.get("bead")?.get("state")?.as_str().map(str::to_string)
}

/// Moves `spira/<id>` to the tip of `law_branch` and deletes `law_branch`. Refuses when that is
/// not a fast-forward, leaving both branches as they were.
pub fn fast_forward_onto(repo_path: &Path, law_branch: &str, carrier_branch: &str) -> Result<(), String> {
    if git_in(repo_path, &["merge-base", "--is-ancestor", carrier_branch, law_branch]).is_err() {
        return Err(format!("rule: {law_branch} does not fast-forward {carrier_branch}; the law/ commit is kept on {law_branch}"));
    }
    git_in(repo_path, &["branch", "-f", carrier_branch, law_branch])?;
    git_in(repo_path, &["branch", "-D", law_branch])?;
    Ok(())
}

fn carry_on(repo_path: &Path, repo_name: &str, law_branch: &str, id: &str) -> Result<String, String> {
    let branch = format!("spira/{id}");
    fast_forward_onto(repo_path, law_branch, &branch)?;
    let queue = spira_config::release_env::tool("queue").map_err(|e| format!("rule: {e}\nrule: the law/ commit is on {branch}"))?;
    let submit = queue_submit_command(&queue, &branch, repo_name).stdin(Stdio::null()).output().map_err(|e| format!("rule: queue: {e}"))?;
    if !submit.status.success() {
        return Err(format!("rule: queue submit {branch} (bead {id}): {}", String::from_utf8_lossy(&submit.stderr).trim()));
    }
    println!("law/: committed on {branch}, carried by {id} (SUBMITTED).");
    Ok(branch)
}

/// Store slugs the landing-ref toc does not list and no pointer accounts for: the statute
/// book is behind the store by these.
pub fn behind(toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Vec<String> {
    let accounted = |s: &str| toc.iter().any(|p| p.slugs.iter().any(|t| t == s)) || pointers.retired.iter().any(|r| r.slug == s) || pointers.merged.iter().any(|m| m.from == s);
    book.keys().filter(|s| s.starts_with("law-") && !accounted(s)).cloned().collect()
}

/// `toc` without the slugs the store lacks, then with every behind slug appended in store key
/// order under Part 1, the enacted key under its own Part, and a `moved` row for each landed
/// slug whose section changed.
pub fn catch_up(toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>, key_part: Option<(&str, u32)>, landed: &BTreeMap<String, String>, today: &str) -> Result<(Vec<Part>, Pointers), String> {
    let mut out = without_missing(toc, pointers, book);
    for slug in behind(&out, pointers, book) {
        let part = match key_part {
            Some((k, n)) if k == slug => n,
            _ => 1,
        };
        out = append(&out, part, &slug)?;
    }
    let pointers = record_moves(landed, &out, pointers, today, book)?;
    Ok((out, pointers))
}

/// The newest unlanded `law/` or `spira/` branch cut on `base`'s current tip whose change stays
/// within the statute book, so a later law commit cut on it fast-forwards.
pub fn newest_book_only_tip(repo_path: &Path, base: &str) -> Option<String> {
    let refs = git_in(repo_path, &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", &format!("--no-merged={base}"), "refs/heads/law", "refs/heads/spira"]).ok()?;
    let held = base_book(repo_path, base);
    refs.lines().find(|r| {
        git_in(repo_path, &["merge-base", "--is-ancestor", base, r]).is_ok()
            && git_in(repo_path, &["diff", "--name-only", &format!("{base}...{r}")]).is_ok_and(|names| !names.is_empty() && names.lines().all(|n| n.starts_with("law/") || n.starts_with("spira/statutes/")))
            && !(r.starts_with("refs/heads/law/") && in_book(repo_path, r, held.as_ref()))
    }).map(str::to_string)
}

fn base_book(repo_path: &Path, base: &str) -> Option<(Vec<Part>, Pointers)> {
    let toc = parse_toc(&git_in(repo_path, &["show", &format!("{base}:law/toc.toml")]).ok()?).ok()?;
    let pointers = git_in(repo_path, &["show", &format!("{base}:law/{POINTERS_FILE}")]).ok().and_then(|t| parse_pointers(&t).ok()).unwrap_or_default();
    Some((toc, pointers))
}

/// Whether every slug and pointer record on `r` is already on the base.
fn in_book(repo_path: &Path, r: &str, held: Option<&(Vec<Part>, Pointers)>) -> bool {
    let Some((toc, pointers)) = held else { return false };
    let Some(theirs) = git_in(repo_path, &["show", &format!("{r}:law/toc.toml")]).ok().and_then(|t| parse_toc(&t).ok()) else { return false };
    let their_pointers = match git_in(repo_path, &["show", &format!("{r}:law/{POINTERS_FILE}")]) {
        Ok(t) => match parse_pointers(&t) {
            Ok(p) => p,
            Err(_) => return false,
        },
        Err(_) if git_in(repo_path, &["cat-file", "-e", &format!("{r}:law/{POINTERS_FILE}")]).is_err() => Pointers::default(),
        Err(_) => return false,
    };
    theirs.iter().all(|p| p.slugs.iter().all(|s| toc.iter().any(|b| b.slugs.contains(s))))
        && their_pointers.moved.iter().all(|m| pointers.moved.contains(m))
        && their_pointers.merged.iter().all(|m| pointers.merged.contains(m))
        && their_pointers.retired.iter().all(|m| pointers.retired.contains(m))
}

/// What `law_branches` finds a `law/*` branch not merged into the base to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LawBranch {
    Tip,
    CarriedBy(String),
    ReplacedBy(String),
    InBook,
    Unreconciled,
}

/// Every `law/*` branch not merged into `base`, newest first, with what becomes of it.
pub fn law_branches(repo_path: &Path, base: &str) -> Vec<(String, LawBranch)> {
    let Ok(refs) = git_in(repo_path, &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", &format!("--no-merged={base}"), "refs/heads/law"]) else {
        return Vec::new();
    };
    let held = base_book(repo_path, base);
    let tip = newest_book_only_tip(repo_path, base);
    refs.lines()
        .map(|r| {
            let kind = if in_book(repo_path, r, held.as_ref()) {
                LawBranch::InBook
            } else if tip.as_deref() == Some(r) {
                LawBranch::Tip
            } else if let Some(t) = tip.as_deref().filter(|t| git_in(repo_path, &["merge-base", "--is-ancestor", r, t]).is_ok()) {
                LawBranch::CarriedBy(t.strip_prefix("refs/heads/").unwrap_or(t).to_string())
            } else if let Some(t) = tip.as_deref().filter(|t| in_book(repo_path, r, base_book(repo_path, t).as_ref())) {
                LawBranch::ReplacedBy(t.strip_prefix("refs/heads/").unwrap_or(t).to_string())
            } else {
                LawBranch::Unreconciled
            };
            (r.strip_prefix("refs/heads/").unwrap_or(r).to_string(), kind)
        })
        .collect()
}

/// Where the newest book-only unlanded tip sits: a `law/*` branch no bead carries yet, or a
/// `spira/<id>` branch already submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookTip {
    Stranded(String),
    InFlight(String),
}

/// Names the branch [`newest_book_only_tip`] measures, short, classified by its namespace.
pub fn book_tip_ref(repo_path: &Path, base: &str) -> Option<BookTip> {
    let r = newest_book_only_tip(repo_path, base)?;
    let name = r.strip_prefix("refs/heads/").unwrap_or(&r).to_string();
    if name.starts_with("law/") {
        Some(BookTip::Stranded(name))
    } else {
        Some(BookTip::InFlight(name))
    }
}

/// Whether the tree `commit_law` would cut from (the newest book-only unlanded tip, else `base`)
/// holds anything but what `toc`, `pointers` and `book` generate.
pub fn book_drifted(repo_path: &Path, base: &str, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> bool {
    let rev = newest_book_only_tip(repo_path, base).unwrap_or_else(|| base.to_string());
    let mut files = generate(toc, pointers, |slug| book.get(slug).cloned());
    files.insert("toc.toml".to_string(), render_toc(toc));
    files.insert(POINTERS_FILE.to_string(), render_pointers(pointers));
    files.iter().any(|(f, want)| git_in(repo_path, &["cat-file", "blob", &format!("{rev}:law/{f}")]).map_or(true, |have| have.trim_end() != want.trim_end()))
}

/// `toc` plus every slug that a branch not yet on `base` has filed in a Part (a `law/` branch,
/// or the `spira/<bead>` branch one was submitted as), so two enacts made before either lands
/// take different section numbers.
pub fn with_in_flight(repo_path: &Path, base: Option<&str>, toc: Vec<Part>) -> Vec<Part> {
    let base = base.unwrap_or("HEAD");
    let Ok(refs) = git_in(repo_path, &["for-each-ref", "--format=%(refname)", &format!("--no-merged={base}"), "refs/heads/law", "refs/heads/spira"]) else {
        return toc;
    };
    let mut merged = toc;
    for r in refs.lines() {
        let Ok(text) = git_in(repo_path, &["show", &format!("{r}:law/toc.toml")]) else { continue };
        let Ok(theirs) = parse_toc(&text) else { continue };
        for p in &theirs {
            for slug in p.slugs.iter().filter(|s| s.starts_with("law-")) {
                if !merged.iter().any(|m| m.slugs.contains(slug)) {
                    if let Some(home) = merged.iter_mut().find(|m| m.number == p.number) {
                        home.slugs.push(slug.clone());
                    }
                }
            }
        }
    }
    merged
}

/// `pointers` plus every record a branch not yet on `base` has filed, so two retirements made
/// before either lands do not overwrite each other's pointer.
pub fn with_in_flight_pointers(repo_path: &Path, base: Option<&str>, pointers: Pointers) -> Pointers {
    let base = base.unwrap_or("HEAD");
    let Ok(refs) = git_in(repo_path, &["for-each-ref", "--format=%(refname)", &format!("--no-merged={base}"), "refs/heads/law", "refs/heads/spira"]) else {
        return pointers;
    };
    let mut merged = pointers;
    for r in refs.lines() {
        let Ok(text) = git_in(repo_path, &["show", &format!("{r}:law/{POINTERS_FILE}")]) else { continue };
        let Ok(theirs) = parse_pointers(&text) else { continue };
        for m in theirs.moved {
            if !merged.moved.contains(&m) {
                merged.moved.push(m);
            }
        }
        for m in theirs.merged {
            if !merged.merged.contains(&m) {
                merged.merged.push(m);
            }
        }
        for m in theirs.retired {
            if !merged.retired.contains(&m) {
                merged.retired.push(m);
            }
        }
    }
    merged
}

/// Files a SUBMITTED bead at the tip of `law_branch`, renames the branch to the bead's own
/// (`spira/<id>`, the name the queue lands) and submits it. Returns the bead id and the branch.
pub fn submit_law(repo_path: &Path, repo_name: &str, wt_root: &Path, law_branch: &str, verb: &str, key: &str) -> Result<(String, String), String> {
    let mut held = law_branch.to_string();
    submit_law_on(repo_path, repo_name, wt_root, law_branch, verb, key, &mut held).map_err(|e| format!("{e}\nrule: the law/ commit is kept on branch {held}"))
}

/// Renames `law_branch` to `branch`; a refiled item's existing branch is replaced, not refused.
pub fn name_law_branch(repo_path: &Path, law_branch: &str, branch: &str, refiled: bool) -> Result<(), String> {
    git_in(repo_path, &["branch", if refiled { "-M" } else { "-m" }, law_branch, branch]).map(|_| ())
}

fn submit_law_on(repo_path: &Path, repo_name: &str, wt_root: &Path, law_branch: &str, verb: &str, key: &str, held: &mut String) -> Result<(String, String), String> {
    let tip = git_in(repo_path, &["rev-parse", law_branch])?;
    let title = format!("law: {verb} {key} — land its law/ commit");
    let body = format!(
        "## Outcome\n\nThe law/ projection of the statute book after `rule.sh {verb} {key}` is on the landing ref.\n\n\
         ## Build\n\nThe commit at the tip of branch `{law_branch}` (`{tip}`), which rewrites the files under law/ (toc.toml, the section files and the pointers file). Nothing else changes: submit that branch as it stands.\n\n\
         ## Acceptance\n\n- The commit lands on the landing ref.\n- `rule.sh code --check` is clean afterwards.\n"
    );
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let body_file = wt_root.join(format!("rule-{verb}-{key}-{}-{stamp}.bead.txt", std::process::id()));
    std::fs::write(&body_file, body).map_err(|e| format!("rule: {}: {e}", body_file.display()))?;
    let filed = spira_config::bounded::bounded("bead.sh")
        .args(["file", &title, "--for", "builder", "--repo", repo_name, "--submitted", "--tip", &tip, "--json", "--body-file"])
        .arg(&body_file)
        .stdin(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&body_file);
    let filed = filed.map_err(|e| format!("rule: bead.sh: {e}"))?;
    if !filed.status.success() {
        return Err(format!("rule: bead.sh file: {}", String::from_utf8_lossy(&filed.stderr).trim()));
    }
    let said = String::from_utf8_lossy(&filed.stderr);
    if said.contains("reporting it as filed") {
        eprintln!("rule: the law/ item was already filed; submitting its branch");
    }
    let id = filed_id(&String::from_utf8_lossy(&filed.stdout))?;
    let branch = format!("spira/{id}");
    name_law_branch(repo_path, law_branch, &branch, said.contains("reporting it as filed"))?;
    *held = branch.clone();
    let queue = spira_config::release_env::tool("queue").map_err(|e| format!("rule: {e}"))?;
    let submit = queue_submit_command(&queue, &branch, repo_name).stdin(Stdio::null()).output().map_err(|e| format!("rule: queue: {e}"))?;
    if let Some(note) = submit_outcome(submit.status.code(), &String::from_utf8_lossy(&submit.stderr), &id, &branch)? {
        eprintln!("{note}");
    }
    Ok((id, branch))
}

/// Marks an error raised after the law/ item was filed: the item exists and the gate judged it red.
pub const RED_AFTER_FILING: &str = "(SUBMITTED) but red at the gate";

/// The closing line for a failed landing: a red on a filed item says so; anything earlier says the item was never submitted.
pub fn unlanded_message(err: &str, not_submitted: &str) -> String {
    if err.contains(RED_AFTER_FILING) {
        "rule: the law/ item is submitted and red; the gate's verdict above stands — fix the branch it names, then resubmit it".to_string()
    } else {
        not_submitted.to_string()
    }
}

/// Reads `queue submit`'s exit status. 75 (no verdict) and 76 (base red) are not a fault in the
/// law/ commit: the item stays SUBMITTED and the round's gate judges it, so they yield a note.
/// Any other failure is a red gate on a filed item.
pub fn submit_outcome(code: Option<i32>, stderr: &str, id: &str, branch: &str) -> Result<Option<String>, String> {
    let gate = stderr.lines().map(str::trim).filter(|l| l.contains("VERDICT=")).collect::<Vec<_>>().join(" ");
    let gate = if gate.is_empty() { stderr.split_whitespace().collect::<Vec<_>>().join(" ") } else { gate };
    match code {
        Some(0) => Ok(None),
        Some(75 | 76) => Ok(Some(format!("rule: {gate}; submitted under {id} (SUBMITTED); the round's gate judges it"))),
        _ => Err(format!("rule: queue submit {branch}: submitted under {id} {RED_AFTER_FILING}: {}", stderr.trim())),
    }
}

/// `queue submit <branch> <repo>`, with no call deadline: queue submit runs the gate before it
/// returns, and a 5 s `timeout` around it SIGTERMs the gate, which then records "died" (sp-esepir).
pub fn queue_submit_command(queue: &str, branch: &str, repo_name: &str) -> std::process::Command {
    // batch-job: runs the gate
    let mut c = std::process::Command::new(queue);
    c.args(["submit", branch, repo_name]);
    c
}

/// The id `bead.sh file --json` printed: the first JSON object (or one-element array) on stdout.
pub fn filed_id(out: &str) -> Result<String, String> {
    let start = out.find(['{', '[']).ok_or_else(|| format!("rule: no JSON in bead.sh file's output: {out:?}"))?;
    let v: serde_json::Value = serde_json::from_str(out[start..].trim()).map_err(|e| format!("rule: bead.sh file's output is not JSON: {e}"))?;
    let v = match v {
        serde_json::Value::Array(a) => a.into_iter().next().ok_or("rule: bead.sh file printed an empty JSON array")?,
        o => o,
    };
    v.get("id").and_then(|i| i.as_str()).map(str::to_string).ok_or_else(|| format!("rule: no id in bead.sh file's output: {out:?}"))
}

/// Writes `toc`, `pointers` and the generated tree under `law` and stages it.
pub fn write_law(law: &Path, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<(), String> {
    let files = generate(toc, pointers, |slug| book.get(slug).cloned());
    write_tree(law, &render_toc(toc), &files).map_err(|e| format!("rule: writing {}: {e}", law.display()))?;
    std::fs::write(law.join(POINTERS_FILE), render_pointers(pointers)).map_err(|e| format!("rule: writing {}: {e}", law.display()))?;
    let root = law.parent().unwrap_or(law);
    let add = spira_config::bounded::bounded("git").arg("-C").arg(root).args(["add", "-A", "--", "law"]).output();
    match add {
        Ok(o) if o.status.success() => {}
        _ => return Err("rule: git add law failed".to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod filed_id_tests {
    use super::filed_id;

    #[test]
    fn filed_id_accepts_the_object_bead_prints_for_an_already_filed_item() {
        assert_eq!(filed_id("{\"id\":\"sp-x\"}\n").as_deref(), Ok("sp-x"));
    }

    #[test]
    fn filed_id_on_a_bare_id_names_bead_sh_s_output() {
        let e = filed_id("sp-x\n").unwrap_err();
        assert!(e.contains("bead.sh file's output"), "{e}");
    }
}
