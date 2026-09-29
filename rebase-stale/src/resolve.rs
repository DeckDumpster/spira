//! Mechanical conflict resolution — the schema of which files resolve how, and the resolvers.
//!
//! During a rebase, index stage 2 ("ours") is the onto side (the land ref plus commits
//! already replayed) and stage 3 ("theirs") is the commit being replayed. Every union keeps
//! ours first. A stop resolves only when EVERY unmerged path is mechanical; otherwise nothing
//! is written and the non-mechanical paths come back with their hunks quoted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
                    path: "spira-config/schema/spira-key-history.txt".into(),
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
                match comment_prefixes(p, ours.as_deref()) {
                    Some(pf) => comment_union(&text, pf).map(Plan::Write),
                    None => Err("not a mechanically resolvable file type".to_string()),
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
                let ok = Command::new("bash")
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

    #[test]
    fn lineset_keeps_ours_order_and_appends_theirs() {
        assert_eq!(
            lineset_union("a\nb\nc-main\n", "a\nb\nc-branch\n"),
            "a\nb\nc-main\nc-branch\n"
        );
        assert_eq!(lineset_union("a\n\nb\n", "a\nb\n\nz\n"), "a\n\nb\nz\n");
    }

    #[test]
    fn comment_union_merges_comment_only_regions() {
        let t = "fn a() {}\n<<<<<<< HEAD\n// from main\n=======\n// from branch\n>>>>>>> abc (x)\nfn b() {}\n";
        assert_eq!(
            comment_union(t, &["//"]).unwrap(),
            "fn a() {}\n// from main\n// from branch\nfn b() {}\n"
        );
    }

    #[test]
    fn comment_union_refuses_code() {
        let t = "<<<<<<< HEAD\n// c\nlet x = 1;\n=======\n// d\n>>>>>>> abc\n";
        assert!(comment_union(t, &["//"]).is_err());
        // #[derive] is code in Rust, not a comment.
        let t = "<<<<<<< HEAD\n#[derive(Debug)]\n=======\n// d\n>>>>>>> abc\n";
        assert!(comment_union(t, comment_prefixes("x.rs", None).unwrap()).is_err());
    }

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

    #[test]
    fn keylist_union_appends_new_keys_to_ours_last_line() {
        let t = "X=1\n<<<<<<< HEAD\nSPIRA_A SPIRA_B\n=======\nSPIRA_A SPIRA_C COCKPIT_D\n>>>>>>> abc\nY=2\n";
        assert_eq!(
            keylist_union(t).unwrap(),
            "X=1\nSPIRA_A SPIRA_B SPIRA_C COCKPIT_D\nY=2\n"
        );
        assert!(keylist_union("<<<<<<< HEAD\nSPIRA_A\n=======\necho hi\n>>>>>>> abc\n").is_err());
    }

    #[test]
    fn markers_quote_and_reject_malformed() {
        let t = "a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> c\nb";
        assert_eq!(quote_hunks(t, 60), "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> c");
        assert!(parse_markers("<<<<<<< HEAD\nx\n").is_err());
        assert!(parse_markers("<<<<<<< HEAD\nx\n||||||| base\nz\n=======\ny\n>>>>>>> c").is_err());
    }
}
