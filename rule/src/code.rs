//! The statute code: `law/toc.toml` plus the statute texts generate a tree of Parts and
//! numbered sections. A section's number is its position in its Part's list and never changes.

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

/// Appends `slug` to Part `number`. A slug already in that Part is left where it is (an
/// amendment); one already in another Part is refused, since a number never moves.
pub fn append(parts: &[Part], number: u32, slug: &str) -> Result<Vec<Part>, String> {
    let mut out = parts.to_vec();
    if let Some(home) = out.iter().find(|p| p.slugs.iter().any(|s| s == slug)) {
        if home.number != number {
            return Err(format!("{slug} is already section of Part {} — a statute's number never changes", home.number));
        }
        return Ok(out);
    }
    let Some(part) = out.iter_mut().find(|p| p.number == number) else {
        return Err(format!("there is no Part {number}"));
    };
    part.slugs.push(slug.to_string());
    Ok(out)
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

/// Every file of the tree, keyed by path relative to `law/`. `toc.toml` is the input and is
/// not part of the output. A slug the book no longer holds renders as a reserved section so
/// the numbers after it stay put.
pub fn generate(parts: &[Part], text_of: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut top = String::from("# Statute code\n\n");
    for p in parts {
        top.push_str(&format!("- Part {} — {} — {}\n", p.number, p.name, p.scope));
        let mut idx = format!("# Part {} — {}\n\n{}\n\n", p.number, p.name, p.scope);
        for (i, slug) in p.slugs.iter().enumerate() {
            let sec = format!("{}.{}", p.number, i + 1);
            let text = text_of(slug);
            let (summary, body) = match &text {
                Some(t) => (first_sentence(t), t.trim_end_matches('\n').to_string()),
                None => ("[Reserved]".to_string(), "[Reserved] — no longer in force.".to_string()),
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

/// Differences between `expected` and the files under `dir` (`toc.toml` excluded).
pub fn drift(dir: &Path, expected: &BTreeMap<String, String>) -> Vec<String> {
    let mut on_disk = Vec::new();
    walk(dir, dir, &mut on_disk);
    on_disk.retain(|f| f != "toc.toml");
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

/// Makes `dir` hold exactly `expected` (plus `toc.toml`, which it writes from `toc`).
pub fn write_tree(dir: &Path, toc: &str, expected: &BTreeMap<String, String>) -> std::io::Result<()> {
    let mut stale = Vec::new();
    walk(dir, dir, &mut stale);
    for f in stale.into_iter().filter(|f| f != "toc.toml") {
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
pub fn commit_law(repo_path: &Path, base: Option<&str>, wt_root: &Path, key: &str, toc: &[Part], book: &BTreeMap<String, String>) -> Result<String, String> {
    std::fs::create_dir_all(wt_root).map_err(|e| format!("rule: {}: {e}", wt_root.display()))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let branch = format!("law/{}-{stamp}", key.trim_start_matches("law-"));
    let wt = wt_root.join(format!("rule-enact-{}-{stamp}", key.trim_start_matches("law-")));
    let wt_s = wt.display().to_string();
    git_in(repo_path, &["worktree", "add", "-q", "-b", &branch, &wt_s, base.unwrap_or("HEAD")])?;
    let done = (|| {
        write_law(&wt.join("law"), toc, book)?;
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
            let _ = writeln!(stdin, "law: enact {key}");
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

/// Writes `toc` and its generated tree under `law` and stages it.
pub fn write_law(law: &Path, toc: &[Part], book: &BTreeMap<String, String>) -> Result<(), String> {
    let files = generate(toc, |slug| book.get(slug).cloned());
    write_tree(law, &render_toc(toc), &files).map_err(|e| format!("rule: writing {}: {e}", law.display()))?;
    let root = law.parent().unwrap_or(law);
    let add = spira_config::bounded::bounded("git").arg("-C").arg(root).args(["add", "-A", "--", "law"]).output();
    match add {
        Ok(o) if o.status.success() => {}
        _ => return Err("rule: git add law failed".to_string()),
    }
    Ok(())
}
