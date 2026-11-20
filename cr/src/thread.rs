//! `cr thread` and `cr reply`: one review comment's code and conversation, and an answer in it.

use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record;
use crate::review;
use serde::{Deserialize, Serialize};
use serde_json::json;

const CONTEXT: u32 = 5;
const COMMENTS_FILE: &str = "comments.json";

/// One comment on a review's pull request as the forge showed it. `path` is `None` for a conversation comment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub in_reply_to: Option<u64>,
    pub author: String,
    pub bot: bool,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub body: String,
    pub created_at: u64,
}

pub fn comments_path(run: &std::path::Path, number: u32) -> std::path::PathBuf {
    record::dir(run, number).join(COMMENTS_FILE)
}

/// The line of the source file that line `line` of the review branch's copy shows. A file the
/// branch holds whole maps to itself; an oversized one is its regions, each gap shown as one marker
/// line, which maps to nothing.
pub fn source_line(regions: Option<&[(u32, u32)]>, total: u32, line: u32) -> Option<u32> {
    let Some(regions) = regions else { return Some(line) };
    let (mut next, mut shown) = (1u32, 0u32);
    for &(a, b) in regions {
        let (a, b) = (a.max(1), b.min(total));
        if a > b {
            continue;
        }
        if next < a {
            shown += 1;
        }
        let len = b - a + 1;
        if line > shown && line <= shown + len {
            return Some(a + (line - shown - 1));
        }
        shown += len;
        next = b + 1;
    }
    None
}

fn refuse(code: i32, msg: &str) -> Outcome {
    Outcome { code, stdout: String::new(), stderr: format!("cr: {msg}\n") }
}

fn root_of(all: &[Comment], id: u64) -> u64 {
    let mut cur = id;
    for _ in 0..all.len() {
        match all.iter().find(|c| c.id == cur).and_then(|c| c.in_reply_to) {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    cur
}

fn code_block(c: &Comment, number: u32, ports: &Ports) -> Result<String, String> {
    let r = record::load(&ports.run_dir, number)?;
    let (Some(path), Some(line)) = (&c.path, c.line) else { return Ok(String::new()) };
    let file = r.unit.files.iter().find(|f| &f.path == path).ok_or_else(|| format!("{path} is not a file of review {number:03}'s unit"))?;
    let bytes = ports.tree.at(&r.source_sha)?.read(path).ok_or_else(|| format!("{path} is not readable at {}", r.source_sha))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let at = source_line(file.regions.as_deref(), lines.len() as u32, line as u32).ok_or_else(|| format!("{path}:{line} is a marker of the oversized file, not source"))?;
    let (from, to) = (at.saturating_sub(CONTEXT).max(1), (at + CONTEXT).min(lines.len() as u32));
    let mut out = String::new();
    for n in from..=to {
        let mark = if n == at { '>' } else { ' ' };
        out.push_str(&format!("{mark}{n:>5}  {}\n", lines[(n - 1) as usize]));
    }
    Ok(out)
}

pub fn thread(number: u32, id: u64, ports: &Ports) -> Outcome {
    let path = comments_path(&ports.run_dir, number);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return refuse(1, &format!("review {number:03}: reading {}: {e}", path.display())),
    };
    let all: Vec<Comment> = match serde_json::from_str(&text) {
        Ok(c) => c,
        Err(e) => return refuse(1, &format!("review {number:03}: {}: {e}", path.display())),
    };
    let Some(asked) = all.iter().find(|c| c.id == id) else {
        return refuse(2, &format!("review {number:03} has no comment {id} in {COMMENTS_FILE}"));
    };
    let root = root_of(&all, id);
    let anchor = all.iter().find(|c| c.id == root).unwrap_or(asked);
    let mut out = match (&anchor.path, anchor.line) {
        (Some(p), Some(l)) => format!("{p}:{l}\n"),
        (Some(p), None) => format!("{p}\n"),
        _ => "conversation\n".to_string(),
    };
    match code_block(anchor, number, ports) {
        Ok(code) => out.push_str(&code),
        Err(e) => out.push_str(&format!("(code unavailable: {e})\n")),
    }
    out.push_str("--- thread ---\n");
    let mut members: Vec<&Comment> = all.iter().filter(|c| root_of(&all, c.id) == root).collect();
    members.sort_by_key(|c| (c.created_at, c.id));
    for c in members {
        out.push_str(&format!("[{}] {} at {}:\n{}\n", c.id, c.author, c.created_at, c.body.trim_end()));
    }
    Outcome { code: 0, stdout: out, stderr: String::new() }
}

pub fn reply(number: u32, id: u64, ports: &Ports) -> Outcome {
    if ports.stdin.trim().is_empty() {
        return refuse(2, "the reply is empty; give its text on stdin");
    }
    let r = match record::load(&ports.run_dir, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    if let Some(o) = review::repo_refusal(ports) {
        return o;
    }
    let Some(pr) = r.pr else {
        return refuse(2, &format!("review {number:03} has no pull request; seal it first"));
    };
    let ep = format!("repos/{}/{}/pulls/{pr}/comments/{id}/replies", ports.repo_owner, ports.repo_name);
    let posted = match review::gh(ports, "POST", &ep, Some(&json!({"body": ports.stdin}))) {
        Ok(v) => v,
        Err(e) => return refuse(1, &e),
    };
    match review::text_field(&posted, &["html_url"], &ep) {
        Ok(url) => Outcome { code: 0, stdout: format!("{url}\n"), stderr: String::new() },
        Err(e) => refuse(1, &e),
    }
}
