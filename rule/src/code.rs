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
/// A slug that ever held a number stays in the toc until retired, so a dropped one is refused.
pub fn record_moves(old: &BTreeMap<String, String>, new_toc: &[Part], pointers: &Pointers, today: &str) -> Result<Pointers, String> {
    if let Some(gone) = old.keys().find(|s| section_of(new_toc, s).is_none()) {
        return Err(format!("law/toc.toml no longer lists {gone}, which holds §{} — a numbered statute stays in the toc; retire it with `rule.sh retire` instead", old[gone]));
    }
    let mut out = pointers.clone();
    for (slug, was) in old {
        let now = section_of(new_toc, slug).unwrap_or_default();
        if now != *was && !out.moved.iter().any(|m| m.section == *was && m.slug == *slug) {
            out.moved.push(Moved { section: was.clone(), slug: slug.clone(), on: today.to_string() });
        }
    }
    Ok(out)
}

/// Problems with `pointers` against `toc`: every slug it names must be in the toc.
pub fn check_pointers(toc: &[Part], pointers: &Pointers) -> Vec<String> {
    let named = pointers
        .moved
        .iter()
        .map(|m| &m.slug)
        .chain(pointers.merged.iter().flat_map(|m| [&m.from, &m.into]))
        .chain(pointers.retired.iter().flat_map(|r| std::iter::once(&r.slug).chain(r.replaced_by.as_ref())));
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
fn without_citation(text: &str) -> &str {
    let mut rest = text.trim_end();
    while rest.ends_with(')') {
        let Some(i) = rest.rfind('(').filter(|&i| is_citation(&rest[i..])) else { break };
        rest = rest[..i].trim_end();
    }
    if rest.len() == text.trim_end().len() {
        text
    } else {
        rest
    }
}

fn is_citation(paren: &str) -> bool {
    paren.starts_with("(per ") || paren.strip_prefix("(sp-").is_some_and(|r| r.split_once(':').is_some_and(|(id, _)| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')))
}

fn first_sentence(text: &str) -> String {
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
                        (first_sentence(t), t.trim_end_matches('\n').to_string())
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

pub fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = spira_config::bounded::bounded("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("rule: git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("rule: git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Regenerates `law/` in a short-lived worktree of `repo`, commits it on a branch of its own and
/// removes the worktree. The caller's directory and the repo's own checkout are never touched.
#[allow(clippy::too_many_arguments)]
pub fn commit_law(repo_path: &Path, base: Option<&str>, wt_root: &Path, verb: &str, key: &str, toc: &[Part], pointers: &Pointers, book: &BTreeMap<String, String>) -> Result<String, String> {
    std::fs::create_dir_all(wt_root).map_err(|e| format!("rule: {}: {e}", wt_root.display()))?;
    let stamp = format!("{}-{}", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0), std::process::id());
    let branch = format!("law/{}-{stamp}", key.trim_start_matches("law-"));
    let wt = wt_root.join(format!("rule-{verb}-{}-{stamp}", key.trim_start_matches("law-")));
    let wt_s = wt.display().to_string();
    git_in(repo_path, &["worktree", "add", "-q", "-b", &branch, &wt_s, base.unwrap_or("HEAD")])?;
    let done = (|| {
        write_law(&wt.join("law"), toc, pointers, book)?;
        let mut child = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&wt)
            .args(["commit", "-q", "-F", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
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
            Err(format!("rule: git commit: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    })();
    let _ = git_in(repo_path, &["worktree", "remove", "--force", &wt_s]);
    done.map(|()| branch)
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
    let law_branch = commit_law(repo_path, repo.base, wt_root, verb, key, toc, pointers, book)?;
    submit_law(repo_path, repo_name, wt_root, &law_branch, verb, key).map_err(|e| format!("{e}\nrule: the law/ commit is on branch {law_branch}"))
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
            for slug in &p.slugs {
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
    let tip = git_in(repo_path, &["rev-parse", law_branch])?;
    let title = format!("law: {verb} {key} — land its law/ commit");
    let body = format!("{law_branch} carries the law/ projection of the statute book after `rule.sh {verb} {key}`. Landing this bead puts that commit on the landing ref.\n");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let body_file = wt_root.join(format!("rule-{verb}-{key}-{}-{stamp}.bead.txt", std::process::id()));
    std::fs::write(&body_file, body).map_err(|e| format!("rule: {}: {e}", body_file.display()))?;
    let filed = spira_config::bounded::bounded("bead.sh")
        .args(["file", &title, "--for", "ops", "--repo", repo_name, "--submitted", "--tip", &tip, "--json", "--body-file"])
        .arg(&body_file)
        .stdin(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&body_file);
    let filed = filed.map_err(|e| format!("rule: bead.sh: {e}"))?;
    if !filed.status.success() {
        return Err(format!("rule: bead.sh file: {}", String::from_utf8_lossy(&filed.stderr).trim()));
    }
    let id = filed_id(&String::from_utf8_lossy(&filed.stdout))?;
    let branch = format!("spira/{id}");
    git_in(repo_path, &["branch", "-m", law_branch, &branch])?;
    let submit = spira_config::bounded::bounded("queue").args(["submit", &branch, repo_name]).stdin(Stdio::null()).output().map_err(|e| format!("rule: queue: {e}"))?;
    if !submit.status.success() {
        return Err(format!("rule: queue submit {branch} (bead {id}): {}", String::from_utf8_lossy(&submit.stderr).trim()));
    }
    Ok((id, branch))
}

/// The id `bead.sh file --json` printed: the first JSON object (or one-element array) on stdout.
fn filed_id(out: &str) -> Result<String, String> {
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
