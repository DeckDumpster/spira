//! `cr open`, `cr seal`, `cr unseal` and `cr status`: a review's life from staged to published.

use crate::cli::Outcome;
use crate::index::Index;
use crate::ledger::{Ledger, LedgerFile, Row};
use crate::pick;
use crate::ports::{Ports, SeededRng, SourceTree};
use crate::record::{self, Kind, Review, State};
use crate::rustsrc;
use crate::units;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

const DAY_MS: u64 = 86_400_000;
pub const OWNER_KEY: &str = "SPIRA_CR_REPO_OWNER";
pub const NAME_KEY: &str = "SPIRA_CR_REPO_NAME";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub path: String,
    pub line: u32,
    pub body: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    base_branch: Option<String>,
    base_sha: Option<String>,
    base_tree: Option<String>,
    blobs: BTreeMap<String, String>,
    tree: Option<String>,
    commit: Option<String>,
    ref_made: bool,
    pr: Option<(u64, String)>,
}

fn done(stdout: String) -> Outcome {
    Outcome { code: 0, stdout, stderr: String::new() }
}

fn refuse(code: i32, msg: &str) -> Outcome {
    Outcome { code, stdout: String::new(), stderr: format!("cr: {msg}\n") }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn slug(title: &str) -> String {
    let mut s = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.chars().take(40).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "unit".to_string()
    } else {
        s
    }
}

// ───────────────────────────── open ─────────────────────────────

pub struct OpenArgs {
    pub count: usize,
    pub aim: Option<String>,
    pub seed: Option<u64>,
    pub scheduled: bool,
}

pub fn open(a: &OpenArgs, ports: &Ports) -> Outcome {
    if a.count == 0 {
        return refuse(2, "--count must be at least 1");
    }
    let now = ports.clock.now_ms();
    let seed = a.seed.or_else(|| ports.env_seed.as_deref().and_then(|s| s.trim().parse().ok())).unwrap_or(now);
    let ix = match Index::build(ports.tree.as_ref()) {
        Ok(i) => i,
        Err(e) => return refuse(1, &e),
    };
    let all = units::all(&ix);
    let pool = match &a.aim {
        None => all,
        Some(aim) => match pick::resolve_aim(&ix, &all, aim) {
            Ok(m) => all.into_iter().filter(|u| m.unit_ids.contains(&u.id)).collect(),
            Err(tried) => return refuse(2, &format!("--aim {aim:?} matched nothing at commit {}\n  tried {}", ix.commit, tried.join("\n  tried "))),
        },
    };
    let scored = pick::score_all(&ix, ports.tree.as_ref(), pool, &ports.config, &ports.ledger, now);
    let mut rng = SeededRng::new(seed);
    let mut ledger = ports.ledger.clone();
    let mut chosen: Vec<(units::Unit, Vec<pick::Shaped>)> = Vec::new();
    for _ in 0..a.count {
        let left: Vec<pick::Scored> = scored.iter().filter(|s| !chosen.iter().any(|(u, _)| u.id == s.unit.id)).cloned().collect();
        let Some((i, _)) = pick::draw(&left, &ledger, &mut rng) else {
            return refuse(1, &format!("only {} of {} units are drawable: the rest are excluded or already reviewed at their current blobs", chosen.len(), a.count));
        };
        let shaped = pick::shape(&ix, &left[i].unit, &ports.config, &mut rng);
        let mut unit = left[i].unit.clone();
        for (f, s) in unit.files.iter_mut().zip(&shaped) {
            f.regions = s.regions.clone();
        }
        ledger.rows.push(Row { review: String::new(), at: now, commit: ix.commit.clone(), kind: unit.kind.as_str().to_string(), unit_id: unit.id.clone(), files: Vec::new() });
        chosen.push((unit, shaped));
    }
    let ledger_path = record::root(&ports.run_dir).join("ledger.jsonl");
    let mut out = String::new();
    for (unit, _) in chosen {
        let number = match record::allocate(&ports.run_dir) {
            Ok(n) => n,
            Err(e) => return refuse(1, &e),
        };
        let r = Review {
            number,
            kind: if a.scheduled { Kind::Scheduled } else { Kind::OnDemand },
            state: State::Staged,
            source_repo: ports.source_repo.clone(),
            source_sha: ix.commit.clone(),
            branch: format!("cr/{number:03}-{}", slug(&unit.title)),
            pr: None,
            pr_url: None,
            unit: unit.clone(),
            sealed_sha256: None,
            staged_at: now,
            opened_at: None,
            published_at: None,
            closed_at: None,
        };
        let row = Row {
            review: format!("{number:03}"),
            at: now,
            commit: ix.commit.clone(),
            kind: unit.kind.as_str().to_string(),
            unit_id: unit.id.clone(),
            files: unit.files.iter().map(|f| LedgerFile { path: f.path.clone(), blob: ix.blobs.get(&f.path).cloned().unwrap_or_default(), regions: f.regions.clone() }).collect(),
        };
        if let Err(e) = record::save(&ports.run_dir, &r).and_then(|()| Ledger::append(&ledger_path, &row)) {
            return refuse(1, &e);
        }
        out.push_str(&format!("{number:03} {}\nseal with: cr seal --review {number:03} < seal.json\n", unit.id));
    }
    done(out)
}

// ───────────────────────────── seal ─────────────────────────────

/// The branch's content for each unit file: bytes as at the source commit, or for an
/// oversized file its regions joined by one marker line naming each elided range.
fn branch_files(tree: &dyn SourceTree, unit: &units::Unit) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    for f in &unit.files {
        let bytes = tree.read(&f.path).ok_or_else(|| format!("{} is not readable at the source commit", f.path))?;
        let Some(regions) = &f.regions else {
            out.push((f.path.clone(), bytes));
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let segs: Vec<&str> = text.split_inclusive('\n').collect();
        let total = segs.len() as u32;
        let mut joined = String::new();
        let mut next = 1u32;
        let marker = |joined: &mut String, from: u32, to: u32| {
            if from <= to {
                if !joined.is_empty() && !joined.ends_with('\n') {
                    joined.push('\n');
                }
                joined.push_str(&format!("// [cr: source lines {from}-{to} elided]\n"));
            }
        };
        for &(a, b) in regions {
            let (a, b) = (a.max(1), b.min(total));
            if a > b {
                continue;
            }
            marker(&mut joined, next, a - 1);
            joined.push_str(&segs[(a - 1) as usize..b as usize].concat());
            next = b + 1;
        }
        marker(&mut joined, next, total);
        out.push((f.path.clone(), joined.into_bytes()));
    }
    Ok(out)
}

pub(crate) fn line_count(bytes: &[u8]) -> u32 {
    String::from_utf8_lossy(bytes).lines().count() as u32
}

fn elided(regions: &[(u32, u32)], total: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut next = 1;
    for &(a, b) in regions {
        if a > next {
            out.push((next, a - 1));
        }
        next = next.max(b + 1);
    }
    if next <= total {
        out.push((next, total));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cover {
    does: String,
    look_first: String,
}

struct SealInput {
    cover: Cover,
    comments: Vec<Comment>,
}

const SHAPE: &str = "stdin is one JSON object {\"does\": \"<sentences>\", \"look_first\": \"<sentences>\", \"comments\": [{path, line, body}, ...]}";

fn parse_input(stdin: &str) -> Result<SealInput, String> {
    let v: Value = serde_json::from_str(stdin).map_err(|e| format!("{SHAPE}: {e}"))?;
    let Some(obj) = v.as_object() else {
        return Err(format!("{SHAPE}; a bare array is refused"));
    };
    let prose = |key: &str| match obj.get(key).and_then(Value::as_str).map(str::trim) {
        Some(t) if t.lines().any(|l| l.starts_with("## ")) => Err(format!("`{key}` has a line starting `## `, which would be read as a section heading of the cover note")),
        Some(t) if !t.is_empty() => Ok(t.to_string()),
        _ => Err(format!("`{key}` is missing or blank; {SHAPE}")),
    };
    let cover = Cover { does: prose("does")?, look_first: prose("look_first")? };
    let comments: Vec<Comment> = serde_json::from_value(obj.get("comments").cloned().unwrap_or(Value::Null)).map_err(|e| format!("`comments` is not a JSON array of {{path, line, body}}: {e}"))?;
    Ok(SealInput { cover, comments })
}

fn validate(comments: &[Comment], files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if comments.is_empty() {
        return Err("the sealed review is empty: an empty array is refused".to_string());
    }
    for c in comments {
        let Some((_, bytes)) = files.iter().find(|(p, _)| *p == c.path) else {
            return Err(format!("comment path {:?} is outside the unit", c.path));
        };
        let n = line_count(bytes);
        if c.line == 0 || c.line > n {
            return Err(format!("comment on {} line {} is outside the file as it appears on the review branch ({n} lines)", c.path, c.line));
        }
    }
    Ok(())
}

fn age_days(from_ms: u64, to_ms: u64) -> u64 {
    to_ms.saturating_sub(from_ms) / DAY_MS
}

const SECTIONS: [&str; 6] = ["## What it does", "## What to look at first", "## Files", "## Who calls it", "## How often it changed", "## Tests that exercise it"];

pub(crate) fn missing_section(body: &str) -> Option<&'static str> {
    SECTIONS.into_iter().find(|h| {
        let Some(at) = body.lines().position(|l| l.trim_end() == *h) else { return true };
        !body.lines().skip(at + 1).take_while(|l| !l.starts_with("## ")).any(|l| !l.trim().is_empty())
    })
}

/// The unit's own files, its child modules, and the files that depend on it.
fn test_search_files(ix: &Index, u: &units::Unit, dependents: &[String]) -> Vec<String> {
    let mut out: Vec<String> = u.files.iter().map(|f| f.path.clone()).collect();
    for f in &u.files {
        let dir = format!("{}/", f.path.trim_end_matches(".rs"));
        out.extend(ix.files.iter().filter(|p| p.starts_with(&dir)).cloned());
    }
    out.extend(dependents.iter().cloned());
    out.retain(|p| p.ends_with(".rs"));
    out.sort();
    out.dedup();
    out
}

fn tests_by_call_path(ix: &Index, u: &units::Unit, dependents: &[String]) -> Vec<String> {
    let mut callees: Vec<String> = Vec::new();
    for f in &u.files {
        if let Some(t) = ix.text.get(&f.path) {
            let raw: Vec<&str> = t.lines().collect();
            callees.extend(rustsrc::pub_names(&raw, &rustsrc::code_lines(t)));
        }
    }
    callees.sort();
    callees.dedup();
    let mut found = Vec::new();
    for path in test_search_files(ix, u, dependents) {
        let Some(t) = ix.text.get(&path) else { continue };
        let raw: Vec<&str> = t.lines().collect();
        for (test, body) in rustsrc::test_fns(&raw, &rustsrc::code_lines(t)) {
            for c in &callees {
                if let Some(used) = used_callee(&body, c) {
                    found.push(format!("- `{path}::{test}` calls `{used}`\n"));
                    break;
                }
            }
        }
    }
    found
}

fn used_callee(body: &str, name: &str) -> Option<String> {
    if rustsrc::calls(body, name) {
        return Some(name.to_string());
    }
    let pat = format!("{name}::");
    body.match_indices(&pat).find(|(at, _)| !body[..*at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')).map(|(at, _)| {
        let rest = &body[at + pat.len()..];
        let tail: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        format!("{name}::{tail}")
    })
}

fn cover_note(r: &Review, tree: &dyn SourceTree, ix: &Index, files: &[(String, Vec<u8>)], cover: &Cover, sealed_sha: &str, ports: &Ports) -> String {
    let now = ports.clock.now_ms();
    let u = &r.unit;
    let mut m = format!(
        "Source repository: {}\nSource commit: {}\nsealed-sha256: {}\n\nThe Concierge's review of this unit is sealed and hashed above; it is posted after the operator publishes his.\nPlease comment with \"Add single comment\" rather than \"Start a review\": comments in a started review stay invisible until it is submitted.\n\n",
        r.source_repo, r.source_sha, sealed_sha
    );
    m.push_str(&format!("{}\n\n{}\n\n", SECTIONS[0], cover.does));
    m.push_str(&format!("{}\n\n{}\n\n", SECTIONS[1], cover.look_first));
    m.push_str(&format!("{}\n\n", SECTIONS[2]));
    for ((path, bytes), f) in files.iter().zip(&u.files) {
        let source_lines = ix.lines(path);
        m.push_str(&format!("- `{path}`: {} lines on this branch", line_count(bytes)));
        if let Some(regions) = &f.regions {
            let gaps: Vec<String> = elided(regions, source_lines).iter().map(|(a, b)| format!("{a}-{b}")).collect();
            m.push_str(&format!("; oversized ({source_lines} lines in the source), elided source lines {}", gaps.join(", ")));
        }
        m.push('\n');
    }
    let mut dependents = pick::dependent_files(ix, u);
    dependents.sort();
    dependents.dedup();
    m.push_str(&format!("\n{}\n\n", SECTIONS[3]));
    if dependents.is_empty() {
        m.push_str("No dependents found.\n");
    }
    for d in &dependents {
        m.push_str(&format!("- `{d}`\n"));
    }
    let since = now.saturating_sub(u64::from(ports.config.churn_days) * DAY_MS);
    let hist: Vec<_> = u.files.iter().map(|f| tree.history(&f.path, since)).collect();
    let total_lines: u32 = u.files.iter().map(|f| ix.lines(&f.path)).sum();
    let commits: u32 = hist.iter().map(|h| h.commits).sum();
    let first = hist.iter().filter_map(|h| h.oldest_ms).min();
    let mut authors: Vec<String> = u.files.iter().flat_map(|f| tree.authors(&f.path)).collect();
    authors.sort();
    authors.dedup();
    m.push_str(&format!("\n{}\n\n", SECTIONS[4]));
    m.push_str(&format!("- Lines: {total_lines}\n"));
    match first {
        Some(t) => m.push_str(&format!("- First commit: {} days before this review was staged\n", age_days(t, r.staged_at))),
        None => m.push_str("- First commit: unknown\n"),
    }
    m.push_str(&format!("- Commits in the last {} days: {commits}\n", ports.config.churn_days));
    m.push_str(&format!("- Authors: {}\n", if authors.is_empty() { "none recorded".to_string() } else { authors.join(", ") }));
    m.push_str(&format!("\n{}\n\n", SECTIONS[5]));
    let tests = tests_by_call_path(ix, u, &dependents);
    if tests.is_empty() {
        m.push_str("None found by call path.\n");
    }
    for t in tests {
        m.push_str(&t);
    }
    m
}

pub(crate) fn gh(ports: &Ports, method: &str, endpoint: &str, body: Option<&Value>) -> Result<Value, String> {
    let (args, input): (Vec<&str>, Option<Vec<u8>>) = match body {
        Some(b) => (vec!["api", "--method", method, endpoint, "--input", "-"], Some(b.to_string().into_bytes())),
        None => (vec!["api", endpoint], None),
    };
    let out = ports.gh.call_merged(None, &args, input.as_deref());
    if !out.ok() {
        return Err(format!("forge call failed (exit {}): gh {}\n{}", out.code, args.join(" "), out.text().trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("forge call gh {}: reply is not JSON ({e}): {}", args.join(" "), out.text().trim()))
}

fn field<'a>(v: &'a Value, path: &[&str], call: &str) -> Result<&'a Value, String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k).ok_or_else(|| format!("forge call {call}: reply has no `{}`", path.join(".")))?;
    }
    Ok(cur)
}

pub(crate) fn text_field(v: &Value, path: &[&str], call: &str) -> Result<String, String> {
    field(v, path, call)?.as_str().map(str::to_string).ok_or_else(|| format!("forge call {call}: `{}` is not a string", path.join(".")))
}

fn load_progress(dir: &Path) -> Progress {
    std::fs::read_to_string(dir.join("progress.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_progress(dir: &Path, p: &Progress) -> Result<(), String> {
    record::write_atomic(&dir.join("progress.json"), serde_json::to_string_pretty(p).map_err(|e| e.to_string())?.as_bytes())
}

pub(crate) fn repo_refusal(ports: &Ports) -> Option<Outcome> {
    for (key, v) in [(OWNER_KEY, &ports.repo_owner), (NAME_KEY, &ports.repo_name)] {
        if v.trim().is_empty() {
            return Some(refuse(2, &format!("{key} is empty; name the review repository in the config, it has no default")));
        }
    }
    None
}

pub fn seal(number: u32, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    let mut r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    if r.state != State::Staged {
        return refuse(2, &format!("review {number:03} is {}, not staged; only a staged review can be sealed", r.state.as_str()));
    }
    if let Some(o) = repo_refusal(ports) {
        return o;
    }
    let dir = record::dir(run, number);
    let source = match ports.tree.at(&r.source_sha) {
        Ok(t) => t,
        Err(e) => return refuse(1, &e),
    };
    let files = match branch_files(source.as_ref(), &r.unit) {
        Ok(f) => f,
        Err(e) => return refuse(1, &e),
    };
    let sealed_path = dir.join("sealed.json");
    let cover_path = dir.join("cover.json");
    let stored_cover: Option<Cover> = std::fs::read(&cover_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let supplied = if ports.stdin.trim().is_empty() { None } else { Some(parse_input(&ports.stdin)) };
    if let Some(Err(e)) = &supplied {
        return refuse(2, e);
    }
    if sealed_path.exists() {
        if let Some(Ok(input)) = &supplied {
            let stored: Option<Vec<Comment>> = std::fs::read(&sealed_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
            if Some(&input.comments) != stored.as_ref() {
                return refuse(2, &format!("review {number:03} is already sealed with different comments; a sealed review is not replaced"));
            }
            if stored_cover.as_ref().is_some_and(|c| *c != input.cover) {
                return refuse(2, &format!("review {number:03} is already sealed with a different cover; a sealed review is not replaced"));
            }
        }
    }
    if !sealed_path.exists() {
        let input = match &supplied {
            Some(Ok(i)) => i,
            Some(Err(e)) => return refuse(2, e),
            None => return refuse(2, &format!("stdin is empty; {SHAPE}")),
        };
        if let Err(e) = validate(&input.comments, &files) {
            return refuse(2, &e);
        }
        let bytes = serde_json::to_string_pretty(&input.comments).expect("comments serialise").into_bytes();
        let cover_bytes = serde_json::to_string_pretty(&input.cover).expect("cover serialises").into_bytes();
        r.sealed_sha256 = Some(sha256_hex(&bytes));
        if let Err(e) = record::write_atomic(&cover_path, &cover_bytes).and_then(|()| record::write_atomic(&sealed_path, &bytes)).and_then(|()| record::save(run, &r)) {
            return refuse(1, &e);
        }
    }
    let cover = match std::fs::read(&cover_path).ok().and_then(|b| serde_json::from_slice::<Cover>(&b).ok()) {
        Some(c) => c,
        None => match &supplied {
            Some(Ok(i)) => {
                if let Err(e) = record::write_atomic(&cover_path, serde_json::to_string_pretty(&i.cover).expect("cover serialises").as_bytes()) {
                    return refuse(1, &e);
                }
                i.cover.clone()
            }
            _ => return refuse(2, &format!("review {number:03} has no cover.json; supply the cover again. {SHAPE}")),
        },
    };
    let sealed = match std::fs::read(&sealed_path) {
        Ok(b) => b,
        Err(e) => return refuse(1, &format!("reading {}: {e}", sealed_path.display())),
    };
    let sha = sha256_hex(&sealed);
    if r.sealed_sha256.as_deref() != Some(sha.as_str()) {
        return refuse(1, &format!("{} no longer hashes to the sealed review recorded for review {number:03}", sealed_path.display()));
    }
    let ix = match Index::build(source.as_ref()) {
        Ok(i) => i,
        Err(e) => return refuse(1, &e),
    };
    let body = cover_note(&r, source.as_ref(), &ix, &files, &cover, &sha, ports);
    if let Some(h) = missing_section(&body) {
        return refuse(2, &format!("the cover note has no text under {h:?}; nothing is sent to the forge"));
    }

    let mut p = load_progress(&dir);
    match push_branch(&mut p, &dir, &r, &files, &body, ports) {
        Ok((pr, url)) => {
            r.pr = Some(pr);
            r.pr_url = Some(url.clone());
            if let Err(e) = record::save(run, &r).and_then(|()| record::set_state(run, number, State::Open, ports.clock.now_ms()).map(|_| ())) {
                return refuse(1, &e);
            }
            done(format!("{url}\n"))
        }
        Err(e) => refuse(1, &e),
    }
}

fn push_branch(p: &mut Progress, dir: &Path, r: &Review, files: &[(String, Vec<u8>)], body: &str, ports: &Ports) -> Result<(u64, String), String> {
    let repo = format!("repos/{}/{}", ports.repo_owner, ports.repo_name);
    if p.base_sha.is_none() {
        let info = gh(ports, "GET", &repo, None)?;
        let branch = text_field(&info, &["default_branch"], &repo)?;
        let ep = format!("{repo}/branches/{branch}");
        let b = gh(ports, "GET", &ep, None)?;
        p.base_sha = Some(text_field(&b, &["commit", "sha"], &ep)?);
        p.base_tree = Some(text_field(&b, &["commit", "commit", "tree", "sha"], &ep)?);
        p.base_branch = Some(branch);
        save_progress(dir, p)?;
    }
    for (path, bytes) in files {
        if p.blobs.contains_key(path) {
            continue;
        }
        let ep = format!("{repo}/git/blobs");
        let v = gh(ports, "POST", &ep, Some(&json!({"content": base64::engine::general_purpose::STANDARD.encode(bytes), "encoding": "base64"})))?;
        p.blobs.insert(path.clone(), text_field(&v, &["sha"], &ep)?);
        save_progress(dir, p)?;
    }
    if p.tree.is_none() {
        let ep = format!("{repo}/git/trees");
        let entries: Vec<Value> = files.iter().map(|(path, _)| json!({"path": path, "mode": "100644", "type": "blob", "sha": p.blobs[path]})).collect();
        let v = gh(ports, "POST", &ep, Some(&json!({"base_tree": p.base_tree, "tree": entries})))?;
        p.tree = Some(text_field(&v, &["sha"], &ep)?);
        save_progress(dir, p)?;
    }
    if p.commit.is_none() {
        let ep = format!("{repo}/git/commits");
        let msg = format!("review {:03}: {}\n\nsource {} at {}", r.number, r.unit.title, r.source_repo, r.source_sha);
        let v = gh(ports, "POST", &ep, Some(&json!({"message": msg, "tree": p.tree, "parents": [p.base_sha]})))?;
        p.commit = Some(text_field(&v, &["sha"], &ep)?);
        save_progress(dir, p)?;
    }
    if !p.ref_made {
        let ep = format!("{repo}/git/refs");
        if let Err(e) = gh(ports, "POST", &ep, Some(&json!({"ref": format!("refs/heads/{}", r.branch), "sha": p.commit}))) {
            let existing = gh(ports, "GET", &format!("{repo}/git/ref/heads/{}", r.branch), None).ok().and_then(|v| v.get("object")?.get("sha")?.as_str().map(str::to_string));
            if existing != p.commit {
                return Err(e);
            }
        }
        p.ref_made = true;
        save_progress(dir, p)?;
    }
    if p.pr.is_none() {
        let ep = format!("{repo}/pulls");
        let v = gh(ports, "POST", &ep, Some(&json!({"title": r.unit.title, "head": r.branch, "base": p.base_branch, "body": body})))?;
        let n = field(&v, &["number"], &ep)?.as_u64().ok_or_else(|| format!("forge call {ep}: `number` is not an integer"))?;
        p.pr = Some((n, text_field(&v, &["html_url"], &ep)?));
        save_progress(dir, p)?;
    }
    Ok(p.pr.clone().expect("pr was just set"))
}

// ───────────────────────────── unseal ─────────────────────────────

pub fn unseal(number: u32, said: Option<&str>, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    let mut r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    if let Some(o) = repo_refusal(ports) {
        return o;
    }
    let Some(pr) = r.pr else {
        return refuse(2, &format!("review {number:03} has no pull request; seal it first"));
    };
    let repo = format!("repos/{}/{}", ports.repo_owner, ports.repo_name);
    match (r.state, said) {
        (State::Published, _) => {}
        (State::Open, Some(id)) => {
            let ep = format!("{repo}/issues/comments/{id}");
            let c = match gh(ports, "GET", &ep, None) {
                Ok(c) => c,
                Err(e) => return refuse(1, &e),
            };
            let on_this_pr = c.get("issue_url").and_then(Value::as_str).is_some_and(|u| u.rsplit('/').take(2).eq([pr.to_string().as_str(), "issues"]));
            if !on_this_pr {
                return refuse(2, &format!("comment {id} is not a conversation comment on pull request {pr}"));
            }
            let kind = c.get("user").and_then(|u| u.get("type")).and_then(Value::as_str);
            if kind.is_none() || kind == Some("Bot") {
                return refuse(2, &format!("comment {id} is not by a person; the operator has not published"));
            }
            r = match record::set_state(run, number, State::Published, ports.clock.now_ms()) {
                Ok(r) => r,
                Err(e) => return refuse(1, &e),
            };
        }
        (state, _) => {
            return refuse(2, &format!("review {number:03} is {}; unseal waits for the operator to publish, or for --said <comment-id> naming his comment", state.as_str()));
        }
    }
    let dir = record::dir(run, number);
    if dir.join("posted").exists() {
        return refuse(2, &format!("review {number:03} was already posted"));
    }
    let sealed = match std::fs::read(dir.join("sealed.json")) {
        Ok(b) => b,
        Err(e) => return refuse(1, &format!("reading sealed.json: {e}")),
    };
    if r.sealed_sha256.as_deref() != Some(sha256_hex(&sealed).as_str()) {
        return refuse(2, &format!("sealed.json no longer hashes to the sha256 recorded when review {number:03} was sealed; nothing is posted"));
    }
    let comments: Vec<Comment> = match serde_json::from_slice(&sealed) {
        Ok(c) => c,
        Err(e) => return refuse(1, &format!("sealed.json: {e}")),
    };
    let payload: Vec<Value> = comments.iter().map(|c| json!({"path": c.path, "line": c.line, "side": "RIGHT", "body": c.body})).collect();
    let ep = format!("{repo}/pulls/{pr}/reviews");
    if let Err(e) = gh(ports, "POST", &ep, Some(&json!({"event": "COMMENT", "comments": payload}))) {
        return refuse(1, &e);
    }
    if let Err(e) = record::write_atomic(&dir.join("posted"), b"") {
        return refuse(1, &e);
    }
    done(format!("posted {} comments on pull request {pr}\n", comments.len()))
}

// ───────────────────────────── status ─────────────────────────────

fn age(from: u64, now: u64) -> String {
    let ms = now.saturating_sub(from);
    if ms >= DAY_MS {
        format!("{}d", ms / DAY_MS)
    } else {
        format!("{}h", ms / 3_600_000)
    }
}

pub fn status(json_out: bool, ports: &Ports) -> Outcome {
    let rs = match record::all(&ports.run_dir) {
        Ok(r) => r,
        Err(e) => return refuse(1, &e),
    };
    let now = ports.clock.now_ms();
    if json_out {
        let v: Vec<Value> = rs
            .iter()
            .map(|r| json!({"number": format!("{:03}", r.number), "kind": r.kind.as_str(), "state": r.state.as_str(), "age": age(r.staged_at, now), "pr": r.pr, "url": r.pr_url, "title": r.unit.title}))
            .collect();
        return done(format!("{}\n", Value::Array(v)));
    }
    let mut out = String::new();
    for r in rs {
        out.push_str(&format!("{:03} {} {} {} {} {}\n", r.number, r.kind.as_str(), r.state.as_str(), age(r.staged_at, now), r.pr_url.as_deref().unwrap_or("-"), r.unit.title));
    }
    done(out)
}
