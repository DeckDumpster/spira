//! The traceability habit: a touched test names a declared use case, a touched UC id is
//! declared, a UC added to the plan is named by some test. The catalogue and the suite header
//! are read by the crates the lint and the gate read them with.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use test_plan::{load_catalogues, LoadedCatalogue};

use crate::diff::{self, Ranges};
use crate::parse::Finding;

const CANDIDATES: usize = 3;

pub fn check(root: &Path, abs: &Path, rel: &str, touched: &Ranges) -> Result<Vec<Finding>, String> {
    let cats = load_catalogues(&root.join("docs").join("test-plan")).map_err(|e| e.join("; "))?;
    let text = std::fs::read_to_string(abs).map_err(|e| e.to_string())?;
    let referenced = referenced(root);
    if rel.ends_with(".toml") {
        return Ok(plan_reminders(&cats, &text, touched, &referenced));
    }
    let declared: BTreeSet<&str> = cats.iter().flat_map(|c| c.catalogue.use_case.iter().map(|u| u.id.as_str())).collect();
    let mut found = unknown_ids(&text, touched, &declared);
    if rel.ends_with(".sh") {
        if suite_select::header::uc_of(&text).is_empty() {
            found.push(unnamed(1, 1, "suite", &format!("{rel} names no UC id on its # covers: line"), rel, &cats, &referenced));
        }
    } else {
        found.extend(untraced_tests(&text, touched).into_iter().map(|(start, end, name)| {
            unnamed(start, end, &name, &format!("line {start}: test {name} names no UC id (// covers: UC-<area>-NN)"), rel, &cats, &referenced)
        }));
    }
    Ok(found)
}

fn unnamed(start: u32, end: u32, name: &str, head: &str, rel: &str, cats: &[LoadedCatalogue], referenced: &BTreeSet<String>) -> Finding {
    let cands = candidates(rel, cats, referenced);
    let text = if cands.is_empty() { head.to_string() } else { format!("{head}; candidates: {}", cands.join(", ")) };
    Finding { start, end, key: format!("untraced {name}"), text }
}

fn uc_tokens(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = line[i..].find("UC-") {
        let s = i + p;
        let mut e = s;
        while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'-') {
            e += 1;
        }
        let t = &line[s..e];
        let tb = t.as_bytes();
        if tb.len() >= 6 && tb[tb.len() - 3] == b'-' && tb[tb.len() - 2].is_ascii_digit() && tb[tb.len() - 1].is_ascii_digit() {
            out.push(t);
        }
        i = e.max(s + 3);
    }
    out
}

fn referenced(root: &Path) -> BTreeSet<String> {
    let out = Command::new("timeout")
        .args(["5", "git", "-C"])
        .arg(root)
        .args(["grep", "-h", "--untracked", "-I", "UC-", "--", "spira/test-*.sh", "*.rs"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines().flat_map(uc_tokens).map(str::to_string).collect()
}

fn unknown_ids(text: &str, touched: &Ranges, declared: &BTreeSet<&str>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i as u32 + 1;
        if !diff::hits(touched, n, n) {
            continue;
        }
        for t in uc_tokens(line).into_iter().filter(|t| !declared.contains(t)) {
            out.push(Finding { start: n, end: n, key: format!("unknown {t}"), text: format!("line {n}: {t} is not declared in docs/test-plan") });
        }
    }
    out
}

fn plan_reminders(cats: &[LoadedCatalogue], text: &str, touched: &Ranges, referenced: &BTreeSet<String>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i as u32 + 1;
        let Some(id) = line.trim().strip_prefix("id = \"").and_then(|r| r.split('"').next()) else { continue };
        let open = cats.iter().flat_map(|c| &c.catalogue.use_case).any(|u| u.id == id && u.uncovered.is_none());
        if open && diff::hits(touched, n, n) && !referenced.contains(id) {
            out.push(Finding { start: n, end: n, key: format!("unreferenced {id}"), text: format!("line {n}: {id} is in the plan and no test names it yet") });
        }
    }
    out
}

/// `(first line, last line, fn name)` of each `#[test]` fn touched by the edit whose attribute
/// block, signature and body hold no UC id.
fn untraced_tests(text: &str, touched: &Ranges) -> Vec<(u32, u32, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if !(t.starts_with("#[test]") || t.starts_with("#[tokio::test")) {
            continue;
        }
        let Some(f) = (i..lines.len().min(i + 8)).find(|&j| lines[j].contains("fn ")) else { continue };
        let name = lines[f].split("fn ").nth(1).and_then(|r| r.split('(').next()).unwrap_or("?").trim().to_string();
        let indent = &lines[f][..lines[f].len() - lines[f].trim_start().len()];
        let close = format!("{indent}}}");
        let end = (f..lines.len()).find(|&j| lines[j].trim_end() == close || (j == f && lines[j].trim_end().ends_with('}'))).unwrap_or(lines.len() - 1);
        let mut start = i;
        while start > 0 && (lines[start - 1].trim_start().starts_with("//") || lines[start - 1].trim_start().starts_with("#[")) {
            start -= 1;
        }
        let (a, b) = (start as u32 + 1, end as u32 + 1);
        if diff::hits(touched, a, b) && !lines[start..=end].iter().any(|l| !uc_tokens(l).is_empty()) {
            out.push((a, b, name));
        }
    }
    out
}

fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.len() >= 4).map(str::to_lowercase).collect()
}

fn candidates(rel: &str, cats: &[LoadedCatalogue], referenced: &BTreeSet<String>) -> Vec<String> {
    let path = words(rel);
    let mut scored: Vec<(usize, bool, &str)> = cats
        .iter()
        .flat_map(|c| c.catalogue.use_case.iter().map(move |u| (c, u)))
        .map(|(c, u)| {
            let score = path.intersection(&words(&format!("{} {}", c.catalogue.area, u.statement))).count();
            (score, !referenced.contains(&u.id), u.id.as_str())
        })
        .filter(|(score, _, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.cmp(a));
    scored.into_iter().take(CANDIDATES).map(|(_, _, id)| id.to_string()).collect()
}
