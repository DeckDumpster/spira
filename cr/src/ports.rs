//! Ports `cr` reads the world through. `Clock` and `Rng` are `runtime::ports`; `SourceTree`
//! answers for one commit of the home repository.

pub use margin_client::Margin;
pub use runtime::ports::{Clock, Rng, SeededRng, VirtualClock, WallClock};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    pub commits: u32,
    pub newest_ms: Option<u64>,
    pub oldest_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub at_ms: u64,
    pub subject: String,
    pub reverts: bool,
}

/// A `#[test]` fn a commit deleted from `path` and that no file of the unit holds today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedTest {
    pub path: String,
    pub name: String,
    pub sha: String,
    pub at_ms: u64,
}

pub trait SourceTree {
    fn commit(&self) -> Result<String, String>;
    fn files(&self) -> Result<Vec<String>, String>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
    fn blob(&self, path: &str) -> Option<String>;
    /// Commits touching `path` at or after `since_ms`, and the newest commit time overall.
    fn history(&self, path: &str, since_ms: u64) -> History;
    /// The names that committed `path`, sorted and distinct.
    fn authors(&self, path: &str) -> Vec<String>;
    /// Every commit touching `path`, all time, newest first.
    fn log(&self, path: &str) -> Vec<CommitInfo>;
    /// The author time of each line of `path` at this commit, in milliseconds.
    fn line_times(&self, path: &str) -> Vec<u64>;
    /// `#[test]` fns removed from `paths` in their history and absent from them today.
    fn removed_tests(&self, paths: &[String]) -> Vec<RemovedTest>;
    /// The same repository at another commit.
    fn at(&self, commit: &str) -> Result<Box<dyn SourceTree>, String>;
    /// Files renamed between `commit` and this tree's commit, old path to new.
    fn renamed_since(&self, commit: &str) -> Result<BTreeMap<String, String>, String>;
    /// Lines that differ between the blob `old_blob` and `path`'s blob here; `None` when unknown.
    fn changed_lines(&self, old_blob: &str, path: &str) -> Option<u32>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub page_lines: u32,
    pub exclude: Vec<String>,
    pub churn_days: u32,
    pub rewrite_days: u32,
    pub count_limit: usize,
    pub status_limit: usize,
}

/// The `issue_type` of each bead named, read through spira-lc; a bead the store lacks is absent.
pub trait Beads {
    fn types(&self, ids: &[String]) -> Result<BTreeMap<String, String>, String>;
    /// Of the beads named, those labelled `incident`, with their titles.
    fn incidents(&self, ids: &[String]) -> Result<BTreeMap<String, String>, String>;
}

const BEAD_CHUNK: usize = 50;

pub struct LcBeads {
    pub incident_label: String,
}

impl Beads for LcBeads {
    fn types(&self, ids: &[String]) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        for item in plan_items(ids)? {
            if let (Some(id), Some(kind)) = (item["id"].as_str(), item["issue_type"].as_str()) {
                out.insert(id.to_string(), kind.to_string());
            }
        }
        Ok(out)
    }
    fn incidents(&self, ids: &[String]) -> Result<BTreeMap<String, String>, String> {
        if self.incident_label.is_empty() {
            return Err("SPIRA_INCIDENT_LABEL is empty: cannot tell which beads are incidents".into());
        }
        Ok(incidents_in(&plan_items(ids)?, &self.incident_label))
    }
}

fn incidents_in(items: &[serde_json::Value], label: &str) -> BTreeMap<String, String> {
    let labelled = |i: &&serde_json::Value| i["labels"].as_array().is_some_and(|l| l.iter().any(|x| x == label));
    items.iter().filter(labelled).filter_map(|i| Some((i["id"].as_str()?.to_string(), i["title"].as_str().unwrap_or("(untitled)").to_string()))).collect()
}

fn plan_items(ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
    let lc = spira_store::lc_call::LcClient::configured()?;
    let mut all = Vec::new();
    for chunk in ids.chunks(BEAD_CHUNK) {
        let argv = ["plan".to_string(), "items".into(), "--ids".into(), chunk.join(",")];
        let reply = lc.call(&argv).map_err(String::from)?;
        let items = serde_json::from_str::<serde_json::Value>(reply.out.trim()).ok().and_then(|mut v| v.get_mut("items").map(serde_json::Value::take));
        let Some(serde_json::Value::Array(items)) = items.filter(|_| reply.code == 0) else {
            return Err(format!("spira-lc plan items: {}", reply.why()));
        };
        all.extend(items);
    }
    Ok(all)
}

/// Every test the registry ever disabled, with the first time (epoch seconds) it was.
pub trait Registry {
    fn disabled(&self) -> Result<Vec<(String, i64)>, String>;
}

pub struct SqliteRegistry {
    pub path: String,
}

impl Registry for SqliteRegistry {
    fn disabled(&self) -> Result<Vec<(String, i64)>, String> {
        if !Path::new(&self.path).exists() {
            return Err(format!("no registry file at {}", self.path));
        }
        test_registry::store::ever_disabled(&test_registry::open(&self.path)?)
    }
}

/// A fixed answer for tests and simulation.
#[derive(Clone)]
pub struct FixtureBeads {
    pub types: Result<BTreeMap<String, String>, String>,
    pub incidents: Result<BTreeMap<String, String>, String>,
}

impl Default for FixtureBeads {
    fn default() -> Self {
        FixtureBeads { types: Ok(BTreeMap::new()), incidents: Ok(BTreeMap::new()) }
    }
}

impl Beads for FixtureBeads {
    fn types(&self, _ids: &[String]) -> Result<BTreeMap<String, String>, String> {
        self.types.clone()
    }
    fn incidents(&self, _ids: &[String]) -> Result<BTreeMap<String, String>, String> {
        self.incidents.clone()
    }
}

#[derive(Clone)]
pub struct FixtureRegistry(pub Result<Vec<(String, i64)>, String>);

impl Default for FixtureRegistry {
    fn default() -> Self {
        FixtureRegistry(Ok(Vec::new()))
    }
}

impl Registry for FixtureRegistry {
    fn disabled(&self) -> Result<Vec<(String, i64)>, String> {
        self.0.clone()
    }
}

/// Commits `paths` in the git repository at `dir`, and only those paths.
pub trait Committer {
    fn commit(&self, dir: &Path, paths: &[&str], message: &str) -> Result<(), String>;
    /// Whether any commit in the repository at `dir` ever touched `path`.
    fn has_history(&self, dir: &Path, path: &str) -> Result<bool, String>;
}

pub struct GitCommitter;

impl Committer for GitCommitter {
    fn has_history(&self, dir: &Path, path: &str) -> Result<bool, String> {
        // batch-job: local git log in the findings repository, no network
        let out = Command::new("git").arg("-C").arg(dir).args(["log", "--format=%H", "--", path]).output().map_err(|e| format!("git log: {e}"))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            if why.contains("does not have any commits yet") {
                return Ok(false);
            }
            return Err(format!("git log in {}: {}", dir.display(), why.trim()));
        }
        Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
    }
    fn commit(&self, dir: &Path, paths: &[&str], message: &str) -> Result<(), String> {
        use std::io::Write;
        let git = |args: &[&str]| {
            // batch-job: local git add/diff/commit in the findings repository, no network
            let mut c = Command::new("git");
            c.arg("-C").arg(dir).args(args).arg("--").args(paths);
            c
        };
        let out = git(&["add"]).output().map_err(|e| format!("git add: {e}"))?;
        if !out.status.success() {
            return Err(format!("git add in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr).trim()));
        }
        let staged = git(&["diff", "--cached", "--quiet"]).status().map_err(|e| format!("git diff: {e}"))?;
        if staged.success() {
            return Ok(());
        }
        let mut child = git(&["commit", "-F", "-"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("git commit: {e}"))?;
        child.stdin.take().ok_or("git commit: no stdin")?.write_all(message.as_bytes()).map_err(|e| format!("git commit: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("git commit: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("git commit in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

pub struct Ports {
    pub run_dir: PathBuf,
    pub margin: Box<dyn Margin>,
    pub findings_dir: String,
    pub committer: Box<dyn Committer>,
    pub source_repo: String,
    /// The home repository's web URL, or why none could be found; `seal` refuses on the error.
    pub source_url: Result<String, String>,
    /// Standard input, read by `main` for the verb that takes it.
    pub stdin: String,
    pub clock: Box<dyn Clock>,
    pub tree: Box<dyn SourceTree>,
    pub beads: Box<dyn Beads>,
    pub registry: Box<dyn Registry>,
    pub config: Config,
    pub env_seed: Option<String>,
}

/// An in-memory tree for tests and simulation.
#[derive(Default, Clone)]
pub struct FixtureTree {
    pub commit_id: String,
    files: BTreeMap<String, String>,
    commits: HashMap<String, Vec<u64>>,
    authors: HashMap<String, Vec<String>>,
    logs: HashMap<String, Vec<CommitInfo>>,
    line_ages: HashMap<String, Vec<u64>>,
    removed: Vec<RemovedTest>,
    renames: HashMap<String, BTreeMap<String, String>>,
    changed: HashMap<(String, String), u32>,
}

impl FixtureTree {
    pub fn new(commit_id: &str) -> Self {
        FixtureTree { commit_id: commit_id.to_string(), ..Default::default() }
    }

    pub fn with_file(mut self, path: &str, text: &str) -> Self {
        self.files.insert(path.to_string(), text.to_string());
        self
    }

    pub fn with_authors(mut self, path: &str, names: &[&str]) -> Self {
        self.authors.insert(path.to_string(), names.iter().map(|n| n.to_string()).collect());
        self
    }

    pub fn with_log(mut self, path: &str, commits: Vec<CommitInfo>) -> Self {
        self.logs.insert(path.to_string(), commits);
        self
    }

    pub fn with_line_times(mut self, path: &str, times_ms: &[u64]) -> Self {
        self.line_ages.insert(path.to_string(), times_ms.to_vec());
        self
    }

    pub fn with_removed_test(mut self, path: &str, name: &str, sha: &str, at_ms: u64) -> Self {
        self.removed.push(RemovedTest { path: path.to_string(), name: name.to_string(), sha: sha.to_string(), at_ms });
        self
    }

    pub fn with_rename(mut self, commit: &str, old: &str, new: &str) -> Self {
        self.renames.entry(commit.to_string()).or_default().insert(old.to_string(), new.to_string());
        self
    }

    pub fn with_changed(mut self, path: &str, old_blob: &str, lines: u32) -> Self {
        self.changed.insert((path.to_string(), old_blob.to_string()), lines);
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
        History { commits: times.iter().filter(|t| **t >= since_ms).count() as u32, newest_ms: times.iter().copied().max(), oldest_ms: times.iter().copied().min() }
    }
    fn authors(&self, path: &str) -> Vec<String> {
        let mut v = self.authors.get(path).cloned().unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }
    fn log(&self, path: &str) -> Vec<CommitInfo> {
        self.logs.get(path).cloned().unwrap_or_default()
    }
    fn line_times(&self, path: &str) -> Vec<u64> {
        self.line_ages.get(path).cloned().unwrap_or_default()
    }
    fn removed_tests(&self, paths: &[String]) -> Vec<RemovedTest> {
        self.removed.iter().filter(|r| paths.contains(&r.path)).cloned().collect()
    }
    fn at(&self, commit: &str) -> Result<Box<dyn SourceTree>, String> {
        if commit == self.commit_id {
            Ok(Box::new(self.clone()))
        } else {
            Err(format!("the fixture tree holds commit {} only, not {commit}", self.commit_id))
        }
    }
    fn renamed_since(&self, commit: &str) -> Result<BTreeMap<String, String>, String> {
        Ok(self.renames.get(commit).cloned().unwrap_or_default())
    }
    fn changed_lines(&self, old_blob: &str, path: &str) -> Option<u32> {
        self.changed.get(&(path.to_string(), old_blob.to_string())).copied()
    }
}

/// `git -C <root>` against one resolved commit.
pub struct GitTree {
    root: PathBuf,
    commit: String,
    listing: RefCell<Option<BTreeMap<String, String>>>,
    contents: RefCell<Option<HashMap<String, Vec<u8>>>>,
    log: RefCell<Option<HashMap<String, Vec<u64>>>>,
    full_log: RefCell<Option<FullLog>>,
    renames: RefCell<HashMap<String, BTreeMap<String, String>>>,
}

/// Every commit, and for each path the indexes of the commits touching it.
type FullLog = (Vec<CommitInfo>, HashMap<String, Vec<usize>>);

const BATCH_EXTENSIONS: [&str; 4] = ["rs", "sh", "md", "toml"];

impl GitTree {
    pub fn new(root: &str, base: &str) -> Result<GitTree, String> {
        let root = PathBuf::from(root);
        let spec = format!("{base}^{{commit}}");
        let out = git(&root, &["rev-parse", "--verify", &spec])?;
        let commit = String::from_utf8_lossy(&out).trim().to_string();
        Ok(GitTree { root, commit, listing: RefCell::new(None), contents: RefCell::new(None), log: RefCell::new(None), full_log: RefCell::new(None), renames: RefCell::new(HashMap::new()) })
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

    fn load_full_log(&self) {
        if self.full_log.borrow().is_some() {
            return;
        }
        let mut commits = Vec::new();
        let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
        if let Ok(out) = git(&self.root, &["-c", "core.quotepath=off", "log", "--format=%H%x00%ct%x00%s%x00%b%x1e", "--name-only", &self.commit]) {
            let text = String::from_utf8_lossy(&out);
            let mut pending: Option<usize> = None;
            for chunk in text.split('\x1e') {
                let header_at = match chunk.find('\0') {
                    Some(nul) => chunk[..nul].rfind('\n').map_or(0, |n| n + 1),
                    None => chunk.len(),
                };
                if let Some(i) = pending {
                    for file in chunk[..header_at].lines().filter(|l| !l.is_empty()) {
                        by_path.entry(file.to_string()).or_default().push(i);
                    }
                }
                pending = None;
                let mut f = chunk[header_at..].splitn(4, '\0');
                if let (Some(sha), Some(ct), Some(subject), Some(body)) = (f.next(), f.next(), f.next(), f.next()) {
                    commits.push(CommitInfo { sha: sha.to_string(), at_ms: ct.parse::<u64>().unwrap_or(0).saturating_mul(1000), subject: subject.to_string(), reverts: body.lines().any(|l| l.starts_with("This reverts commit ")) });
                    pending = Some(commits.len() - 1);
                }
            }
        }
        *self.full_log.borrow_mut() = Some((commits, by_path));
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
        History { commits: times.iter().filter(|t| **t >= since_ms).count() as u32, newest_ms: times.iter().copied().max(), oldest_ms: times.iter().copied().min() }
    }
    fn authors(&self, path: &str) -> Vec<String> {
        let Ok(out) = git(&self.root, &["-c", "core.quotepath=off", "log", "--format=%an", &self.commit, "--", path]) else { return Vec::new() };
        let mut names: Vec<String> = String::from_utf8_lossy(&out).lines().filter(|l| !l.is_empty()).map(str::to_string).collect();
        names.sort();
        names.dedup();
        names
    }
    fn log(&self, path: &str) -> Vec<CommitInfo> {
        self.load_full_log();
        let cache = self.full_log.borrow();
        let Some((commits, by_path)) = cache.as_ref() else { return Vec::new() };
        by_path.get(path).into_iter().flatten().map(|i| commits[*i].clone()).collect()
    }
    fn line_times(&self, path: &str) -> Vec<u64> {
        let Ok(out) = git(&self.root, &["-c", "core.quotepath=off", "blame", "--porcelain", &self.commit, "--", path]) else { return Vec::new() };
        let mut time_of: HashMap<String, u64> = HashMap::new();
        let mut current = String::new();
        let mut times = Vec::new();
        for line in String::from_utf8_lossy(&out).lines() {
            if line.starts_with('\t') {
                times.push(time_of.get(&current).copied().unwrap_or(0));
            } else if let Some(t) = line.strip_prefix("author-time ") {
                time_of.insert(current.clone(), t.trim().parse::<u64>().unwrap_or(0).saturating_mul(1000));
            } else if let Some(sha) = line.split(' ').next().filter(|w| w.len() == 40 && w.bytes().all(|b| b.is_ascii_hexdigit())) {
                current = sha.to_string();
            }
        }
        times
    }
    fn removed_tests(&self, paths: &[String]) -> Vec<RemovedTest> {
        if paths.is_empty() {
            return Vec::new();
        }
        let mut args = vec!["-c", "core.quotepath=off", "log", "-p", "-U1", "--format=%x01%H %ct", self.commit.as_str(), "--"];
        args.extend(paths.iter().map(String::as_str));
        let Ok(out) = git(&self.root, &args) else { return Vec::new() };
        let today: String = paths.iter().filter_map(|p| self.read(p)).map(|b| String::from_utf8_lossy(&b).into_owned()).collect::<Vec<_>>().join("\n");
        let (mut sha, mut at_ms, mut file) = (String::new(), 0u64, String::new());
        let mut after_attr = false;
        let mut found: Vec<RemovedTest> = Vec::new();
        for line in String::from_utf8_lossy(&out).lines() {
            if let Some(h) = line.strip_prefix('\x01') {
                let mut w = h.split(' ');
                sha = w.next().unwrap_or_default().to_string();
                at_ms = w.next().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0).saturating_mul(1000);
                after_attr = false;
            } else if let Some(p) = line.strip_prefix("--- a/") {
                file = p.to_string();
            } else if line.starts_with("+++ ") || line.starts_with("--- ") {
            } else if let Some(removed) = line.strip_prefix('-') {
                let t = removed.trim();
                if after_attr {
                    let decl = t.strip_prefix("pub ").unwrap_or(t);
                    if let Some(name) = decl.strip_prefix("fn ").map(|r| r.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>()).filter(|n| !n.is_empty()) {
                        let held = today.contains(&format!("fn {name}("));
                        if !held && !found.iter().any(|f| f.path == file && f.name == name) {
                            found.push(RemovedTest { path: file.clone(), name, sha: sha.clone(), at_ms });
                        }
                    }
                }
                after_attr = t == "#[test]";
            } else {
                after_attr = false;
            }
        }
        found
    }
    fn at(&self, commit: &str) -> Result<Box<dyn SourceTree>, String> {
        Ok(Box::new(GitTree::new(&self.root.to_string_lossy(), commit)?))
    }
    fn renamed_since(&self, commit: &str) -> Result<BTreeMap<String, String>, String> {
        if let Some(m) = self.renames.borrow().get(commit) {
            return Ok(m.clone());
        }
        let out = git(&self.root, &["diff", "-M", "--name-status", "-z", commit, &self.commit])?;
        let mut map = BTreeMap::new();
        let mut toks = out.split(|b| *b == 0).filter(|t| !t.is_empty()).map(|t| String::from_utf8_lossy(t).into_owned());
        while let Some(status) = toks.next() {
            let paths = if status.starts_with('R') || status.starts_with('C') { 2 } else { 1 };
            let named: Vec<String> = toks.by_ref().take(paths).collect();
            if status.starts_with('R') && named.len() == 2 {
                map.insert(named[0].clone(), named[1].clone());
            }
        }
        self.renames.borrow_mut().insert(commit.to_string(), map.clone());
        Ok(map)
    }
    fn changed_lines(&self, old_blob: &str, path: &str) -> Option<u32> {
        let now = self.blob(path)?;
        let out = git(&self.root, &["diff", "--numstat", old_blob, &now]).ok()?;
        let line = String::from_utf8_lossy(&out).lines().next()?.to_string();
        let mut cols = line.split('\t');
        let (added, deleted): (u32, u32) = (cols.next()?.parse().ok()?, cols.next()?.parse().ok()?);
        Some(added.max(deleted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tl05c11)]
    #[test]
    fn incidents_in_keeps_only_beads_labelled_incident_and_never_an_empty_title() {
        let items: Vec<serde_json::Value> = serde_json::from_str(r#"[{"id":"sp-a","title":"lost write","labels":["incident"]},{"id":"sp-b","title":"x","labels":["other"]},{"id":"sp-c","title":"y"},{"id":"sp-d","labels":["incident"]}]"#).unwrap();
        let got = incidents_in(&items, "incident");
        assert!(incidents_in(&items, "alarm").is_empty());
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec![("sp-a".into(), "lost write".into()), ("sp-d".into(), "(untitled)".into())]);
    }

    #[test_id(td7c7z4)]
    #[test]
    fn an_unconfigured_incident_label_is_an_error_not_none_found() {
        let beads = LcBeads { incident_label: String::new() };
        assert!(beads.incidents(&["sp-a".to_string()]).unwrap_err().contains("SPIRA_INCIDENT_LABEL"));
    }
}
