//! `design-review`: one brain design page as one document of one Margin collection, a new version per revision.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

use margin_client::{Doc, Margin};
use runtime::ports::Clock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
mod tests;

pub struct Ports {
    pub run_dir: PathBuf,
    pub wiki: PathBuf,
    pub margin: Box<dyn Margin>,
    pub clock: Box<dyn Clock>,
    pub stdin: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub page: String,
    pub collection: String,
    pub document: String,
    pub version: u64,
    pub url: String,
    pub sha256: String,
    /// `open` or `closed`.
    pub state: String,
    pub posted_at_ms: u64,
}

const HELP: &str = "design-review — post a brain design page to the Margin reader for review\n\n  post   --page <path under the wiki>   post the page (cover note on stdin the first time), or its new text as a new version\n  reply  --page <path> --thread <id>     answer a thread with stdin\n  close  --page <path>                   close the page's collection\n  status                                 one line per posted page\n";

pub fn root(run_dir: &Path) -> PathBuf {
    run_dir.join("design-review")
}

fn record_path(run_dir: &Path, page: &str) -> PathBuf {
    root(run_dir).join(format!("{}.json", page.replace('/', "__")))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("record path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {}: {e}", tmp.display()))
}

fn load(run_dir: &Path, page: &str) -> Result<Option<Record>, String> {
    let path = record_path(run_dir, page);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

fn save(run_dir: &Path, r: &Record) -> Result<(), String> {
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    write_atomic(&record_path(run_dir, &r.page), text.as_bytes())
}

pub fn all(run_dir: &Path) -> Result<Vec<Record>, String> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(root(run_dir)) {
        Ok(e) => e.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("reading {}: {e}", root(run_dir).display())),
    };
    files.sort();
    files
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| format!("reading {}: {e}", p.display()))?).map_err(|e| format!("{}: {e}", p.display())))
        .collect()
}

/// The design record whose collection is `id`, a closed one included.
pub fn by_collection(run_dir: &Path, id: &str) -> Option<Record> {
    all(run_dir).ok()?.into_iter().find(|r| r.collection == id)
}

fn sha_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn out(stdout: String) -> Outcome {
    Outcome { code: 0, stdout, stderr: String::new() }
}

fn refuse(code: i32, msg: &str) -> Outcome {
    Outcome { code, stdout: String::new(), stderr: format!("design-review: {msg}\n") }
}

fn flags(verb: &str, argv: &[String], valued: &[&str]) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut values = std::collections::BTreeMap::new();
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        if valued.contains(&a.as_str()) {
            values.insert(a.clone(), it.next().ok_or(format!("{a} needs a value"))?.clone());
        } else if a.starts_with('-') {
            return Err(format!("unknown flag `{a}` for `design-review {verb}` (see -h)"));
        } else {
            return Err(format!("positional argument `{a}` is not accepted; name it: `--page {a}`"));
        }
    }
    Ok(values)
}

fn page_file(wiki: &Path, page: &str) -> Result<PathBuf, Outcome> {
    let p = Path::new(page);
    let clean = p.components().all(|c| matches!(c, Component::Normal(_)));
    if !clean || !p.starts_with("wiki") || p.components().count() < 2 {
        return Err(refuse(2, &format!("{page} is not a page under wiki/ of the wiki")));
    }
    let file = wiki.join(p);
    if !file.is_file() {
        return Err(refuse(2, &format!("{page}: no such file at {}", file.display())));
    }
    Ok(file)
}

fn title_of(page: &str, text: &str) -> String {
    text.lines().find_map(|l| l.strip_prefix("# ")).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| Path::new(page).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| page.to_string()))
}

pub fn run(argv: &[String], ports: &Ports) -> Outcome {
    let verb = argv.first().map(String::as_str);
    let result = match verb {
        Some("-h" | "--help") => return out(HELP.to_string()),
        Some("post") => flags("post", argv, &["--page"]).map(|f| post(f.get("--page"), ports)),
        Some("reply") => flags("reply", argv, &["--page", "--thread"]).map(|f| reply(f.get("--page"), f.get("--thread"), ports)),
        Some("close") => flags("close", argv, &["--page"]).map(|f| close(f.get("--page"), ports)),
        Some("status") => flags("status", argv, &[]).map(|_| status(ports)),
        Some(other) => Err(format!("unknown command `{other}`; the commands are post, reply, close, status (see -h)")),
        None => Err("no command; use one of post, reply, close, status (see -h)".to_string()),
    };
    result.unwrap_or_else(|e| refuse(2, &e))
}

fn post(page: Option<&String>, ports: &Ports) -> Outcome {
    let Some(page) = page else { return refuse(2, "--page <path under the wiki> is required") };
    let file = match page_file(&ports.wiki, page) {
        Ok(f) => f,
        Err(o) => return o,
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => return refuse(2, &format!("{}: {e}", file.display())),
    };
    let record = match load(&ports.run_dir, page) {
        Ok(r) => r,
        Err(e) => return refuse(1, &e),
    };
    let sha = sha_hex(&text);
    match record {
        Some(r) if r.state == "open" && r.sha256 == sha => out(format!("{}\n", r.url)),
        Some(r) if r.state == "open" => {
            let version = match ports.margin.post_version(&r.document, &text) {
                Ok(v) => v,
                Err(e) => return refuse(1, &e),
            };
            let next = Record { version, sha256: sha, posted_at_ms: ports.clock.now_ms(), ..r };
            match save(&ports.run_dir, &next) {
                Ok(()) => out(format!("{} version {version}\n", next.url)),
                Err(e) => refuse(1, &e),
            }
        }
        _ => {
            if ports.stdin.trim().is_empty() {
                return refuse(2, "the cover note is empty; give it on stdin");
            }
            let doc = Doc { path: page.clone(), kind: "prose".into(), text };
            let c = match ports.margin.post_collection(&title_of(page, &doc.text), &ports.stdin, &[doc], None) {
                Ok(c) => c,
                Err(e) => return refuse(1, &e),
            };
            let info = match ports.margin.collection(&c.id) {
                Ok(i) => i,
                Err(e) => return refuse(1, &e),
            };
            let Some(d) = info.documents.first() else { return refuse(1, "the reader returned a collection with no document") };
            let r = Record { page: page.clone(), collection: c.id, document: d.id.clone(), version: d.version, url: c.url, sha256: sha, state: "open".into(), posted_at_ms: ports.clock.now_ms() };
            match save(&ports.run_dir, &r) {
                Ok(()) => out(format!("{}\n", r.url)),
                Err(e) => refuse(1, &e),
            }
        }
    }
}

fn recorded(page: Option<&String>, ports: &Ports) -> Result<Record, Outcome> {
    let page = page.ok_or_else(|| refuse(2, "--page <path under the wiki> is required"))?;
    match load(&ports.run_dir, page) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(refuse(2, &format!("{page} has not been posted"))),
        Err(e) => Err(refuse(1, &e)),
    }
}

fn reply(page: Option<&String>, thread: Option<&String>, ports: &Ports) -> Outcome {
    let Some(thread) = thread.filter(|t| !t.trim().is_empty()) else { return refuse(2, "--thread <id> is required") };
    if ports.stdin.trim().is_empty() {
        return refuse(2, "the reply is empty; give its text on stdin");
    }
    let r = match recorded(page, ports) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let t = match ports.margin.thread(&r.collection, thread) {
        Ok(t) => t,
        Err(e) => return refuse(1, &e),
    };
    if t.document.id != r.document {
        return refuse(2, &format!("thread {thread} is not on {}", r.page));
    }
    match ports.margin.post_comment(&r.collection, &t.anchor, &ports.stdin, false, Some(thread)) {
        Ok(c) => out(format!("{}\n", c.url)),
        Err(e) => refuse(1, &e),
    }
}

fn close(page: Option<&String>, ports: &Ports) -> Outcome {
    let r = match recorded(page, ports) {
        Ok(r) => r,
        Err(o) => return o,
    };
    if let Err(e) = ports.margin.close(&r.collection) {
        return refuse(1, &e);
    }
    match save(&ports.run_dir, &Record { state: "closed".into(), ..r }) {
        Ok(()) => out(String::new()),
        Err(e) => refuse(1, &e),
    }
}

fn status(ports: &Ports) -> Outcome {
    match all(&ports.run_dir) {
        Ok(rs) => out(rs.iter().map(|r| format!("{} {} version {} {} at {}\n", r.page, r.state, r.version, r.url, r.posted_at_ms)).collect()),
        Err(e) => refuse(1, &e),
    }
}
