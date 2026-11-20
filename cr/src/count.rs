//! `cr count`: how often a pattern occurs in code at one commit, with a bounded list of sites.

use crate::glob;
use crate::ports::SourceTree;
use regex::Regex;

pub struct Site {
    pub path: String,
    pub line: u32,
    pub text: String,
}

pub struct Count {
    pub commit: String,
    pub total: usize,
    pub files: usize,
    pub searched: usize,
    pub skipped_comments: usize,
    pub sites: Vec<Site>,
    pub more: usize,
}

fn comment_marker(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()? {
        "rs" => Some("//"),
        "sh" | "toml" | "py" => Some("#"),
        _ => None,
    }
}

pub fn count(tree: &dyn SourceTree, re: &Regex, paths: &[String], exclude: &[String], limit: usize) -> Result<Count, String> {
    let commit = tree.commit()?;
    let mut files = tree.files()?;
    files.sort();
    let mut c = Count { commit, total: 0, files: 0, searched: 0, skipped_comments: 0, sites: vec![], more: 0 };
    for path in files {
        let Some(marker) = comment_marker(&path) else { continue };
        if glob::any(exclude, &path) || !(paths.is_empty() || glob::any(paths, &path)) {
            continue;
        }
        let bytes = tree.read(&path).ok_or_else(|| format!("cannot read {path} at commit {}", c.commit))?;
        c.searched += 1;
        let text = String::from_utf8_lossy(&bytes);
        let mut hit = false;
        for (i, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            if line.trim_start().starts_with(marker) {
                c.skipped_comments += 1;
                continue;
            }
            hit = true;
            c.total += 1;
            if c.sites.len() < limit {
                c.sites.push(Site { path: path.clone(), line: i as u32 + 1, text: line.trim().to_string() });
            }
        }
        c.files += usize::from(hit);
    }
    c.more = c.total - c.sites.len();
    Ok(c)
}
