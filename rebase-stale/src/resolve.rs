//! Mechanical conflict resolution — the schema of which files resolve how, and the resolvers.
//!
//! During a rebase, index stage 2 ("ours") is the onto side (the land ref plus commits
//! already replayed) and stage 3 ("theirs") is the commit being replayed. Every union keeps
//! ours first. A stop resolves only when EVERY unmerged path is mechanical; otherwise nothing
//! is written and the non-mechanical paths come back with their hunks quoted.

use std::process::Command;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::git::Git;

/// How to regenerate a derived file instead of merging it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regen {
    /// Program and arguments, run in the worktree root.
    pub argv: Vec<String>,
    /// Tried when `argv` fails (e.g. the online form of an `--offline` command).
    pub fallback_argv: Option<Vec<String>>,
    /// The command's stdout IS the file (e.g. `spira-config schema`).
    pub stdout_to_file: bool,
    /// Write stage 2 into the file before running (the command amends it in place).
    pub seed_from_ours: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ResolveKind {
    /// Append-only line set: stage 2 in order, then stage 3's lines not already present.
    LineSetUnion,
    /// `SPIRA_*`/`COCKPIT_*` key-list regions of a shell file; result must pass `bash -n`.
    KeyListUnion,
    /// A derived file: regenerated from source, never merged.
    Regenerate(Regen),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveRule {
    pub path: String,
    pub kind: ResolveKind,
}

/// The resolution table. Paths not in `rules` fall back to comment-union when their comment
/// syntax is known, else they are a real conflict.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rules {
    pub rules: Vec<ResolveRule>,
    /// Where regeneration commands build (`CARGO_TARGET_DIR`), so a scratch tree does not
    /// rebuild the workspace from nothing on every stop.
    pub target_dir: Option<PathBuf>,
}

fn cargo(args: &[&str]) -> Vec<String> {
    std::iter::once("cargo")
        .chain(args.iter().copied())
        .map(String::from)
        .collect()
}

impl Rules {
    pub fn standard(target_dir: Option<PathBuf>) -> Rules {
        Rules {
            rules: vec![
                ResolveRule {
                    path: ".gitignore".into(),
                    kind: ResolveKind::LineSetUnion,
                },
                ResolveRule {
                    path: "spira/conf.key-history".into(),
                    kind: ResolveKind::LineSetUnion,
                },
                ResolveRule {
                    path: "spira/conf.sh".into(),
                    kind: ResolveKind::KeyListUnion,
                },
                ResolveRule {
                    path: "spira-config/schema/spira.schema.json".into(),
                    kind: ResolveKind::Regenerate(Regen {
                        argv: cargo(&["run", "-q", "--bin", "spira-config", "--", "schema"]),
                        fallback_argv: None,
                        stdout_to_file: true,
                        seed_from_ours: false,
                    }),
                },
                ResolveRule {
                    path: "Cargo.lock".into(),
                    // NOT `cargo generate-lockfile`: that re-resolves every dependency to the
                    // newest version. Starting from ours and letting `cargo metadata` fill in
                    // only what the branch added keeps every existing pin.
                    kind: ResolveKind::Regenerate(Regen {
                        argv: cargo(&["metadata", "-q", "--format-version", "1", "--offline"]),
                        fallback_argv: Some(cargo(&["metadata", "-q", "--format-version", "1"])),
                        stdout_to_file: false,
                        seed_from_ours: true,
                    }),
                },
            ],
            target_dir,
        }
    }

    fn kind_for(&self, path: &str) -> Option<&ResolveKind> {
        self.rules.iter().find(|r| r.path == path).map(|r| &r.kind)
    }
}

// ---------------------------------------------------------------------------------------
// Conflict markers
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Line(String),
    Conflict(Hunk),
}

/// Splits a file with two-way conflict markers. Err for an unterminated region or a diff3
/// base section (never produced here: the rebase forces `merge.conflictStyle=merge`).
pub fn parse_markers(text: &str) -> Result<Vec<Segment>, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if !l.starts_with("<<<<<<< ") && l != "<<<<<<<" {
            out.push(Segment::Line(l.to_string()));
            i += 1;
            continue;
        }
        let mut ours = Vec::new();
        let mut j = i + 1;
        loop {
            let x = lines.get(j).ok_or("unterminated conflict region")?;
            if x.starts_with("|||||||") {
                return Err("diff3 base section".into());
            }
            if x.starts_with("=======") {
                break;
            }
            ours.push(x.to_string());
            j += 1;
        }
        let mut theirs = Vec::new();
        let mut k = j + 1;
        loop {
            let x = lines.get(k).ok_or("unterminated conflict region")?;
            if x.starts_with(">>>>>>>") {
                break;
            }
            theirs.push(x.to_string());
            k += 1;
        }
        out.push(Segment::Conflict(Hunk { ours, theirs }));
        i = k + 1;
    }
    Ok(out)
}

fn render(segs: Vec<Segment>, mut merge: impl FnMut(Hunk) -> Vec<String>) -> String {
    let mut lines = Vec::new();
    for s in segs {
        match s {
            Segment::Line(l) => lines.push(l),
            Segment::Conflict(h) => lines.extend(merge(h)),
        }
    }
    lines.join("\n")
}

/// The marker regions of `text`, verbatim, at most `max` lines — what a returned bead quotes.
pub fn quote_hunks(text: &str, max: usize) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for l in text.split('\n') {
        if l.starts_with("<<<<<<<") {
            inside = true;
        }
        if inside {
            out.push(l);
        }
        if l.starts_with(">>>>>>>") {
            inside = false;
        }
        if out.len() >= max {
            break;
        }
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------------------
// Comment syntax
// ---------------------------------------------------------------------------------------

/// Line-comment prefixes for a path, or None when the file type has no comment syntax this
/// resolver trusts (Markdown and other prose: every line there is content).
pub fn comment_prefixes(path: &str, ours: Option<&str>) -> Option<&'static [&'static str]> {
    const SLASH: &[&str] = &["//"];
    const HASH: &[&str] = &["#"];
    const DASH: &[&str] = &["--"];
    let base = Path::new(path).file_name()?.to_str()?;
    if base == "Makefile" || base == ".gitignore" || base == ".gitattributes" {
        return Some(HASH);
    }
    match Path::new(base).extension().and_then(|e| e.to_str()) {
        Some("rs" | "js" | "ts" | "c" | "h" | "cc" | "cpp" | "go" | "java") => Some(SLASH),
        Some("sh" | "bash" | "py" | "toml" | "yml" | "yaml" | "conf" | "cfg") => Some(HASH),
        Some("sql" | "lua") => Some(DASH),
        Some(_) => None,
        None => {
            let first = ours?.lines().next()?;
            (first.starts_with("#!") && (first.contains("sh") || first.contains("python")))
                .then_some(HASH)
        }
    }
}

fn is_comment(line: &str, prefixes: &[&str]) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return true;
    }
    // `#!` / `#[` in a hash-comment file is still a comment line; `#[derive]` can only reach
    // here in a Rust file, whose prefix set does not include `#`.
    prefixes.iter().any(|p| t.starts_with(p))
}

/// Every region's lines, both sides, must be comments or blank. Union: ours, then theirs'
/// lines whose trimmed text ours does not already carry.
pub fn comment_union(text: &str, prefixes: &[&str]) -> Result<String, String> {
    let segs = parse_markers(text)?;
    let mut any = false;
    for s in &segs {
        if let Segment::Conflict(h) = s {
            any = true;
            if let Some(x) = h
                .ours
                .iter()
                .chain(&h.theirs)
                .find(|x| !is_comment(x, prefixes))
            {
                return Err(format!("not a comment-only hunk: {:?}", truncate(x, 80)));
            }
        }
    }
    if !any {
        return Err("no conflict markers (a modify/delete or binary conflict)".into());
    }
    Ok(render(segs, |h| {
        let have: std::collections::HashSet<String> =
            h.ours.iter().map(|x| x.trim().to_string()).collect();
        let mut v = h.ours.clone();
        v.extend(h.theirs.into_iter().filter(|x| !have.contains(x.trim())));
        v
    }))
}

/// Both sides only inserted lines (no base line changed or deleted): ours first, then theirs, at
/// each base position both filled. A `.rs` file's inserted blocks must each be brace-balanced.
/// Equal lines make an insertion's position ambiguous; the union is refused unless it reads the
/// same with every insertion placed as early as it can sit and as late.
pub fn insertion_union(base: &str, ours: &str, theirs: &str, rust: bool) -> Result<String, String> {
    let base: Vec<&str> = base.lines().collect();
    let (o, o_late) = insertions(&base, ours, "ours")?;
    let (t, t_late) = insertions(&base, theirs, "theirs")?;
    if rust {
        for (side, blocks) in [("ours", &o), ("theirs", &t)] {
            for (at, block) in blocks.iter().enumerate().filter(|(_, b)| !b.is_empty()) {
                if let Some(why) = brace_imbalance(block) {
                    return Err(format!(
                        "{side}'s inserted block before base line {} ({:?}) is {why}",
                        at + 1,
                        truncate(block[0], 60)
                    ));
                }
            }
        }
    }
    let out = interleave(&base, &o, &t);
    if out != interleave(&base, &o_late, &t_late) {
        return Err("an inserted line equals a base line, so where it sits is ambiguous".into());
    }
    Ok(out)
}

type Blocks<'a> = Vec<Vec<&'a str>>;

fn interleave(base: &[&str], o: &Blocks, t: &Blocks) -> String {
    let mut out = String::new();
    for i in 0..=base.len() {
        let theirs = if o[i] == t[i] { &[][..] } else { &t[i][..] };
        for l in o[i].iter().chain(theirs).chain(base.get(i)) {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// The lines `side` inserted before each base line (index `base.len()` = at the end), placed as
/// early as they can sit and as late, or Err when the base is not a subsequence of `side`.
fn insertions<'a>(base: &[&str], side: &'a str, who: &str) -> Result<(Blocks<'a>, Blocks<'a>), String> {
    let early = align(base, side.lines(), who)?;
    let rb: Vec<&str> = base.iter().rev().copied().collect();
    let mut late = align(&rb, side.lines().rev(), who)?;
    late.reverse();
    let late = late.into_iter().map(|b| b.into_iter().rev().collect()).collect();
    Ok((early, late))
}

fn align<'a>(base: &[&str], side: impl Iterator<Item = &'a str>, who: &str) -> Result<Blocks<'a>, String> {
    let mut at = vec![Vec::new(); base.len() + 1];
    let mut p = 0;
    for l in side {
        if p < base.len() && l == base[p] {
            p += 1;
        } else {
            at[p].push(l);
        }
    }
    if p < base.len() {
        return Err(format!("{who} changed or deleted base line {}", p + 1));
    }
    Ok(at)
}

/// Why `block`'s braces do not balance, counting `{`/`}` outside strings, chars and comments.
fn brace_imbalance(block: &[&str]) -> Option<String> {
    let text = block.join("\n");
    let c: Vec<char> = text.chars().collect();
    let (mut i, mut depth) = (0, 0i64);
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                let mut nest = 1;
                i += 2;
                while i < c.len() && nest > 0 {
                    match (c[i], c.get(i + 1)) {
                        ('/', Some('*')) => {
                            nest += 1;
                            i += 1;
                        }
                        ('*', Some('/')) => {
                            nest -= 1;
                            i += 1;
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            'r' if matches!(c.get(i + 1), Some('"' | '#')) && (i == 0 || !c[i - 1].is_alphanumeric() && c[i - 1] != '_') => {
                let hashes = c[i + 1..].iter().take_while(|x| **x == '#').count();
                if c.get(i + 1 + hashes) != Some(&'"') {
                    i += 1;
                    continue;
                }
                let close: Vec<char> = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                i += hashes + 2;
                while i < c.len() && !c[i..].starts_with(&close) {
                    i += 1;
                }
                i += close.len();
            }
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
            }
            '\'' => {
                if c.get(i + 1) == Some(&'\\') {
                    i += 2;
                    while i < c.len() && c[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if c.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            '{' => {
                depth += 1;
                i += 1;
            }
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return Some("closing a brace it never opened".into());
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    (depth != 0).then(|| format!("left {depth} brace(s) open"))
}

fn is_key_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return true;
    }
    t.split_whitespace().all(|tok| {
        let rest = tok.strip_prefix("SPIRA_").or_else(|| tok.strip_prefix("COCKPIT_"));
        matches!(rest, Some(r) if !r.is_empty() && r.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
    })
}

/// conf.sh key-list regions: both sides pure key lists; theirs' new keys appended to ours'
/// last line (keylist-union.py's rule).
pub fn keylist_union(text: &str) -> Result<String, String> {
    let segs = parse_markers(text)?;
    for s in &segs {
        if let Segment::Conflict(h) = s {
            if let Some(x) = h.ours.iter().chain(&h.theirs).find(|x| !is_key_line(x)) {
                return Err(format!("not a pure key list: {:?}", truncate(x, 80)));
            }
        }
    }
    Ok(render(segs, |h| {
        let have: std::collections::HashSet<&str> =
            h.ours.iter().flat_map(|x| x.split_whitespace()).collect();
        let mut new: Vec<String> = Vec::new();
        for t in h.theirs.iter().flat_map(|x| x.split_whitespace()) {
            if !have.contains(t) && !new.iter().any(|n| n == t) {
                new.push(t.to_string());
            }
        }
        let mut ours = h.ours.clone();
        if !new.is_empty() {
            match ours.iter().rposition(|x| !x.trim().is_empty()) {
                Some(i) => ours[i] = format!("{} {}", ours[i].trim_end(), new.join(" ")),
                None => ours = vec![new.join(" ")],
            }
        }
        ours
    }))
}

/// Append-only line set from the two stages.
pub fn lineset_union(ours: &str, theirs: &str) -> String {
    let body = ours.strip_suffix('\n').unwrap_or(ours);
    let mut lines: Vec<String> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(String::from).collect()
    };
    let mut seen: std::collections::HashSet<String> = lines.iter().cloned().collect();
    for l in theirs.lines() {
        if !l.trim().is_empty() && seen.insert(l.to_string()) {
            lines.push(l.to_string());
        }
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------------------------------
// One rebase stop
// ---------------------------------------------------------------------------------------

/// A path that did not resolve, with why and the hunks to quote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictFile {
    pub path: String,
    pub reason: String,
    pub quoted: String,
}

enum Plan {
    Write(String),
    Regen(Regen),
}

fn run_argv(argv: &[String], dir: &Path, target: Option<&Path>) -> Result<String, String> {
    let (prog, args) = argv.split_first().ok_or("empty regeneration command")?;
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut c = Command::new(prog);
    c.args(args).current_dir(dir).stdin(Stdio::null());
    if let Some(t) = target {
        c.env("CARGO_TARGET_DIR", t);
    }
    let o = c.output().map_err(|e| format!("{prog}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        return Err(format!(
            "{} exited {}: {}",
            argv.join(" "),
            o.status.code().unwrap_or(-1),
            err.lines().last().unwrap_or("").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn regenerate(git: &Git, path: &str, r: &Regen, target: Option<&Path>) -> Result<(), String> {
    let file = git.dir.join(path);
    if r.seed_from_ours {
        let ours = git
            .stage(2, path)
            .ok_or("no stage-2 copy to regenerate from")?;
        std::fs::write(&file, ours).map_err(|e| e.to_string())?;
    }
    let out = run_argv(&r.argv, &git.dir, target).or_else(|e| match &r.fallback_argv {
        Some(fb) => run_argv(fb, &git.dir, target),
        None => Err(e),
    })?;
    if r.stdout_to_file {
        if out.trim().is_empty() {
            return Err("regeneration printed nothing".into());
        }
        std::fs::write(&file, out).map_err(|e| e.to_string())?;
    }
    let now = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    if now
        .lines()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
    {
        return Err("conflict markers survived regeneration".into());
    }
    Ok(())
}

/// Resolves every unmerged path of the current stop, or none of them. Ok(n) = paths resolved.
pub fn resolve_stop(git: &Git, rules: &Rules) -> Result<usize, Vec<ConflictFile>> {
    let paths = git.unmerged_paths();
    let mut plans: Vec<(String, Plan)> = Vec::new();
    let mut failed = Vec::new();
    for p in &paths {
        let text = std::fs::read_to_string(git.dir.join(p)).unwrap_or_default();
        let fail = |reason: String| ConflictFile {
            path: p.clone(),
            reason,
            quoted: {
                let q = quote_hunks(&text, 60);
                if q.is_empty() {
                    "<no conflict markers in the file>".into()
                } else {
                    q
                }
            },
        };
        let plan = match rules.kind_for(p) {
            Some(ResolveKind::LineSetUnion) => match (git.stage(2, p), git.stage(3, p)) {
                (Some(o), Some(t)) => Ok(Plan::Write(lineset_union(&o, &t))),
                _ => Err("one side deleted an append-only file".to_string()),
            },
            Some(ResolveKind::KeyListUnion) => keylist_union(&text).map(Plan::Write),
            Some(ResolveKind::Regenerate(r)) => {
                if git.stage(2, p).is_some() && git.stage(3, p).is_some() {
                    Ok(Plan::Regen(r.clone()))
                } else {
                    Err("one side deleted a derived file".to_string())
                }
            }
            None => {
                let ours = git.stage(2, p);
                let inserted = match (git.stage(1, p), &ours, git.stage(3, p)) {
                    (Some(b), Some(o), Some(t)) => {
                        insertion_union(&b, o, &t, p.ends_with(".rs")).map_err(|e| format!("insertion union refused: {e}"))
                    }
                    _ => Err("insertion union needs all three stages".to_string()),
                };
                match inserted {
                    Ok(s) => Ok(Plan::Write(s)),
                    Err(why) => match comment_prefixes(p, ours.as_deref()) {
                        Some(pf) => comment_union(&text, pf).map(Plan::Write).map_err(|e| format!("{e}; {why}")),
                        None => Err(format!("not a mechanically resolvable file type; {why}")),
                    },
                }
            }
        };
        match plan {
            Ok(pl) => plans.push((p.clone(), pl)),
            Err(reason) => failed.push(fail(reason)),
        }
    }
    if !failed.is_empty() {
        return Err(failed);
    }
    // Unions first, then regenerations — a derived file is regenerated from resolved sources.
    let (writes, regens): (Vec<_>, Vec<_>) = plans
        .into_iter()
        .partition(|(_, pl)| matches!(pl, Plan::Write(_)));
    let fail_now = |p: &str, reason: String| {
        vec![ConflictFile {
            path: p.to_string(),
            reason,
            quoted: String::new(),
        }]
    };
    for (p, pl) in &writes {
        if let Plan::Write(s) = pl {
            std::fs::write(git.dir.join(p), s).map_err(|e| fail_now(p, e.to_string()))?;
            if rules.kind_for(p) == Some(&ResolveKind::KeyListUnion) {
                let ok = spira_config::bounded::bounded("bash")
                    .arg("-n")
                    .arg(git.dir.join(p))
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    return Err(fail_now(p, "key-list union does not pass bash -n".into()));
                }
            }
            if !git.ok(["add", "--", p.as_str()]) {
                return Err(fail_now(p, "git add failed".into()));
            }
        }
    }
    for (p, pl) in &regens {
        if let Plan::Regen(r) = pl {
            regenerate(git, p, r, rules.target_dir.as_deref())
                .map_err(|e| fail_now(p, format!("regeneration failed: {e}")))?;
            if !git.ok(["add", "--", p.as_str()]) {
                return Err(fail_now(p, "git add failed".into()));
            }
        }
    }
    Ok(writes.len() + regens.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tlwvchl)]
    #[test]
    fn lineset_keeps_ours_order_and_appends_theirs() {
        assert_eq!(
            lineset_union("a\nb\nc-main\n", "a\nb\nc-branch\n"),
            "a\nb\nc-main\nc-branch\n"
        );
        assert_eq!(lineset_union("a\n\nb\n", "a\nb\n\nz\n"), "a\n\nb\nz\n");
    }

    #[test_id(t57t3tr)]
    #[test]
    fn comment_union_merges_comment_only_regions() {
        let t = "fn a() {}\n<<<<<<< HEAD\n// from main\n=======\n// from branch\n>>>>>>> abc (x)\nfn b() {}\n";
        assert_eq!(
            comment_union(t, &["//"]).unwrap(),
            "fn a() {}\n// from main\n// from branch\nfn b() {}\n"
        );
    }

    #[test_id(tjxaq2l)]
    #[test]
    fn comment_union_refuses_code() {
        let t = "<<<<<<< HEAD\n// c\nlet x = 1;\n=======\n// d\n>>>>>>> abc\n";
        assert!(comment_union(t, &["//"]).is_err());
        // #[derive] is code in Rust, not a comment.
        let t = "<<<<<<< HEAD\n#[derive(Debug)]\n=======\n// d\n>>>>>>> abc\n";
        assert!(comment_union(t, comment_prefixes("x.rs", None).unwrap()).is_err());
    }

    #[test_id(tjs3ohm)]
    #[test]
    fn comment_syntax_is_per_file_type() {
        assert_eq!(comment_prefixes("a/b.rs", None), Some(&["//"][..]));
        assert_eq!(comment_prefixes("spira/x.sh", None), Some(&["#"][..]));
        assert_eq!(comment_prefixes("README.md", None), None);
        assert_eq!(
            comment_prefixes("bin/tool", Some("#!/usr/bin/env bash\n")),
            Some(&["#"][..])
        );
        assert_eq!(comment_prefixes("bin/blob", Some("\x7fELF")), None);
    }

    #[test_id(tyt81fr)]
    #[test]
    fn keylist_union_appends_new_keys_to_ours_last_line() {
        let t = "X=1\n<<<<<<< HEAD\nSPIRA_A SPIRA_B\n=======\nSPIRA_A SPIRA_C COCKPIT_D\n>>>>>>> abc\nY=2\n";
        assert_eq!(
            keylist_union(t).unwrap(),
            "X=1\nSPIRA_A SPIRA_B SPIRA_C COCKPIT_D\nY=2\n"
        );
        assert!(keylist_union("<<<<<<< HEAD\nSPIRA_A\n=======\necho hi\n>>>>>>> abc\n").is_err());
    }

    const BASE: &str = "mod tests {\n    #[test]\n    fn base() {\n    }\n}\n";
    fn with(test: &str) -> String {
        BASE.replace("}\n}\n", &format!("}}\n{test}}}\n"))
    }

    #[test_id(t1w0jp3)]
    #[test]
    fn insertion_union_keeps_ours_then_theirs_before_the_one_closing_brace() {
        let o = with("    #[test]\n    fn ours() {\n    }\n");
        let t = with("    #[test]\n    fn theirs() {\n    }\n");
        let got = insertion_union(BASE, &o, &t, true).unwrap();
        let want = with("    #[test]\n    fn ours() {\n    }\n    #[test]\n    fn theirs() {\n    }\n");
        assert_eq!(got, want);
    }

    #[test_id(tfjazkf)]
    #[test]
    fn insertion_union_refuses_a_side_that_edits_a_base_line() {
        let o = with("    fn ours() {\n    }\n");
        let t = BASE.replace("fn base()", "fn renamed()");
        assert!(insertion_union(BASE, &o, &t, true).unwrap_err().contains("theirs changed or deleted"));
    }

    #[test_id(tcmifbw)]
    #[test]
    fn insertion_union_refuses_a_block_with_an_unbalanced_brace() {
        let o = with("    fn ours() {\n");
        let t = with("    fn theirs() {\n    }\n");
        let e = insertion_union(BASE, &o, &t, true).unwrap_err();
        assert!(e.contains("ours's inserted block") && e.contains("fn ours"), "{e}");
    }

    #[test_id(twrs6rs)]
    #[test]
    fn insertion_union_ignores_braces_in_strings_chars_and_comments() {
        let o = with("    fn ours() {\n        let _ = (\"{\", '}', r#\"{\"#); // {\n    }\n");
        assert!(insertion_union(BASE, &o, BASE, true).is_ok());
    }

    #[test_id(tj5dy1h)]
    #[test]
    fn insertion_union_applies_insertions_at_different_places_as_they_are() {
        let o = format!("// top\n{BASE}");
        let t = with("    fn theirs() {\n    }\n");
        let got = insertion_union(BASE, &o, &t, true).unwrap();
        assert!(got.starts_with("// top\nmod tests") && got.contains("fn theirs"));
    }

    #[test_id(tdgzh3q)]
    #[test]
    fn the_none_arm_alone_calls_inserted_rust_a_real_conflict() {
        let text = "<<<<<<< HEAD\n    fn ours() {\n    }\n=======\n    fn theirs() {\n    }\n>>>>>>> x\n";
        let pf = comment_prefixes("a.rs", None).unwrap();
        assert!(comment_union(text, pf).unwrap_err().contains("not a comment-only hunk"));
    }

    #[test_id(t2mac32)]
    #[test]
    fn insertion_union_writes_a_block_both_sides_inserted_once() {
        let both = with("    use_it();\n");
        assert_eq!(insertion_union(BASE, &both, &both, true).unwrap(), both);
    }

    #[test_id(to8ljou)]
    #[test]
    fn insertion_union_refuses_an_insertion_beside_an_identical_base_line() {
        let base = "a\n\nb\n";
        let o = "a\n\nx\n\nb\n";
        let t = "a\ny\n\nb\n";
        let e = insertion_union(base, o, t, false).unwrap_err();
        assert!(e.contains("ambiguous"), "{e}");
    }
}
