//! The statute code: the book's `toc.toml` plus the statute texts generate a tree of Parts and
//! numbered sections. A slug is a statute's identity; its §N.M number is presentation, derived
//! from `toc.toml`. A change to the book leaves a pointer in `pointers.toml`, never a gap.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub number: u32,
    pub name: String,
    pub slug: String,
    pub scope: String,
    pub slugs: Vec<String>,
}

pub fn parse_toc(text: &str) -> Result<Vec<Part>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("toc.toml: {e}"))?;
    let rows = match table.get("part") {
        Some(toml::Value::Array(a)) => a.clone(),
        Some(_) => return Err("toc.toml: `part` must be an array of tables".into()),
        None => Vec::new(),
    };
    let mut parts: Vec<Part> = Vec::new();
    for row in &rows {
        let str_of = |k: &str| row.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| format!("toc.toml: a part has no string `{k}`"));
        let number = row
            .get("number")
            .and_then(|v| v.as_integer())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or("toc.toml: a part has no positive integer `number`")?;
        let slugs = match row.get("slugs") {
            Some(toml::Value::Array(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or("toc.toml: `slugs` must be strings")).collect::<Result<Vec<_>, _>>()?,
            _ => return Err(format!("toc.toml: part {number} has no `slugs` array")),
        };
        let part = Part { number, name: str_of("name")?, slug: str_of("slug")?, scope: str_of("scope")?, slugs };
        if parts.iter().any(|p| p.number == number) {
            return Err(format!("toc.toml: part {number} appears twice"));
        }
        parts.push(part);
    }
    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    for p in &parts {
        for s in &p.slugs {
            if let Some(other) = seen.insert(s, p.number) {
                return Err(format!("toc.toml: {s} is in part {other} and part {}", p.number));
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
        return "  (no Parts in toc.toml)\n".to_string();
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

/// `pointers.toml`: where a statute's old number, or the statute itself, went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pointers {
    pub moved: Vec<Moved>,
    pub merged: Vec<Merged>,
    pub retired: Vec<Retired>,
}

fn pointer_rows(table: &toml::Table, kind: &str, keys: &[&str], optional: &[&str]) -> Result<Vec<BTreeMap<String, String>>, String> {
    let file = "pointers.toml";
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
    let file = "pointers.toml";
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
        return Err(format!("toc.toml no longer lists {gone}, which holds §{} and is still in the store — a statute in force stays in the toc; retire it with `rule.sh retire` instead", old[gone]));
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
            return Err(format!("{slug} is already section of Part {} — to move it, edit toc.toml and run `rule.sh code`", home.number));
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

/// Every file of the tree, keyed by path relative to the book directory. `toc.toml` and `pointers.toml` are
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

/// Writes `toc`, `pointers` and the generated tree under `law`.
pub fn write_law(law: &Path, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<(), String> {
    let files = generate(toc, pointers, |slug| book.get(slug).cloned());
    write_tree(law, &render_toc(toc), &files).map_err(|e| format!("rule: writing {}: {e}", law.display()))?;
    std::fs::write(law.join(POINTERS_FILE), render_pointers(pointers)).map_err(|e| format!("rule: writing {}: {e}", law.display()))
}

/// The brain checkout and the book's directory within it, from `SPIRA_WIKI` and `SPIRA_LAW_BOOK`.
pub fn wiki_and_book() -> Result<(std::path::PathBuf, String), String> {
    let key = |k: &str| match spira_config::process::cfg(k) {
        Ok(v) if !v.is_empty() => Ok(v),
        Ok(_) => Err(format!("rule: {k} is empty — the statute book lives in the wiki checkout it names")),
        Err(e) => Err(format!("rule: {k}: {e}")),
    };
    let wiki = key("SPIRA_WIKI")?;
    let rel = key("SPIRA_LAW_BOOK")?;
    if !Path::new(&wiki).join(".git").exists() {
        return Err(format!("rule: SPIRA_WIKI {wiki} is not a git checkout — refusing"));
    }
    let rel_path = Path::new(&rel);
    if rel_path.is_absolute() || rel_path.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("rule: SPIRA_LAW_BOOK {rel} must be a plain path relative to SPIRA_WIKI"));
    }
    Ok((std::path::PathBuf::from(wiki), rel.trim_end_matches('/').to_string()))
}

/// The directory that holds the statute book: `SPIRA_LAW_BOOK` within `SPIRA_WIKI`.
pub fn book_dir() -> Result<std::path::PathBuf, String> {
    wiki_and_book().map(|(wiki, rel)| wiki.join(rel))
}

/// Paths, relative to `wiki`, under `rel` that git sees as added, modified or deleted.
pub fn changed_paths(wiki: &Path, rel: &str) -> Result<Vec<String>, String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(wiki)
        .args(["status", "--porcelain", "-z", "--untracked-files=all", "--"])
        .arg(rel)
        .output()
        .map_err(|e| format!("rule: git: {e}"))?;
    if !out.status.success() {
        return Err(format!("rule: git status {rel}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split('\0').filter(|e| e.len() > 3).map(|e| e[3..].to_string()).collect())
}

/// What a book commit does: the slugs it adds and drops and the pointer records it writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookChange {
    pub added: Vec<String>,
    pub dropped: Vec<String>,
    pub moved: usize,
    pub retired: usize,
    pub merged: usize,
}

pub fn book_change(before: &[Part], before_ptrs: &Pointers, after: &[Part], after_ptrs: &Pointers) -> BookChange {
    let slugs = |parts: &[Part]| parts.iter().flat_map(|p| p.slugs.iter().cloned()).collect::<Vec<_>>();
    let (was, now) = (slugs(before), slugs(after));
    BookChange {
        added: now.iter().filter(|s| !was.contains(s)).cloned().collect(),
        dropped: was.iter().filter(|s| !now.contains(s)).cloned().collect(),
        moved: after_ptrs.moved.iter().filter(|r| !before_ptrs.moved.contains(r)).count(),
        retired: after_ptrs.retired.iter().filter(|r| !before_ptrs.retired.contains(r)).count(),
        merged: after_ptrs.merged.iter().filter(|r| !before_ptrs.merged.contains(r)).count(),
    }
}

impl std::fmt::Display for BookChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == BookChange::default() {
            return write!(f, "no statute added or dropped, no pointer record written");
        }
        write!(f, "added {}", self.added.len())?;
        if !self.added.is_empty() {
            write!(f, " ({})", self.added.join(", "))?;
        }
        write!(f, "; dropped {}", self.dropped.len())?;
        if !self.dropped.is_empty() {
            write!(f, " ({})", self.dropped.join(", "))?;
        }
        write!(f, "; {} move(s), {} retirement(s), {} merge(s) recorded", self.moved, self.retired, self.merged)
    }
}
