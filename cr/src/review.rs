//! `cr open`, `cr seal`, `cr unseal`, `cr close` and `cr status`: a review's life from staged to closed.

use crate::cli::Outcome;
use crate::coverage::{self, Coverage, Tally};
use crate::findings;
use crate::history;
use crate::index::Index;
use crate::ledger::{self, ClosedAs, LedgerFile, Row};
use crate::pick;
use crate::ports::{Ports, SeededRng, SourceTree};
use crate::record::{self, Kind, Review, State};
use crate::rustsrc;
use crate::units;
use serde::{Deserialize, Serialize};
use margin_client::{Anchor, Doc, Source};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use sha2::{Digest, Sha256};
use std::path::Path;

const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub path: String,
    pub line: u32,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sealed {
    pub lens: Vec<Comment>,
    pub fresh: Vec<Comment>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredSeal {
    Halves { lens: Vec<Comment>, fresh: Vec<Comment> },
    Single(Vec<Comment>),
}

impl Sealed {
    /// `None` when the review was never sealed. A review sealed as one undivided array is all lens.
    pub(crate) fn load(run: &Path, number: u32) -> Result<Option<Sealed>, String> {
        let path = record::dir(run, number).join("sealed.json");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("reading sealed.json: {e}")),
        };
        match serde_json::from_slice(&bytes).map_err(|e| format!("sealed.json: {e}"))? {
            StoredSeal::Halves { lens, fresh } => Ok(Some(Sealed { lens, fresh })),
            StoredSeal::Single(lens) => Ok(Some(Sealed { lens, fresh: Vec::new() })),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    collection: Option<String>,
    url: Option<String>,
    posted: Vec<usize>,
    ledger_row: bool,
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
    if a.scheduled {
        let known = match record::all(&ports.run_dir) {
            Ok(k) => k,
            Err(e) => return refuse(1, &e),
        };
        if let Some((n, state)) = record::scheduled_blocker(&known) {
            return refuse(1, &format!("scheduled review {n:03} is {}; it must close first (cr close --review {n:03})", state.as_str()));
        }
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
    let (scored, mut ledger) = match pick::candidates(ports, &ix, pool, now) {
        Ok(c) => c,
        Err(e) => return refuse(1, &e),
    };
    let mut rng = SeededRng::new(seed);
    let mut chosen: Vec<units::Unit> = Vec::new();
    for _ in 0..a.count {
        let left: Vec<pick::Scored> = scored.iter().filter(|s| !chosen.iter().any(|u| u.id == s.unit.id)).cloned().collect();
        let Some((i, _)) = pick::draw(&left, &ledger, &mut rng) else {
            return refuse(1, &format!("only {} of {} units are drawable: the rest are excluded, in flight, or reviewed at their current blobs", chosen.len(), a.count));
        };
        let unit = left[i].unit.clone();
        ledger.rows.push(Row { review: String::new(), kind: unit.kind.as_str().to_string(), unit_id: unit.id.clone(), commit: ix.commit.clone(), files: Vec::new(), opened_at: now, closed_at: None, closed_as: None, attention: None });
        chosen.push(unit);
    }
    let mut out = String::new();
    for unit in chosen {
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
            collection: None,
            url: None,
            unit: unit.clone(),
            sealed_sha256: None,
            staged_at: now,
            opened_at: None,
            published_at: None,
            closed_at: None,
        };
        if let Err(e) = record::save(&ports.run_dir, &r) {
            return refuse(1, &e);
        }
        out.push_str(&format!("{number:03} {}\nseal with: cr seal --review {number:03} < seal.json\n", unit.id));
    }
    done(out)
}

// ───────────────────────────── seal ─────────────────────────────

fn branch_files(tree: &dyn SourceTree, unit: &units::Unit) -> Result<Vec<(String, Vec<u8>)>, String> {
    unit.files
        .iter()
        .map(|f| tree.read(&f.path).map(|bytes| (f.path.clone(), bytes)).ok_or_else(|| format!("{} is not readable at the source commit", f.path)))
        .collect()
}

pub(crate) fn line_count(bytes: &[u8]) -> u32 {
    String::from_utf8_lossy(bytes).lines().count() as u32
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cover {
    does: String,
    look_first: String,
}

struct SealInput {
    cover: Cover,
    sealed: Sealed,
}

const SHAPE: &str = "stdin is one JSON object {\"does\": \"<sentences>\", \"look_first\": \"<sentences>\", \"lens\": [{path, line, body}, ...], \"fresh\": [{path, line, body}, ...]}";

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
    let half = |key: &str| -> Result<Vec<Comment>, String> {
        serde_json::from_value(obj.get(key).cloned().unwrap_or(Value::Null)).map_err(|e| format!("`{key}` is missing or not a JSON array of {{path, line, body}}: {e}"))
    };
    Ok(SealInput { cover, sealed: Sealed { lens: half("lens")?, fresh: half("fresh")? } })
}

fn validate(half: &str, comments: &[Comment], files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if comments.is_empty() {
        return Err(format!("the `{half}` half of the sealed review is empty: each half needs at least one comment"));
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

const SECTIONS: [&str; 7] = ["## What it does", "## What to look at first", "## Files", "## Who calls it", "## How often it changed", "## Incidents that touched it", "## Tests that exercise it"];

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

/// `(file, test fn, callee used)` for each test whose body calls a `pub` item of the unit.
fn tests_by_call_path(ix: &Index, u: &units::Unit, dependents: &[String]) -> Vec<(String, String, String)> {
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
                    found.push((path.clone(), test.clone(), used));
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
        "The Concierge's review of this unit is sealed and hashed below; it is held in this collection and revealed when the operator publishes his.\n\nsealed-sha256: {}\n\n",
        sealed_sha
    );
    m.push_str(&format!("{}\n\n{}\n\n", SECTIONS[0], cover.does));
    m.push_str(&format!("{}\n\n{}\n\n", SECTIONS[1], cover.look_first));
    m.push_str(&format!("{}\n\n", SECTIONS[2]));
    let mut total = 0;
    for (path, bytes) in files {
        let lines = line_count(bytes);
        total += lines;
        m.push_str(&format!("- `{path}`: {lines} lines\n"));
    }
    m.push_str(&format!("- Total: {total} lines\n"));
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
    let paths: Vec<String> = u.files.iter().map(|f| f.path.clone()).collect();
    m.push_str(&history::commits_line(&paths, tree, ports.beads.as_ref()));
    m.push_str(&history::rewritten_line(&paths, tree, now, ports.config.rewrite_days));
    m.push_str(&format!("- Authors: {}\n", if authors.is_empty() { "none recorded".to_string() } else { authors.join(", ") }));
    m.push_str(&format!("\n{}\n\n", SECTIONS[5]));
    m.push_str(&history::incident_lines(&paths, tree, ports.beads.as_ref()));
    m.push_str(&format!("\n{}\n\n", SECTIONS[6]));
    let tests = tests_by_call_path(ix, u, &dependents);
    if tests.is_empty() {
        m.push_str("None found by call path.\n");
    }
    let mut test_fns: BTreeSet<String> = BTreeSet::new();
    for (path, test, used) in &tests {
        m.push_str(&format!("- `{path}::{test}` calls `{used}`\n"));
        test_fns.insert(test.clone());
    }
    let removed = tree.removed_tests(&test_search_files(ix, u, &[]));
    test_fns.extend(removed.iter().map(|r| r.name.clone()));
    m.push('\n');
    m.push_str(&history::deleted_lines(&removed));
    m.push_str(&history::disabled_lines(ports.registry.as_ref(), &test_fns));
    m
}

fn load_progress(dir: &Path) -> Progress {
    std::fs::read_to_string(dir.join("progress.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_progress(dir: &Path, p: &Progress) -> Result<(), String> {
    record::write_atomic(&dir.join("progress.json"), serde_json::to_string_pretty(p).map_err(|e| e.to_string())?.as_bytes())
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
    if let Err(e) = &ports.source_url {
        return refuse(1, e);
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
            let stored = Sealed::load(run, number).ok().flatten();
            if Some(&input.sealed) != stored.as_ref() {
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
        for (half, comments) in [("lens", &input.sealed.lens), ("fresh", &input.sealed.fresh)] {
            if let Err(e) = validate(half, comments, &files) {
                return refuse(2, &e);
            }
        }
        let bytes = serde_json::to_string_pretty(&input.sealed).expect("comments serialise").into_bytes();
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
        return refuse(2, &format!("the cover note has no text under {h:?}; nothing is sent to the reader"));
    }

    let mut p = load_progress(&dir);
    let url = match post_review(&mut p, &dir, &r, &files, &body, &sealed_halves(run, number), ports) {
        Ok(u) => u,
        Err(e) => return refuse(1, &e),
    };
    if !p.ledger_row {
        let row = Row {
            review: format!("{number:03}"),
            kind: r.unit.kind.as_str().to_string(),
            unit_id: r.unit.id.clone(),
            commit: r.source_sha.clone(),
            files: r.unit.files.iter().map(|f| LedgerFile { path: f.path.clone(), blob: ix.blobs.get(&f.path).cloned().unwrap_or_default() }).collect(),
            opened_at: ports.clock.now_ms(),
            closed_at: None,
            closed_as: None,
            attention: None,
        };
        if let Err(e) = ledger::append(ports, &[row]).and_then(|()| {
            p.ledger_row = true;
            save_progress(&dir, &p)
        }) {
            return refuse(1, &e);
        }
    }
    r.collection = p.collection.clone();
    r.url = Some(url.clone());
    if let Err(e) = record::save(run, &r).and_then(|()| record::set_state(run, number, State::Open, ports.clock.now_ms()).map(|_| ())) {
        return refuse(1, &e);
    }
    done(format!("{url}\n"))
}

fn sealed_halves(run: &Path, number: u32) -> Sealed {
    Sealed::load(run, number).ok().flatten().unwrap_or(Sealed { lens: Vec::new(), fresh: Vec::new() })
}

/// Posts the collection, then each sealed comment held, recording each step so a rerun resumes.
fn post_review(p: &mut Progress, dir: &Path, r: &Review, files: &[(String, Vec<u8>)], body: &str, sealed: &Sealed, ports: &Ports) -> Result<String, String> {
    if p.collection.is_none() {
        let docs = files
            .iter()
            .map(|(path, bytes)| String::from_utf8(bytes.clone()).map(|text| Doc { path: path.clone(), kind: "code".to_string(), text }).map_err(|_| format!("{path} is not UTF-8; the reader holds text, and a file is posted whole or not at all")))
            .collect::<Result<Vec<Doc>, String>>()?;
        let source = Source { repository: ports.source_url.clone()?, commit: r.source_sha.clone() };
        let c = ports.margin.post_collection(&r.unit.title, body, &docs, Some(&source))?;
        p.collection = Some(c.id);
        p.url = Some(c.url);
        save_progress(dir, p)?;
    }
    let collection = p.collection.clone().unwrap_or_default();
    let labelled = sealed.lens.iter().map(|c| ("**His lens:** ", c)).chain(sealed.fresh.iter().map(|c| ("**Fresh read:** ", c)));
    for (i, (label, c)) in labelled.enumerate() {
        if p.posted.contains(&i) {
            continue;
        }
        let anchor = Anchor { path: c.path.clone(), first: c.line, last: c.line };
        ports.margin.post_comment(&collection, &anchor, &format!("{label}{}", c.body), true, None)?;
        p.posted.push(i);
        save_progress(dir, p)?;
    }
    Ok(p.url.clone().unwrap_or_default())
}

// ───────────────────────────── unseal ─────────────────────────────

pub fn unseal(number: u32, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    let r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    let published = done(format!("review {number:03} published\n"));
    match r.state {
        State::Published => return published,
        State::Open => {}
        state => return refuse(2, &format!("review {number:03} is {}; only an open review can be unsealed", state.as_str())),
    }
    let Some(collection) = &r.collection else {
        return refuse(2, &format!("review {number:03} has no collection; seal it first"));
    };
    let info = match ports.margin.collection(collection) {
        Ok(i) => i,
        Err(e) => return refuse(1, &e),
    };
    if info.state != "published" {
        return refuse(2, &format!("review {number:03} is not published in the reader; it waits on his Publish"));
    }
    match record::set_state(run, number, State::Published, info.published_at_ms.unwrap_or_else(|| ports.clock.now_ms())) {
        Ok(_) => published,
        Err(e) => refuse(1, &e),
    }
}

// ───────────────────────────── close ─────────────────────────────

pub fn close(number: u32, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    let mut r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    if r.state == State::Closed {
        return done(format!("review {number:03} already closed\n"));
    }
    let id = format!("{number:03}");
    let result = (|| -> Result<(), String> {
        if let (State::Open, Some(c)) = (r.state, &r.collection) {
            let info = ports.margin.collection(c)?;
            if info.state == "published" {
                r = record::set_state(run, number, State::Published, info.published_at_ms.unwrap_or_else(|| ports.clock.now_ms()))?;
            }
        }
        let how = if r.published_at.is_some() { ClosedAs::Published } else { ClosedAs::Unpublished };
        let now = ports.clock.now_ms();
        let latest = ledger::load(ports)?.latest_of(&id).cloned();
        let closing = match latest {
            Some(row) if row.closed_at.is_none() => Some(row.closing(now, how, if how == ClosedAs::Published { Some(ledger::attention_of(ports, &r)?) } else { None })),
            _ => None,
        };
        if let Some(c) = &r.collection {
            ports.margin.close(c)?;
        }
        if let Some(row) = closing {
            ledger::append(ports, &[row])?;
        }
        record::set_state(run, number, State::Closed, now).map(|_| ())
    })();
    match result {
        Ok(()) => done(format!("review {id} closed\n")),
        Err(e) => refuse(1, &e),
    }
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
    let cov = match status_coverage(ports) {
        Ok(c) => c,
        Err(e) => return refuse(1, &e),
    };
    let scores = match review_scores(&rs, ports) {
        Ok(s) => s,
        Err(e) => return refuse(1, &e),
    };
    let limit = ports.config.status_limit;
    let cut = |n: usize| n.saturating_sub(limit);
    if json_out {
        let reviews: Vec<Value> = rs
            .iter()
            .map(|r| json!({"number": format!("{:03}", r.number), "kind": r.kind.as_str(), "state": r.state.as_str(), "age": age(r.staged_at, now), "collection": r.collection, "url": r.url, "title": r.unit.title}))
            .collect();
        let tally = |a: &str, t: &Tally| json!({"area": a, "reviewed": t.reviewed, "current": t.current, "total": t.total});
        return done(format!(
            "{}\n",
            json!({"reviews": reviews, "scores": scores.iter().map(|(n, s)| json!({"review": format!("{n:03}"), "lens_predicted": s.lens_predicted, "his_comments": s.his_comments, "fresh_endorsed": s.fresh_endorsed, "fresh_points": s.fresh_points})).collect::<Vec<_>>(), "coverage": {
                "commit": cov.commit,
                "files": {"reviewed": cov.files.reviewed, "current": cov.files.current, "total": cov.files.total},
                "areas": cov.areas.iter().map(|(a, t)| tally(a, t)).collect::<Vec<_>>(),
                "kinds": cov.kinds.iter().map(|(k, r, t)| json!({"kind": k, "reviewed": r, "total": t})).collect::<Vec<_>>(),
                "never_reviewed": {"files": cov.never_reviewed.iter().take(limit).map(|n| json!({"path": n.path, "first_commit_ms": n.first_commit_ms})).collect::<Vec<_>>(), "more": cut(cov.never_reviewed.len())},
                "changed": {"files": cov.changed.iter().take(limit).map(|c| json!({"path": c.path, "review": c.review, "changed_lines": c.changed_lines})).collect::<Vec<_>>(), "more": cut(cov.changed.len())},
            }})
        ));
    }
    let pct = |r: u32, t: u32| if t == 0 { 0 } else { u64::from(r) * 100 / u64::from(t) };
    let mut out = String::from("open reviews\n");
    for r in rs {
        out.push_str(&format!("{:03} {} {} {} {} {}\n", r.number, r.kind.as_str(), r.state.as_str(), age(r.staged_at, now), r.url.as_deref().unwrap_or("-"), r.unit.title));
    }
    out.push_str("\nscores\n");
    for (n, s) in &scores {
        out.push_str(&format!("  {n:03} lens {}/{} fresh {}/{}\n", s.lens_predicted, s.his_comments, s.fresh_endorsed, s.fresh_points));
    }
    out.push_str(&format!("\ncoverage at {}: {}/{} files ({}%), {} at their current blob\n", cov.commit, cov.files.reviewed, cov.files.total, pct(cov.files.reviewed, cov.files.total), cov.files.current));
    for (a, t) in &cov.areas {
        out.push_str(&format!("  area {a}: {}/{} ({}%), {} current\n", t.reviewed, t.total, pct(t.reviewed, t.total), t.current));
    }
    for (k, r, t) in &cov.kinds {
        out.push_str(&format!("  kind {k}: {r}/{t} units ({}%)\n", pct(*r, *t)));
    }
    out.push_str("\noldest never-reviewed files\n");
    for n in cov.never_reviewed.iter().take(limit) {
        out.push_str(&format!("  {} first committed {}\n", n.path, n.first_commit_ms.map_or("unknown".to_string(), |m| format!("{} ago", age(m, now)))));
    }
    if cut(cov.never_reviewed.len()) > 0 {
        out.push_str(&format!("  … {} more\n", cut(cov.never_reviewed.len())));
    }
    out.push_str("\nchanged since review\n");
    for c in cov.changed.iter().take(limit) {
        out.push_str(&format!("  {} {} lines since review {}\n", c.path, c.changed_lines, c.review));
    }
    if cut(cov.changed.len()) > 0 {
        out.push_str(&format!("  … {} more\n", cut(cov.changed.len())));
    }
    done(out)
}

fn review_scores(rs: &[Review], ports: &Ports) -> Result<Vec<(u32, findings::Scores)>, String> {
    let mut out = Vec::new();
    for r in rs.iter().filter(|r| matches!(r.state, State::Published | State::Closed)) {
        if record::dir(&ports.run_dir, r.number).join("sealed.json").exists() {
            out.push((r.number, findings::scores(r, ports)?));
        }
    }
    Ok(out)
}

fn status_coverage(ports: &Ports) -> Result<Coverage, String> {
    let ix = Index::build(ports.tree.as_ref())?;
    let ledger = ledger::load(ports)?;
    let ledger = ledger.at_tree(ports.tree.as_ref())?;
    Ok(coverage::coverage(&ix, ports.tree.as_ref(), &ledger, &ports.config))
}
