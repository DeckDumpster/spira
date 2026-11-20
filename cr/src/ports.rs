//! Ports `cr` reads the world through. `Clock` and `Rng` are `runtime::ports`; `SourceTree`
//! answers for one commit of the home repository.

use crate::ledger::Ledger;
pub use runtime::ports::{Clock, Rng, SeededRng, VirtualClock, WallClock};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    pub commits: u32,
    pub newest_ms: Option<u64>,
}

pub trait SourceTree {
    fn commit(&self) -> Result<String, String>;
    fn files(&self) -> Result<Vec<String>, String>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
    fn blob(&self, path: &str) -> Option<String>;
    /// Commits touching `path` at or after `since_ms`, and the newest commit time overall.
    fn history(&self, path: &str, since_ms: u64) -> History;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub page_lines: u32,
    pub oversize_pages: u32,
    pub exclude: Vec<String>,
    pub churn_days: u32,
    pub count_limit: usize,
}

impl Config {
    pub fn oversize_lines(&self) -> u32 {
        self.page_lines.saturating_mul(self.oversize_pages)
    }
}

pub struct Ports {
    pub clock: Box<dyn Clock>,
    pub tree: Box<dyn SourceTree>,
    pub ledger: Ledger,
    pub config: Config,
    pub env_seed: Option<String>,
}

/// An in-memory tree for tests and simulation.
#[derive(Default)]
pub struct FixtureTree {
    pub commit_id: String,
    files: BTreeMap<String, String>,
    commits: HashMap<String, Vec<u64>>,
}

impl FixtureTree {
    pub fn new(commit_id: &str) -> Self {
        FixtureTree { commit_id: commit_id.to_string(), ..Default::default() }
    }

    pub fn with_file(mut self, path: &str, text: &str) -> Self {
        self.files.insert(path.to_string(), text.to_string());
        self
    }

    pub fn with_commits(mut self, path: &str, times_ms: &[u64]) -> Self {
        self.commits.insert(path.to_string(), times_ms.to_vec());
        self
    }
}

pub fn blob_of(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl SourceTree for FixtureTree {
    fn commit(&self) -> Result<String, String> {
        Ok(self.commit_id.clone())
    }
    fn files(&self) -> Result<Vec<String>, String> {
        Ok(self.files.keys().cloned().collect())
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).map(|t| t.as_bytes().to_vec())
    }
    fn blob(&self, path: &str) -> Option<String> {
        self.files.get(path).map(|t| blob_of(t.as_bytes()))
    }
    fn history(&self, path: &str, since_ms: u64) -> History {
        let times = self.commits.get(path).map(Vec::as_slice).unwrap_or(&[]);
        History { commits: times.iter().filter(|t| **t >= since_ms).count() as u32, newest_ms: times.iter().copied().max() }
    }
}

/// `git -C <root>` against one resolved commit.
pub struct GitTree {
    root: PathBuf,
    commit: String,
    listing: RefCell<Option<BTreeMap<String, String>>>,
    contents: RefCell<Option<HashMap<String, Vec<u8>>>>,
    log: RefCell<Option<HashMap<String, Vec<u64>>>>,
}

const BATCH_EXTENSIONS: [&str; 4] = ["rs", "sh", "md", "toml"];

impl GitTree {
    pub fn new(root: &str, base: &str) -> Result<GitTree, String> {
        let root = PathBuf::from(root);
        let spec = format!("{base}^{{commit}}");
        let out = git(&root, &["rev-parse", "--verify", &spec])?;
        let commit = String::from_utf8_lossy(&out).trim().to_string();
        Ok(GitTree { root, commit, listing: RefCell::new(None), contents: RefCell::new(None), log: RefCell::new(None) })
    }

    fn listing(&self) -> Result<std::cell::Ref<'_, BTreeMap<String, String>>, String> {
        if self.listing.borrow().is_none() {
            let out = git(&self.root, &["ls-tree", "-r", "-z", &self.commit])?;
            let mut map = BTreeMap::new();
            for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                let text = String::from_utf8_lossy(rec);
                let Some((meta, path)) = text.split_once('\t') else { continue };
                let mut parts = meta.split(' ');
                let (_mode, kind, id) = (parts.next(), parts.next(), parts.next());
                if kind == Some("blob") {
                    map.insert(path.to_string(), id.unwrap_or_default().to_string());
                }
            }
            *self.listing.borrow_mut() = Some(map);
        }
        Ok(std::cell::Ref::map(self.listing.borrow(), |l| l.as_ref().expect("listing was just filled")))
    }

    fn load_contents(&self) {
        if self.contents.borrow().is_some() {
            return;
        }
        let mut found = HashMap::new();
        if let Ok(listing) = self.listing() {
            let wanted: Vec<(&String, &String)> = listing
                .iter()
                .filter(|(p, _)| p.rsplit('.').next().is_some_and(|e| BATCH_EXTENSIONS.contains(&e)))
                .collect();
            let input: String = wanted.iter().map(|(_, id)| format!("{id}\n")).collect();
            if let Ok(out) = git_stdin(&self.root, &["cat-file", "--batch"], input.as_bytes()) {
                let mut at = 0;
                for (path, _) in &wanted {
                    let Some(nl) = out[at..].iter().position(|b| *b == b'\n') else { break };
                    let header = String::from_utf8_lossy(&out[at..at + nl]).to_string();
                    let size: usize = header.rsplit(' ').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let body = at + nl + 1;
                    if body + size > out.len() {
                        break;
                    }
                    found.insert((*path).clone(), out[body..body + size].to_vec());
                    at = body + size + 1;
                }
            }
        }
        *self.contents.borrow_mut() = Some(found);
    }

    fn load_log(&self) {
        if self.log.borrow().is_some() {
            return;
        }
        let mut map: HashMap<String, Vec<u64>> = HashMap::new();
        if let Ok(out) = git(&self.root, &["-c", "core.quotepath=off", "log", "--name-only", "--format=@%ct", &self.commit]) {
            let mut stamp = 0u64;
            for line in String::from_utf8_lossy(&out).lines().filter(|l| !l.is_empty()) {
                if let Some(t) = line.strip_prefix('@') {
                    stamp = t.parse::<u64>().unwrap_or(0).saturating_mul(1000);
                } else {
                    map.entry(line.to_string()).or_default().push(stamp);
                }
            }
        }
        *self.log.borrow_mut() = Some(map);
    }
}

fn git(root: &std::path::Path, args: &[&str]) -> Result<Vec<u8>, String> {
    // batch-job: reads one commit of a local repository; git does not touch the network
    let out = Command::new("git").arg("-C").arg(root).args(args).output().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn git_stdin(root: &std::path::Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::Stdio;
    // batch-job: reads one commit of a local repository; git does not touch the network
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let mut stdin = child.stdin.take().ok_or("git stdin unavailable")?;
    let payload = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&payload));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    Ok(out.stdout)
}

impl SourceTree for GitTree {
    fn commit(&self) -> Result<String, String> {
        Ok(self.commit.clone())
    }
    fn files(&self) -> Result<Vec<String>, String> {
        Ok(self.listing()?.keys().cloned().collect())
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.load_contents();
        if let Some(b) = self.contents.borrow().as_ref().and_then(|c| c.get(path)) {
            return Some(b.clone());
        }
        let id = self.blob(path)?;
        git(&self.root, &["cat-file", "blob", &id]).ok()
    }
    fn blob(&self, path: &str) -> Option<String> {
        self.listing().ok()?.get(path).cloned()
    }
    fn history(&self, path: &str, since_ms: u64) -> History {
        self.load_log();
        let log = self.log.borrow();
        let times = log.as_ref().and_then(|l| l.get(path)).map(Vec::as_slice).unwrap_or(&[]);
        History { commits: times.iter().filter(|t| **t >= since_ms).count() as u32, newest_ms: times.iter().copied().max() }
    }
}
