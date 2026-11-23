//! `cr thread` and `cr reply`: one reader thread's code and conversation, and an answer in it.

use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record::{self, Review};

const CONTEXT: u32 = 5;

/// One comment of his on a review's collection: where it stands and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct His {
    pub path: String,
    pub line: u32,
    pub body: String,
}

fn refuse(code: i32, msg: &str) -> Outcome {
    Outcome { code, stdout: String::new(), stderr: format!("cr: {msg}\n") }
}

/// Every comment by someone who is not an agent, over the collection's threads. A review with no collection has none.
pub fn his_comments(ports: &Ports, r: &Review) -> Result<Vec<His>, String> {
    let Some(collection) = &r.collection else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for t in ports.margin.threads(collection)? {
        if !r.unit.files.iter().any(|f| f.path == t.anchor.path) {
            continue;
        }
        out.extend(t.comments.iter().filter(|c| !c.by_agent()).map(|c| His { path: t.anchor.path.clone(), line: t.anchor.first, body: c.body.clone() }));
    }
    Ok(out)
}

fn code_block(r: &Review, path: &str, first: u32, last: u32, ports: &Ports) -> Result<String, String> {
    r.unit.files.iter().find(|f| f.path == path).ok_or_else(|| format!("{path} is not a file of review {:03}'s unit", r.number))?;
    let bytes = ports.tree.at(&r.source_sha)?.read(path).ok_or_else(|| format!("{path} is not readable at {}", r.source_sha))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let (from, to) = (first.saturating_sub(CONTEXT).max(1), (last + CONTEXT).min(lines.len() as u32));
    let mut out = String::new();
    for n in from..=to {
        let mark = if (first..=last).contains(&n) { '>' } else { ' ' };
        out.push_str(&format!("{mark}{n:>5}  {}\n", lines[(n - 1) as usize]));
    }
    Ok(out)
}

pub fn thread(number: u32, id: &str, ports: &Ports) -> Outcome {
    let r = match record::load(&ports.run_dir, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    let Some(collection) = &r.collection else {
        return refuse(2, &format!("review {number:03} has no collection; seal it first"));
    };
    let t = match ports.margin.thread(collection, id) {
        Ok(t) => t,
        Err(e) => return refuse(1, &e),
    };
    let (first, last) = (t.anchor.first, t.anchor.last.max(t.anchor.first));
    let path = t.anchor.path.clone();
    if !r.unit.files.iter().any(|f| f.path == path) {
        return refuse(2, &format!("thread {id} is not on a document of review {number:03}"));
    }
    let mut out = format!("{path}:{first}-{last}\n");
    match code_block(&r, &path, first, last, ports) {
        Ok(code) => out.push_str(&code),
        Err(e) => out.push_str(&format!("(code unavailable: {e})\n")),
    }
    out.push_str("--- thread ---\n");
    for c in &t.comments {
        out.push_str(&format!("[{}] {} at {}:\n{}\n", c.id, c.author, c.created_at_ms, c.body.trim_end()));
    }
    Outcome { code: 0, stdout: out, stderr: String::new() }
}

pub fn reply(number: u32, id: &str, ports: &Ports) -> Outcome {
    if ports.stdin.trim().is_empty() {
        return refuse(2, "the reply is empty; give its text on stdin");
    }
    let r = match record::load(&ports.run_dir, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    let Some(collection) = &r.collection else {
        return refuse(2, &format!("review {number:03} has no collection; seal it first"));
    };
    let posted = ports.margin.thread(collection, id).and_then(|t| ports.margin.post_comment(collection, &t.anchor, &ports.stdin, false, Some(id)));
    match posted {
        Ok(c) => Outcome { code: 0, stdout: format!("{}\n", c.url), stderr: String::new() },
        Err(e) => refuse(1, &e),
    }
}
