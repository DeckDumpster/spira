//! `cr finding` records one verdict of a review; `cr findings` regenerates the review's page,
//! the pattern catalogue and the closeout comment from the records, and commits the two pages.

use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record::{self, Review};
use crate::review::{self, Comment, Sealed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DIR_KEY: &str = "SPIRA_CR_FINDINGS_DIR";
const AGREE_WITHIN: i64 = 3;
const CATALOGUE: &str = "patterns.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Question,
    Refactor,
    Condemn,
    Replicate,
    Style,
    Fix,
    Other,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Refactor => "refactor",
            Kind::Condemn => "condemn",
            Kind::Replicate => "replicate",
            Kind::Style => "style",
            Kind::Fix => "fix",
            Kind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Unit,
    Crate,
    Rust,
    Shell,
    All,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Unit => "unit",
            Scope::Crate => "crate",
            Scope::Rust => "rust",
            Scope::Shell => "shell",
            Scope::All => "all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exemplar {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Became {
    #[serde(default)]
    pub statutes: Vec<String>,
    #[serde(default)]
    pub catalogue: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default)]
    pub id: Option<String>,
    pub words: String,
    #[serde(default)]
    pub comment_id: Option<Value>,
    pub exemplar: Exemplar,
    pub kind: Kind,
    pub rule: String,
    pub scope: Scope,
    #[serde(default)]
    pub became: Became,
    #[serde(default)]
    pub generalised_by: Option<Value>,
    #[serde(default)]
    pub endorses: Option<u32>,
}

fn refuse(code: i32, msg: &str) -> Outcome {
    Outcome { code, stdout: String::new(), stderr: format!("cr: {msg}\n") }
}

fn findings_path(run: &Path, number: u32) -> PathBuf {
    record::dir(run, number).join("findings.jsonl")
}

pub(crate) fn load(run: &Path, number: u32) -> Result<Vec<Finding>, String> {
    let path = findings_path(run, number);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", path.display()))).collect()
}

fn save(run: &Path, number: u32, rows: &[Finding]) -> Result<(), String> {
    let mut text = String::new();
    for r in rows {
        text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    record::write_atomic(&findings_path(run, number), text.as_bytes())
}

pub fn record_finding(number: u32, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    let r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    let mut f: Finding = match serde_json::from_str(&ports.stdin) {
        Ok(f) => f,
        Err(e) => return refuse(2, &format!("stdin is not a finding {{id?, words, comment_id?, exemplar: {{path, line}}, kind, rule, scope, became, generalised_by?, endorses?}}: {e}")),
    };
    let widens = !f.became.statutes.is_empty() || !f.became.catalogue.is_empty();
    if f.kind == Kind::Fix && (f.scope != Scope::Unit || widens) {
        let field = if f.scope != Scope::Unit { "scope" } else if !f.became.statutes.is_empty() { "became.statutes" } else { "became.catalogue" };
        return refuse(2, &format!("a fix finding is for this unit only: {field} must be unit with no statute or catalogue entry"));
    }
    if f.generalised_by.as_ref().is_some_and(|g| !(g.is_number() || g.as_str().is_some_and(|s| !s.is_empty()))) {
        return refuse(2, "generalised_by is a comment id: a number or a non-empty string");
    }
    if f.kind == Kind::Other && widens && f.generalised_by.is_none() {
        return refuse(2, "an other finding names a statute or catalogue entry only with generalised_by, the id of the comment where the operator said to generalise it");
    }
    if !r.unit.files.iter().any(|u| u.path == f.exemplar.path) {
        return refuse(2, &format!("exemplar path {:?} is outside the unit of review {number:03}", f.exemplar.path));
    }
    let source = match ports.tree.at(&r.source_sha) {
        Ok(t) => t,
        Err(e) => return refuse(1, &e),
    };
    let Some(bytes) = source.read(&f.exemplar.path) else {
        return refuse(1, &format!("{} is not readable at {}", f.exemplar.path, r.source_sha));
    };
    let total = review::line_count(&bytes);
    if f.exemplar.line == 0 || f.exemplar.line > total {
        return refuse(2, &format!("exemplar line {} is past the end of {} at {} ({total} lines)", f.exemplar.line, f.exemplar.path, r.source_sha));
    }
    if let Some(n) = f.endorses {
        let fresh = match Sealed::load(run, number) {
            Ok(s) => s.map_or(0, |s| s.fresh.len()),
            Err(e) => return refuse(1, &e),
        };
        if n == 0 || n as usize > fresh {
            return refuse(2, &format!("endorses {n} is not a fresh-read comment: review {number:03} has {fresh}, numbered from 1"));
        }
    }
    let mut rows = match load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(1, &e),
    };
    let id = match f.id.clone() {
        Some(id) => {
            let Some(at) = rows.iter().position(|x| x.id.as_deref() == Some(id.as_str())) else {
                return refuse(2, &format!("finding {id} does not exist in review {number:03}; omit id to record a new one"));
            };
            f.id = Some(id.clone());
            rows[at] = f;
            id
        }
        None => {
            let next = rows.iter().filter_map(|x| x.id.as_deref()?.rsplit('-').next()?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
            let id = format!("{number:03}-{next}");
            f.id = Some(id.clone());
            rows.push(f);
            id
        }
    };
    if let Err(e) = save(run, number, &rows) {
        return refuse(1, &e);
    }
    Outcome { code: 0, stdout: format!("{id}\n"), stderr: String::new() }
}

fn list(label: &str, items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut s = format!("  - {label}:\n");
    for i in items {
        s.push_str(&format!("    - {i}\n"));
    }
    s
}

fn became_text(b: &Became) -> String {
    let parts: Vec<String> = [("statute", &b.statutes), ("catalogue", &b.catalogue), ("test", &b.tests), ("item", &b.items)]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(l, v)| format!("{l} {}", v.join(", ")))
        .collect();
    if parts.is_empty() {
        "nothing yet".to_string()
    } else {
        parts.join("; ")
    }
}

fn id_of(f: &Finding) -> &str {
    f.id.as_deref().unwrap_or("?")
}

fn page(r: &Review, rows: &[Finding]) -> String {
    let mut s = format!("---\ntype: note\nreview: {:03}\nsource: {}\npr: {}\n---\n\n# Review {:03} findings\n", r.number, r.source_sha, r.pr_url.as_deref().unwrap_or(""), r.number);
    for f in rows {
        s.push_str(&format!("\n## {}\n\n- words: {}\n- exemplar: {}:{} @ {}\n- kind: {}\n- rule: {}\n- scope: {}\n- became:\n", id_of(f), f.words, f.exemplar.path, f.exemplar.line, r.source_sha, f.kind.as_str(), f.rule, f.scope.as_str()));
        if let Some(c) = &f.comment_id {
            s.push_str(&format!("- comment: {}\n", c.as_str().map_or_else(|| c.to_string(), str::to_string)));
        }
        if let Some(g) = &f.generalised_by {
            s.push_str(&format!("- generalised_by: {}\n", g.as_str().map_or_else(|| g.to_string(), str::to_string)));
        }
        let b = &f.became;
        let body = [list("statute", &b.statutes), list("catalogue entry", &b.catalogue), list("test", &b.tests), list("item", &b.items)].concat();
        s.push_str(if body.is_empty() { "  - nothing yet\n" } else { &body });
    }
    s
}

#[derive(Default)]
struct Entry {
    scope: Vec<&'static str>,
    exemplars: Vec<String>,
    statutes: Vec<String>,
}

fn catalogue(run: &Path) -> Result<String, String> {
    let mut groups: BTreeMap<&str, BTreeMap<String, Entry>> = BTreeMap::new();
    for r in record::all(run)? {
        for f in load(run, r.number)? {
            let group = match f.kind {
                Kind::Condemn => "Condemned",
                Kind::Replicate => "Endorsed",
                Kind::Style => "Style",
                Kind::Other if f.generalised_by.is_some() && !f.became.catalogue.is_empty() => "Generalised by the operator",
                _ => continue,
            };
            let e = groups.entry(group).or_default().entry(f.rule.clone()).or_default();
            if !e.scope.contains(&f.scope.as_str()) {
                e.scope.push(f.scope.as_str());
            }
            let at = format!("{}:{} @ {}", f.exemplar.path, f.exemplar.line, r.source_sha);
            if !e.exemplars.contains(&at) {
                e.exemplars.push(at);
            }
            for s in &f.became.statutes {
                if !e.statutes.contains(s) {
                    e.statutes.push(s.clone());
                }
            }
        }
    }
    let mut s = String::from("---\ntype: note\n---\n\n# Patterns\n\nGenerated by `cr findings`; edit the findings, not this page.\n");
    for name in ["Condemned", "Endorsed", "Style", "Generalised by the operator"] {
        let Some(entries) = groups.get(name) else { continue };
        s.push_str(&format!("\n## {name}\n"));
        for (rule, e) in entries {
            s.push_str(&format!("\n### {rule}\n\n- scope: {}\n", e.scope.join(", ")));
            s.push_str(&list("exemplars", &e.exemplars));
            s.push_str(&list("statutes", &e.statutes));
        }
    }
    Ok(s)
}

#[derive(Deserialize)]
struct Author {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Theirs {
    path: String,
    #[serde(alias = "original_line")]
    line: Option<u32>,
    #[serde(default)]
    body: String,
    user: Option<Author>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    Lens,
    Fresh,
}

impl Half {
    fn tag(self) -> &'static str {
        match self {
            Half::Lens => "lens",
            Half::Fresh => "fresh",
        }
    }
}

struct Agreement {
    agreed: Vec<(Half, Comment, String)>,
    only_you: Vec<String>,
    only_concierge: Vec<(Half, Comment)>,
    his_comments: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scores {
    pub lens_predicted: usize,
    pub his_comments: usize,
    pub fresh_endorsed: usize,
    pub fresh_points: usize,
}

fn agreement(run: &Path, r: &Review) -> Result<Agreement, String> {
    let sealed = Sealed::load(run, r.number)?.unwrap_or(Sealed { lens: Vec::new(), fresh: Vec::new() });
    let path = record::dir(run, r.number).join("comments.json");
    let theirs: Vec<Theirs> = match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut theirs: Vec<(Theirs, bool)> = theirs.into_iter().filter(|t| t.user.as_ref().and_then(|u| u.kind.as_deref()) != Some("Bot")).map(|t| (t, false)).collect();
    let mut out = Agreement { agreed: Vec::new(), only_you: Vec::new(), only_concierge: Vec::new(), his_comments: theirs.len() };
    let tagged = sealed.lens.into_iter().map(|c| (Half::Lens, c)).chain(sealed.fresh.into_iter().map(|c| (Half::Fresh, c)));
    for (half, c) in tagged {
        let here = i64::from(c.line);
        let hit = theirs
            .iter()
            .enumerate()
            .filter(|(_, (t, used))| !used && t.path == c.path && t.line.is_some_and(|l| (i64::from(l) - here).abs() <= AGREE_WITHIN))
            .min_by_key(|(_, (t, _))| (i64::from(t.line.unwrap_or(0)) - here).abs())
            .map(|(i, _)| i);
        match hit {
            Some(i) => {
                theirs[i].1 = true;
                out.agreed.push((half, c, theirs[i].0.body.clone()));
            }
            None => out.only_concierge.push((half, c)),
        }
    }
    out.only_you = theirs.into_iter().filter(|(_, used)| !used).map(|(t, _)| format!("{}:{} {}", t.path, t.line.unwrap_or(0), t.body)).collect();
    Ok(out)
}

fn score(a: &Agreement, fresh_points: usize, rows: &[Finding]) -> Scores {
    let mut endorsed: Vec<u32> = rows.iter().filter_map(|f| f.endorses).collect();
    endorsed.sort_unstable();
    endorsed.dedup();
    Scores { lens_predicted: a.agreed.iter().filter(|(h, _, _)| *h == Half::Lens).count(), his_comments: a.his_comments, fresh_endorsed: endorsed.len(), fresh_points }
}

pub(crate) fn scores(r: &Review, ports: &Ports) -> Result<Scores, String> {
    let run = &ports.run_dir;
    let fresh = Sealed::load(run, r.number)?.map_or(0, |s| s.fresh.len());
    Ok(score(&agreement(run, r)?, fresh, &load(run, r.number)?))
}

fn closeout(r: &Review, rows: &[Finding], a: &Agreement, sc: &Scores) -> String {
    let mut s = format!("## Review {:03}: findings\n\n", r.number);
    if rows.is_empty() {
        s.push_str("No findings recorded.\n");
    }
    for f in rows {
        s.push_str(&format!("- {} [{}] {} → {}\n", id_of(f), f.kind.as_str(), f.rule, became_text(&f.became)));
    }
    s.push_str(&format!("\n## The Concierge's review\n\n- His lens: predicted {} of your {} comments\n- Fresh read: you endorsed {} of its {} points\n", sc.lens_predicted, sc.his_comments, sc.fresh_endorsed, sc.fresh_points));
    s.push_str("\n## Agreement\n\n### Agreed\n");
    for (half, c, theirs) in &a.agreed {
        s.push_str(&format!("- {}:{} — Concierge ({}): {} / you: {}\n", c.path, c.line, half.tag(), c.body, theirs));
    }
    s.push_str("\n### Only you\n");
    for t in &a.only_you {
        s.push_str(&format!("- {t}\n"));
    }
    s.push_str("\n### Only the Concierge\n");
    for (half, c) in &a.only_concierge {
        s.push_str(&format!("- {}:{} ({}) {}\n", c.path, c.line, half.tag(), c.body));
    }
    s
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

pub fn findings(number: u32, post: bool, ports: &Ports) -> Outcome {
    let run = &ports.run_dir;
    if ports.findings_dir.trim().is_empty() {
        return refuse(2, &format!("{DIR_KEY} is empty; name the findings directory in the config, it has no default"));
    }
    let r = match record::load(run, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    if post {
        if r.pr.is_none() {
            return refuse(2, &format!("review {number:03} has no pull request to post on; seal it first"));
        }
        if let Some(o) = review::repo_refusal(ports) {
            return o;
        }
    }
    let result = (|| -> Result<String, String> {
        let rows = load(run, number)?;
        let dir = Path::new(&ports.findings_dir);
        let review_name = format!("cr-{number:03}.md");
        let a = agreement(run, &r)?;
        write_if_changed(&dir.join(&review_name), &page(&r, &rows))?;
        write_if_changed(&dir.join(CATALOGUE), &catalogue(run)?)?;
        let message = format!("cr findings: review {number:03}, {} finding(s)\n\nRegenerated {review_name} and {CATALOGUE} from the findings records.\n", rows.len());
        ports.committer.commit(dir, &[review_name.as_str(), CATALOGUE], &message)?;
        let fresh = Sealed::load(run, number)?.map_or(0, |s| s.fresh.len());
        Ok(closeout(&r, &rows, &a, &score(&a, fresh, &rows)))
    })();
    let text = match result {
        Ok(t) => t,
        Err(e) => return refuse(1, &e),
    };
    if !post {
        return Outcome { code: 0, stdout: text, stderr: String::new() };
    }
    let ep = format!("repos/{}/{}/issues/{}/comments", ports.repo_owner, ports.repo_name, r.pr.unwrap_or(0));
    match review::gh(ports, "POST", &ep, Some(&json!({"body": text}))) {
        Ok(v) => match v.get("html_url").and_then(Value::as_str) {
            Some(url) => Outcome { code: 0, stdout: format!("{text}\n{url}\n"), stderr: String::new() },
            None => refuse(1, "the forge's reply to the comment has no html_url"),
        },
        Err(e) => refuse(1, &e),
    }
}
